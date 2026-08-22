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
    /// User-defined aliases, injected into the spawned shell as real `alias`
    /// definitions so typing the name runs the underlying command.
    pub aliases: Vec<Alias>,
    /// Which informational segments the prompt shows (user/host/dir/branch/…).
    pub prompt_info: PromptInfo,
    /// Banner printed once at shell startup, before the first prompt. Empty for
    /// none. `\e[…m` escapes are honoured so banners can be coloured.
    pub banner: String,
}

/// A user-defined shell alias from Settings. `command` is single-quote-escaped
/// when written into the rcfile, so arbitrary commands are safe.
#[derive(Clone, Debug)]
pub struct Alias {
    pub name: String,
    pub command: String,
}

/// Which informational segments the ParrotOS prompt's first line shows. Off
/// segments are simply omitted. These are surfaced as toggles in Settings and
/// passed through to the shell as `SV_SHOW_*` environment variables that
/// `__sv_prompt` reads each time it rebuilds the prompt.
#[derive(Clone, Copy, Debug)]
pub struct PromptInfo {
    pub user: bool,
    pub host: bool,
    pub dir: bool,
    pub branch: bool,
    pub time: bool,
    pub exit_status: bool,
    /// How much of the working directory the `dir` segment shows, passed through
    /// as `SV_DIR_SEGMENTS`: `0` keeps the whole path (with the middle elided
    /// once it is deep), `n > 0` keeps only the last `n` segments — counted from
    /// the end, so `2` of `/d/projects/project-x/mobile-app` is
    /// `project-x/mobile-app`.
    pub dir_segments: u8,
}

