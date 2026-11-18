//! The process side of SplitView: live PTYs, their reader/waiter threads, and the
//! remote-control server. The UI holds an `Arc<Hub>`; so do the reader threads
//! and remote sessions.
//!
//! Rust owns processes, the UI owns layout (CLAUDE.md §2.5): nothing here knows
//! what a split is. Anything that changes the layout — a shell exiting, a remote
//! client asking for a pane — is sent to the UI thread as a [`UiEvent`].

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, OnceLock};
use std::thread;

use bytes::Bytes;
use parking_lot::Mutex;
use portable_pty::{ChildKiller, MasterPty, PtySize, native_pty_system};
use sv_core::layout::PaneId;
use sv_core::osc::{Osc, OscScanner};
use sv_core::quote::ShellFamily;
use sv_term::{Size, TermEvent, Terminal};
use tokio::sync::broadcast;

use crate::error::{Error, Result};
use crate::remote::{RemoteState, RemoteUi};
use crate::shell::{ShellConfig, build_command, detect_program};

/// Read buffer for the reader thread: big enough to drain bursts (`cat` of a
/// large file) without thousands of tiny wakeups.
const READ_BUF: usize = 64 * 1024;

/// Scrollback lines kept per pane.
pub const SCROLLBACK: usize = 10_000;

/// How many chunks a remote subscriber may fall behind before it is lagged out.
/// A lagged receiver loses history and resyncs; it never applies backpressure to
/// the reader thread.
const REMOTE_BACKLOG: usize = 128;

/// Things the UI thread must act on.
#[derive(Debug)]
pub enum UiEvent {
    /// A pane's shell exited on its own: close the pane.
    Exit(PaneId),
    /// The shell reported its working directory (OSC 7 payload, parsed).
    Cwd(PaneId, String),
    /// The injected `splitview …` command ran in a pane.
    AppCommand(PaneId, String),
    /// The file sidebar's watch saw these directories change.
    FsChanged(u64, crate::watch::Batch),
    /// A directory listing finished (sidebar reads run off the UI thread).
    Listing(String, std::result::Result<crate::sidebar::DirRead, String>),
    /// A sidebar's `git status` finished (`None`: not a repository).
    Git(u64, Option<Box<sv_core::git::GitStatus>>),
    /// The native file picker returned.
    FilePicked(Option<String>),
    /// Remote control wants something from the UI.
    Remote(RemoteUi),
}

/// A PTY's output fan-out to remote subscribers (the local terminal is fed
/// directly by the reader thread).
pub struct OutputSink {
    remote: broadcast::Sender<Bytes>,
    seq: AtomicU64,
}

impl OutputSink {
    fn new() -> Self {
        let (remote, _) = broadcast::channel(REMOTE_BACKLOG);
        Self {
            remote,
            seq: AtomicU64::new(0),
        }
    }

    fn send(&self, chunk: &[u8]) {
        self.seq.fetch_add(1, Ordering::Relaxed);
        // One atomic load is the entire cost while nothing is subscribed.
        if self.remote.receiver_count() > 0 {
            let _ = self.remote.send(Bytes::copy_from_slice(chunk));
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Bytes> {
        self.remote.subscribe()
    }

    /// Chunks written so far. Only ever compared against itself.
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }
}

type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

/// Everything needed to keep one PTY alive and talk to it.
pub struct PtyHandle {
    writer: Writer,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    pub output: Arc<OutputSink>,
    pub term: Terminal,
}

/// What a fresh pane gets back from [`Hub::spawn`].
pub struct Spawned {
    pub term: Terminal,
    pub title: Arc<Mutex<Option<String>>>,
    pub family: ShellFamily,
    pub program: String,
    /// The msys install an msys shell's absolute paths live under.
    pub msys_root: Option<String>,
}

pub struct Hub {
    ptys: Mutex<HashMap<PaneId, PtyHandle>>,
    pub remote: RemoteState,
    ui: Mutex<Sender<UiEvent>>,
    ctx: egui::Context,
    exits: broadcast::Sender<PaneId>,
    runtime: OnceLock<tokio::runtime::Runtime>,
    next_id: AtomicU64,
}

impl Hub {
    pub fn new(ctx: egui::Context, ui: Sender<UiEvent>) -> Arc<Self> {
        let (exits, _) = broadcast::channel(64);
        Arc::new(Self {
            ptys: Mutex::new(HashMap::new()),
            remote: RemoteState::new(),
            ui: Mutex::new(ui),
            ctx,
            exits,
            runtime: OnceLock::new(),
            next_id: AtomicU64::new(1),
        })
    }

