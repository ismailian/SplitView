//! Backend application state.
//!
//! Rust owns processes; the frontend owns layout (CLAUDE.md §2.5). `AppState`
//! is therefore just a registry of live PTYs keyed by `PaneId`. Scrollback,
//! the layout tree, and focus all live in the frontend.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use portable_pty::{ChildKiller, MasterPty};
use tauri::ipc::Channel;

use crate::watch::DirWatch;

/// Mirrors the frontend's `PaneId` (an opaque string).
pub type PaneId = String;

/// The PTY's output sink: the reader thread forwards each chunk here, and
/// `attach_pty` swaps it to a different window's `Channel` when a pane is
/// detached or reattached. Shared (`Arc`) between the reader thread and the
/// `PtyHandle`; locked only for the brief send (never across the blocking read).
pub type OutputSink = Arc<Mutex<Channel<Vec<u8>>>>;

/// Everything we need to keep a single PTY alive and talk to it.
///
/// Neither the reader nor the `Child` itself is stored here: the reader lives
/// in a dedicated reader thread, and the `Child` is owned by a dedicated waiter
/// thread that blocks on `child.wait()` and fires `pty-exit` the moment the
/// shell exits (see `pty.rs`). We keep only a `ChildKiller` clone so a manual
/// `close_pty` can terminate the shell; the waiter thread then reaps it.
pub struct PtyHandle {
    /// Master writer — used briefly under the lock by `write_pty`.
    pub writer: Box<dyn std::io::Write + Send>,
    /// Master handle — used for `resize`.
    pub master: Box<dyn MasterPty + Send>,
    /// Kills the shell process on `close_pty`. Reaping happens in the waiter
    /// thread (see `pty.rs`), which owns the actual `Child`.
    pub killer: Box<dyn ChildKiller + Send + Sync>,
    /// Where the reader thread streams output. `attach_pty` re-points this to a
    /// new window's channel so a pane can move between windows (CLAUDE.md §4).
    pub output: OutputSink,
}

#[derive(Default)]
pub struct AppState {
    /// Live panes. Never hold this lock across a blocking PTY read or a
    /// `Channel` send (CLAUDE.md §11).
    pub panes: Mutex<HashMap<PaneId, PtyHandle>>,
    /// Serialized xterm.js buffers in flight between windows during a
    /// detach/reattach. Set by `stash_handoff`, consumed by `take_handoff`
    /// (the new window reads its pane's scrollback here rather than smuggling a
    /// large buffer through a URL query string).
    pub handoff: Mutex<HashMap<PaneId, String>>,
    /// Each window's file-sidebar watch, keyed by window label. Present only
    /// while that window's sidebar is open; removed when the window is destroyed
    /// even if its page never said so (see `lib.rs`).
    pub watchers: Mutex<HashMap<String, DirWatch>>,
}
