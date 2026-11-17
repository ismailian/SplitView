//! The file sidebar's tree shape, as pure functions: what is on screen, and so
//! what gets watched.
//!
//! The sidebar keeps one listing per directory, shared by every pane, and per
//! pane the set of open folders. Rows are derived from those two; nothing about
//! the tree is stored twice.

use std::collections::{HashMap, HashSet};

/// One entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// File name only.
    pub name: String,
    /// Absolute native path.
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    /// Dot-file (or hidden/system on Windows).
    pub hidden: bool,
}

/// What is known about one directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// Sorted entries, or `None` until read.
    pub entries: Option<Vec<DirEntry>>,
    pub parent: Option<String>,
    /// The read cut the list short.
    pub truncated: bool,
    /// Why the last read failed, if it did.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    Loading,
    Error,
}

/// One visible line of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Index into the parent listing's entries.
    pub dir: String,
    pub index: usize,
    pub depth: usize,
    /// For each level, whether that ancestor was the last of its group (decides
    /// `│  ` vs `   `); the final flag decides `├─ ` vs `└─ `.
    pub last_flags: Vec<bool>,
    pub open: bool,
    pub status: Option<RowStatus>,
}

impl Row {
    pub fn entry<'a>(&self, listings: &'a HashMap<String, Listing>) -> Option<&'a DirEntry> {
        listings.get(&self.dir)?.entries.as_ref()?.get(self.index)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeShape {
    pub rows: Vec<Row>,
    /// The root and every visible open folder, in tree order, minus unreadable
    /// ones: exactly the set to watch.
    pub shown: Vec<String>,
}

/// Flatten the tree under `root`.
pub fn build_tree(
    root: &str,
    listings: &HashMap<String, Listing>,
    open: &HashSet<String>,
    show_hidden: bool,
) -> TreeShape {
    let mut shape = TreeShape::default();
    if !root.is_empty() {
        walk(
            root,
            &mut Vec::new(),
            listings,
            open,
            show_hidden,
            &mut shape,
        );
    }
    shape
}

fn walk(
    dir: &str,
    flags: &mut Vec<bool>,
    listings: &HashMap<String, Listing>,
    open: &HashSet<String>,
    show_hidden: bool,
    out: &mut TreeShape,
) {
    let listing = listings.get(dir);
    if listing.is_some_and(|l| l.error.is_some()) {
        return;
    }
    out.shown.push(dir.to_owned());
    let Some(entries) = listing.and_then(|l| l.entries.as_ref()) else {
        return;
    };
    let visible: Vec<usize> = (0..entries.len())
        .filter(|&i| show_hidden || !entries[i].hidden)
        .collect();
    for (n, &i) in visible.iter().enumerate() {
        let e = &entries[i];
        let is_open = e.is_dir && open.contains(&e.path);
        let child = if is_open { listings.get(&e.path) } else { None };
        flags.push(n == visible.len() - 1);
        out.rows.push(Row {
            dir: dir.to_owned(),
            index: i,
            depth: flags.len() - 1,
            last_flags: flags.clone(),
            open: is_open,
            status: if !is_open {
                None
            } else if child.is_some_and(|c| c.error.is_some()) {
                Some(RowStatus::Error)
            } else if child.is_some_and(|c| c.entries.is_some()) {
                None
            } else {
                Some(RowStatus::Loading)
            },
        });
        if is_open {
            walk(&e.path, flags, listings, open, show_hidden, out);
        }
        flags.pop();
    }
}

/// The `│  ├─ ` text drawn before a row's name.
pub fn connectors(row: &Row) -> String {
    let mut s = String::with_capacity(row.depth * 4 + 4);
    for d in 0..row.depth {
        s.push_str(if row.last_flags[d] { "   " } else { "│  " });
    }
    s.push_str(if row.last_flags[row.depth] {
        "└─ "
    } else {
        "├─ "
    });
    s
}

/// True when a fresh read shows exactly what is displayed already.
pub fn same_entries(a: Option<&[DirEntry]>, b: &[DirEntry]) -> bool {
    a.is_some_and(|a| a == b)
}

/// Where the cursor lands after the rows changed: on the same entry if it is
/// still there, else at the same position, clamped.
pub fn selection_index(
    rows: &[Row],
    listings: &HashMap<String, Listing>,
    selected: Option<&str>,
    previous: usize,
) -> usize {
    if rows.is_empty() {
        return 0;
    }
    if let Some(sel) = selected
        && let Some(i) = rows
            .iter()
            .position(|r| r.entry(listings).is_some_and(|e| e.path == sel))
    {
        return i;
    }
    previous.min(rows.len() - 1)
}

/// Directories first, then case-insensitive by name.
pub fn sort_entries(entries: &mut [DirEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(dir: &str, name: &str, is_dir: bool) -> DirEntry {
        DirEntry {
            name: name.into(),
            path: format!("{dir}/{name}"),
            is_dir,
            is_symlink: false,
            hidden: name.starts_with('.'),
        }
    }

    fn listing(entries: Vec<DirEntry>) -> Listing {
        Listing {
            entries: Some(entries),
            ..Default::default()
        }
    }

    fn fixture() -> HashMap<String, Listing> {
        let mut m = HashMap::new();
        m.insert(
            "/r".into(),
            listing(vec![
                e("/r", "src", true),
                e("/r", ".git", true),
                e("/r", "a.txt", false),
            ]),
        );
        m.insert(
            "/r/src".into(),
            listing(vec![e("/r/src", "main.rs", false)]),
        );
        m
    }

    #[test]
    fn flattens_open_folders_and_hides_dotfiles() {
        let l = fixture();
        let open: HashSet<String> = ["/r/src".to_string()].into();
        let t = build_tree("/r", &l, &open, false);
        let names: Vec<&str> = t
            .rows
            .iter()
            .map(|r| r.entry(&l).unwrap().name.as_str())
            .collect();
        assert_eq!(names, ["src", "main.rs", "a.txt"]);
        assert_eq!(t.shown, ["/r", "/r/src"]);
        assert_eq!(connectors(&t.rows[0]), "├─ ");
        assert_eq!(connectors(&t.rows[1]), "│  └─ ");
        assert_eq!(connectors(&t.rows[2]), "└─ ");
        let all = build_tree("/r", &l, &open, true);
        assert_eq!(all.rows.len(), 4);
    }

    #[test]
    fn loading_and_error_status() {
        let mut l = fixture();
        let open: HashSet<String> = ["/r/src".to_string(), "/r/.git".to_string()].into();
        l.remove("/r/src");
        l.insert(
            "/r/.git".into(),
            Listing {
                error: Some("denied".into()),
                ..Default::default()
            },
        );
        let t = build_tree("/r", &l, &open, true);
        assert_eq!(t.rows[0].status, Some(RowStatus::Loading));
        assert_eq!(t.rows[1].status, Some(RowStatus::Error));
        assert_eq!(t.shown, ["/r", "/r/src"]);
    }

    #[test]
    fn selection_survives_and_clamps() {
        let l = fixture();
        let t = build_tree("/r", &l, &HashSet::new(), false);
        assert_eq!(selection_index(&t.rows, &l, Some("/r/a.txt"), 0), 1);
        assert_eq!(selection_index(&t.rows, &l, Some("/gone"), 9), 1);
        assert_eq!(selection_index(&[], &l, None, 3), 0);
    }

    #[test]
    fn sorts_dirs_first() {
        let mut v = vec![e("/", "b", false), e("/", "Z", true), e("/", "a", false)];
        sort_entries(&mut v);
        let n: Vec<&str> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(n, ["Z", "a", "b"]);
    }
}
