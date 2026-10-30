//! Tauri command handlers — the IPC surface (CLAUDE.md §10).
//!
//! These are the *only* entry points the frontend may call, and each one is
//! granted a matching permission in `capabilities/default.json`. No `unwrap`,
//! `expect`, or `panic!` here — every fallible path returns a typed `Error`.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use tauri::ipc::Channel;
use tauri::{AppHandle, State};

use crate::error::{Error, Result};
use crate::pty;
use crate::remote::{
    DeviceInfo, RemoteConfig, RemoteState, RemoteStatus, SnapshotReply, WindowPanes,
};
use crate::shell::{normalize_cwd, Alias, PromptInfo, PromptStyle, ShellConfig};
use crate::state::{AppState, PaneId};
use crate::watch::DirWatch;

/// Options for spawning a pane's shell. Field names are camelCase to match the
/// JS side; Tauri maps them onto this struct.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnOpts {
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub cwd: Option<String>,
    pub shell: Option<String>,
    pub prompt: Option<String>,
    /// `"parrot"` (default) or `"plain"`; selects the injected prompt.
    pub prompt_style: Option<String>,
    /// User-defined aliases to inject into the spawned shell.
    pub aliases: Option<Vec<AliasOpt>>,
    /// Which prompt info segments to show. Defaults to the signature look.
    pub prompt_info: Option<PromptInfoOpt>,
    /// Banner printed once before the first prompt.
    pub banner: Option<String>,
}

/// One `{ name, command }` alias from the frontend Settings.
#[derive(Debug, serde::Deserialize)]
pub struct AliasOpt {
    pub name: String,
    pub command: String,
}

/// Prompt info toggles from the frontend Settings (camelCase on the wire).
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptInfoOpt {
    pub user: bool,
    pub host: bool,
    pub dir: bool,
    pub branch: bool,
    pub time: bool,
    pub exit_status: bool,
    /// How much of the working directory to show: `0` = the whole path (middle
    /// elided when long), `n > 0` = only the last `n` segments. Defaulted so an
    /// older stored settings blob still deserializes.
    #[serde(default)]
    pub dir_segments: u8,
}

