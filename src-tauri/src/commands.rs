//! Tauri command handlers — the IPC surface (CLAUDE.md §10).
//!
//! These are the *only* entry points the frontend may call, and each one is
//! granted a matching permission in `capabilities/default.json`. No `unwrap`,
//! `expect`, or `panic!` here — every fallible path returns a typed `Error`.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

use tauri::ipc::Channel;
use tauri::{AppHandle, State};

use crate::error::{Error, Result};
use crate::pty;
use crate::shell::{Alias, PromptInfo, PromptStyle, ShellConfig};
use crate::state::{AppState, PaneId};

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
    *handle.output.lock().map_err(|_| Error::Poisoned)? = on_output;
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
