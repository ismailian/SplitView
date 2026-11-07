//! Shell profiles: which shells are installed, and where (CLAUDE.md §8, §18).
//!
//! A *profile* is a shell the user can pick as the default for new panes or open
//! a single pane with. Detection is a list of well-known install locations plus
//! a `PATH` search; nothing is spawned to find out.
//!
//! On Windows this is what makes a fresh machine work at all. A GUI app there has
//! no `$SHELL` and no `/bin/bash`, and Git for Windows puts only `Git\cmd` on
//! `PATH` — that directory holds `git.exe` but not `bash.exe` — so asking the OS
//! for a bare `bash` fails even with Git installed. Looking in Git's own install
//! directory is the only reliable way to find Git Bash.

use std::path::{Path, PathBuf};

/// Which syntax family a shell speaks. Decides how the prompt is injected
/// (`shell.rs`) and how the frontend quotes a path it types for the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellFamily {
    /// bash / zsh / fish / sh: POSIX quoting, msys paths on Windows.
    Posix,
    /// Windows PowerShell 5.1 or PowerShell 7 (`pwsh`).
    Powershell,
    /// `cmd.exe`.
    Cmd,
}

impl ShellFamily {
    /// The family of a shell, from its program name or path.
    pub fn of(program: &str) -> Self {
        match base_name(program).as_str() {
            "powershell" | "pwsh" => ShellFamily::Powershell,
            "cmd" => ShellFamily::Cmd,
            _ => ShellFamily::Posix,
        }
    }
}

/// One installed shell, as offered in the profile picker.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellProfile {
    /// Stable id, persisted as the user's default (`git-bash`, `powershell`, …).
    pub id: String,
    /// What the picker shows (`Git Bash`, `Windows PowerShell`, …).
    pub name: String,
    /// Absolute path to the executable, handed back to `spawn_pty` as `shell`.
    pub program: String,
    pub family: ShellFamily,
}

/// Lowercased file name without directory or `.exe`: `C:\Git\bin\bash.exe` →
/// `bash`.
pub fn base_name(program: &str) -> String {
    let base = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_lowercase();
    match base.strip_suffix(".exe") {
        Some(b) => b.to_string(),
        None => base,
    }
}

/// Every directory on `PATH` holding `name` (with `.exe` appended on Windows).
fn which_all(name: &str) -> Vec<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    std::env::split_paths(&path)
        .map(|dir| dir.join(&file))
        .filter(|p| p.is_file())
        .collect()
}

/// `path` if it is an existing file.
#[cfg(windows)]
fn existing(path: PathBuf) -> Option<PathBuf> {
    path.is_file().then_some(path)
}

/// Push a profile unless one with the same program (compared case-insensitively,
/// which is right on Windows and harmless elsewhere) is already listed.
fn push(out: &mut Vec<ShellProfile>, id: &str, name: &str, program: &Path) {
    let program = program.to_string_lossy().to_string();
    if out
        .iter()
        .any(|p| p.id == id || p.program.eq_ignore_ascii_case(&program))
    {
        return;
    }
    out.push(ShellProfile {
        id: id.to_string(),
        name: name.to_string(),
        family: ShellFamily::of(&program),
        program,
    });
}

/// The installed shells, best default first: on Windows a POSIX shell (Git Bash,
/// then MSYS2) ahead of PowerShell 7, Windows PowerShell and cmd, so the
/// automatic fallback stays the minimal POSIX experience whenever one exists.
#[cfg(windows)]
pub fn detect() -> Vec<ShellProfile> {
    let env_path = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut out = Vec::new();

    // Git Bash. `bin\bash.exe` is Git's launcher: it puts `/usr/bin` and
    // `/mingw64/bin` on PATH before starting the real `usr\bin\bash.exe`, which is
    // what Windows Terminal runs too.
    let mut git_roots: Vec<PathBuf> = Vec::new();
    for key in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(p) = env_path(key) {
            git_roots.push(p.join("Git"));
        }
    }
    if let Some(p) = env_path("LOCALAPPDATA") {
        git_roots.push(p.join("Programs").join("Git"));
    }
    // Wherever `git.exe` is on PATH (`<root>\cmd\git.exe`), the root is one up —
    // this catches custom install locations, scoop and portable Git.
    for git in which_all("git") {
        if let Some(root) = git.parent().and_then(Path::parent) {
            git_roots.push(root.to_path_buf());
        }
    }
    if let Some(bash) = git_roots
        .iter()
        .find_map(|root| existing(root.join("bin").join("bash.exe")))
    {
        push(&mut out, "git-bash", "Git Bash", &bash);
    }

    // MSYS2's bash, if it is installed in its default place.
    if let Some(bash) = existing(PathBuf::from(r"C:\msys64\usr\bin\bash.exe")) {
        push(&mut out, "msys2-bash", "MSYS2 Bash", &bash);
    }

    // Any other bash on PATH (Cygwin, a Git `usr\bin` someone added) — but not
    // the WSL launchers: `System32\bash.exe`, and the Store's app-execution alias
    // in `WindowsApps`. Both only print an install hint and exit when no
    // distribution is set up, which is a pane that closes as soon as it opens.
    let system_root = env_path("SystemRoot").unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let system32 = system_root.join("System32");
    let system32_lower = system32.to_string_lossy().to_lowercase();
    for bash in which_all("bash") {
        let dir = bash
            .parent()
            .map(|d| d.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if dir != system32_lower && !dir.ends_with(r"\windowsapps") {
            push(&mut out, "bash", "Bash", &bash);
            break;
        }
    }

    // PowerShell 7 is a separate install from the Windows PowerShell 5.1 that
    // ships with the OS; list both when both are there.
    let pwsh = which_all("pwsh").into_iter().next().or_else(|| {
        env_path("ProgramFiles")
            .and_then(|p| existing(p.join("PowerShell").join("7").join("pwsh.exe")))
    });
    if let Some(pwsh) = pwsh {
        push(&mut out, "pwsh", "PowerShell 7", &pwsh);
    }
    if let Some(ps) = existing(
        system32
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe"),
    ) {
        push(&mut out, "powershell", "Windows PowerShell", &ps);
    }

    let cmd = env_path("ComSpec")
        .and_then(existing)
        .or_else(|| existing(system32.join("cmd.exe")));
    if let Some(cmd) = cmd {
        push(&mut out, "cmd", "Command Prompt", &cmd);
    }
    out
}