fn next_pane_id() -> PaneId {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    format!("pane-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Open a PTY, spawn the shell, start the reader thread streaming to `channel`,
/// and return the new pane id.
#[tauri::command]
pub fn spawn_pty(
    app: AppHandle,
    state: State<'_, AppState>,
    on_output: Channel<Vec<u8>>,
    opts: SpawnOpts,
) -> Result<PaneId> {
    let cols = opts.cols.unwrap_or(80);
    let rows = opts.rows.unwrap_or(24);
    let aliases = opts
        .aliases
        .unwrap_or_default()
        .into_iter()
        .map(|a| Alias {
            name: a.name,
            command: a.command,
        })
        .collect();
    let prompt_info = opts
        .prompt_info
        .map(|p| PromptInfo {
            user: p.user,
            host: p.host,
            dir: p.dir,
            branch: p.branch,
            time: p.time,
            exit_status: p.exit_status,
            dir_segments: p.dir_segments,
        })
        .unwrap_or_default();
    let shell_cfg = ShellConfig {
        program: opts.shell,
        cwd: opts.cwd,
        prompt: opts
            .prompt
            .unwrap_or_else(|| "{user} @ {cwd} $ ".to_string()),
        style: PromptStyle::from_opt(opts.prompt_style.as_deref()),
        aliases,
        prompt_info,
        banner: opts.banner.unwrap_or_default(),
    };

    // Assign the id up front so the reader thread can name this pane in the
    // `pty-exit` event it emits when the shell exits.
    let id = next_pane_id();
    let handle = pty::spawn(cols, rows, &shell_cfg, on_output, app, id.clone())?;
    state
        .panes
        .lock()
        .map_err(|_| Error::Poisoned)?
        .insert(id.clone(), handle);
    Ok(id)
}

/// Write keystrokes/bytes to a pane's PTY master. The lock is held only briefly.
#[tauri::command]
pub fn write_pty(state: State<'_, AppState>, pane_id: PaneId, data: Vec<u8>) -> Result<()> {
    let mut panes = state.panes.lock().map_err(|_| Error::Poisoned)?;
    let handle = panes
        .get_mut(&pane_id)
        .ok_or_else(|| Error::PaneNotFound(pane_id.clone()))?;
    handle.writer.write_all(&data)?;
    handle.writer.flush()?;
    Ok(())
}

/// Resize a pane's PTY. Must stay in lock-step with the xterm.js cols/rows or
/// full-screen TUIs corrupt (CLAUDE.md §20).
#[tauri::command]
pub fn resize_pty(state: State<'_, AppState>, pane_id: PaneId, cols: u16, rows: u16) -> Result<()> {
    let panes = state.panes.lock().map_err(|_| Error::Poisoned)?;
    let handle = panes
        .get(&pane_id)
        .ok_or_else(|| Error::PaneNotFound(pane_id.clone()))?;
    handle
        .master
        .resize(portable_pty::PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| Error::Pty(e.to_string()))?;
    Ok(())
}

/// Kill the shell, then drop the master so the reader thread observes EOF and
/// exits (CLAUDE.md §11, §20). The shell is reaped by its waiter thread, which
/// also fires `pty-exit` (harmless if the pane is already gone).
#[tauri::command]
pub fn close_pty(state: State<'_, AppState>, pane_id: PaneId) -> Result<()> {
    let handle = {
        let mut panes = state.panes.lock().map_err(|_| Error::Poisoned)?;
        panes.remove(&pane_id)
    };
    if let Some(mut handle) = handle {
        let _ = handle.killer.kill();
        // Dropping the handle drops the writer and master, fully closing the
        // master side so the reader thread observes EOF and exits.
        drop(handle);
    }
    Ok(())
}

/// List live pane ids. Mostly for debugging/recovery.
#[tauri::command]
pub fn list_panes(state: State<'_, AppState>) -> Result<Vec<PaneId>> {
    let panes = state.panes.lock().map_err(|_| Error::Poisoned)?;
    Ok(panes.keys().cloned().collect())
}

/// Re-point an existing pane's PTY output to a new window's channel. Used when a
/// pane is detached into its own window or reattached to the main window: the
/// PTY (and shell) keeps running untouched; only the sink the reader thread
/// streams to is swapped (CLAUDE.md §4). The lock is held only briefly.
#[tauri::command]
pub fn attach_pty(
    state: State<'_, AppState>,
    pane_id: PaneId,
    on_output: Channel<Vec<u8>>,
) -> Result<()> {
    let panes = state.panes.lock().map_err(|_| Error::Poisoned)?;
    let handle = panes
        .get(&pane_id)
        .ok_or_else(|| Error::PaneNotFound(pane_id.clone()))?;
    handle.output.set_local(on_output)?;
    Ok(())
}

/// Stash a pane's serialized xterm.js buffer so the window it is moving to can
/// pick it up (see `take_handoff`). Keyed by pane id. This carries scrollback
/// across a detach/reattach without smuggling a large buffer through a URL.
#[tauri::command]
pub fn stash_handoff(state: State<'_, AppState>, pane_id: PaneId, data: String) -> Result<()> {
    state
        .handoff
        .lock()
        .map_err(|_| Error::Poisoned)?
        .insert(pane_id, data);
    Ok(())
}

/// Take (and remove) a pane's stashed serialized buffer, if any. The receiving
/// window writes it into its fresh terminal before attaching the live stream so
/// scrollback survives the move.
#[tauri::command]
pub fn take_handoff(state: State<'_, AppState>, pane_id: PaneId) -> Result<Option<String>> {
    Ok(state
        .handoff
        .lock()
        .map_err(|_| Error::Poisoned)?
        .remove(&pane_id))
}

/// One entry of a directory listing, for the file sidebar.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntryInfo {
    /// File name only (no directory part).
    pub name: String,
    /// Absolute native path — the sidebar passes this straight back to
    /// `read_dir` when expanding, so it never has to join paths itself.
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    /// Dot-file (or hidden/system on Windows). The sidebar hides these until
    /// the user asks for them.
    pub hidden: bool,
}

/// A directory listing: the directory itself, its parent (for "go up"), and its
/// entries sorted directories-first then case-insensitively by name.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirListing {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<DirEntryInfo>,
    /// True when the directory held more than [`MAX_ENTRIES`] entries and the
    /// list was cut short (the sidebar says so rather than rendering 100k rows).
    pub truncated: bool,
}

