//! Git working-tree status for the sidebar, as pure functions.
//!
//! The app runs `git status --porcelain=v2 -z --branch` off the UI thread and
//! hands the bytes here. Porcelain v2 is git's stable, machine-readable format
//! (it does not change with the user's config or git version); `-z` makes paths
//! NUL-terminated and unquoted, so spaces and non-ASCII names come through as is.
//!
//! [`GitStatus::lookup`] answers "what is this sidebar row's state" for files
//! and folders (a folder takes the strongest state found inside it).

use std::collections::HashMap;

/// Upper bound on parsed entries: a repo with 200k untracked files must not
/// become a 200k-entry map rebuilt on every prompt.
pub const MAX_ENTRIES: usize = 20_000;

/// One path's state, strongest last (folders take the max of their contents).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Change {
    Untracked,
    Added,
    Renamed,
    Modified,
    Deleted,
    Conflicted,
}

impl Change {
    /// The one-letter badge drawn at the end of the row.
    pub fn letter(self) -> &'static str {
        match self {
            Change::Untracked => "U",
            Change::Added => "A",
            Change::Renamed => "R",
            Change::Modified => "M",
            Change::Deleted => "D",
            Change::Conflicted => "!",
        }
    }

    /// What a folder containing this change shows: deletions and renames read as
    /// "something in here was modified".
    fn for_folder(self) -> Change {
        match self {
            Change::Deleted | Change::Renamed => Change::Modified,
            c => c,
        }
    }
}

/// One changed path, relative to the repository root, `/`-separated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub change: Change,
    /// Something is staged (index differs from HEAD).
    pub staged: bool,
    /// The work tree differs from the index.
    pub unstaged: bool,
    /// Renames: where it came from.
    pub from: Option<String>,
}

/// Per-kind totals for the recap line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub staged: usize,
    pub modified: usize,
    pub added: usize,
    pub deleted: usize,
    pub renamed: usize,
    pub untracked: usize,
    pub conflicted: usize,
}

impl Counts {
    pub fn total(&self) -> usize {
        self.modified + self.added + self.deleted + self.renamed + self.untracked + self.conflicted
    }
}

/// A parsed `git status`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitStatus {
    /// Native path of the repository root (`git rev-parse --show-toplevel`).
    pub root: String,
    /// Branch name; `None` when detached (see `oid`).
    pub branch: Option<String>,
    /// Abbreviated commit, `None` before the first commit.
    pub oid: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub entries: Vec<Entry>,
    /// More changes than [`MAX_ENTRIES`] were reported.
    pub truncated: bool,
    pub counts: Counts,
    /// Normalized relative path → state, files and every ancestor folder.
    files: HashMap<String, Change>,
    folders: HashMap<String, Change>,
    /// Untracked directories (git reports `dir/`, not its contents).
    untracked_dirs: Vec<String>,
}

/// Parse `git status --porcelain=v2 -z --branch` output. `root` is the repo
/// root as returned by `git rev-parse --show-toplevel` (any separator).
pub fn parse_status(root: &str, out: &[u8]) -> GitStatus {
    let mut st = GitStatus {
        root: root.trim().to_owned(),
        ..GitStatus::default()
    };
    let text = String::from_utf8_lossy(out);
    let mut records = text.split('\0');
    while let Some(rec) = records.next() {
        if rec.is_empty() {
            continue;
        }
        if let Some(h) = rec.strip_prefix("# ") {
            header(&mut st, h);
            continue;
        }
        if st.entries.len() >= MAX_ENTRIES {
            st.truncated = true;
            continue;
        }
        let entry = match rec.as_bytes()[0] {
            b'1' => ordinary(rec, 8),
            b'2' => ordinary(rec, 9).map(|mut e| {
                // With -z the original path is the next record.
                e.from = records.next().map(str::to_owned);
                e
            }),
            b'u' => rec.splitn(11, ' ').nth(10).map(|p| Entry {
                path: p.to_owned(),
                change: Change::Conflicted,
                staged: false,
                unstaged: true,
                from: None,
            }),
            b'?' => rec.get(2..).map(|p| Entry {
                path: p.to_owned(),
                change: Change::Untracked,
                staged: false,
                unstaged: true,
                from: None,
            }),
            _ => None, // `!` (ignored) is never requested
        };
        if let Some(e) = entry {
            st.entries.push(e);
        }
    }
    index(&mut st);
    st
}

