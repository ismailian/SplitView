//! Prefix engine + binding registry (CLAUDE.md §9).
//!
//! SplitView uses a tmux-style prefix model so it never steals keys from
//! terminal programs. The engine is data-driven and UI-agnostic: [`Keymap::handle`]
//! is a state machine over canonical chord strings (`"C-a"`, `"S-h"`, `"arrowleft"`),
//! so it is unit-tested without a window. The app turns egui key events into
//! chords and acts on the [`Outcome`].

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Chord string → action name.
pub type Bindings = BTreeMap<String, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Chord<'a> {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
    pub key: &'a str,
}

impl<'a> Chord<'a> {
    /// Split `"C-S-h"` into modifiers and key. Robust to the key itself being
    /// `"-"`: modifier tokens are stripped in their fixed order, and whatever
    /// remains is the key.
    pub fn parse(s: &'a str) -> Self {
        let mut rest = s;
        let mut take = |tok: &str| {
            if rest.len() > tok.len()
                && let Some(r) = rest.strip_prefix(tok)
            {
                rest = r;
                return true;
            }
            false
        };
        let ctrl = take("C-");
        let alt = take("A-");
        let shift = take("S-");
        let meta = take("M-");
        Chord {
            ctrl,
            alt,
            shift,
            meta,
            key: rest,
        }
    }
}

/// Canonical chord string (modifier order C, A, S, M; key lower-cased).
pub fn chord(ctrl: bool, alt: bool, shift: bool, meta: bool, key: &str) -> String {
    let mut s = String::with_capacity(key.len() + 8);
    if ctrl {
        s.push_str("C-");
    }
    if alt {
        s.push_str("A-");
    }
    if shift {
        s.push_str("S-");
    }
    if meta {
        s.push_str("M-");
    }
    s.push_str(&key.to_lowercase());
    s
}

#[derive(Debug, Clone)]
pub struct KeymapConfig {
    /// Prefix chord, e.g. `"C-a"`.
    pub prefix: String,
    /// Bindings active *after* the prefix.
    pub prefix_bindings: Bindings,
    /// Bindings active without the prefix (explicit, opt-in, all carry a modifier).
    pub global_bindings: Bindings,
    /// Prefix-mode timeout; an unrecognized key also cancels it.
    pub timeout: Duration,
}