/// Upper bound on entries returned for one directory. A `node_modules` with tens
/// of thousands of children must not turn into tens of thousands of DOM rows.
const MAX_ENTRIES: usize = 4096;

/// Dot-file, or hidden/system on Windows (where `C:\` is full of entries no one
/// wants in a project tree).
fn is_hidden(name: &str, meta: Option<&std::fs::Metadata>) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
        if let Some(m) = meta {
            return m.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0;
        }
    }
    let _ = meta;
    false
}

/// Directories first, then case-insensitive by name — the ordering every file
/// tree uses, and stable across platforms (`read_dir` order is not).
fn sort_entries(entries: &mut [DirEntryInfo]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// List a directory for the file sidebar (CLAUDE.md §1 — the sidebar opens on
/// whatever directory the focused terminal is in).
///
/// `path` is optional and may arrive in msys form (`/c/Users/john`) because it
/// comes from the shell's OSC 7 report; [`normalize_cwd`] converts it to a native
/// path first. `None` (or empty) lists the user's home directory. Errors — a
/// missing directory, no permission — surface as a typed error the sidebar
/// renders in place; nothing panics (CLAUDE.md §11).
///
/// `async` so the blocking filesystem walk runs off the main thread.
#[tauri::command]
pub async fn read_dir(path: Option<String>) -> Result<DirListing> {
    let dir: PathBuf = match path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(normalize_cwd(p)),
        None => dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")),
    };

    let mut entries: Vec<DirEntryInfo> = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        // A single unreadable entry must not sink the whole listing.
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        let file_type = entry.file_type().ok();
        let is_symlink = file_type.is_some_and(|t| t.is_symlink());
        // For a symlink, resolve the target to decide whether it expands like a
        // directory. A broken link resolves to nothing and reads as a file.
        let is_dir = match file_type {
            Some(t) if t.is_symlink() => std::fs::metadata(entry.path())
                .map(|m| m.is_dir())
                .unwrap_or(false),
            Some(t) => t.is_dir(),
            None => false,
        };
        let meta = entry.metadata().ok();
        entries.push(DirEntryInfo {
            hidden: is_hidden(&name, meta.as_ref()),
            path: entry.path().to_string_lossy().to_string(),
            name,
            is_dir,
            is_symlink,
        });
    }

    sort_entries(&mut entries);
    let truncated = entries.len() > MAX_ENTRIES;
    entries.truncate(MAX_ENTRIES);

    Ok(DirListing {
        parent: dir.parent().map(|p| p.to_string_lossy().to_string()),
        path: dir.to_string_lossy().to_string(),
        entries,
        truncated,
    })
}

/// Start this window's file-sidebar watch. Changed directories arrive on
/// `on_change` in batches, spelled exactly as they were passed to
/// [`fs_watch_dirs`]. Replaces any watch the window already had (a reloaded page
/// subscribes again). Watches nothing until `fs_watch_dirs` says what to.
///
/// `async` (like the other watch commands) so OS watch registration stays off
/// the main thread.
#[tauri::command]
pub async fn fs_watch(
    window: tauri::Window,
    state: State<'_, AppState>,
    on_change: Channel<Vec<String>>,
) -> Result<()> {
    let watch = DirWatch::new(on_change)?;
    let previous = state
        .watchers
        .lock()
        .map_err(|_| Error::Poisoned)?
        .insert(window.label().to_string(), watch);
    drop(previous); // outside the lock: dropping a watch talks to the OS
    Ok(())
}

