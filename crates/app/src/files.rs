//! Text file access for the editor panel (CLAUDE.md §9).
//!
//! Three operations: read a file, stat it, and write it back. A file is opened
//! only if it is valid UTF-8 and small enough to edit comfortably, so a binary
//! can never be opened, "edited" and saved back corrupted.
//!
//! Every read comes with a [`FileStamp`] and a write states the stamp it expects.
//! The terminal sits right next to the editor, so a file changing underneath an
//! open buffer (`git checkout`, a formatter) is the normal case, not an edge one,
//! and a save must not silently undo it.
//!
//! These are for the local editor only. Like `read_dir`, they are never
//! exposed to a remote client (CLAUDE.md §21).

use std::fs::{self, File, Metadata};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::error::{Error, Result};
use crate::shell::normalize_cwd;

/// Largest file the editor opens. Past this it stops being a source file, and
/// re-laying-out the whole buffer on every keystroke stops feeling instant.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// How much of a file is searched for a NUL when deciding whether it is binary.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// Which version of a file this is. Compared for equality, never ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileStamp {
    pub modified_ms: u64,
    pub size: u64,
}

impl FileStamp {
    fn of(meta: &Metadata) -> Self {
        let modified_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as u64);
        Self {
            modified_ms,
            size: meta.len(),
        }
    }
}

/// An opened file. `path` is the native spelling, so a file opened twice under
/// different spellings (`/c/x`, `C:\x`) lands in the same tab.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextFile {
    pub path: String,
    pub content: String,
    pub stamp: FileStamp,
}

fn resolve(path: &str) -> PathBuf {
    PathBuf::from(normalize_cwd(path.trim()))
}

/// Why `bytes` can't be opened as text, if so. Separate from reading so the rule
/// is testable without touching the disk.
fn decode(bytes: Vec<u8>) -> std::result::Result<String, &'static str> {
    let sniff = &bytes[..bytes.len().min(BINARY_SNIFF_BYTES)];
    if sniff.contains(&0) {
        return Err("binary file");
    }
    String::from_utf8(bytes).map_err(|_| "not UTF-8 text")
}

/// Read a text file. The stamp is taken *before* the read: if the file changes
/// in between, the stamp is stale and the change is noticed later, rather than a
/// newer stamp vouching for older content.
pub fn read_text(path: &str) -> Result<TextFile> {
    let path = resolve(path);
    let meta = fs::metadata(&path)?;
    if meta.is_dir() {
        return Err(Error::File("is a directory".to_string()));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(Error::File(format!(
            "too large ({} MB, the limit is {} MB)",
            meta.len() / (1024 * 1024),
            MAX_FILE_BYTES / (1024 * 1024)
        )));
    }
    let stamp = FileStamp::of(&meta);
    let content = decode(fs::read(&path)?).map_err(|why| Error::File(why.to_string()))?;
    Ok(TextFile {
        path: path.to_string_lossy().to_string(),
        content,
        stamp,
    })
}

/// A file's current stamp, or `None` if it no longer exists.
pub fn stamp(path: &str) -> Result<Option<FileStamp>> {
    stamp_of(&resolve(path))
}

