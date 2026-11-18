//! Filesystem change notifications for the file sidebar (CLAUDE.md §9).
//!
//! The sidebar shows a handful of directories — its root plus whatever folders
//! are open — and a window's [`DirWatch`] watches exactly those, telling the
//! frontend which of them changed so it re-lists only those. While the sidebar
//! is closed there is no watch at all. The rest is about keeping it cheap:
//!
//! - **Only listing changes count.** Creating, removing or renaming an entry
//!   changes what a directory shows; writing a file's contents or touching its
//!   metadata (a build rewriting outputs, an editor saving) does not. Those are
//!   dropped in the OS callback, before anything is allocated or locked.
//! - **Content changes are a flag, not a re-list.** They can't change what a
//!   directory shows, but they do change `git status`, so a batch says whether
//!   anything in the tree was written ([`Batch::contents`]). A batch with only
//!   that flag waits [`CONTENT_DEBOUNCE`], so a build writing into `target/`
//!   wakes the UI about once a second, not ten times. Git's own bookkeeping in
//!   `.git/` is ignored except the index and `HEAD` (a commit, a checkout).
//! - **Coalesced.** After the first change, changes are gathered for
//!   [`DEBOUNCE`] and delivered as one de-duplicated batch of directories, so
//!   `npm install` produces a few messages a second, not thousands.
//! - **Idle is free.** The flusher thread blocks on a channel; nothing polls.
//! - **Only deltas reach the OS.** Opening a folder registers that one folder;
//!   the rest of the watch set is left alone.
//! - **Windows holds one handle.** ReadDirectoryChangesW keeps a handle open on
//!   every watched directory, and Windows refuses to rename or delete a directory
//!   while a handle inside it is open — watching each open folder separately would
//!   make `mv src lib` or a `git checkout` in the terminal fail. So on Windows only
//!   the outermost directories are watched (recursively, which the kernel does
//!   natively) and events are filtered back down to the displayed set. inotify
//!   and FSEvents pin nothing, so elsewhere each displayed directory is watched
//!   on its own, non-recursively, and churn deeper down never reaches us.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::event::ModifyKind;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::error::{Error, Result};

/// How long changes are gathered after the first one before the batch is sent.
/// Long enough to fold a burst (`git checkout`, `tar x`) into one re-list, short
/// enough that a `touch` in the terminal shows up before you look over.
const DEBOUNCE: Duration = Duration::from_millis(120);

/// How long a batch that only says "contents changed" gathers before it is sent.
const CONTENT_DEBOUNCE: Duration = Duration::from_millis(1000);

/// What one flush reports.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Batch {
    /// Displayed directories whose listing may have changed.
    pub dirs: Vec<String>,
    /// Something under the watch was created, removed or written.
    pub contents: bool,
}

/// What the OS callback tells the flusher.
enum Signal {
    Dir(Arc<str>),
    Contents,
}

/// Whether a watch covers subdirectories (see the module docs for why this
/// differs by platform).
const RECURSIVE: bool = cfg!(windows);

/// Displayed directories, keyed by the path events arrive with, mapped to the
/// exact string the frontend knows the directory by (it gets that string back).
type DirMap = HashMap<PathBuf, Arc<str>>;

/// One window's sidebar watch.
pub struct DirWatch {
    watcher: RecommendedWatcher,
    /// Shared with the OS callback, which routes events through it.
    dirs: Arc<Mutex<DirMap>>,
    /// What is registered with the OS right now.
    registered: HashSet<PathBuf>,
}

