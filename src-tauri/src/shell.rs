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

/// Shell setup that installs the ParrotOS prompt, rebuilt on every prompt so the
/// directory and git branch stay live. The first line is
/// `┌─[user@host]─[dir]─[branch]`: red frame, white `user@host`, a blue dir that
/// `__sv_tilde` collapses to `~` (case-insensitively, so Windows `/c/users` paths
/// still match a `/c/Users` `$HOME`) before `__sv_compact` shortens it to
/// first-two-plus-last segments, and a yellow branch segment shown only inside a
/// git repo. The second line is `└──╼ $`, green, with the trailing colour left
/// un-reset so the typed command is green too.
///
/// bash builds `PS1` from a `PROMPT_COMMAND` hook so the `\[ \]` width markers are
/// honoured; the dir/branch splice in as plain text between single-quoted,
/// escape-bearing literals. Raw string => the shell's backslashes need no escaping.
const PARROT_BASH_SETUP: &str = r##"
__sv_compact() {
  local p="$1" first rest second n=0 t="$1"
  while [ "$t" != "${t#*/}" ]; do t="${t#*/}"; n=$((n+1)); done
  if [ "$n" -lt 3 ]; then printf '%s' "$p"; return; fi
  first="${p%%/*}"; rest="${p#*/}"; second="${rest%%/*}"
  printf '%s/%s/.../%s' "$first" "$second" "${p##*/}"
}
__sv_tilde() {
  local p="$1" pl hl
  case "$p" in
    "$HOME") printf '~'; return;;
    "$HOME"/*) printf '~%s' "${p#"$HOME"}"; return;;
  esac
  # Case-insensitive retry: on Windows the cwd can reach the shell as
  # /c/users/john while $HOME is /c/Users/john, so the exact prefix above
  # misses. Compare lowercased, but slice the original to keep the real case.
  pl=$(printf '%s' "$p" | tr 'A-Z' 'a-z')
  hl=$(printf '%s' "$HOME" | tr 'A-Z' 'a-z')
  case "$pl" in
    "$hl") printf '~'; return;;
    "$hl"/*) printf '~%s' "${p:${#HOME}}"; return;;
  esac
  printf '%s' "$p"
}
__sv_prompt() {
  local ec=$? d b bseg
  d="$(__sv_compact "$(__sv_tilde "$PWD")")"
  b="$(git symbolic-ref --quiet --short HEAD 2>/dev/null)"
  if [ -n "$b" ]; then
    bseg="\[\e[1;31m\]─[\[\e[1;33m\]$b\[\e[1;31m\]]"
  else
    bseg=""
  fi
  PS1='\[\e[1;31m\]┌─[\[\e[1;37m\]\u@\h\[\e[1;31m\]]─[\[\e[1;34m\]'"$d"'\[\e[1;31m\]]'"$bseg"'\n\[\e[1;31m\]└──╼ \[\e[1;32m\]\$ '
  return $ec
}
case "${PROMPT_COMMAND:-}" in
  *__sv_prompt*) ;;
  *) PROMPT_COMMAND="__sv_prompt${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
esac
"##;

/// zsh counterpart of [`PARROT_BASH_SETUP`], installed via a `precmd` hook. Uses
/// `%F{…}` colour escapes (zsh tracks their width), `%n@%m` for user@host and `%#`
/// for the prompt char; `%` in the dir/branch is doubled so it isn't reinterpreted.
/// `__sv_tilde` collapses `$HOME` to `~` (case-insensitively) before `__sv_compact`
/// shortens the path. The newline between the two prompt lines is literal.
const PARROT_ZSH_SETUP: &str = r##"
__sv_compact() {
  local p="$1" first rest second n=0 t="$1"
  while [ "$t" != "${t#*/}" ]; do t="${t#*/}"; n=$((n+1)); done
  if [ "$n" -lt 3 ]; then printf '%s' "$p"; return; fi
  first="${p%%/*}"; rest="${p#*/}"; second="${rest%%/*}"
  printf '%s/%s/.../%s' "$first" "$second" "${p##*/}"
}
__sv_tilde() {
  local p="$1" pl hl
  case "$p" in
    "$HOME") printf '~'; return;;
    "$HOME"/*) printf '~%s' "${p#"$HOME"}"; return;;
  esac
  # Case-insensitive retry: on Windows the cwd can reach the shell as
  # /c/users/john while $HOME is /c/Users/john, so the exact prefix above
  # misses. Compare lowercased, but slice the original to keep the real case.
  pl=$(printf '%s' "$p" | tr 'A-Z' 'a-z')
  hl=$(printf '%s' "$HOME" | tr 'A-Z' 'a-z')
  case "$pl" in
    "$hl") printf '~'; return;;
    "$hl"/*) printf '~%s' "${p:${#HOME}}"; return;;
  esac
  printf '%s' "$p"
}
__sv_prompt() {
  local ec=$? d b bseg
  d="$(__sv_compact "$(__sv_tilde "$PWD")")"
  d=${d//\%/%%}
  b="$(git symbolic-ref --quiet --short HEAD 2>/dev/null)"
  if [ -n "$b" ]; then
    b=${b//\%/%%}
    bseg="%F{red}─[%F{yellow}${b}%F{red}]"
  else
    bseg=""
  fi
  PROMPT="%F{red}┌─[%F{white}%n@%m%F{red}]─[%F{blue}${d}%F{red}]${bseg}
%F{red}└──╼ %F{green}%# "
  return $ec
}
typeset -ga precmd_functions
if [[ -z "${precmd_functions[(r)__sv_prompt]}" ]]; then
  precmd_functions+=(__sv_prompt)
fi
"##;

fn unique_path(prefix: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("{prefix}-{}-{}", std::process::id(), nanos))
}

/// Temp rcfile that sources the user's real bash config, then appends the prompt
/// `setup` shell code last (a static `PS1=…` for the plain style, or the dynamic
/// ParrotOS prompt builder in [`PARROT_BASH_SETUP`]).
fn bash_rcfile(setup: &str) -> Result<PathBuf> {
    let path = unique_path("splitview-bashrc");
    // Source login profiles first so PATH/Homebrew are populated (important on
    // macOS where a non-login shell has a broken environment, CLAUDE.md §8),
    // then the interactive rc, then set up the prompt last.
    let contents = format!(
        "# SplitView generated rcfile — do not edit\n\
         if [ -f \"$HOME/.bash_profile\" ]; then . \"$HOME/.bash_profile\";\n\
         elif [ -f \"$HOME/.bash_login\" ]; then . \"$HOME/.bash_login\";\n\
         elif [ -f \"$HOME/.profile\" ]; then . \"$HOME/.profile\"; fi\n\
         [ -f \"$HOME/.bashrc\" ] && . \"$HOME/.bashrc\"\n\
         {setup}"
    );
    fs::write(&path, contents)?;
    Ok(path)
}

/// Temp `ZDOTDIR` whose `.zshrc` sources the real one, then appends the prompt
/// `setup` shell code last (a static `PROMPT=…` for the plain style, or the
/// dynamic ParrotOS prompt builder in [`PARROT_ZSH_SETUP`]).
fn zsh_zdotdir(setup: &str) -> Result<PathBuf> {
    let dir = unique_path("splitview-zdotdir");
    fs::create_dir_all(&dir)?;
    let contents = format!(
        "# SplitView generated .zshrc — do not edit\n\
         [ -f \"${{HOME}}/.zprofile\" ] && source \"${{HOME}}/.zprofile\"\n\
         [ -f \"${{HOME}}/.zshrc\" ] && source \"${{HOME}}/.zshrc\"\n\
         {setup}"
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
            let setup = match cfg.style {
                PromptStyle::Parrot => PARROT_BASH_SETUP.to_string(),
                PromptStyle::Plain => format!("PS1='{}'\n", to_bash_ps1(&cfg.prompt)),
            };
            let rc = bash_rcfile(&setup)?;
            cmd.args(["--rcfile", &rc.to_string_lossy(), "-i"]);
        }
        ShellKind::Zsh => {
            let setup = match cfg.style {
                PromptStyle::Parrot => PARROT_ZSH_SETUP.to_string(),
                PromptStyle::Plain => format!("PROMPT='{}'\n", to_zsh_prompt(&cfg.prompt)),
            };
            let zdot = zsh_zdotdir(&setup)?;
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
    fn parrot_setup_compacts_dir_and_shows_branch() {
        // Rebuilt each prompt: compacts the cwd, adds a branch segment only in a
        // repo, and keeps the red frame + box characters.
        for setup in [PARROT_BASH_SETUP, PARROT_ZSH_SETUP] {
            assert!(setup.contains("__sv_compact"));
            assert!(setup.contains("/.../")); // the collapsed-middle marker
                                              // Home is collapsed to ~ through __sv_tilde (case-insensitive for
                                              // Windows), not a raw case-sensitive ${PWD/#$HOME/~} substitution.
            assert!(setup.contains("__sv_tilde"));
            assert!(setup.contains("tr 'A-Z' 'a-z'"));
            assert!(!setup.contains("PWD/#"));
            assert!(setup.contains("git symbolic-ref --quiet --short HEAD"));
            assert!(setup.contains("┌─["));
            assert!(setup.contains("└──╼"));
            assert!(setup.contains("return $ec"));
        }
        // bash uses \[ \] width markers + PROMPT_COMMAND; zsh uses %F + precmd.
        assert!(PARROT_BASH_SETUP.contains("\\u@\\h"));
        assert!(PARROT_BASH_SETUP.contains("1;32")); // green typed input
        assert!(PARROT_BASH_SETUP.contains("PROMPT_COMMAND"));
        assert!(PARROT_ZSH_SETUP.contains("%n@%m"));
        assert!(PARROT_ZSH_SETUP.contains("%F{green}"));
        assert!(PARROT_ZSH_SETUP.contains("precmd_functions+=(__sv_prompt)"));
    }
}
