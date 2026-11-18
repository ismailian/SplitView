//! Runs `git` for the sidebar's change decorations (parsing: `sv_core::git`).
//!
//! We shell out to the user's own `git` rather than linking libgit2 or gitoxide:
//! no C build or large dependency tree, and the answer is exactly what `git
//! status` in the terminal next to it says (same config, excludes, fsmonitor,
//! safe.directory). Always on a worker thread; a status takes milliseconds on a
//! normal repo and the sidebar keeps showing the previous one meanwhile.
//!
//! `--no-optional-locks`: a plain `git status` may refresh the index and take
//! `index.lock`, which would make a `git commit` typed at that moment fail with
//! "index.lock exists". A background reader must never do that.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use sv_core::git::{GitStatus, parse_status};

fn program() -> Option<&'static PathBuf> {
    static GIT: OnceLock<Option<PathBuf>> = OnceLock::new();
    GIT.get_or_init(crate::profiles::git_program).as_ref()
}

fn git(dir: &str) -> Option<Command> {
    let mut c = Command::new(program()?);
    c.arg("--no-optional-locks")
        .arg("-C")
        .arg(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000; // a GUI app: no console flash
        c.creation_flags(CREATE_NO_WINDOW);
    }
    Some(c)
}

/// The status of the repository containing `dir`; `None` if `dir` is not in a
/// work tree, or git isn't installed (neither is worth an error in the UI).
pub fn status(dir: &str) -> Option<GitStatus> {
    let top = git(dir)?
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !top.status.success() {
        return None;
    }
    let mut root = String::from_utf8_lossy(&top.stdout).trim().to_owned();
    if cfg!(windows) {
        root = root.replace('/', "\\"); // git says `D:/x`
    }
    if root.is_empty() {
        return None; // inside `.git` itself
    }
    let out = git(dir)?
        .args(["status", "--porcelain=v2", "-z", "--branch"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| parse_status(&root, &out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sv_core::git::Change;

    #[test]
    fn status_of_a_scratch_repo() {
        if program().is_none() {
            return; // no git on this machine
        }
        let dir = std::env::temp_dir().join(format!("sv-git-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let d = dir.to_string_lossy().into_owned();
        let ok = git(&d).unwrap().args(["init", "-q"]).status().unwrap();
        assert!(ok.success());
        std::fs::write(dir.join("sub").join("new file.txt"), "x").unwrap();
        let st = status(&d).expect("a repo");
        assert!(st.contains(&d));
        let f = dir.join("sub").join("new file.txt");
        assert_eq!(
            st.lookup(&f.to_string_lossy(), false),
            Some(Change::Untracked)
        );
        assert_eq!(
            st.lookup(&dir.join("sub").to_string_lossy(), true),
            Some(Change::Untracked)
        );
        assert_eq!(st.oid, None); // no commits yet
        let _ = std::fs::remove_dir_all(&dir);
    }
}