/// The installed shells, the user's login shell (`$SHELL`) first.
#[cfg(not(windows))]
pub fn detect() -> Vec<ShellProfile> {
    let mut out = Vec::new();
    if let Ok(shell) = std::env::var("SHELL") {
        let path = PathBuf::from(shell.trim());
        if path.is_file() {
            let id = base_name(&shell);
            let name = format!("{} (login shell)", display_name(&id));
            push(&mut out, &id, &name, &path);
        }
    }
    let candidates = [
        (
            "bash",
            &[
                "/bin/bash",
                "/usr/bin/bash",
                "/opt/homebrew/bin/bash",
                "/usr/local/bin/bash",
            ][..],
        ),
        (
            "zsh",
            &["/bin/zsh", "/usr/bin/zsh", "/opt/homebrew/bin/zsh"][..],
        ),
        (
            "fish",
            &[
                "/usr/bin/fish",
                "/opt/homebrew/bin/fish",
                "/usr/local/bin/fish",
            ][..],
        ),
        ("sh", &["/bin/sh"][..]),
    ];
    for (id, paths) in candidates {
        let found = paths
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
            .or_else(|| which_all(id).into_iter().next());
        if let Some(path) = found {
            push(&mut out, id, display_name(id), &path);
        }
    }
    // PowerShell 7 runs on macOS and Linux too, for whoever installed it.
    if let Some(pwsh) = which_all("pwsh").into_iter().next() {
        push(&mut out, "pwsh", "PowerShell 7", &pwsh);
    }
    out
}

#[cfg(not(windows))]
fn display_name(id: &str) -> &'static str {
    match id {
        "bash" => "Bash",
        "zsh" => "Zsh",
        "fish" => "Fish",
        "sh" => "sh",
        "pwsh" => "PowerShell 7",
        _ => "Shell",
    }
}

/// The program to run when neither the frontend nor the config named one: the
/// first detected profile, which is a POSIX shell whenever one is installed.
pub fn default_program() -> Option<String> {
    detect().into_iter().next().map(|p| p.program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_follows_the_program_name() {
        assert_eq!(ShellFamily::of("/bin/bash"), ShellFamily::Posix);
        assert_eq!(
            ShellFamily::of(r"C:\Program Files\Git\bin\bash.exe"),
            ShellFamily::Posix
        );
        assert_eq!(
            ShellFamily::of(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"),
            ShellFamily::Powershell
        );
        assert_eq!(ShellFamily::of("pwsh"), ShellFamily::Powershell);
        assert_eq!(
            ShellFamily::of(r"C:\Windows\system32\CMD.EXE"),
            ShellFamily::Cmd
        );
    }

    #[test]
    fn base_name_strips_directories_and_exe() {
        assert_eq!(base_name(r"C:\Git\bin\bash.exe"), "bash");
        assert_eq!(base_name("/usr/bin/zsh"), "zsh");
        assert_eq!(base_name("PWSH.EXE"), "pwsh");
    }

    #[test]
    fn detected_profiles_exist_and_are_unique() {
        let profiles = detect();
        for (i, p) in profiles.iter().enumerate() {
            assert!(Path::new(&p.program).is_file(), "{} is missing", p.program);
            assert!(
                profiles[..i].iter().all(|q| q.id != p.id),
                "duplicate {}",
                p.id
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_always_has_cmd_and_never_offers_the_wsl_launcher() {
        let profiles = detect();
        assert!(profiles.iter().any(|p| p.family == ShellFamily::Cmd));
        assert!(profiles
            .iter()
            .all(|p| !p.program.to_lowercase().ends_with(r"system32\bash.exe")));
    }
}