fn header(st: &mut GitStatus, h: &str) {
    let (key, val) = h.split_once(' ').unwrap_or((h, ""));
    match key {
        "branch.oid" if val != "(initial)" => st.oid = Some(val.chars().take(7).collect()),
        "branch.head" if val != "(detached)" => st.branch = Some(val.to_owned()),
        "branch.upstream" => st.upstream = Some(val.to_owned()),
        "branch.ab" => {
            for part in val.split(' ') {
                if let Some(n) = part.strip_prefix('+') {
                    st.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    st.behind = n.parse().unwrap_or(0);
                }
            }
        }
        _ => {}
    }
}

/// `1 XY sub mH mI mW hH hI path` (fields = 8) or
/// `2 XY sub mH mI mW hH hI Xscore path` (fields = 9).
fn ordinary(rec: &str, fields: usize) -> Option<Entry> {
    let mut it = rec.splitn(fields + 1, ' ');
    let xy = it.nth(1)?.as_bytes();
    let path = it.nth(fields - 2)?;
    let (x, y) = (*xy.first()?, *xy.get(1)?);
    let change = if x == b'D' || y == b'D' {
        Change::Deleted
    } else if x == b'R' || x == b'C' {
        Change::Renamed
    } else if x == b'A' {
        Change::Added
    } else {
        Change::Modified
    };
    Some(Entry {
        path: path.to_owned(),
        change,
        staged: x != b'.',
        unstaged: y != b'.',
        from: None,
    })
}

fn index(st: &mut GitStatus) {
    let mut c = Counts::default();
    for e in &st.entries {
        match e.change {
            Change::Untracked => c.untracked += 1,
            Change::Added => c.added += 1,
            Change::Renamed => c.renamed += 1,
            Change::Modified => c.modified += 1,
            Change::Deleted => c.deleted += 1,
            Change::Conflicted => c.conflicted += 1,
        }
        if e.staged {
            c.staged += 1;
        }
        let key = norm(&e.path);
        if e.change == Change::Untracked && key.ends_with('/') {
            st.untracked_dirs.push(key.trim_end_matches('/').to_owned());
        }
        let key = key.trim_end_matches('/').to_owned();
        // Every ancestor folder takes the strongest change inside it.
        let fc = e.change.for_folder();
        let mut cut = key.as_str();
        while let Some((parent, _)) = cut.rsplit_once('/') {
            let slot = st.folders.entry(parent.to_owned()).or_insert(fc);
            *slot = (*slot).max(fc);
            cut = parent;
        }
        st.files.insert(key, e.change);
    }
    st.counts = c;
}

/// Separators to `/`; case folded on Windows, where paths are case-insensitive
/// and the cwd a shell reports can differ in case from git's.
fn norm(p: &str) -> String {
    let p = p.replace('\\', "/");
    if cfg!(windows) { p.to_lowercase() } else { p }
}

impl GitStatus {
    /// The state of an absolute native path (a sidebar row), if it changed or
    /// (for a folder) contains a change. `None` outside the repo or clean.
    pub fn lookup(&self, path: &str, is_dir: bool) -> Option<Change> {
        let rel = self.relative(path)?;
        if rel.is_empty() {
            return self.files.values().map(|c| c.for_folder()).max();
        }
        if is_dir {
            if let Some(c) = self.files.get(&rel) {
                return Some(c.for_folder()); // an untracked `dir/`, or a submodule
            }
            if let Some(c) = self.folders.get(&rel) {
                return Some(*c);
            }
        } else if let Some(c) = self.files.get(&rel) {
            return Some(*c);
        }
        // Inside an untracked directory: everything in it is untracked.
        self.untracked_dirs
            .iter()
            .any(|d| rel.starts_with(d.as_str()) && rel.as_bytes().get(d.len()) == Some(&b'/'))
            .then_some(Change::Untracked)
    }

    /// `path` relative to the repo root, normalized; `None` if outside it.
    fn relative(&self, path: &str) -> Option<String> {
        let root = norm(&self.root);
        let root = root.trim_end_matches('/');
        let p = norm(path);
        let rest = p.strip_prefix(root)?;
        if rest.is_empty() {
            return Some(String::new());
        }
        rest.strip_prefix('/')
            .map(|r| r.trim_end_matches('/').to_owned())
    }

    /// `path` is inside this repository's work tree.
    pub fn contains(&self, path: &str) -> bool {
        self.relative(path).is_some()
    }

