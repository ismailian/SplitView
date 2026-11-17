//! The user's TOML config (CLAUDE.md §3): `config.toml` in the platform config
//! dir. This module turns a TOML *string* into a validated, clamped [`Config`];
//! file I/O lives in the app.

use crate::keymap::Bindings;
use crate::layout::{MAX_RATIO, MIN_RATIO};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptStyle {
    /// The ParrotOS two-line prompt (default).
    Parrot,
    /// The `prompt` template.
    Plain,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Prompt template, e.g. `"{user} @ {cwd} $ "` (used when `prompt_style` is Plain).
    pub prompt: String,
    pub prompt_style: PromptStyle,
    /// Prefix chord, e.g. `"C-a"`.
    pub prefix: String,
    /// Optional shell override; outranks the saved default profile.
    pub shell: Option<String>,
    /// Optional path to a .ttf/.otf used for the terminal instead of the bundled
    /// JetBrains Mono.
    pub font_file: Option<String>,
    pub font_size: f32,
    /// How much a resize step changes a ratio.
    pub resize_step: f32,
    /// Prefix-binding overrides: chord → action name.
    pub keybindings: Bindings,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            prompt: "{user} @ {cwd} $ ".into(),
            prompt_style: PromptStyle::Parrot,
            prefix: "C-a".into(),
            shell: None,
            font_file: None,
            font_size: 14.0,
            resize_step: 0.03,
            keybindings: Bindings::new(),
        }
    }
}

pub const FONT_MIN: f32 = 6.0;
pub const FONT_MAX: f32 = 48.0;

fn clamp(v: f64, lo: f32, hi: f32) -> f32 {
    if v.is_nan() {
        lo
    } else {
        (v as f32).clamp(lo, hi)
    }
}

/// Parse + validate a TOML string. Unknown keys are ignored; wrong types fall
/// back to defaults; numbers are clamped.
pub fn parse(src: &str) -> Result<Config, String> {
    let table: toml::Table = src.parse().map_err(|e: toml::de::Error| e.to_string())?;
    let def = Config::default();
    let s = |k: &str| table.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    let n = |k: &str| {
        table
            .get(k)
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
    };
    let mut cfg = Config {
        prompt: s("prompt").unwrap_or(def.prompt),
        prompt_style: if s("promptStyle").as_deref() == Some("plain") {
            PromptStyle::Plain
        } else {
            PromptStyle::Parrot
        },
        prefix: s("prefix").filter(|p| !p.is_empty()).unwrap_or(def.prefix),
        shell: s("shell").filter(|p| !p.trim().is_empty()),
        font_file: s("fontFile").filter(|p| !p.trim().is_empty()),
        font_size: n("fontSize").map_or(def.font_size, |v| clamp(v, FONT_MIN, FONT_MAX)),
        resize_step: n("resizeStep")
            .map_or(def.resize_step, |v| clamp(v, 0.01, MAX_RATIO - MIN_RATIO)),
        keybindings: Bindings::new(),
    };
    if let Some(t) = table.get("keybindings").and_then(|v| v.as_table()) {
        for (chord, action) in t {
            if let Some(a) = action.as_str() {
                cfg.keybindings.insert(chord.clone(), a.to_owned());
            }
        }
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_for_empty() {
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn parses_and_clamps() {
        let c = parse(
            r#"
            promptStyle = "plain"
            prompt = "{user} $ "
            prefix = "C-b"
            fontSize = 400
            resizeStep = 0
            shell = "/bin/zsh"
            [keybindings]
            "C-x" = "close_pane"
            "#,
        )
        .unwrap();
        assert_eq!(c.prompt_style, PromptStyle::Plain);
        assert_eq!(c.prefix, "C-b");
        assert_eq!(c.font_size, FONT_MAX);
        assert_eq!(c.resize_step, 0.01);
        assert_eq!(c.shell.as_deref(), Some("/bin/zsh"));
        assert_eq!(
            c.keybindings.get("C-x").map(String::as_str),
            Some("close_pane")
        );
    }

    #[test]
    fn integer_font_size_and_bad_types() {
        let c = parse("fontSize = 16\nprefix = 3").unwrap();
        assert_eq!(c.font_size, 16.0);
        assert_eq!(c.prefix, "C-a");
        assert!(parse("prompt = ").is_err());
    }
}