/// Set the directories this window's sidebar displays — its root and open
/// folders, as native paths from `read_dir`. Only the difference from the last
/// set is registered or released. A no-op if the window has no watch.
#[tauri::command]
pub async fn fs_watch_dirs(
    window: tauri::Window,
    state: State<'_, AppState>,
    dirs: Vec<String>,
) -> Result<()> {
    let mut watchers = state.watchers.lock().map_err(|_| Error::Poisoned)?;
    match watchers.get_mut(window.label()) {
        Some(watch) => watch.set_dirs(dirs),
        None => Ok(()),
    }
}

/// Stop this window's sidebar watch (the sidebar closed): every OS watch is
/// released and its thread ends.
#[tauri::command]
pub async fn fs_unwatch(window: tauri::Window, state: State<'_, AppState>) -> Result<()> {
    let watch = state
        .watchers
        .lock()
        .map_err(|_| Error::Poisoned)?
        .remove(window.label());
    drop(watch);
    Ok(())
}

/// Open the OS-native file picker and return the absolute path of the selected
/// file, or `None` if the user cancelled.
///
/// Drives the dialog plugin's Rust API directly (so the frontend needs no dialog
/// permission). The plugin shows the dialog on the main thread and runs the
/// blocking wait on a dedicated thread, so this never blocks the UI event loop.
/// The frontend writes the returned path into the focused pane (CLAUDE.md §1,
/// the Win+O "insert path" action).
#[tauri::command]
pub async fn pick_file(app: AppHandle) -> Result<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app
        .dialog()
        .file()
        .set_title("Insert file path")
        .blocking_pick_file();
    Ok(picked.map(|p| p.to_string()))
}

// --- remote control (docs/remote-control-desktop.md) ------------------------
//
// The webview publishes what only it knows (the pane roster, a pane's screen)
// and relays what only it may do (anything that touches the layout tree). The
// backend owns the socket, the crypto and the device registry. See
// `remote::mod` for why the boundary sits exactly there.

/// Publish this window's pane roster so remote clients can see it.
///
/// Called from `RemoteBridge` on every layout change (debounced) and on a timer
/// while a session is live. Ignored entirely while remote control is off — a
/// list of every terminal you have open is not something to keep around when
/// nobody asked for the feature (CLAUDE.md §20).
#[tauri::command]
pub fn publish_panes(
    state: State<'_, AppState>,
    remote: State<'_, RemoteState>,
    snapshot: WindowPanes,
) -> Result<()> {
    if !remote.is_enabled() {
        return Ok(());
    }
    let seqs = state.output_seqs()?;
    remote.publish_window(snapshot, &seqs)
}

/// Answer a `remote-snapshot-request`: the serialized xterm.js buffer a newly
/// attached client needs before the live stream makes sense.
#[tauri::command]
pub fn provide_snapshot(
    remote: State<'_, RemoteState>,
    request_id: u64,
    pane_id: PaneId,
    serialized: String,
    cols: u16,
    rows: u16,
) -> Result<()> {
    // The request id already identifies the reply; the pane id travels with it
    // so the frontend and the log can be read without cross-referencing.
    let _ = pane_id;
    remote.provide_snapshot(
        request_id,
        SnapshotReply {
            serialized,
            cols,
            rows,
        },
    )
}

/// Everything the Settings "Remote" section renders.
#[tauri::command]
pub fn remote_status(remote: State<'_, RemoteState>) -> Result<RemoteStatus> {
    Ok(remote.status())
}

