//! egui key events → keymap chords, and the routing that gives the keymap first
//! refusal on every keystroke (CLAUDE.md §9).
//!
//! Routing runs at the start of each window's frame, before any widget reads
//! input. Whatever the keymap consumes is removed from the frame's event list —
//! together with the `Text` event the same keystroke produced — so the terminal
//! (or editor) underneath never sees it. Everything else is left alone: outside
//! prefix mode keys flow to the focused widget untouched.

use std::collections::HashSet;
use std::time::Instant;

use egui::{Event, Key, Modifiers};
use sv_core::keymap::{Keymap, Outcome, chord};

/// The keymap's name for a key, or `None` for keys it never binds.
pub fn key_name(key: Key) -> Option<&'static str> {
    use Key::*;
    Some(match key {
        A => "a",
        B => "b",
        C => "c",
        D => "d",
        E => "e",
        F => "f",
        G => "g",
        H => "h",
        I => "i",
        J => "j",
        K => "k",
        L => "l",
        M => "m",
        N => "n",
        O => "o",
        P => "p",
        Q => "q",
        R => "r",
        S => "s",
        T => "t",
        U => "u",
        V => "v",
        W => "w",
        X => "x",
        Y => "y",
        Z => "z",
        Num0 => "0",
        Num1 => "1",
        Num2 => "2",
        Num3 => "3",
        Num4 => "4",
        Num5 => "5",
        Num6 => "6",
        Num7 => "7",
        Num8 => "8",
        Num9 => "9",
        Minus => "-",
        Plus => "+",
        Equals => "=",
        Backslash => "\\",
        Pipe => "|",
        Slash => "/",
        Comma => ",",
        Period => ".",
        Semicolon => ";",
        Colon => ":",
        Quote => "'",
        Backtick => "`",
        OpenBracket => "[",
        CloseBracket => "]",
        Questionmark => "?",
        ArrowLeft => "arrowleft",
        ArrowRight => "arrowright",
        ArrowUp => "arrowup",
        ArrowDown => "arrowdown",
        Enter => "enter",
        Escape => "escape",
        Tab => "tab",
        Space => "space",
        Backspace => "backspace",
        Delete => "delete",
        Insert => "insert",
        Home => "home",
        End => "end",
        PageUp => "pageup",
        PageDown => "pagedown",
        F1 => "f1",
        F2 => "f2",
        F3 => "f3",
        F4 => "f4",
        F5 => "f5",
        F6 => "f6",
        F7 => "f7",
        F8 => "f8",
        F9 => "f9",
        F10 => "f10",
        F11 => "f11",
        F12 => "f12",
        _ => return None,
    })
}

pub fn chord_of(key: Key, m: Modifiers) -> Option<String> {
    let name = key_name(key)?;
    // Shifted punctuation already arrives as its own key (`+`, `|`, `?`): the
    // shift is part of the character, not a modifier on it.
    let shifted_symbol = matches!(key, Key::Plus | Key::Pipe | Key::Questionmark | Key::Colon);
    Some(chord(
        m.ctrl,
        m.alt,
        m.shift && !shifted_symbol,
        m.mac_cmd,
        name,
    ))
}

/// What routing decided this frame.
#[derive(Default, Debug)]
pub struct Routed {
    pub actions: Vec<String>,
    /// Send the literal prefix to the focused terminal.
    pub send_prefix: bool,
}

