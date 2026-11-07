//! Backend application state.
//!
//! Rust owns processes; the frontend owns layout (CLAUDE.md §2.5). `AppState`
//! is therefore just a registry of live PTYs keyed by `PaneId`. Scrollback,
//! the layout tree, and focus all live in the frontend.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use portable_pty::{ChildKiller, MasterPty};
use tauri::ipc::Channel;
use tokio::sync::broadcast;

use crate::error::Error;
use crate::profiles::ShellFamily;
use crate::watch::DirWatch;

/// Mirrors the frontend's `PaneId` (an opaque string).
pub type PaneId = String;

/// How many chunks a remote subscriber may fall behind before it is lagged out.
/// A lagged receiver loses history and resyncs (see `remote::session`); it never
/// applies backpressure to the reader thread, because a phone on a bad
/// connection must not be able to stall the laptop's terminal.
const REMOTE_BACKLOG: usize = 128;

/// A PTY's output sink: the reader thread forwards every chunk here and it fans
/// the chunk out to
///
/// - **the owning window's `Channel`** (`local`), swapped by `attach_pty` when a
///   pane moves between windows — semantics unchanged from before remote
///   control existed, and
/// - **remote subscribers** (`remote`), a `tokio::sync::broadcast` so a session
///   can be added or dropped without the reader thread knowing.
///
/// `send` is synchronous and non-blocking on both paths, which is what keeps the
/// reader thread in `pty.rs` the same dumb blocking loop it always was
/// (CLAUDE.md §11, §20). Locks are held only for the duration of a send, never
/// across the blocking read.
pub struct OutputSink {
    /// The owning window's channel.
    local: Mutex<Channel<Vec<u8>>>,
    /// Remote subscribers. Bounded; a slow receiver is lagged, not waited on.
    remote: broadcast::Sender<Bytes>,
    /// Monotonic chunk counter, surfaced in the remote roster so a client can
    /// cheaply tell whether a pane it is *not* watching has produced output.
    seq: AtomicU64,
}

impl OutputSink {
    pub fn new(local: Channel<Vec<u8>>) -> Self {
        let (remote, _) = broadcast::channel(REMOTE_BACKLOG);
        Self {
            local: Mutex::new(local),
            remote,
            seq: AtomicU64::new(0),
        }
    }

    /// Fan one chunk out. Called by the reader thread for every read.
    ///
    /// A send failure on either path is deliberately ignored: during a detach the
    /// window that owned the channel may be gone for the instant before
    /// `attach_pty` installs the new one, and a broadcast with no receivers is
    /// the normal case while remote control is off.
    pub fn send(&self, chunk: &[u8]) {
        self.seq.fetch_add(1, Ordering::Relaxed);
        if let Ok(local) = self.local.lock() {
            let _ = local.send(chunk.to_vec());
        }
        // One atomic load is the entire cost of the remote path while nothing is
        // subscribed — no allocation, no copy.
        if self.remote.receiver_count() > 0 {
            let _ = self.remote.send(Bytes::copy_from_slice(chunk));
        }
    }

    /// Re-point the owning window's channel (`attach_pty`).
    pub fn set_local(&self, local: Channel<Vec<u8>>) -> Result<(), Error> {
        *self.local.lock().map_err(|_| Error::Poisoned)? = local;
        Ok(())
    }

    /// Subscribe a remote session to this pane's output.
    pub fn subscribe(&self) -> broadcast::Receiver<Bytes> {
        self.remote.subscribe()
    }

    /// Chunks written so far. Only ever compared against itself.
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }
}

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
    /// Where the reader thread streams output: the owning window's channel plus
    /// any remote subscribers (CLAUDE.md §4, §21).
    pub output: Arc<OutputSink>,
    /// Which shell syntax runs in this PTY. Travels with the pane when it moves
    /// between windows (`attach_pty` returns it), so the window it lands in
    /// still quotes inserted paths the way that shell expects.
    pub family: ShellFamily,
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

