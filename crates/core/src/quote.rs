//! Quote a path for insertion at a shell prompt, in the syntax of the shell that
//! will read it. The sidebar, the file picker and `cd` from the sidebar all type
//! paths into a terminal.

use crate::paths::to_shell_path;

/// Which syntax runs in a PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellFamily {
    #[default]
    Posix,
    Powershell,
    Cmd,
}

impl ShellFamily {
    /// Classify a shell program by its file name.
    pub fn of(program: &str) -> Self {
        let name = crate::paths::base_name(program).to_ascii_lowercase();
        let name = name.strip_suffix(".exe").unwrap_or(&name);
        match name {
            "pwsh" | "powershell" => Self::Powershell,
            "cmd" => Self::Cmd,
            _ => Self::Posix,
        }
    }
}

/// POSIX-quote: safe strings verbatim, anything else single-quoted.
pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b))
    {
        return s.into();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn plain_native(p: &str) -> bool {
    p.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_:.\\/-".contains(&b))
}

/// A native path, ready to type into a `family` shell.
pub fn quote_path_for(family: ShellFamily, native: &str) -> String {
    match family {
        ShellFamily::Posix => shell_quote(&to_shell_path(native)),
        ShellFamily::Powershell if plain_native(native) => native.into(),
        ShellFamily::Powershell => format!("'{}'", native.replace('\'', "''")),
        ShellFamily::Cmd if plain_native(native) => native.into(),
        ShellFamily::Cmd => format!("\"{native}\""),
    }
}

/// The command line (with its Enter) that moves a `family` shell into `dir`.
pub fn cd_command_for(family: ShellFamily, dir: &str) -> String {
    let q = quote_path_for(family, dir);
    match family {
        ShellFamily::Cmd => format!("cd /d {q}\r"),
        _ => format!("cd {q}\r"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix() {
        assert_eq!(shell_quote("/a/b.txt"), "/a/b.txt");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(quote_path_for(ShellFamily::Posix, r"C:\x y"), "'/c/x y'");
    }

    #[test]
    fn windows_shells() {
        assert_eq!(quote_path_for(ShellFamily::Powershell, r"C:\a"), r"C:\a");
        assert_eq!(
            quote_path_for(ShellFamily::Powershell, r"C:\it's"),
            r"'C:\it''s'"
        );
        assert_eq!(quote_path_for(ShellFamily::Cmd, r"C:\a b"), "\"C:\\a b\"");
        assert_eq!(cd_command_for(ShellFamily::Cmd, r"D:\x"), "cd /d D:\\x\r");
        assert_eq!(cd_command_for(ShellFamily::Posix, "/tmp"), "cd /tmp\r");
    }

    #[test]
    fn family_of_program() {
        assert_eq!(
            ShellFamily::of(r"C:\Windows\System32\cmd.exe"),
            ShellFamily::Cmd
        );
        assert_eq!(ShellFamily::of("pwsh"), ShellFamily::Powershell);
        assert_eq!(ShellFamily::of("/bin/bash"), ShellFamily::Posix);
    }
}
