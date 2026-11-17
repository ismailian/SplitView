//! Path helpers (pure string transforms) shared by the sidebar, the editor and
//! the prompt preview.

/// True for a native Windows path (`C:\…` or `C:/…`).
pub fn is_windows_path(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

/// Native path → the form a POSIX shell understands: on Windows `C:\Users\john`
/// becomes the msys `/c/Users/john`. POSIX paths pass through (backslashes become
/// slashes).
pub fn to_shell_path(p: &str) -> String {
    if is_windows_path(p) {
        let drive = p[..1].to_ascii_lowercase();
        let rest = p[2..].replace('\\', "/");
        format!("/{drive}/{}", rest.trim_start_matches('/'))
    } else {
        p.replace('\\', "/")
    }
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Best-effort percent-decoding; a malformed escape is kept literally.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse an OSC 7 payload (`file://host/path`) into the bare path. bash on
/// Windows reports its msys `$PWD` (`/c/Users/x`), returned as-is; PowerShell
/// and cmd report a native path after the host's slash (`/C:/Users/x`), returned
/// as the native `C:\Users\x`.
pub fn parse_osc7(data: &str) -> Option<String> {
    let rest = data.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let path = percent_decode(&rest[slash..]);
    let b = path.as_bytes();
    if b.len() >= 3
        && b[1].is_ascii_alphabetic()
        && b[2] == b':'
        && (b.len() == 3 || b[3] == b'/' || b[3] == b'\\')
    {
        let native = path[1..].replace('/', "\\");
        return Some(if native.len() == 2 {
            format!("{native}\\")
        } else {
            native
        });
    }
    Some(path)
}

/// Last segment of a path (either separator), or the path itself for a root.
pub fn base_name(p: &str) -> &str {
    p.split(['/', '\\']).rfind(|s| !s.is_empty()).unwrap_or(p)
}

/// Resolve a path typed at the shell against that shell's directory.
pub fn resolve_path(cwd: Option<&str>, p: &str) -> String {
    match cwd {
        Some(c) if !p.starts_with('/') && !is_windows_path(p) => {
            format!("{}/{p}", c.trim_end_matches(['/', '\\']))
        }
        _ => p.to_owned(),
    }
}

/// How the injected prompt renders the working directory (the Settings preview
/// mirrors `__sv_compact` / `__sv_tail_segs` in `shell.rs`; keep them in sync).
pub fn prompt_dir(path: &str, segments: usize) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if segments > 0 {
        let named: Vec<&str> = parts.iter().copied().filter(|s| !s.is_empty()).collect();
        return if named.len() > segments {
            named[named.len() - segments..].join("/")
        } else {
            path.to_owned()
        };
    }
    if parts.len() - 1 < 3 {
        return path.to_owned();
    }
    format!("{}/{}/.../{}", parts[0], parts[1], parts[parts.len() - 1])
}

/// Shorten a path for a narrow header: `first/…/last-two` once over `max` chars.
pub fn compact_path(p: &str, max: usize) -> String {
    if p.chars().count() <= max {
        return p.to_owned();
    }
    let sep = if p.contains('\\') && !p.contains('/') {
        "\\"
    } else {
        "/"
    };
    let parts: Vec<&str> = p.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    if parts.len() <= 2 {
        return p.to_owned();
    }
    let short = format!(
        "{}{sep}…{sep}{}",
        parts[0],
        parts[parts.len() - 2..].join(sep)
    );
    if short.chars().count() <= max {
        short
    } else {
        format!("…{sep}{}", parts[parts.len() - 1])
    }
}

/// msys `/c/Users/x` → native `C:\Users\x` (Windows only); anything else as-is.
/// Mirrors `shell::normalize_cwd`, which the app applies before spawning.
pub fn to_native(p: &str) -> String {
    if cfg!(windows) {
        let b = p.as_bytes();
        if b.len() >= 2
            && b[0] == b'/'
            && b[1].is_ascii_alphabetic()
            && (b.len() == 2 || b[2] == b'/')
        {
            let drive = (b[1] as char).to_ascii_uppercase();
            let rest = if b.len() > 3 { &p[3..] } else { "" };
            return format!("{drive}:\\{}", rest.replace('/', "\\"));
        }
    }
    p.to_owned()
}

/// The msys root of a Windows POSIX shell (`C:\Program Files\Git` for
/// `...\Git\bin\bash.exe` or `...\Git\usr\bin\bash.exe`), whose mounts its
/// absolute paths live under. `None` for anything that isn't an msys shell.
pub fn msys_root(program: &str) -> Option<String> {
    if !is_windows_path(program) {
        return None;
    }
    let p = program.replace('/', "\\");
    let dir = &p[..p.rfind('\\')?];
    let lower = dir.to_ascii_lowercase();
    let root = if lower.ends_with("\\usr\\bin") {
        &dir[..dir.len() - 8]
    } else if lower.ends_with("\\bin") {
        &dir[..dir.len() - 4]
    } else {
        return None;
    };
    Some(root.to_owned())
}

/// A path an msys shell reported (OSC 7) → a native one. `/c/x` is a drive;
/// `/tmp` is the user's temp dir (Git for Windows mounts it there); any other
/// absolute path lives under the msys root.
pub fn msys_to_native(path: &str, root: Option<&str>, temp: &str) -> String {
    let b = path.as_bytes();
    let drive_form = b.len() >= 2
        && b[0] == b'/'
        && b[1].is_ascii_alphabetic()
        && (b.len() == 2 || b[2] == b'/');
    match root {
        Some(root) if path.starts_with('/') && !drive_form => {
            let (base, rest) = match path.strip_prefix("/tmp") {
                Some(r) if r.is_empty() || r.starts_with('/') => (temp, r),
                _ => (root, path),
            };
            format!(
                "{}{}",
                base.trim_end_matches(['\\', '/']),
                rest.replace('/', "\\")
            )
        }
        _ => to_native(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msys_paths() {
        assert_eq!(
            msys_root(r"C:\Program Files\Git\bin\bash.exe").as_deref(),
            Some(r"C:\Program Files\Git")
        );
        assert_eq!(
            msys_root(r"C:\msys64\usr\bin\bash.exe").as_deref(),
            Some(r"C:\msys64")
        );
        assert_eq!(msys_root("/bin/bash"), None);
        assert_eq!(msys_root(r"C:\Windows\System32\cmd.exe"), None);
        let root = Some(r"C:\Git");
        assert_eq!(msys_to_native("/tmp/a/b", root, r"C:\T\"), r"C:\T\a\b");
        assert_eq!(msys_to_native("/tmp", root, r"C:\T"), r"C:\T");
        assert_eq!(
            msys_to_native("/usr/share", root, r"C:\T"),
            r"C:\Git\usr\share"
        );
        assert_eq!(msys_to_native("/tmpx", root, r"C:\T"), r"C:\Git\tmpx");
        if cfg!(windows) {
            assert_eq!(msys_to_native("/c/Users/x", root, "T"), r"C:\Users\x");
        }
        assert_eq!(msys_to_native("/home/x", None, "T"), "/home/x");
    }

    #[test]
    fn shell_paths() {
        assert_eq!(to_shell_path(r"C:\Users\john\proj"), "/c/Users/john/proj");
        assert_eq!(to_shell_path("D:/x"), "/d/x");
        assert_eq!(to_shell_path("/home/j"), "/home/j");
    }

    #[test]
    fn osc7() {
        assert_eq!(
            parse_osc7("file://host/home/j%20x").as_deref(),
            Some("/home/j x")
        );
        assert_eq!(
            parse_osc7("file://host/c/Users/x").as_deref(),
            Some("/c/Users/x")
        );
        assert_eq!(
            parse_osc7("file://host/C:/Users/x").as_deref(),
            Some(r"C:\Users\x")
        );
        assert_eq!(
            parse_osc7(r"file://host/C:\Users\x").as_deref(),
            Some(r"C:\Users\x")
        );
        assert_eq!(parse_osc7("file://host/C:").as_deref(), Some(r"C:\"));
        assert_eq!(parse_osc7("file://host/a%zz").as_deref(), Some("/a%zz"));
        assert_eq!(parse_osc7("http://x/y"), None);
        assert_eq!(parse_osc7("file://nohostpath"), None);
    }

    #[test]
    fn names_and_resolution() {
        assert_eq!(base_name("/a/b/c.txt"), "c.txt");
        assert_eq!(base_name(r"C:\a\b\"), "b");
        assert_eq!(base_name("/"), "/");
        assert_eq!(resolve_path(Some("/a/b/"), "c"), "/a/b/c");
        assert_eq!(resolve_path(Some("/a"), "/x"), "/x");
        assert_eq!(resolve_path(None, "c"), "c");
    }

    #[test]
    fn prompt_dirs() {
        assert_eq!(prompt_dir("~/projects/x/mobile", 2), "x/mobile");
        assert_eq!(prompt_dir("~/a", 2), "~/a");
        assert_eq!(prompt_dir("~/a/b/c/d", 0), "~/a/.../d");
        assert_eq!(prompt_dir("~/a/b", 0), "~/a/b");
    }

    #[test]
    fn compact() {
        assert_eq!(compact_path("/short", 32), "/short");
        assert_eq!(
            compact_path("/home/user/projects/thing/src", 20),
            "home/…/thing/src"
        );
        assert_eq!(
            compact_path("/a/b/averyveryverylongsegmentname", 12),
            "…/averyveryverylongsegmentname"
        );
    }

    #[test]
    fn native() {
        if cfg!(windows) {
            assert_eq!(to_native("/c/Users/x"), r"C:\Users\x");
            assert_eq!(to_native("/d"), r"D:\");
        }
        assert_eq!(to_native("/home/x"), "/home/x");
    }
}