/// Run every key press in this frame's input through the keymap, removing what
/// it consumed. `filter` sees each surviving key press first and may claim it
/// (the sidebar's own keys) by returning true.
pub fn route(
    ctx: &egui::Context,
    keymap: &mut Keymap,
    mut filter: impl FnMut(Key, Modifiers, bool) -> bool,
) -> Routed {
    let now = Instant::now();
    let mut out = Routed::default();
    // Keys whose press we saw, across frames: a release without one is a
    // press egui-winit turned into a clipboard command (see below).
    let down_id = egui::Id::new("sv-keys-down");
    let mut down: HashSet<Key> = ctx.data(|d| d.get_temp(down_id)).unwrap_or_default();
    ctx.input_mut(|i| {
        let events = std::mem::take(&mut i.events);
        let mut kept = Vec::with_capacity(events.len());
        let mut swallow_text = false;
        for ev in events {
            match &ev {
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    down.insert(*key);
                    swallow_text = false;
                    let in_prefix = keymap.in_prefix(now);
                    // The sidebar gets first refusal on its own keys, but never
                    // while a prefix is pending: that keystroke is SplitView's.
                    if !in_prefix && filter(*key, *modifiers, false) {
                        swallow_text = true;
                        continue;
                    }
                    let Some(c) = chord_of(*key, *modifiers) else {
                        kept.push(ev);
                        continue;
                    };
                    match keymap.handle(&c, now) {
                        Outcome::Pass => kept.push(ev),
                        Outcome::Consumed => swallow_text = true,
                        Outcome::Action(a) => {
                            out.actions.push(a);
                            swallow_text = true;
                        }
                        Outcome::SendPrefix => {
                            out.send_prefix = true;
                            swallow_text = true;
                        }
                    }
                }
                // Ctrl held through `prefix, v`: egui-winit turns Ctrl+V into a
                // Paste event and drops the key press — and with no text on the
                // clipboard (empty, an image, files copied in Explorer) it sends
                // nothing at all. Only the release survives, so a C/X/V release
                // whose press never arrived stands in for it while a prefix is
                // pending. (With text on the clipboard the Paste arm below fires
                // first and ends the prefix, so this can't double up.)
                Event::Key {
                    key,
                    pressed: false,
                    ..
                } if !down.remove(key)
                    && matches!(key, Key::C | Key::X | Key::V)
                    && keymap.in_prefix(now) =>
                {
                    let name = key_name(*key).unwrap_or_default();
                    match keymap.handle(&chord(true, false, false, false, name), now) {
                        Outcome::Action(a) => out.actions.push(a),
                        Outcome::SendPrefix => out.send_prefix = true,
                        _ => {}
                    }
                }
                Event::Key {
                    key,
                    pressed: false,
                    ..
                } => {
                    down.remove(key);
                    kept.push(ev);
                }
                // The character a consumed keystroke would have typed.
                Event::Text(_) if swallow_text => swallow_text = false,
                // Ctrl+C / Ctrl+V / Ctrl+X arrive as clipboard events, not keys;
                // in prefix mode they still belong to the keymap.
                Event::Copy | Event::Cut | Event::Paste(_) if keymap.in_prefix(now) => {
                    let key = match &ev {
                        Event::Copy => "c",
                        Event::Cut => "x",
                        _ => "v",
                    };
                    match keymap.handle(&chord(true, false, false, false, key), now) {
                        Outcome::Action(a) => out.actions.push(a),
                        Outcome::SendPrefix => out.send_prefix = true,
                        _ => {}
                    }
                }
                _ => kept.push(ev),
            }
        }
        i.events = kept;
    });
    ctx.data_mut(|d| d.insert_temp(down_id, down));
    out
}