impl Default for PromptInfo {
    /// The signature look: user@host, directory and branch on; clock and
    /// exit-status off, full working directory. Matches the prompt before the
    /// toggles existed.
    fn default() -> Self {
        Self {
            user: true,
            host: true,
            dir: true,
            branch: true,
            time: false,
            exit_status: false,
            dir_segments: 0,
        }
    }
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
/// still match a `/c/Users` `$HOME`) before it is shortened — by `__sv_compact`
/// to first-two-plus-last segments, or, when `SV_DIR_SEGMENTS` is set, by
/// `__sv_tail_segs` to that many trailing segments — and a yellow branch segment
/// shown only inside a git repo. The second line is `└──╼ $`, green, with the
/// trailing colour left un-reset so the typed command is green too.
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
# Last $2 segments of path $1, counted from the end (2 of /d/proj/x/app -> x/app).
# A path with no more segments than asked for is printed whole, leading / and all.
__sv_tail_segs() {
  local p="$1" n="$2" out rest
  case "$p" in */) p="${p%/}";; esac
  if [ -z "$p" ] || [ "$n" -lt 1 ]; then printf '%s' "$1"; return; fi
  out="${p##*/}"; rest="${p%/*}"
  while [ "$n" -gt 1 ] && [ -n "$rest" ]; do
    case "$rest" in
      */*) out="${rest##*/}/$out"; rest="${rest%/*}";;
      *) out="$rest/$out"; rest="";;
    esac
    n=$((n-1))
  done
  if [ -z "$rest" ]; then printf '%s' "$p"; else printf '%s' "$out"; fi
}
# The dir segment as configured: SV_DIR_SEGMENTS trailing segments, or the whole
# (middle-elided) path when it is 0 / unset / not a number.
__sv_dir() {
  local s="${SV_DIR_SEGMENTS:-0}"
  case "$s" in ''|*[!0-9]*) s=0;; esac
  if [ "$s" -gt 0 ]; then __sv_tail_segs "$1" "$s"; else __sv_compact "$1"; fi
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
  local ec=$? d b line userhost
  line='\[\e[1;31m\]┌'
  if [ "${SV_SHOW_USER:-1}" = 1 ] || [ "${SV_SHOW_HOST:-1}" = 1 ]; then
    if [ "${SV_SHOW_USER:-1}" = 1 ] && [ "${SV_SHOW_HOST:-1}" = 1 ]; then userhost='\u@\h';
    elif [ "${SV_SHOW_USER:-1}" = 1 ]; then userhost='\u';
    else userhost='\h'; fi
    line=$line'─[\[\e[1;37m\]'"$userhost"'\[\e[1;31m\]]'
  fi
  if [ "${SV_SHOW_DIR:-1}" = 1 ]; then
    d="$(__sv_dir "$(__sv_tilde "$PWD")")"
    line=$line'─[\[\e[1;34m\]'"$d"'\[\e[1;31m\]]'
  fi
  if [ "${SV_SHOW_BRANCH:-1}" = 1 ]; then
    b="$(git symbolic-ref --quiet --short HEAD 2>/dev/null)"
    if [ -n "$b" ]; then line=$line'─[\[\e[1;33m\]'"$b"'\[\e[1;31m\]]'; fi
  fi
  if [ "${SV_SHOW_TIME:-0}" = 1 ]; then
    line=$line'─[\[\e[1;36m\]\t\[\e[1;31m\]]'
  fi
  if [ "${SV_SHOW_EXIT:-0}" = 1 ] && [ "$ec" != 0 ]; then
    line=$line'─[\[\e[1;31m\]✗ '"$ec"'\[\e[1;31m\]]'
  fi
  PS1=$line'\n\[\e[1;31m\]└──╼ \[\e[1;32m\]\$ '
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
# Last $2 segments of path $1, counted from the end (2 of /d/proj/x/app -> x/app).
# A path with no more segments than asked for is printed whole, leading / and all.
__sv_tail_segs() {
  local p="$1" n="$2" out rest
  case "$p" in */) p="${p%/}";; esac
  if [ -z "$p" ] || [ "$n" -lt 1 ]; then printf '%s' "$1"; return; fi
  out="${p##*/}"; rest="${p%/*}"
  while [ "$n" -gt 1 ] && [ -n "$rest" ]; do
    case "$rest" in
      */*) out="${rest##*/}/$out"; rest="${rest%/*}";;
      *) out="$rest/$out"; rest="";;
    esac
    n=$((n-1))
  done
  if [ -z "$rest" ]; then printf '%s' "$p"; else printf '%s' "$out"; fi
}
# The dir segment as configured: SV_DIR_SEGMENTS trailing segments, or the whole
# (middle-elided) path when it is 0 / unset / not a number.
__sv_dir() {
  local s="${SV_DIR_SEGMENTS:-0}"
  case "$s" in ''|*[!0-9]*) s=0;; esac
  if [ "$s" -gt 0 ]; then __sv_tail_segs "$1" "$s"; else __sv_compact "$1"; fi
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
  local ec=$? d b line userhost
  line='%F{red}┌'
  if [[ "${SV_SHOW_USER:-1}" == 1 || "${SV_SHOW_HOST:-1}" == 1 ]]; then
    if [[ "${SV_SHOW_USER:-1}" == 1 && "${SV_SHOW_HOST:-1}" == 1 ]]; then userhost='%n@%m';
    elif [[ "${SV_SHOW_USER:-1}" == 1 ]]; then userhost='%n';
    else userhost='%m'; fi
    line="${line}─[%F{white}${userhost}%F{red}]"
  fi
  if [[ "${SV_SHOW_DIR:-1}" == 1 ]]; then
    d="$(__sv_dir "$(__sv_tilde "$PWD")")"
    d=${d//\%/%%}
    line="${line}─[%F{blue}${d}%F{red}]"
  fi
  if [[ "${SV_SHOW_BRANCH:-1}" == 1 ]]; then
    b="$(git symbolic-ref --quiet --short HEAD 2>/dev/null)"
    if [ -n "$b" ]; then b=${b//\%/%%}; line="${line}─[%F{yellow}${b}%F{red}]"; fi
  fi
  if [[ "${SV_SHOW_TIME:-0}" == 1 ]]; then
    line="${line}─[%F{cyan}%*%F{red}]"
  fi
  if [[ "${SV_SHOW_EXIT:-0}" == 1 && "$ec" != 0 ]]; then
    line="${line}─[%F{red}✗ ${ec}%F{red}]"
  fi
  PROMPT="${line}
%F{red}└──╼ %F{green}%# "
  return $ec
}
typeset -ga precmd_functions
if [[ -z "${precmd_functions[(r)__sv_prompt]}" ]]; then
  precmd_functions+=(__sv_prompt)
fi
"##;

/// Bash hook that reports the working directory to SplitView via an OSC 7
/// sequence on every prompt. The frontend listens for this and spawns new split
/// panes in the same directory. Kept separate from the prompt so cwd reporting
/// works regardless of prompt style. BEL-terminated; xterm.js consumes it so
/// nothing is shown.
const OSC7_BASH: &str = r##"
__sv_osc7() { printf '\033]7;file://%s%s\a' "${HOSTNAME:-localhost}" "$PWD"; }
case "${PROMPT_COMMAND:-}" in
  *__sv_osc7*) ;;
  *) PROMPT_COMMAND="__sv_osc7${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
esac
"##;

/// zsh counterpart of [`OSC7_BASH`], installed via a `precmd` hook.
const OSC7_ZSH: &str = r##"
__sv_osc7() { printf '\033]7;file://%s%s\a' "${HOST:-localhost}" "$PWD"; }
typeset -ga precmd_functions
if [[ -z "${precmd_functions[(r)__sv_osc7]}" ]]; then
  precmd_functions+=(__sv_osc7)
fi
"##;

/// `"1"`/`"0"` for an `SV_SHOW_*` env flag.
fn bit(b: bool) -> &'static str {
    if b {
        "1"
    } else {
        "0"
    }
}

/// Single-quote a string for safe inclusion in a shell command. Embedded single
/// quotes become the classic `'\''` escape, so an alias command can contain any
/// characters without breaking out of the quoting.
fn sh_squote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// An alias name must be a bare word so it can't inject shell syntax. Anything
/// else is dropped silently (the Settings UI restricts input too).
fn valid_alias_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Shell snippet appended after the prompt setup for *every* style: the user's
/// aliases as real `alias` definitions, plus the `splitview` app command. The
/// app command emits a private OSC 5170 sequence carrying its arguments; the
/// frontend interprets it (e.g. `splitview settings` opens the Settings window).
fn app_snippet(aliases: &[Alias]) -> String {
    let mut s = String::new();
    for a in aliases {
        if valid_alias_name(&a.name) {
            s.push_str(&format!("alias {}={}\n", a.name, sh_squote(&a.command)));
        }
    }
    s.push_str("splitview() { printf '\\033]5170;%s\\a' \"$*\"; }\n");
    s
}

/// Shell snippet that prints the banner once, during rcfile sourcing, so it
/// lands just before the first prompt as part of the shell's own output (the
/// reliable place for it — writing it from the frontend races with the terminal
/// mounting and gets cleared). Each line is single-quote-escaped; `printf %b`
/// honours `\e[…m` colour escapes. Empty banner → no snippet.
fn banner_snippet(banner: &str) -> String {
    if banner.is_empty() {
        return String::new();
    }
    let mut args = String::new();
    for line in banner.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        args.push(' ');
        args.push_str(&sh_squote(line));
    }
    format!("printf '%b\\n'{args}\n")
}

/// Normalize a working directory to a native path. On Windows the cwd reported
/// by the shell arrives as an msys path (e.g. `/c/Users/john`, the shell's
/// `$PWD`); convert it back to a native `C:\Users\john` so `CommandBuilder::cwd`
/// — and the file sidebar's `read_dir` — actually land there. On other platforms
/// the path is already native.
pub fn normalize_cwd(cwd: &str) -> String {
    #[cfg(windows)]
    {
        let b = cwd.as_bytes();
        if b.len() >= 2
            && b[0] == b'/'
            && (b[1] as char).is_ascii_alphabetic()
            && (b.len() == 2 || b[2] == b'/')
        {
            let drive = (b[1] as char).to_ascii_uppercase();
            let rest = cwd.get(3..).unwrap_or("").replace('/', "\\");
            return format!("{drive}:\\{rest}");
        }
    }
    cwd.to_string()
}

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

    // Prompt-segment toggles, read by `__sv_prompt` each time it rebuilds.
    let pi = cfg.prompt_info;
    cmd.env("SV_SHOW_USER", bit(pi.user));
    cmd.env("SV_SHOW_HOST", bit(pi.host));
    cmd.env("SV_SHOW_DIR", bit(pi.dir));
    cmd.env("SV_SHOW_BRANCH", bit(pi.branch));
    cmd.env("SV_SHOW_TIME", bit(pi.time));
    cmd.env("SV_SHOW_EXIT", bit(pi.exit_status));
    // How much of the cwd the dir segment shows: 0 = all of it, n = last n
    // segments. Read by `__sv_dir` on every prompt, like the toggles above.
    cmd.env("SV_DIR_SEGMENTS", pi.dir_segments.to_string());

    // Aliases + the `splitview` app command + OSC 7 cwd reporting, appended
    // after the prompt setup so they apply for both prompt styles. The banner
    // goes last so it prints immediately before the first prompt.
    let app = app_snippet(&cfg.aliases);
    let banner = banner_snippet(&cfg.banner);

    match kind {
        ShellKind::Bash => {
            let setup = match cfg.style {
                PromptStyle::Parrot => PARROT_BASH_SETUP.to_string(),
                PromptStyle::Plain => format!("PS1='{}'\n", to_bash_ps1(&cfg.prompt)),
            };
            let rc = bash_rcfile(&format!("{setup}\n{OSC7_BASH}\n{app}\n{banner}"))?;
            cmd.args(["--rcfile", &rc.to_string_lossy(), "-i"]);
        }
        ShellKind::Zsh => {
            let setup = match cfg.style {
                PromptStyle::Parrot => PARROT_ZSH_SETUP.to_string(),
                PromptStyle::Plain => format!("PROMPT='{}'\n", to_zsh_prompt(&cfg.prompt)),
            };
            let zdot = zsh_zdotdir(&format!("{setup}\n{OSC7_ZSH}\n{app}\n{banner}"))?;
            cmd.env("ZDOTDIR", &zdot);
            cmd.arg("-i");
        }
        // fish / others: best-effort. Don't mangle the user's prompt; just run
        // interactively and let them keep their own (CLAUDE.md §8). Aliases and
        // cwd reporting aren't injected for these shells.
        ShellKind::Fish | ShellKind::Sh | ShellKind::Other => {
            cmd.arg("-i");
        }
    }

    match &cfg.cwd {
        Some(cwd) if !cwd.trim().is_empty() => cmd.cwd(normalize_cwd(cwd)),
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
            // The box frame is now assembled segment-by-segment, so `┌` and the
            // `─[` segment opener appear separately rather than as one literal.
            assert!(setup.contains("┌"));
            assert!(setup.contains("─["));
            assert!(setup.contains("└──╼"));
            assert!(setup.contains("return $ec"));
            // Segments are gated on the SV_SHOW_* toggles.
            assert!(setup.contains("SV_SHOW_DIR"));
            assert!(setup.contains("SV_SHOW_BRANCH"));
            assert!(setup.contains("SV_SHOW_USER"));
            // The dir segment goes through __sv_dir, which picks between the
            // elided-middle path and the last SV_DIR_SEGMENTS segments.
            assert!(setup.contains("__sv_dir \"$(__sv_tilde \"$PWD\")\""));
            assert!(setup.contains("__sv_tail_segs"));
            assert!(setup.contains("SV_DIR_SEGMENTS"));
        }
        // bash uses \[ \] width markers + PROMPT_COMMAND; zsh uses %F + precmd.
        assert!(PARROT_BASH_SETUP.contains("\\u@\\h"));
        assert!(PARROT_BASH_SETUP.contains("1;32")); // green typed input
        assert!(PARROT_BASH_SETUP.contains("PROMPT_COMMAND"));
        assert!(PARROT_ZSH_SETUP.contains("%n@%m"));
        assert!(PARROT_ZSH_SETUP.contains("%F{green}"));
        assert!(PARROT_ZSH_SETUP.contains("precmd_functions+=(__sv_prompt)"));
    }

    #[test]
    fn osc7_hooks_report_cwd_separately_from_prompt() {
        // cwd reporting is a dedicated hook, not part of the prompt body.
        for hook in [OSC7_BASH, OSC7_ZSH] {
            assert!(hook.contains("]7;file://"));
        }
        assert!(!PARROT_BASH_SETUP.contains("]7;file://"));
        assert!(!PARROT_ZSH_SETUP.contains("]7;file://"));
    }

    #[test]
    fn app_snippet_injects_aliases_and_app_command() {
        let aliases = vec![
            Alias {
                name: "gs".into(),
                command: "git status".into(),
            },
            // Single quotes in the command are escaped, not broken out of.
            Alias {
                name: "say".into(),
                command: "echo 'hi there'".into(),
            },
            // Invalid name is dropped.
            Alias {
                name: "bad name!".into(),
                command: "rm -rf /".into(),
            },
        ];
        let s = app_snippet(&aliases);
        assert!(s.contains("alias gs='git status'"));
        assert!(s.contains("alias say='echo '\\''hi there'\\'''"));
        assert!(!s.contains("bad name"));
        // The app command emits the private OSC 5170 sequence.
        assert!(s.contains("splitview()"));
        assert!(s.contains("]5170;"));
    }

    #[test]
    fn banner_snippet_prints_each_line_safely() {
        assert_eq!(banner_snippet(""), "");
        // Multi-line, with an embedded single quote that must be escaped.
        let s = banner_snippet("hello\nit's me");
        assert_eq!(s, "printf '%b\\n' 'hello' 'it'\\''s me'\n");
        // CRLF line endings are normalized.
        assert_eq!(banner_snippet("a\r\nb"), "printf '%b\\n' 'a' 'b'\n");
    }

    #[test]
    fn valid_alias_name_rejects_shell_metacharacters() {
        assert!(valid_alias_name("gs"));
        assert!(valid_alias_name("g-s_2"));
        assert!(!valid_alias_name(""));
        assert!(!valid_alias_name("a b"));
        assert!(!valid_alias_name("a;b"));
        assert!(!valid_alias_name("$(x)"));
    }

    #[test]
    fn prompt_info_reaches_the_shell_as_env_flags() {
        let cfg = ShellConfig {
            program: Some("/bin/bash".into()),
            cwd: None,
            prompt: "{user} @ {cwd} $ ".into(),
            style: PromptStyle::Parrot,
            aliases: Vec::new(),
            prompt_info: PromptInfo {
                time: true,
                dir_segments: 2,
                ..PromptInfo::default()
            },
            banner: String::new(),
        };
        let cmd = build_command(&cfg).expect("build command");
        assert_eq!(
            cmd.get_env("SV_SHOW_DIR").and_then(|v| v.to_str()),
            Some("1")
        );
        assert_eq!(
            cmd.get_env("SV_SHOW_TIME").and_then(|v| v.to_str()),
            Some("1")
        );
        assert_eq!(
            cmd.get_env("SV_SHOW_EXIT").and_then(|v| v.to_str()),
            Some("0")
        );
        // The directory detail setting: 0 = whole path, n = last n segments.
        assert_eq!(
            cmd.get_env("SV_DIR_SEGMENTS").and_then(|v| v.to_str()),
            Some("2")
        );

        let cfg = ShellConfig {
            prompt_info: PromptInfo::default(),
            ..cfg
        };
        let cmd = build_command(&cfg).expect("build command");
        assert_eq!(
            cmd.get_env("SV_DIR_SEGMENTS").and_then(|v| v.to_str()),
            Some("0")
        );
    }

    #[test]
    fn normalize_cwd_passes_through_posix_paths() {
        assert_eq!(normalize_cwd("/home/john/proj"), "/home/john/proj");
    }

    #[cfg(windows)]
    #[test]
    fn normalize_cwd_converts_msys_drive_paths() {
        assert_eq!(normalize_cwd("/c/Users/john"), "C:\\Users\\john");
        assert_eq!(normalize_cwd("/c/"), "C:\\");
        assert_eq!(normalize_cwd("/c"), "C:\\");
        // Already-native paths are untouched.
        assert_eq!(normalize_cwd("D:\\work"), "D:\\work");
    }
}