impl DirWatch {
    /// Start a watch that reports changed directories to `on_change`. It watches
    /// nothing until [`set_dirs`](Self::set_dirs).
    /// `sink` returns false once nobody is listening, which ends the flusher
    /// thread.
    pub fn new(sink: impl FnMut(Batch) -> bool + Send + 'static) -> Result<Self> {
        let dirs: Arc<Mutex<DirMap>> = Arc::default();
        let (tx, rx) = mpsc::channel::<Signal>();
        let routes = Arc::clone(&dirs);
        // The callback owns the only sender, so dropping the watcher ends the
        // flusher thread (its `recv` fails) — no thread outlives its watch.
        let watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                route(&event, &routes, &tx);
            }
        })
        .map_err(|e| Error::Watch(e.to_string()))?;
        std::thread::Builder::new()
            .name("sv-fs-watch".into())
            .spawn(move || flush(&rx, sink))?;
        Ok(Self {
            watcher,
            dirs,
            registered: HashSet::new(),
        })
    }

    /// Watch exactly `dirs` — native paths, as the frontend received them from
    /// `read_dir` — replacing the previous set. Only the difference touches the
    /// OS. A directory that can't be watched (gone, no permission) is skipped
    /// rather than failing the rest.
    pub fn set_dirs(&mut self, dirs: Vec<String>) -> Result<()> {
        let mut map = DirMap::with_capacity(dirs.len());
        let mut paths = Vec::with_capacity(dirs.len());
        for dir in dirs {
            let key: Arc<str> = Arc::from(dir);
            let path: PathBuf = Path::new(&*key).components().collect();
            // FSEvents reports resolved paths (`/private/tmp` for `/tmp`).
            #[cfg(target_os = "macos")]
            if let Ok(real) = std::fs::canonicalize(&path) {
                if real != path {
                    map.insert(real, Arc::clone(&key));
                }
            }
            map.insert(path.clone(), key);
            paths.push(path);
        }
        *self.dirs.lock().map_err(|_| Error::Poisoned)? = map;

        let wanted = watch_targets(&paths, RECURSIVE);
        let mode = if RECURSIVE {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        // One batch, so FSEvents restarts its stream once rather than per path.
        let mut batch = self.watcher.paths_mut();
        for gone in self.registered.difference(&wanted) {
            let _ = batch.remove(gone);
        }
        let mut registered: HashSet<PathBuf> =
            self.registered.intersection(&wanted).cloned().collect();
        for new in wanted.difference(&self.registered) {
            if batch.add(new, mode).is_ok() {
                registered.insert(new.clone());
            }
        }
        let committed = batch.commit();
        self.registered = registered;
        committed.map_err(|e| Error::Watch(e.to_string()))
    }
}

/// What to register with the OS for a displayed set: with recursive watches only
/// the outermost directories (each covers the rest), otherwise every directory.
fn watch_targets(dirs: &[PathBuf], recursive: bool) -> HashSet<PathBuf> {
    dirs.iter()
        .filter(|d| !recursive || !dirs.iter().any(|o| o != *d && d.starts_with(o)))
        .cloned()
        .collect()
}

/// Whether an event can change a directory listing: an entry appeared, went
/// away, or was renamed. Content and metadata writes can't; the catch-all kinds
/// backends use when they can't tell are assumed to.
fn changes_listing(kind: &EventKind) -> bool {
    match kind {
        EventKind::Create(_) | EventKind::Remove(_) => true,
        EventKind::Modify(ModifyKind::Name(_)) => true,
        EventKind::Modify(_) | EventKind::Access(_) => false,
        EventKind::Any | EventKind::Other => true,
    }
}

/// Whether a change at `path` can change `git status`: anything in the work
/// tree, but inside `.git` only the index and `HEAD` (lock files, objects and
/// logs churn on every git command and say nothing new).
fn affects_status(path: &Path) -> bool {
    let mut parts = path.components().map(|c| c.as_os_str());
    if !parts.any(|c| c == ".git") {
        return true;
    }
    let rest: Vec<_> = parts.collect();
    matches!(rest.as_slice(), [f] if *f == "index" || *f == "HEAD")
}

/// OS callback: turn one event into the displayed directories it affects, and
/// say whether it touched the tree at all.
fn route(event: &Event, dirs: &Mutex<DirMap>, tx: &Sender<Signal>) {
    let rescan = event.need_rescan();
    if matches!(event.kind, EventKind::Access(_)) && !rescan {
        return;
    }
    if rescan || event.paths.iter().any(|p| affects_status(p)) {
        let _ = tx.send(Signal::Contents);
    }
    if !rescan && !changes_listing(&event.kind) {
        return;
    }
    let Ok(dirs) = dirs.lock() else { return };
    if rescan {
        // The OS dropped events: anything on screen may be stale.
        for key in dirs.values() {
            let _ = tx.send(Signal::Dir(Arc::clone(key)));
        }
        return;
    }
    for path in &event.paths {
        // The directory that gained or lost this entry…
        if let Some(key) = path.parent().and_then(|p| dirs.get(p)) {
            let _ = tx.send(Signal::Dir(Arc::clone(key)));
        }
        // …and a displayed directory that was itself removed or renamed.
        if let Some(key) = dirs.get(path) {
            let _ = tx.send(Signal::Dir(Arc::clone(key)));
        }
    }
}