fn bind(pairs: &[(&str, &str)]) -> Bindings {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

impl Default for KeymapConfig {
    fn default() -> Self {
        Self {
            prefix: "C-a".into(),
            timeout: Duration::from_millis(1500),
            prefix_bindings: bind(&[
                ("-", "split_down"),
                ("s", "split_down"),
                ("\\", "split_right"),
                ("v", "split_right"),
                ("h", "focus_left"),
                ("j", "focus_down"),
                ("k", "focus_up"),
                ("l", "focus_right"),
                ("arrowleft", "focus_left"),
                ("arrowdown", "focus_down"),
                ("arrowup", "focus_up"),
                ("arrowright", "focus_right"),
                ("o", "focus_next"),
                ("S-h", "move_left"),
                ("S-j", "move_down"),
                ("S-k", "move_up"),
                ("S-l", "move_right"),
                ("S-arrowleft", "move_left"),
                ("S-arrowdown", "move_down"),
                ("S-arrowup", "move_up"),
                ("S-arrowright", "move_right"),
                ("C-h", "resize_left"),
                ("C-j", "resize_down"),
                ("C-k", "resize_up"),
                ("C-l", "resize_right"),
                ("b", "background_pane"),
                ("S-b", "show_background"),
                ("z", "toggle_zoom"),
                ("x", "close_pane"),
                ("d", "detach_pane"),
                ("n", "new_window"),
                ("e", "toggle_sidebar"),
                ("S-e", "toggle_editor"),
                ("r", "reload_config"),
                (",", "open_settings"),
                ("p", "pick_shell"),
                ("S-p", "command_palette"),
            ]),
            // Global chords active without the prefix: the explicit exception to
            // "never swallow keys outside prefix mode". Each carries a modifier and
            // none maps to meaningful terminal input.
            global_bindings: bind(&[
                ("C-S-o", "open_file_dialog"),
                ("C-S-p", "command_palette"),
                ("C-=", "zoom_in"),
                ("C-+", "zoom_in"),
                ("C-S-=", "zoom_in"),
                ("C-S-+", "zoom_in"),
                ("C--", "zoom_out"),
                ("C-0", "zoom_reset"),
            ]),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Not for us — let it flow to the focused terminal.
    Pass,
    /// Consumed (entered or cancelled prefix mode).
    Consumed,
    /// Consumed; run this action.
    Action(String),
    /// Consumed; send the literal prefix keystroke to the shell.
    SendPrefix,
}

#[derive(Debug, Clone)]
pub struct Keymap {
    cfg: KeymapConfig,
    prefix_since: Option<Instant>,
}

impl Keymap {
    pub fn new(cfg: KeymapConfig) -> Self {
        Self {
            cfg,
            prefix_since: None,
        }
    }

    pub fn config(&self) -> &KeymapConfig {
        &self.cfg
    }

    pub fn set_config(&mut self, cfg: KeymapConfig) {
        self.prefix_since = None;
        self.cfg = cfg;
    }

    /// Prefix mode is active (and not timed out) at `now`.
    pub fn in_prefix(&self, now: Instant) -> bool {
        self.prefix_since
            .is_some_and(|t| now.duration_since(t) < self.cfg.timeout)
    }

    /// When prefix mode will time out, if active — so the UI can repaint then.
    pub fn prefix_deadline(&self) -> Option<Instant> {
        self.prefix_since
            .map(|t| t + self.cfg.timeout)
            .filter(|d| *d > Instant::now())
    }

    pub fn cancel(&mut self) {
        self.prefix_since = None;
    }

    /// Core state machine (CLAUDE.md §9).
    pub fn handle(&mut self, chord: &str, now: Instant) -> Outcome {
        if !self.in_prefix(now) {
            self.prefix_since = None;
            if chord == self.cfg.prefix {
                self.prefix_since = Some(now);
                return Outcome::Consumed;
            }
            if let Some(a) = self.cfg.global_bindings.get(chord) {
                return Outcome::Action(a.clone());
            }
            // Outside prefix mode we never swallow keys.
            return Outcome::Pass;
        }
        // In prefix mode: this keystroke resolves it either way.
        self.prefix_since = None;
        if chord == self.cfg.prefix {
            return Outcome::SendPrefix;
        }
        match self.lookup(chord) {
            Some(a) => Outcome::Action(a),
            // Unrecognized key cancels prefix silently and is consumed.
            None => Outcome::Consumed,
        }
    }

    /// Exact match first, then retry with the prefix's own modifiers removed:
    /// people keep Ctrl held while tapping the command key, so `s` arrives as
    /// `C-s`. Exact bindings win, so the `C-h/j/k/l` resize chords still work.
    fn lookup(&self, chord: &str) -> Option<String> {
        if let Some(a) = self.cfg.prefix_bindings.get(chord) {
            return Some(a.clone());
        }
        let pre = Chord::parse(&self.cfg.prefix);
        let c = Chord::parse(chord);
        let relaxed = self::chord(
            c.ctrl && !pre.ctrl,
            c.alt && !pre.alt,
            c.shift && !pre.shift,
            c.meta && !pre.meta,
            c.key,
        );
        if relaxed != chord {
            self.cfg.prefix_bindings.get(&relaxed).cloned()
        } else {
            None
        }
    }

    /// The raw bytes of the prefix keystroke, for double-prefix passthrough.
    pub fn prefix_bytes(&self) -> Vec<u8> {
        chord_to_bytes(&self.cfg.prefix)
    }
}

/// `"C-a"` → `0x01`. Only `Ctrl+<letter>` is needed for passthrough; anything
/// else sends its last character literally.
pub fn chord_to_bytes(chord: &str) -> Vec<u8> {
    let c = Chord::parse(chord);
    if c.ctrl && c.key.len() == 1 {
        let b = c.key.as_bytes()[0].to_ascii_lowercase();
        if b.is_ascii_lowercase() {
            return vec![b - b'a' + 1];
        }
    }
    chord
        .chars()
        .last()
        .map(|ch| ch.to_string().into_bytes())
        .unwrap_or_default()
}

/// Human-readable form of a chord: `"C-S-e"` → `"Ctrl Shift E"`.
pub fn pretty(chord: &str) -> String {
    let c = Chord::parse(chord);
    let mut s = String::new();
    for (on, name) in [
        (c.ctrl, "Ctrl "),
        (c.alt, "Alt "),
        (c.shift, "Shift "),
        (c.meta, "Super "),
    ] {
        if on {
            s.push_str(name);
        }
    }
    let key = match c.key {
        "arrowleft" => "←".to_string(),
        "arrowright" => "→".to_string(),
        "arrowup" => "↑".to_string(),
        "arrowdown" => "↓".to_string(),
        k if k.len() == 1 => k.to_uppercase(),
        k => {
            let mut ch = k.chars();
            ch.next()
                .map(|f| f.to_uppercase().collect::<String>() + ch.as_str())
                .unwrap_or_default()
        }
    };
    s.push_str(&key);
    s
}

/// Label + category for every action, in display order.
pub const ACTIONS: &[(&str, &str, &str)] = &[
    ("split_down", "Split horizontally (stacked)", "Panes"),
    ("split_right", "Split vertically (side by side)", "Panes"),
    ("toggle_zoom", "Toggle zoom (maximize pane)", "Panes"),
    ("close_pane", "Close focused pane", "Panes"),
    ("detach_pane", "Detach pane into its own window", "Panes"),
    ("reattach_pane", "Reattach pane to its window", "Panes"),
    (
        "background_pane",
        "Background pane (keep it running)",
        "Panes",
    ),
    ("show_background", "Show backgrounded panes", "Panes"),
    ("focus_left", "Focus pane left", "Focus"),
    ("focus_down", "Focus pane down", "Focus"),
    ("focus_up", "Focus pane up", "Focus"),
    ("focus_right", "Focus pane right", "Focus"),
    ("focus_next", "Cycle focus to next pane", "Focus"),
    ("move_left", "Move pane left", "Move / Resize"),
    ("move_down", "Move pane down", "Move / Resize"),
    ("move_up", "Move pane up", "Move / Resize"),
    ("move_right", "Move pane right", "Move / Resize"),
    ("resize_left", "Resize pane left", "Move / Resize"),
    ("resize_down", "Resize pane down", "Move / Resize"),
    ("resize_up", "Resize pane up", "Move / Resize"),
    ("resize_right", "Resize pane right", "Move / Resize"),
    ("zoom_in", "Increase font size", "Font"),
    ("zoom_out", "Decrease font size", "Font"),
    ("zoom_reset", "Reset font size", "Font"),
    ("command_palette", "Command palette", "App"),
    ("open_file_dialog", "Insert file path (picker)", "App"),
    ("open_settings", "Open Settings", "App"),
    ("reload_config", "Reload config", "App"),
    ("new_window", "Open a new window", "App"),
    ("toggle_sidebar", "Toggle file sidebar", "App"),
    ("toggle_editor", "Toggle editor panel", "App"),
    (
        "pick_shell",
        "Pick a shell (open a pane / set default)",
        "App",
    ),
];

pub fn action_label(name: &str) -> Option<&'static str> {
    ACTIONS.iter().find(|a| a.0 == name).map(|a| a.1)
}

/// All chords bound to `action` (prefix bindings first), for display.
pub fn chords_for<'a>(cfg: &'a KeymapConfig, action: &str) -> Vec<(bool, &'a str)> {
    let mut v: Vec<(bool, &str)> = cfg
        .prefix_bindings
        .iter()
        .filter(|(_, a)| a.as_str() == action)
        .map(|(c, _)| (true, c.as_str()))
        .collect();
    v.extend(
        cfg.global_bindings
            .iter()
            .filter(|(_, a)| a.as_str() == action)
            .map(|(c, _)| (false, c.as_str())),
    );
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn km() -> (Keymap, Instant) {
        (Keymap::new(KeymapConfig::default()), Instant::now())
    }

    #[test]
    fn passes_keys_outside_prefix() {
        let (mut k, t) = km();
        assert_eq!(k.handle("s", t), Outcome::Pass);
        assert_eq!(k.handle("C-c", t), Outcome::Pass);
        assert!(!k.in_prefix(t));
    }

    #[test]
    fn prefix_then_action() {
        let (mut k, t) = km();
        assert_eq!(k.handle("C-a", t), Outcome::Consumed);
        assert!(k.in_prefix(t));
        assert_eq!(k.handle("s", t), Outcome::Action("split_down".into()));
        assert!(!k.in_prefix(t));
        assert_eq!(k.handle("s", t), Outcome::Pass);
    }

    #[test]
    fn double_prefix_passes_through() {
        let (mut k, t) = km();
        k.handle("C-a", t);
        assert_eq!(k.handle("C-a", t), Outcome::SendPrefix);
        assert_eq!(k.prefix_bytes(), vec![1]);
    }

    #[test]
    fn timeout_cancels() {
        let (mut k, t) = km();
        k.handle("C-a", t);
        let later = t + Duration::from_secs(2);
        assert!(!k.in_prefix(later));
        assert_eq!(k.handle("s", later), Outcome::Pass);
    }

    #[test]
    fn unknown_key_cancels_silently() {
        let (mut k, t) = km();
        k.handle("C-a", t);
        assert_eq!(k.handle("q", t), Outcome::Consumed);
        assert!(!k.in_prefix(t));
    }

    #[test]
    fn held_ctrl_is_relaxed_but_exact_wins() {
        let (mut k, t) = km();
        k.handle("C-a", t);
        assert_eq!(k.handle("C-s", t), Outcome::Action("split_down".into()));
        k.handle("C-a", t);
        assert_eq!(k.handle("C-h", t), Outcome::Action("resize_left".into()));
        k.handle("C-a", t);
        assert_eq!(k.handle("S-h", t), Outcome::Action("move_left".into()));
    }

    #[test]
    fn global_bindings_fire_without_prefix() {
        let (mut k, t) = km();
        assert_eq!(k.handle("C-=", t), Outcome::Action("zoom_in".into()));
        assert_eq!(
            k.handle("C-S-p", t),
            Outcome::Action("command_palette".into())
        );
    }

    #[test]
    fn config_overrides() {
        let mut cfg = KeymapConfig {
            prefix: "C-b".into(),
            ..KeymapConfig::default()
        };
        cfg.prefix_bindings
            .insert("C-x".into(), "close_pane".into());
        let mut k = Keymap::new(cfg);
        let t = Instant::now();
        assert_eq!(k.handle("C-a", t), Outcome::Pass);
        k.handle("C-b", t);
        assert_eq!(k.handle("C-x", t), Outcome::Action("close_pane".into()));
        assert_eq!(k.prefix_bytes(), vec![2]);
    }

    #[test]
    fn chord_parsing() {
        let c = Chord::parse("C-S-h");
        assert!(c.ctrl && c.shift && !c.alt);
        assert_eq!(c.key, "h");
        assert_eq!(Chord::parse("-").key, "-");
        assert_eq!(Chord::parse("C--").key, "-");
        assert!(Chord::parse("C--").ctrl);
        assert_eq!(chord(true, false, true, false, "H"), "C-S-h");
        assert_eq!(pretty("C-S-arrowleft"), "Ctrl Shift ←");
        assert_eq!(pretty(","), ",");
    }
}
