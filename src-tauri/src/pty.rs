//! PTY spawn/read plumbing via `portable-pty` (CLAUDE.md §4).
//!
//! The backend is a dumb, fast pipe: it opens a PTY, spawns the shell, and
//! streams raw bytes to the frontend over a Tauri `Channel`. Scrollback,
//! parsing, and rendering all live in xterm.js on the frontend.

use std::io::Read;
use std::sync::Arc;
use std::thread;

use portable_pty::{native_pty_system, PtySize};
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter};

use crate::error::{Error, Result};
use crate::profiles::ShellFamily;
use crate::shell::{build_command, detect_program, ShellConfig};
use crate::state::{OutputSink, PaneId, PtyHandle};

/// Event name emitted (with the `PaneId` payload) when a pane's shell exits, so
/// the frontend can close the pane. See `commands::spawn_pty` and the frontend
/// `onPtyExit` listener.
pub const PTY_EXIT_EVENT: &str = "pty-exit";

/// Read buffer size for the reader thread. Big enough to drain bursts (e.g. a
/// `cat` of a large file) without thousands of tiny channel sends.
const READ_BUF: usize = 32 * 1024;

/// Open a PTY, spawn the shell, start the dedicated reader thread, and return a
/// handle for the caller to store in `AppState`.
///
/// `on_output` is moved into the reader thread, which forwards each chunk and
/// exits on EOF or error.
///
/// Shell-exit detection does **not** rely on the reader reaching EOF. On Windows
/// (ConPTY) the master read keeps blocking as long as the [`MasterPty`] is held
/// alive — and we hold it in `AppState` for the life of the pane — so `exit`
/// would leave the pane "stuck" (CLAUDE.md §20). Instead a dedicated *waiter*
/// thread blocks on `child.wait()` and emits [`PTY_EXIT_EVENT`] the moment the
/// shell process exits, regardless of platform or reader state. The frontend
/// then closes the pane (which drops the master and unblocks the reader).
///
/// [`MasterPty`]: portable_pty::MasterPty
pub fn spawn(
    cols: u16,
    rows: u16,
    shell_cfg: &ShellConfig,
    on_output: Channel<Vec<u8>>,
    app: AppHandle,
    pane_id: PaneId,
) -> Result<PtyHandle> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| Error::Pty(e.to_string()))?;

    let family = ShellFamily::of(&detect_program(shell_cfg));
    let cmd = build_command(shell_cfg)?;
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| Error::Pty(e.to_string()))?;

    // The child holds its own slave fd; drop ours so the only thing keeping the
    // slave open is the child.
    drop(pair.slave);

    // A killer clone lets `close_pty` terminate the shell on manual close; the
    // waiter thread below owns the `Child` and does the actual reaping.
    let killer = child.clone_killer();

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| Error::Pty(e.to_string()))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| Error::Pty(e.to_string()))?;

    // The output sink fans each chunk out to the owning window's `Channel` and
    // to any remote subscribers. The window's channel is swappable so a pane can
    // move between windows: the reader always sends to whatever `Channel` is
    // currently installed, and `attach_pty` re-points it (CLAUDE.md §4). Shared
    // between this thread and the `PtyHandle`; locked only for the brief send.
    let output = Arc::new(OutputSink::new(on_output));
    let reader_output = Arc::clone(&output);

    // One dedicated reader thread per PTY. It blocks on the read (never under
    // the state lock, never on the async command path) and hands each chunk to
    // the sink, which fans it out. It is purely a pipe: it exits on read
    // EOF/error and is *not* what detects shell exit (see the waiter thread
    // below for why). `OutputSink::send` never fails and never blocks — a window
    // that closed for the instant before `attach_pty` installs its replacement,
    // or a remote client on a bad connection, must not be able to stall this
    // loop — so only read-EOF (master dropped by `close_pty`) stops the thread.
    thread::spawn(move || {
        let mut buf = [0u8; READ_BUF];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF: master dropped
                Ok(n) => reader_output.send(&buf[..n]),
                Err(_) => break,
            }
        }
    });

    // Dedicated waiter thread: block until the shell process exits, then tell
    // the frontend to close this pane. This is the reliable, cross-platform
    // exit signal — unlike read-EOF, `wait()` observes the process directly and
    // returns on Windows too, where the master read would otherwise block
    // forever while we hold the master in `AppState`. `wait()` also reaps the
    // child (no zombies). Emitting for an already-closed pane is harmless: the
    // frontend ignores unknown ids.
    thread::spawn(move || {
        let _ = child.wait();
        let _ = app.emit(PTY_EXIT_EVENT, &pane_id);
    });

    Ok(PtyHandle {
        writer,
        master: pair.master,
        killer,
        output,
        family,
    })
}