    pub fn alloc_id(&self) -> PaneId {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send an event to the UI thread and wake it. False if the UI is gone.
    pub fn emit(&self, ev: UiEvent) -> bool {
        let ok = self.ui.lock().send(ev).is_ok();
        self.ctx.request_repaint();
        ok
    }

    pub fn repaint(&self) {
        self.ctx.request_repaint();
    }

    /// The tokio runtime the remote server runs on, created on first use so a
    /// user who never enables remote control never pays for its threads.
    pub fn runtime(&self) -> &tokio::runtime::Runtime {
        self.runtime.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("sv-remote")
                .enable_all()
                .build()
                .unwrap_or_else(|_| {
                    // Two worker threads could not be started; one will do.
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap_or_else(|e| panic!("no tokio runtime: {e}"))
                })
        })
    }

    pub fn subscribe_exits(&self) -> broadcast::Receiver<PaneId> {
        self.exits.subscribe()
    }

    /// Open a PTY, spawn the shell, and start its reader and waiter threads.
    pub fn spawn(self: &Arc<Self>, id: PaneId, size: Size, cfg: &ShellConfig) -> Result<Spawned> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: size.rows.max(1),
                cols: size.cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| Error::Pty(e.to_string()))?;
        let program = detect_program(cfg);
        let family = ShellFamily::of(&program);
        let cmd = build_command(cfg)?;
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| Error::Pty(e.to_string()))?;
        // The child holds its own slave; drop ours.
        drop(pair.slave);
        let killer = child.clone_killer();
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| Error::Pty(e.to_string()))?;
        let writer: Writer = Arc::new(Mutex::new(
            pair.master
                .take_writer()
                .map_err(|e| Error::Pty(e.to_string()))?,
        ));

        // The emulator answers queries (DA, cursor position) by writing back to
        // the PTY, from the reader thread, while it holds the terminal lock. The
        // writer has its own lock, which is never held while taking the terminal's.
        let title = Arc::new(Mutex::new(None));
        let sink: sv_term::EventSink = {
            let (w, t) = (writer.clone(), title.clone());
            Arc::new(move |e| match e {
                TermEvent::PtyWrite(bytes) => {
                    let mut w = w.lock();
                    let _ = w.write_all(&bytes);
                    let _ = w.flush();
                }
                TermEvent::Title(s) => *t.lock() = Some(sanitize(&s)),
                TermEvent::ResetTitle => *t.lock() = None,
                TermEvent::Bell => {}
            })
        };
        let term = Terminal::new(size, SCROLLBACK, sink);
        let output = Arc::new(OutputSink::new());

        // Reader: PTY bytes → OSC scan → emulator → remote fan-out → repaint.
        // Blocks on the read (never under the registry lock) and exits on EOF,
        // which dropping the master in `close` causes.
        {
            let (term, output, hub) = (term.clone(), output.clone(), Arc::downgrade(self));
            thread::Builder::new()
                .name(format!("sv-pty-{id}"))
                .spawn(move || {
                    let mut buf = vec![0u8; READ_BUF];
                    let mut scanner = OscScanner::new();
                    let mut found = Vec::new();
                    loop {
                        let n = match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        let chunk = &buf[..n];
                        scanner.scan(chunk, &mut found);
                        term.advance(chunk);
                        output.send(chunk);
                        let Some(hub) = hub.upgrade() else { break };
                        for osc in found.drain(..) {
                            hub.emit(match osc {
                                Osc::Cwd(p) => match sv_core::paths::parse_osc7(&p) {
                                    Some(p) => UiEvent::Cwd(id, p),
                                    None => continue,
                                },
                                Osc::AppCommand(c) => UiEvent::AppCommand(id, c),
                            });
                        }
                        hub.repaint();
                    }
                })?;
        }

        // Waiter: the reliable exit signal. On Windows (ConPTY) the master read
        // keeps blocking while the master is alive, so read-EOF can't tell us the
        // shell exited; `wait()` observes the process directly and reaps it.
        {
            let hub = Arc::downgrade(self);
            thread::Builder::new()
                .name(format!("sv-wait-{id}"))
                .spawn(move || {
                    let _ = child.wait();
                    if let Some(hub) = hub.upgrade() {
                        let _ = hub.exits.send(id);
                        hub.emit(UiEvent::Exit(id));
                    }
                })?;
        }

        self.ptys.lock().insert(
            id,
            PtyHandle {
                writer,
                master: pair.master,
                killer,
                output,
                term: term.clone(),
            },
        );
        Ok(Spawned {
            term,
            title,
            family,
            msys_root: sv_core::paths::msys_root(&program),
            program,
        })
    }

    /// Write bytes to a pane's PTY (keystrokes, local or remote).
    pub fn write(&self, id: PaneId, bytes: &[u8]) -> Result<()> {
        let w = self
            .ptys
            .lock()
            .get(&id)
            .map(|h| h.writer.clone())
            .ok_or(Error::PaneNotFound(id))?;
        let mut w = w.lock();
        w.write_all(bytes)?;
        w.flush()?;
        Ok(())
    }

    /// Resize a pane's PTY (the grid is resized by whoever asked).
    pub fn resize(&self, id: PaneId, size: Size) -> Result<()> {
        let ptys = self.ptys.lock();
        let h = ptys.get(&id).ok_or(Error::PaneNotFound(id))?;
        h.master
            .resize(PtySize {
                rows: size.rows.max(1),
                cols: size.cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| Error::Pty(e.to_string()))
    }

    /// Resize both the emulator grid and the PTY (a remote client resizing).
    pub fn resize_both(&self, id: PaneId, size: Size) -> Result<()> {
        let term = self
            .ptys
            .lock()
            .get(&id)
            .map(|h| h.term.clone())
            .ok_or(Error::PaneNotFound(id))?;
        term.resize(size);
        self.resize(id, size)?;
        self.repaint();
        Ok(())
    }

    /// Kill the shell, drop the master (EOF ends the reader), forget the pane.
    pub fn close(&self, id: PaneId) {
        let handle = self.ptys.lock().remove(&id);
        if let Some(mut h) = handle {
            let _ = h.killer.kill();
            drop(h); // master dropped after the kill
        }
    }

    pub fn close_all(&self) {
        let all: Vec<PaneId> = self.ptys.lock().keys().copied().collect();
        for id in all {
            self.close(id);
        }
    }

    pub fn subscribe(&self, id: PaneId) -> Option<(broadcast::Receiver<Bytes>, u64)> {
        self.ptys
            .lock()
            .get(&id)
            .map(|h| (h.output.subscribe(), h.output.seq()))
    }

    pub fn terminal(&self, id: PaneId) -> Option<Terminal> {
        self.ptys.lock().get(&id).map(|h| h.term.clone())
    }

    pub fn seqs(&self) -> HashMap<PaneId, u64> {
        self.ptys
            .lock()
            .iter()
            .map(|(id, h)| (*id, h.output.seq()))
            .collect()
    }

    pub fn pane_count(&self) -> usize {
        self.ptys.lock().len()
    }
}

