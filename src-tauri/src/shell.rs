//! Shell detection and prompt injection (CLAUDE.md §8).
//!
//! The terminal emulator does not draw the prompt — the shell does. To get the
//! `{user} @ {cwd} $` prompt without destroying the user's environment, we
//! detect the shell and spawn it with a custom init that sources the user's
//! normal config *first*, then overrides only the prompt variable.
//!
//! Setting `PS1` as an environment variable does not work: the user's rc file
//! runs afterwards and overwrites it. Hence the `--rcfile` / `ZDOTDIR` dance.

use std::fs;
use std::path::PathBuf;

use portable_pty::CommandBuilder;

use crate::error::Result;

/// Inputs that decide how the shell is launched. The prompt `template` uses the
/// product-level placeholders `{user}` and `{cwd}`; translating those into each
/// shell's prompt syntax is this module's job and lives nowhere else.
#[derive(Clone, Debug)]
pub struct ShellConfig {
    /// Explicit shell override (absolute path or bare name). Falls back to
    /// `$SHELL`, then bash → zsh → sh.
    pub program: Option<String>,
    /// Working directory for the spawned shell.
    pub cwd: Option<String>,
    /// Prompt template, e.g. `"{user} @ {cwd} $ "`. Used only by `Plain` style.
    pub prompt: String,
    /// Which prompt to inject (the ParrotOS look, or the plain template).
    pub style: PromptStyle,
}

/// Which prompt SplitView injects into the spawned shell.
///
/// `Parrot` reproduces the iconic ParrotOS two-line prompt — a red box frame,
/// the user@host in light text, a blue path, and a green `$` whose colour is
/// intentionally left un-reset so typed input is green too. `Plain` falls back
/// to translating the `{user} @ {cwd}` template (the original behaviour).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PromptStyle {
    #[default]
    Parrot,
    Plain,
}