fn stamp_of(path: &Path) -> Result<Option<FileStamp>> {
    match fs::metadata(path) {
        Ok(meta) => Ok(Some(FileStamp::of(&meta))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Write `content` over `path`, provided the file is still at `expected` (`None`
/// writes regardless). Returns the new stamp, or `None` if the file has moved on
/// and nothing was written — the caller decides whether to overwrite.
pub fn write_text(
    path: &str,
    content: &str,
    expected: Option<FileStamp>,
) -> Result<Option<FileStamp>> {
    let path = resolve(path);
    // Write through a symlink rather than replacing the link with a file.
    let target = fs::canonicalize(&path).unwrap_or(path);
    if expected.is_some() && stamp_of(&target)? != expected {
        return Ok(None);
    }
    replace(&target, content.as_bytes())?;
    stamp_of(&target)
}

/// Replace a file's contents so a failure part-way (a full disk, a crash) leaves
/// the old file intact: stage a sibling, then rename it over the target. Only if
/// no sibling can exist, or the rename is refused (Windows, with the file open
/// elsewhere), does it fall back to writing in place.
fn replace(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let meta = fs::metadata(target).ok();
    // A rename would succeed on a read-only file, because it only needs write
    // access to the directory. Read-only has to keep meaning read-only.
    if meta.as_ref().is_some_and(|m| m.permissions().readonly()) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }

    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let staged = target.with_file_name(format!(".{name}.splitview-{}.tmp", std::process::id()));
    let mut file = match File::create(&staged) {
        Ok(file) => file,
        Err(_) => return fs::write(target, bytes), // e.g. no rights in the directory
    };

    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file); // Windows can't rename a file that is still open
    let result = written
        .and_then(|()| match &meta {
            Some(m) => fs::set_permissions(&staged, m.permissions()),
            None => Ok(()),
        })
        .and_then(|()| {
            // The staged copy is complete, so there is room to write this.
            fs::rename(&staged, target).or_else(|_| fs::write(target, bytes))
        });
    let _ = fs::remove_file(&staged);
    result
}

/// What "reveal" is called on this OS (sidebar menus).
pub const REVEAL_LABEL: &str = if cfg!(windows) {
    "Reveal in File Explorer"
} else if cfg!(target_os = "macos") {
    "Reveal in Finder"
} else {
    "Open in file manager"
};

/// Show `path` in the OS file manager: a folder opens; a file is selected in its
/// folder (Windows, macOS — Linux has no portable "select", so its folder opens).
/// The path goes over as one argument, never through a shell, and the child is
/// reaped on a worker thread.
pub fn reveal(path: &str) {
    let path = path.to_owned();
    let _ = std::thread::Builder::new()
        .name("sv-reveal".into())
        .spawn(move || {
            let p = Path::new(&path);
            let is_dir = p.is_dir();
            #[cfg(windows)]
            let child = {
                use std::os::windows::process::CommandExt;
                let explorer = std::env::var_os("SystemRoot")
                    .map(|r| PathBuf::from(r).join("explorer.exe"))
                    .unwrap_or_else(|| PathBuf::from("explorer.exe"));
                let mut c = std::process::Command::new(explorer);
                if is_dir {
                    c.arg(&path);
                } else {
                    // Explorer parses `/select,"<path>"` itself; std's quoting of
                    // the whole argument would break it on paths with spaces.
                    // `"` can't occur in a Windows path, so this can't be escaped.
                    c.raw_arg(format!("/select,\"{path}\""));
                }
                c.spawn()
            };
            #[cfg(target_os = "macos")]
            let child = {
                let mut c = std::process::Command::new("open");
                if !is_dir {
                    c.arg("-R");
                }
                c.arg(&path).spawn()
            };
            #[cfg(all(unix, not(target_os = "macos")))]
            let child = {
                let dir = if is_dir { p } else { p.parent().unwrap_or(p) };
                std::process::Command::new("xdg-open").arg(dir).spawn()
            };
            if let Ok(mut c) = child {
                let _ = c.wait();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("splitview-files-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("create test dir");
            Self(dir)
        }

        fn file(&self, name: &str, bytes: &[u8]) -> String {
            let path = self.0.join(name);
            fs::write(&path, bytes).expect("write test file");
            path.to_string_lossy().to_string()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn decode_accepts_text_and_refuses_what_it_would_corrupt() {
        assert_eq!(
            decode(b"fn main() {}\n".to_vec()).as_deref(),
            Ok("fn main() {}\n")
        );
        assert_eq!(decode("héllo".as_bytes().to_vec()).as_deref(), Ok("héllo"));
        assert_eq!(decode(b"\x7fELF\0\0".to_vec()), Err("binary file"));
        assert_eq!(decode(vec![0x66, 0xff, 0x66]), Err("not UTF-8 text"));
    }

    #[test]
    fn reads_and_writes_a_round_trip() {
        let dir = TempDir::new("round-trip");
        let path = dir.file("a.txt", b"old\r\n");

        let file = read_text(&path).expect("read");
        assert_eq!(file.content, "old\r\n");
        let stamp = write_text(&path, "new\r\n", Some(file.stamp))
            .expect("write")
            .expect("not a conflict");

        assert_eq!(fs::read(&path).expect("read back"), b"new\r\n");
        assert_eq!(read_text(&path).expect("re-read").stamp, stamp);
    }

    #[test]
    fn a_write_leaves_no_staged_file_behind() {
        let dir = TempDir::new("no-litter");
        let path = dir.file("a.txt", b"old");
        write_text(&path, "new", None).expect("write");
        let names: Vec<_> = fs::read_dir(&dir.0)
            .expect("list")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(names, ["a.txt"]);
    }

    #[test]
    fn a_stale_stamp_writes_nothing() {
        let dir = TempDir::new("conflict");
        let path = dir.file("a.txt", b"mine");
        let stamp = read_text(&path).expect("read").stamp;
        // Same mtime is possible on a fast disk, so the size has to differ.
        fs::write(&path, b"theirs, longer").expect("change the file");

        assert_eq!(
            write_text(&path, "mine 2", Some(stamp)).expect("write"),
            None
        );
        assert_eq!(fs::read(&path).expect("read back"), b"theirs, longer");

        // Forcing it is the caller's explicit decision.
        assert!(write_text(&path, "mine 2", None).expect("force").is_some());
        assert_eq!(fs::read(&path).expect("read back"), b"mine 2");
    }

    #[test]
    fn a_deleted_file_is_a_conflict_not_a_silent_recreate() {
        let dir = TempDir::new("deleted");
        let path = dir.file("a.txt", b"x");
        let stamp = read_text(&path).expect("read").stamp;
        fs::remove_file(&path).expect("delete");

        assert_eq!(super::stamp(&path).expect("stat"), None);
        assert_eq!(write_text(&path, "y", Some(stamp)).expect("write"), None);
        assert!(!Path::new(&path).exists());
    }

    #[test]
    fn a_read_only_file_stays_read_only() {
        let dir = TempDir::new("readonly");
        let path = dir.file("a.txt", b"x");
        let mut perms = fs::metadata(&path).expect("stat").permissions();
        perms.set_readonly(true);
        fs::set_permissions(&path, perms.clone()).expect("set read-only");

        assert!(write_text(&path, "y", None).is_err());
        assert_eq!(fs::read(&path).expect("read back"), b"x");

        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false); // or the temp dir can't be removed on Windows
        fs::set_permissions(&path, perms).expect("clear read-only");
    }

    #[test]
    fn refuses_directories_and_oversized_files() {
        let dir = TempDir::new("refuse");
        assert!(read_text(&dir.0.to_string_lossy()).is_err());

        let big = dir.0.join("big.txt");
        let file = File::create(&big).expect("create");
        file.set_len(MAX_FILE_BYTES + 1).expect("grow");
        assert!(read_text(&big.to_string_lossy()).is_err());
    }
}