    /// Everything clean (nothing to show but the branch).
    pub fn is_clean(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let recs = [
            "# branch.oid 02e806a1234567890",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -1",
            "1 .M N... 100644 100644 100644 aaa bbb crates/app/src/sidebar.rs",
            "1 A. N... 000000 100644 100644 000 ccc crates/core/src/git.rs",
            "1 D. N... 100644 000000 000000 ddd 000 old file.txt",
            "2 R. N... 100644 100644 100644 eee eee R100 docs/new name.md",
            "docs/old name.md",
            "u UU N... 100644 100644 100644 100644 f1 f2 f3 src/conflict.rs",
            "? notes/",
            "? scratch.txt",
        ];
        let mut out = Vec::new();
        for r in recs {
            out.extend_from_slice(r.as_bytes());
            out.push(0);
        }
        out
    }

    fn root() -> &'static str {
        if cfg!(windows) {
            "D:/Projects/SplitView\n"
        } else {
            "/home/u/SplitView\n"
        }
    }

    fn abs(rel: &str) -> String {
        if cfg!(windows) {
            format!("d:\\projects\\splitview\\{}", rel.replace('/', "\\"))
        } else {
            format!("/home/u/SplitView/{rel}")
        }
    }

    #[test]
    fn headers() {
        let st = parse_status(root(), &sample());
        assert_eq!(st.branch.as_deref(), Some("main"));
        assert_eq!(st.oid.as_deref(), Some("02e806a"));
        assert_eq!(st.upstream.as_deref(), Some("origin/main"));
        assert_eq!((st.ahead, st.behind), (2, 1));
    }

    #[test]
    fn entries_and_counts() {
        let st = parse_status(root(), &sample());
        assert_eq!(st.entries.len(), 7);
        let ren = st
            .entries
            .iter()
            .find(|e| e.change == Change::Renamed)
            .unwrap();
        assert_eq!(ren.path, "docs/new name.md");
        assert_eq!(ren.from.as_deref(), Some("docs/old name.md"));
        let c = st.counts;
        assert_eq!(
            (
                c.modified,
                c.added,
                c.deleted,
                c.renamed,
                c.untracked,
                c.conflicted
            ),
            (1, 1, 1, 1, 2, 1)
        );
        assert_eq!(c.staged, 3); // A., D., R.
        assert_eq!(c.total(), 7);
    }

    #[test]
    fn lookup_files_folders_untracked() {
        let st = parse_status(root(), &sample());
        let f = |p: &str| st.lookup(&abs(p), false);
        let d = |p: &str| st.lookup(&abs(p), true);
        assert_eq!(f("crates/app/src/sidebar.rs"), Some(Change::Modified));
        assert_eq!(f("crates/core/src/git.rs"), Some(Change::Added));
        assert_eq!(f("crates/core/src/lib.rs"), None);
        assert_eq!(d("crates/core"), Some(Change::Added));
        assert_eq!(d("crates"), Some(Change::Modified)); // max(Modified, Added)
        assert_eq!(d("docs"), Some(Change::Modified)); // a rename inside
        assert_eq!(d("src"), Some(Change::Conflicted));
        assert_eq!(d("notes"), Some(Change::Untracked));
        assert_eq!(f("notes/deep/a.txt"), Some(Change::Untracked));
        assert_eq!(f("notesx/a.txt"), None);
        assert_eq!(d(""), Some(Change::Conflicted)); // the root itself
    }

    #[test]
    fn outside_repo_and_clean() {
        let st = parse_status(root(), &sample());
        let outside = if cfg!(windows) {
            "D:\\Other\\x.rs"
        } else {
            "/home/u/Other/x.rs"
        };
        assert_eq!(st.lookup(outside, false), None);
        // A sibling whose name extends the root's is not inside it.
        let sib = if cfg!(windows) {
            "D:\\Projects\\SplitViewX\\a"
        } else {
            "/home/u/SplitViewX/a"
        };
        assert_eq!(st.lookup(sib, false), None);
        let clean = parse_status(
            root(),
            b"# branch.oid (initial)\0# branch.head (detached)\0",
        );
        assert!(clean.is_clean());
        assert_eq!(clean.branch, None);
        assert_eq!(clean.oid, None);
        assert_eq!(clean.lookup(&abs("a"), false), None);
    }

    #[test]
    fn garbage_does_not_panic() {
        for input in [
            &b"1"[..],
            b"2 R.",
            b"u",
            b"?",
            b"# ",
            b"#",
            b"1 X",
            b"\0\0\0",
            b"\xff\xfe",
        ] {
            let _ = parse_status("/r", input);
        }
    }

    #[test]
    fn caps_entries() {
        let mut out = Vec::new();
        for i in 0..MAX_ENTRIES + 5 {
            out.extend_from_slice(format!("? f{i}").as_bytes());
            out.push(0);
        }
        let st = parse_status("/r", &out);
        assert_eq!(st.entries.len(), MAX_ENTRIES);
        assert!(st.truncated);
    }
}