impl AppState {
    /// Subscribe to a pane's output and read its current chunk counter, without
    /// holding the `panes` lock any longer than the lookup itself.
    pub fn subscribe_pane(
        &self,
        pane_id: &str,
    ) -> Result<Option<(broadcast::Receiver<Bytes>, u64)>, Error> {
        let panes = self.panes.lock().map_err(|_| Error::Poisoned)?;
        Ok(panes
            .get(pane_id)
            .map(|handle| (handle.output.subscribe(), handle.output.seq())))
    }

    /// Current chunk counter for every live pane, for the remote roster.
    pub fn output_seqs(&self) -> Result<HashMap<PaneId, u64>, Error> {
        let panes = self.panes.lock().map_err(|_| Error::Poisoned)?;
        Ok(panes
            .iter()
            .map(|(id, handle)| (id.clone(), handle.output.seq()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A `Channel<Vec<u8>>` that records what was sent to it, so a test can
    /// assert the local and remote paths see the *same* bytes.
    fn recording_channel() -> (Channel<Vec<u8>>, mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::channel();
        let channel = Channel::new(move |body: tauri::ipc::InvokeResponseBody| {
            let bytes = match body {
                tauri::ipc::InvokeResponseBody::Json(json) => {
                    serde_json::from_str(&json).unwrap_or_default()
                }
                tauri::ipc::InvokeResponseBody::Raw(raw) => raw,
            };
            let _ = tx.send(bytes);
            Ok(())
        });
        (channel, rx)
    }

    #[test]
    fn fans_identical_bytes_to_the_window_and_a_remote_subscriber() {
        let (channel, local) = recording_channel();
        let sink = OutputSink::new(channel);
        let mut remote = sink.subscribe();

        // Non-UTF-8 on purpose: terminal output is arbitrary bytes (CLAUDE.md §10).
        let chunk = [0x1b, 0x5b, 0x33, 0x31, 0x6d, 0xff, 0xfe, 0x00, 0x41];
        sink.send(&chunk);

        assert_eq!(local.recv().expect("local chunk"), chunk.to_vec());
        assert_eq!(
            remote.try_recv().expect("remote chunk").as_ref(),
            chunk.as_slice()
        );
    }

    #[test]
    fn counts_chunks_even_with_no_remote_subscriber() {
        let (channel, _local) = recording_channel();
        let sink = OutputSink::new(channel);
        assert_eq!(sink.seq(), 0);
        sink.send(b"a");
        sink.send(b"b");
        assert_eq!(sink.seq(), 2);
    }

    #[test]
    fn a_lagging_subscriber_never_blocks_the_producer() {
        let (channel, local) = recording_channel();
        let sink = OutputSink::new(channel);
        let mut slow = sink.subscribe();

        // Overrun the backlog without ever reading: the reader thread must not
        // notice, and the window's channel must still receive everything.
        for i in 0..(REMOTE_BACKLOG * 2) {
            sink.send(&[i as u8]);
        }
        for i in 0..(REMOTE_BACKLOG * 2) {
            assert_eq!(local.recv().expect("local chunk"), vec![i as u8]);
        }

        // The slow subscriber is told it lost data instead of having stalled us.
        match slow.try_recv() {
            Err(broadcast::error::TryRecvError::Lagged(n)) => assert!(n > 0),
            other => panic!("expected Lagged, got {other:?}"),
        }
    }

    #[test]
    fn set_local_re_points_the_window_channel() {
        let (first, first_rx) = recording_channel();
        let (second, second_rx) = recording_channel();
        let sink = OutputSink::new(first);

        sink.send(b"one");
        sink.set_local(second).expect("swap");
        sink.send(b"two");

        assert_eq!(first_rx.recv().expect("first"), b"one".to_vec());
        assert!(first_rx.try_recv().is_err());
        assert_eq!(second_rx.recv().expect("second"), b"two".to_vec());
    }
}