impl PromptStyle {
    /// Map the frontend's `promptStyle` string onto a style. Anything other than
    /// an explicit `"plain"` defaults to `Parrot`, the product's signature look.
    pub fn from_opt(s: Option<&str>) -> Self {
        match s.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("plain") => PromptStyle::Plain,
            _ => PromptStyle::Parrot,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ShellKind {
    Bash,
    Zsh,
    Fish,
    Sh,
    Other,
}

fn detect_program(cfg: &ShellConfig) -> String {
    if let Some(p) = &cfg.program {
        if !p.trim().is_empty() {
            return p.clone();
        }
    }
    if let Ok(s) = std::env::var("SHELL") {
        if !s.trim().is_empty() {
            return s;
        }
    }
    for candidate in ["/bin/bash", "/bin/zsh", "/bin/sh"] {
        if std::path::Path::new(candidate).exists() {
            return candidate.to_string();
        }
    }
    // Last resort: rely on PATH resolution. We never default to PowerShell or
    // cmd.exe (CLAUDE.md §18) — a POSIX shell is assumed.
    "bash".to_string()
}

fn kind_of(program: &str) -> ShellKind {
    let base = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_lowercase();
    let base = base.strip_suffix(".exe").unwrap_or(&base);
    match base {
        "bash" => ShellKind::Bash,
        "zsh" => ShellKind::Zsh,
        "fish" => ShellKind::Fish,
        "sh" | "dash" => ShellKind::Sh,
        _ => ShellKind::Other,
    }
}

/// `{user} @ {cwd} $ ` → bash `PS1` (`\u` = user, `\w` = cwd).
fn to_bash_ps1(template: &str) -> String {
    template.replace("{user}", "\\u").replace("{cwd}", "\\w")
}

/// `{user} @ {cwd} $ ` → zsh `PROMPT` (`%n` = user, `%~` = cwd).
fn to_zsh_prompt(template: &str) -> String {
    template.replace("{user}", "%n").replace("{cwd}", "%~")
}

/// The iconic ParrotOS bash prompt.
///
/// Two lines: `┌─[user@host]─[cwd]` then `└──╼ $`. The frame is red, user@host
/// light, the path blue, and the `$` green. Every escape is wrapped in `\[ \]`
/// so bash measures the prompt width correctly, and the trailing green is left
/// un-reset on purpose so the command the user types is green, matching Parrot.
fn parrot_bash_ps1() -> String {
    "\\[\\e[1;31m\\]┌─[\\[\\e[1;37m\\]\\u@\\h\\[\\e[1;31m\\]]─[\\[\\e[1;34m\\]\\w\
     \\[\\e[1;31m\\]]\\n\\[\\e[1;31m\\]└──╼ \\[\\e[1;32m\\]\\$ "
        .to_string()
}

/// The iconic ParrotOS zsh prompt. `%F{…}`/`%f` are zsh prompt escapes, so zsh
/// tracks their (zero) width for us; the layout/colours mirror `parrot_bash_ps1`.
fn parrot_zsh_prompt() -> String {
    "%F{red}┌─[%F{white}%n@%m%F{red}]─[%F{blue}%~%F{red}]\n%F{red}└──╼ %F{green}%# ".to_string()
}

fn unique_path(prefix: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("{prefix}-{}-{}", std::process::id(), nanos))
}

/// Temp rcfile that sources the user's real bash config, then sets `PS1` last.
fn bash_rcfile(ps1: &str) -> Result<PathBuf> {
    let path = unique_path("splitview-bashrc");
    // Source login profiles first so PATH/Homebrew are populated (important on
    // macOS where a non-login shell has a broken environment, CLAUDE.md §8),
    // then the interactive rc, then override only the prompt.
    let contents = format!(
        "# SplitView generated rcfile — do not edit\n\
         if [ -f \"$HOME/.bash_profile\" ]; then . \"$HOME/.bash_profile\";\n\
         elif [ -f \"$HOME/.bash_login\" ]; then . \"$HOME/.bash_login\";\n\
         elif [ -f \"$HOME/.profile\" ]; then . \"$HOME/.profile\"; fi\n\
         [ -f \"$HOME/.bashrc\" ] && . \"$HOME/.bashrc\"\n\
         PS1='{ps1}'\n"
    );
    fs::write(&path, contents)?;
    Ok(path)
}

/// Temp `ZDOTDIR` whose `.zshrc` sources the real one, then sets `PROMPT` last.
fn zsh_zdotdir(prompt: &str) -> Result<PathBuf> {
    let dir = unique_path("splitview-zdotdir");
    fs::create_dir_all(&dir)?;
    let contents = format!(
        "# SplitView generated .zshrc — do not edit\n\
         [ -f \"${{HOME}}/.zprofile\" ] && source \"${{HOME}}/.zprofile\"\n\
         [ -f \"${{HOME}}/.zshrc\" ] && source \"${{HOME}}/.zshrc\"\n\
         PROMPT='{prompt}'\n"
    );
    fs::write(dir.join(".zshrc"), contents)?;
    Ok(dir)
}

/// Build the `CommandBuilder` for the user's shell with prompt injection wired
/// in. The environment is inherited from the parent process by default, so
/// PATH/aliases/completions survive.
pub fn build_command(cfg: &ShellConfig) -> Result<CommandBuilder> {
    let program = detect_program(cfg);
    let kind = kind_of(&program);
    let mut cmd = CommandBuilder::new(&program);

    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    match kind {
        ShellKind::Bash => {
            let ps1 = match cfg.style {
                PromptStyle::Parrot => parrot_bash_ps1(),
                PromptStyle::Plain => to_bash_ps1(&cfg.prompt),
            };
            let rc = bash_rcfile(&ps1)?;
            cmd.args(["--rcfile", &rc.to_string_lossy(), "-i"]);
        }
        ShellKind::Zsh => {
            let prompt = match cfg.style {
                PromptStyle::Parrot => parrot_zsh_prompt(),
                PromptStyle::Plain => to_zsh_prompt(&cfg.prompt),
            };
            let zdot = zsh_zdotdir(&prompt)?;
            cmd.env("ZDOTDIR", &zdot);
            cmd.arg("-i");
        }
        // fish / others: best-effort. Don't mangle the user's prompt; just run
        // interactively and let them keep their own (CLAUDE.md §8).
        ShellKind::Fish | ShellKind::Sh | ShellKind::Other => {
            cmd.arg("-i");
        }
    }

    match &cfg.cwd {
        Some(cwd) if !cwd.trim().is_empty() => cmd.cwd(cwd),
        _ => {
            if let Some(home) = dirs::home_dir() {
                cmd.cwd(home);
            }
        }
    }

    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_kind_from_path() {
        assert_eq!(kind_of("/bin/bash"), ShellKind::Bash);
        assert_eq!(kind_of("/usr/local/bin/zsh"), ShellKind::Zsh);
        assert_eq!(kind_of("fish"), ShellKind::Fish);
        assert_eq!(kind_of("C:\\msys64\\usr\\bin\\bash.exe"), ShellKind::Bash);
        assert_eq!(kind_of("/bin/sh"), ShellKind::Sh);
        assert_eq!(kind_of("nu"), ShellKind::Other);
    }

    #[test]
    fn translates_prompt_templates() {
        assert_eq!(to_bash_ps1("{user} @ {cwd} $ "), "\\u @ \\w $ ");
        assert_eq!(to_zsh_prompt("{user} @ {cwd} $ "), "%n @ %~ $ ");
    }

    #[test]
    fn prompt_style_parses_from_frontend() {
        assert_eq!(PromptStyle::from_opt(Some("plain")), PromptStyle::Plain);
        assert_eq!(PromptStyle::from_opt(Some("PLAIN")), PromptStyle::Plain);
        assert_eq!(PromptStyle::from_opt(Some("parrot")), PromptStyle::Parrot);
        // Unknown/missing falls back to the signature Parrot look.
        assert_eq!(PromptStyle::from_opt(Some("nonsense")), PromptStyle::Parrot);
        assert_eq!(PromptStyle::from_opt(None), PromptStyle::Parrot);
        assert_eq!(PromptStyle::default(), PromptStyle::Parrot);
    }

    #[test]
    fn parrot_prompts_carry_frame_and_colours() {
        let bash = parrot_bash_ps1();
        // Box frame, the bash user/cwd tokens, and a green (1;32) input colour.
        assert!(bash.contains("┌─["));
        assert!(bash.contains("└──╼"));
        assert!(bash.contains("\\u@\\h"));
        assert!(bash.contains("\\w"));
        assert!(bash.contains("1;32"));
        // Escapes are wrapped for bash width tracking, and there is no stray space
        // collapsed by the source line-continuation.
        assert!(bash.contains("\\w\\[\\e[1;31m\\]]"));

        let zsh = parrot_zsh_prompt();
        assert!(zsh.contains("┌─["));
        assert!(zsh.contains("%n@%m"));
        assert!(zsh.contains("%F{green}"));
    }
}