/// Program-supplied text (titles) is untrusted: strip control characters before
/// it is drawn outside the terminal.
pub fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_for(mut f: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// A real shell in a real PTY: typed bytes reach it, its output reaches the
    /// emulator (and the OSC 7 cwd report reaches the UI), and closing the pane
    /// ends the shell.
    #[test]
    fn spawns_a_shell_types_into_it_and_closes_it() {
        let Some(profile) = crate::profiles::detect()
            .into_iter()
            .find(|p| p.family == ShellFamily::Posix)
        else {
            eprintln!("no POSIX shell installed; skipping");
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let hub = Hub::new(egui::Context::default(), tx);
        let cfg = ShellConfig {
            program: Some(profile.program.clone()),
            cwd: None,
            prompt: "$ ".into(),
            style: crate::shell::PromptStyle::Plain,
            aliases: Vec::new(),
            prompt_info: Default::default(),
            banner: String::new(),
        };
        let id = hub.alloc_id();
        let s = hub
            .spawn(id, Size { cols: 80, rows: 24 }, &cfg)
            .expect("spawn");
        hub.write(id, b"echo sv-$((40+2))-ok\r").expect("write");
        assert!(
            wait_for(|| s.term.screen_text().contains("sv-42-ok")),
            "output never arrived:\n{}",
            s.term.screen_text()
        );
        // The injected hook reported the shell's directory.
        assert!(wait_for(|| rx
            .try_iter()
            .any(|e| matches!(e, UiEvent::Cwd(p, _) if p == id))));
        hub.resize(
            id,
            Size {
                cols: 100,
                rows: 30,
            },
        )
        .expect("resize");
        hub.close(id);
        assert!(hub.write(id, b"x").is_err(), "pane is gone after close");
        assert_eq!(hub.pane_count(), 0);
    }

    #[test]
    fn sanitizes_titles() {
        assert_eq!(sanitize(" vim\x1b[31m x\u{7} "), "vim[31m x");
    }

    #[test]
    fn lagging_subscriber_never_blocks() {
        let sink = OutputSink::new();
        let mut slow = sink.subscribe();
        for i in 0..(REMOTE_BACKLOG * 2) {
            sink.send(&[i as u8]);
        }
        assert_eq!(sink.seq(), (REMOTE_BACKLOG * 2) as u64);
        assert!(matches!(
            slow.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(_))
        ));
    }
}