/// Remove every keyboard event (a modal owns the keyboard, or the window is
/// inactive). Returns the key presses and typed text, for the modal to use.
pub fn take_all(ctx: &egui::Context) -> (Vec<(Key, Modifiers)>, String) {
    let mut keys = Vec::new();
    let mut text = String::new();
    ctx.input_mut(|i| {
        i.events.retain(|ev| match ev {
            Event::Key {
                key,
                pressed,
                modifiers,
                ..
            } => {
                if *pressed {
                    keys.push((*key, *modifiers));
                }
                false
            }
            Event::Text(t) => {
                text.push_str(t);
                false
            }
            Event::Copy | Event::Cut | Event::Paste(_) => false,
            _ => true,
        })
    });
    (keys, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sv_core::keymap::KeymapConfig;

    fn key(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        }
    }

    /// Run one frame of `events` through `route`.
    fn frame(ctx: &egui::Context, km: &mut Keymap, events: Vec<Event>) -> Routed {
        let mut routed = Routed::default();
        let raw = egui::RawInput {
            events,
            ..Default::default()
        };
        let _ = ctx.run_ui(raw, |ui| routed = route(ui.ctx(), km, |_, _, _| false));
        routed
    }

    /// `Ctrl+A`, then `V` with Ctrl still held and nothing on the clipboard:
    /// egui-winit delivers only the prefix press and V's release.
    #[test]
    fn prefix_then_ctrl_v_with_an_empty_clipboard_still_splits() {
        let ctx = egui::Context::default();
        let mut km = Keymap::new(KeymapConfig::default());
        frame(&ctx, &mut km, vec![key(Key::A, true, Modifiers::CTRL)]);
        let r = frame(&ctx, &mut km, vec![key(Key::V, false, Modifiers::CTRL)]);
        assert_eq!(r.actions, ["split_right"]);
    }

    /// With text on the clipboard the Paste event carries it; the later
    /// release must not fire a second time.
    #[test]
    fn prefix_then_ctrl_v_with_text_splits_once() {
        let ctx = egui::Context::default();
        let mut km = Keymap::new(KeymapConfig::default());
        frame(&ctx, &mut km, vec![key(Key::A, true, Modifiers::CTRL)]);
        let r = frame(&ctx, &mut km, vec![Event::Paste("text".into())]);
        assert_eq!(r.actions, ["split_right"]);
        let r = frame(&ctx, &mut km, vec![key(Key::V, false, Modifiers::CTRL)]);
        assert!(r.actions.is_empty());
    }

    /// A V typed before the prefix and released after it is not a command.
    #[test]
    fn a_release_whose_press_was_seen_is_ignored() {
        let ctx = egui::Context::default();
        let mut km = Keymap::new(KeymapConfig::default());
        frame(&ctx, &mut km, vec![key(Key::V, true, Modifiers::NONE)]);
        frame(&ctx, &mut km, vec![key(Key::A, true, Modifiers::CTRL)]);
        let r = frame(&ctx, &mut km, vec![key(Key::V, false, Modifiers::NONE)]);
        assert!(r.actions.is_empty());
        // The prefix is still pending: a plain `v` now splits.
        let r = frame(&ctx, &mut km, vec![key(Key::V, true, Modifiers::NONE)]);
        assert_eq!(r.actions, ["split_right"]);
    }

    #[test]
    fn prefix_then_plain_v_splits() {
        let ctx = egui::Context::default();
        let mut km = Keymap::new(KeymapConfig::default());
        frame(&ctx, &mut km, vec![key(Key::A, true, Modifiers::CTRL)]);
        let r = frame(&ctx, &mut km, vec![key(Key::V, true, Modifiers::NONE)]);
        assert_eq!(r.actions, ["split_right"]);
    }

    #[test]
    fn chords_for_keys() {
        assert_eq!(chord_of(Key::A, Modifiers::CTRL).as_deref(), Some("C-a"));
        assert_eq!(chord_of(Key::H, Modifiers::SHIFT).as_deref(), Some("S-h"));
        assert_eq!(chord_of(Key::Minus, Modifiers::NONE).as_deref(), Some("-"));
        assert_eq!(
            chord_of(Key::Backslash, Modifiers::NONE).as_deref(),
            Some("\\")
        );
        assert_eq!(
            chord_of(Key::Plus, Modifiers::CTRL | Modifiers::SHIFT).as_deref(),
            Some("C-+")
        );
        assert_eq!(
            chord_of(Key::ArrowLeft, Modifiers::SHIFT).as_deref(),
            Some("S-arrowleft")
        );
    }
}