/// Turn remote control on: generate or load the TLS identity, bind, and (on a
/// LAN bind) start advertising over mDNS.
#[tauri::command]
pub fn remote_enable(
    app: AppHandle,
    remote: State<'_, RemoteState>,
    config: RemoteConfig,
) -> Result<RemoteStatus> {
    remote.enable(&app, config)
}

/// Turn it off: stop accepting, end every live session, forget the roster.
#[tauri::command]
pub fn remote_disable(app: AppHandle, remote: State<'_, RemoteState>) -> Result<RemoteStatus> {
    remote.disable(&app)
}

/// Issue a single-use pairing code (the user pressed "Pair a device").
#[tauri::command]
pub fn remote_pairing_start(
    app: AppHandle,
    remote: State<'_, RemoteState>,
) -> Result<RemoteStatus> {
    remote.start_pairing(&app)
}

/// Drop the live pairing code (the user closed the dialog).
#[tauri::command]
pub fn remote_pairing_cancel(
    app: AppHandle,
    remote: State<'_, RemoteState>,
) -> Result<RemoteStatus> {
    remote.cancel_pairing(&app)
}

/// The answer to a `remote-pairing-request` prompt. Nothing is issued until
/// this says `true` (`docs/remote-control-desktop.md` §7.2).
#[tauri::command]
pub fn remote_pairing_respond(
    remote: State<'_, RemoteState>,
    request_id: u64,
    approve: bool,
) -> Result<()> {
    remote.respond_to_pairing(request_id, approve)
}

/// Paired devices, newest first.
#[tauri::command]
pub fn remote_devices(remote: State<'_, RemoteState>) -> Result<Vec<DeviceInfo>> {
    Ok(remote.status().devices)
}

/// Forget a device **and** cut any session using it.
#[tauri::command]
pub fn remote_revoke_device(
    app: AppHandle,
    remote: State<'_, RemoteState>,
    device_id: String,
) -> Result<RemoteStatus> {
    remote.revoke_device(&app, &device_id)
}

/// End one live session without revoking its device.
#[tauri::command]
pub fn remote_disconnect_session(
    app: AppHandle,
    remote: State<'_, RemoteState>,
    session_id: u64,
) -> Result<RemoteStatus> {
    remote.disconnect_session(&app, session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool) -> DirEntryInfo {
        DirEntryInfo {
            name: name.to_string(),
            path: format!("/tmp/{name}"),
            is_dir,
            is_symlink: false,
            hidden: name.starts_with('.'),
        }
    }

    #[test]
    fn sorts_directories_first_then_case_insensitively() {
        let mut entries = vec![
            entry("readme.md", false),
            entry("Zebra", true),
            entry("apple", true),
            entry("Build.rs", false),
        ];
        sort_entries(&mut entries);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["apple", "Zebra", "Build.rs", "readme.md"]);
    }

    #[test]
    fn dot_files_are_hidden() {
        assert!(is_hidden(".bashrc", None));
        assert!(!is_hidden("src", None));
    }

    #[test]
    fn read_dir_lists_a_real_directory() {
        let base = std::env::temp_dir().join(format!("splitview-readdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("zdir")).expect("create test dir");
        std::fs::write(base.join("a.txt"), b"x").expect("write test file");
        std::fs::write(base.join(".dotfile"), b"x").expect("write test dotfile");

        let listing =
            tauri::async_runtime::block_on(read_dir(Some(base.to_string_lossy().to_string())))
                .expect("listing");

        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["zdir", ".dotfile", "a.txt"]);
        assert!(listing.entries[0].is_dir);
        assert!(listing.entries[1].hidden);
        assert!(!listing.truncated);
        assert!(listing.parent.is_some());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn read_dir_reports_a_missing_directory_instead_of_panicking() {
        let missing = std::env::temp_dir().join("splitview-does-not-exist-ever");
        let result =
            tauri::async_runtime::block_on(read_dir(Some(missing.to_string_lossy().to_string())));
        assert!(result.is_err());
    }
}