/// Flusher thread: wait (without polling) for a change, gather more for
/// [`DEBOUNCE`] ([`CONTENT_DEBOUNCE`] while only contents changed), send the
/// de-duplicated batch. Returns when the watcher is dropped or the sink stops
/// listening.
fn flush(rx: &Receiver<Signal>, mut sink: impl FnMut(Batch) -> bool) {
    while let Ok(first) = rx.recv() {
        let start = Instant::now();
        let mut dirs: HashSet<Arc<str>> = HashSet::new();
        let mut contents = false;
        let mut take = |sig: Signal, dirs: &mut HashSet<Arc<str>>| match sig {
            Signal::Dir(d) => {
                dirs.insert(d);
            }
            Signal::Contents => contents = true,
        };
        take(first, &mut dirs);
        loop {
            // A listing change is worth showing quickly; a content-only batch
            // (git marks) can wait, and folds a build's writes together.
            let wait = if dirs.is_empty() {
                CONTENT_DEBOUNCE
            } else {
                DEBOUNCE
            };
            let deadline = start + wait;
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(sig) => take(sig, &mut dirs),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let batch = Batch {
            dirs: dirs.iter().map(|d| d.to_string()).collect(),
            contents,
        };
        if !sink(batch) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, DataChange, MetadataKind, RemoveKind, RenameMode};

    fn map(dirs: &[&str]) -> Mutex<DirMap> {
        Mutex::new(
            dirs.iter()
                .map(|d| (PathBuf::from(d), Arc::from(*d)))
                .collect(),
        )
    }

    fn routed(event: Event, dirs: &Mutex<DirMap>) -> Vec<String> {
        routed_all(event, dirs).0
    }

    /// (dirs, whether a contents signal was sent)
    fn routed_all(event: Event, dirs: &Mutex<DirMap>) -> (Vec<String>, bool) {
        let (tx, rx) = mpsc::channel();
        route(&event, dirs, &tx);
        drop(tx);
        let mut out = Vec::new();
        let mut contents = false;
        for s in rx.iter() {
            match s {
                Signal::Dir(k) => out.push(k.to_string()),
                Signal::Contents => contents = true,
            }
        }
        out.sort();
        (out, contents)
    }

    #[test]
    fn content_writes_raise_the_contents_flag_but_git_internals_do_not() {
        let dirs = map(&["/p"]);
        let write = |p: &str| Event::new(EventKind::Modify(ModifyKind::Any)).add_path(p.into());
        assert_eq!(routed_all(write("/p/src/deep/a.rs"), &dirs), (vec![], true));
        assert_eq!(routed_all(write("/p/.git/index"), &dirs), (vec![], true));
        assert_eq!(routed_all(write("/p/.git/HEAD"), &dirs), (vec![], true));
        assert_eq!(
            routed_all(write("/p/.git/index.lock"), &dirs),
            (vec![], false)
        );
        assert_eq!(
            routed_all(write("/p/.git/objects/ab/cd"), &dirs),
            (vec![], false)
        );
        let access =
            Event::new(EventKind::Access(notify::event::AccessKind::Any)).add_path("/p/a".into());
        assert_eq!(routed_all(access, &dirs), (vec![], false));
        let create = Event::new(EventKind::Create(CreateKind::File)).add_path("/p/n".into());
        assert_eq!(routed_all(create, &dirs), (vec!["/p".to_string()], true));
    }

    #[test]
    fn only_listing_changes_count() {
        assert!(changes_listing(&EventKind::Create(CreateKind::File)));
        assert!(changes_listing(&EventKind::Remove(RemoveKind::Folder)));
        assert!(changes_listing(&EventKind::Modify(ModifyKind::Name(
            RenameMode::From
        ))));
        assert!(changes_listing(&EventKind::Any));
        // A build rewriting files, or Windows' generic "modified", is noise.
        assert!(!changes_listing(&EventKind::Modify(ModifyKind::Data(
            DataChange::Content
        ))));
        assert!(!changes_listing(&EventKind::Modify(ModifyKind::Metadata(
            MetadataKind::Any
        ))));
        assert!(!changes_listing(&EventKind::Modify(ModifyKind::Any)));
    }

    #[test]
    fn routes_an_event_to_the_displayed_parent() {
        let dirs = map(&["/p", "/p/src"]);
        let create = |p: &str| Event::new(EventKind::Create(CreateKind::File)).add_path(p.into());
        assert_eq!(routed(create("/p/src/a.rs"), &dirs), ["/p/src"]);
        assert_eq!(routed(create("/p/b.rs"), &dirs), ["/p"]);
        // Deeper than anything displayed (a recursive Windows watch sees these).
        assert!(routed(create("/p/src/deep/c.rs"), &dirs).is_empty());
        // Content writes are dropped before routing.
        let write = Event::new(EventKind::Modify(ModifyKind::Any)).add_path("/p/b.rs".into());
        assert!(routed(write, &dirs).is_empty());
    }

    #[test]
    fn removing_a_displayed_directory_reports_it_and_its_parent() {
        let dirs = map(&["/p", "/p/src"]);
        let remove = Event::new(EventKind::Remove(RemoveKind::Folder)).add_path("/p/src".into());
        assert_eq!(routed(remove, &dirs), ["/p", "/p/src"]);
    }

    #[test]
    fn a_rescan_reports_every_displayed_directory() {
        let dirs = map(&["/p", "/p/src"]);
        let rescan = Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
        assert_eq!(routed(rescan, &dirs), ["/p", "/p/src"]);
    }

    #[test]
    fn recursive_watches_cover_nested_directories() {
        let dirs: Vec<PathBuf> = ["/p", "/p/src", "/p/src/ui", "/q"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let outer = watch_targets(&dirs, true);
        assert_eq!(outer, HashSet::from(["/p".into(), "/q".into()]));
        assert_eq!(watch_targets(&dirs, false).len(), 4);
        // Component-wise: `/pq` is not inside `/p`.
        let siblings = [PathBuf::from("/p"), PathBuf::from("/pq")];
        assert_eq!(watch_targets(&siblings, true).len(), 2);
    }

    #[test]
    fn flush_coalesces_a_burst_into_one_batch() {
        let (tx, rx) = mpsc::channel::<Signal>();
        for d in ["/a", "/b", "/a", "/a"] {
            tx.send(Signal::Dir(Arc::from(d))).expect("send");
        }
        drop(tx);
        let mut batches = Vec::new();
        flush(&rx, |mut b| {
            b.dirs.sort();
            batches.push(b.dirs);
            true
        });
        // The sender vanished mid-gather, so nothing is sent: the watch is gone.
        assert!(batches.is_empty());

        let (tx, rx) = mpsc::channel::<Signal>();
        let handle = std::thread::spawn(move || {
            let mut batches = Vec::new();
            flush(&rx, |mut b| {
                b.dirs.sort();
                batches.push(b.dirs);
                false // stop after the first batch
            });
            batches
        });
        for d in ["/a", "/b", "/a"] {
            tx.send(Signal::Dir(Arc::from(d))).expect("send");
        }
        let batches = handle.join().expect("flusher");
        assert_eq!(batches, [vec!["/a".to_string(), "/b".to_string()]]);
    }

    /// Wait up to `timeout` for a batch containing `dir`.
    fn wait_for(rx: &Receiver<Batch>, dir: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while let Ok(batch) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            if batch.dirs.iter().any(|d| d == dir) {
                return true;
            }
        }
        false
    }

    #[test]
    fn reports_real_creations_in_watched_directories() {
        let base = std::env::temp_dir().join(format!("splitview-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).expect("create test dirs");
        std::fs::write(base.join("existing.txt"), b"x").expect("write test file");
        let root = base.to_string_lossy().to_string();
        let sub = base.join("sub").to_string_lossy().to_string();

        let (tx, rx) = mpsc::channel();
        let mut watch = DirWatch::new(move |b| tx.send(b).is_ok()).expect("watch");
        watch
            .set_dirs(vec![root.clone(), sub.clone()])
            .expect("set dirs");

        std::fs::write(base.join("sub").join("new.txt"), b"x").expect("create in sub");
        assert!(wait_for(&rx, &sub, Duration::from_secs(5)));

        std::fs::create_dir(base.join("made")).expect("create in root");
        assert!(wait_for(&rx, &root, Duration::from_secs(5)));

        // Once a directory leaves the set, its changes stop arriving.
        watch.set_dirs(vec![root.clone()]).expect("narrow");
        while rx.recv_timeout(Duration::from_millis(300)).is_ok() {}
        std::fs::write(base.join("sub").join("later.txt"), b"x").expect("create later");
        assert!(!wait_for(&rx, &sub, Duration::from_millis(600)));

        drop(watch);
        let _ = std::fs::remove_dir_all(&base);
    }

    // FSEvents may repeat a recent creation flag on a later write, so the
    // "writes are silent" guarantee is only asserted where the OS is exact.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn content_writes_do_not_report() {
        let base =
            std::env::temp_dir().join(format!("splitview-watch-quiet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("create test dir");
        std::fs::write(base.join("log.txt"), b"x").expect("write test file");
        let root = base.to_string_lossy().to_string();

        let (tx, rx) = mpsc::channel();
        let mut watch = DirWatch::new(move |b| tx.send(b).is_ok()).expect("watch");
        watch.set_dirs(vec![root.clone()]).expect("set dirs");
        std::thread::sleep(Duration::from_millis(200));

        for _ in 0..20 {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(base.join("log.txt"))
                .expect("open for append");
            f.write_all(b"more").expect("append");
        }
        assert!(!wait_for(&rx, &root, Duration::from_millis(600)));

        drop(watch);
        let _ = std::fs::remove_dir_all(&base);
    }
}
