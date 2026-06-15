//! Backend application state.
//!
//! Rust owns processes; the frontend owns layout (CLAUDE.md §2.5). `AppState`
//! is therefore just a registry of live PTYs keyed by `PaneId`. Scrollback,
//! the layout tree, and focus all live in the frontend.

use std::collections::HashMap;
use std::sync::Mutex;

use portable_pty::{ChildKiller, MasterPty};

/// Mirrors the frontend's `PaneId` (an opaque string).
pub type PaneId = String;

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
}

#[derive(Default)]
pub struct AppState {
    /// Live panes. Never hold this lock across a blocking PTY read or a
    /// `Channel` send (CLAUDE.md §11).
    pub panes: Mutex<HashMap<PaneId, PtyHandle>>,
}
