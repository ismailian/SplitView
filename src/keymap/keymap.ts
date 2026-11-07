/**
 * Prefix engine + binding registry (CLAUDE.md §9).
 *
 * SplitView uses a tmux-style prefix model so it never steals keys from
 * terminal programs. The engine is data-driven and DOM-agnostic: `handleKey`
 * is a pure-ish state machine over plain key descriptors, so it can be unit
 * tested without a browser. `attach()` wires it to the real document.
 */

/** A minimal, testable view of a keyboard event. */
export interface KeyEvent {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}

export type Bindings = Record<string, string>;

export interface KeymapConfig {
  /** Prefix chord, e.g. `"C-a"` for Ctrl+A. */
  prefix: string;
  /** Bindings active *after* the prefix. chord -> action name. */
  prefixBindings: Bindings;
  /** Optional global bindings active without the prefix. chord -> action. */
  globalBindings?: Bindings;
  /** Prefix-mode timeout in ms; an unrecognized key also cancels it. */
  timeoutMs?: number;
}

export type KeyOutcome =
  /** Not for us — let it flow to the focused terminal. */
  | { handled: false }
  /** Consumed; run this action (may be undefined for enter/cancel). */
  | { handled: true; action?: string }
  /** Consumed; send the literal prefix keystroke to the shell. */
  | { handled: true; sendPrefix: true };

/** Build the canonical chord string for an event (modifier order C,A,S,M). */
export function chordOf(ev: KeyEvent): string {
  // Lower-case the key so Shift's uppercasing (e.g. "H") and named keys
  // (e.g. "ArrowLeft") both have a single canonical form; the Shift modifier
  // is tracked separately below.
  const key = ev.key.toLowerCase();
  let s = "";
  if (ev.ctrlKey) s += "C-";
  if (ev.altKey) s += "A-";
  if (ev.shiftKey) s += "S-";
  if (ev.metaKey) s += "M-";
  return s + key;
}

interface ParsedChord {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
  key: string;
}

/**
 * Split a canonical chord (`"C-S-h"`, `"-"`, `"arrowleft"`) into its modifiers
 * and key. Robust to the key itself being `"-"`: modifier tokens are stripped
 * by their `"C-"`/`"A-"`/`"S-"`/`"M-"` prefixes in their fixed order, and
 * whatever remains is the key.
 */
export function parseChord(chord: string): ParsedChord {
  let rest = chord;
  const take = (tok: string): boolean => {
    if (rest.startsWith(tok)) {
      rest = rest.slice(tok.length);
      return true;
    }
    return false;
  };
  const ctrl = take("C-");
  const alt = take("A-");
  const shift = take("S-");
  const meta = take("M-");
  return { ctrl, alt, shift, meta, key: rest };
}

/** True for a lone modifier key event (Shift/Control/Alt/Meta by themselves). */
function isBareModifier(key: string): boolean {
  const k = key.toLowerCase();
  return k === "shift" || k === "control" || k === "alt" || k === "meta";
}

function buildChord(p: ParsedChord): string {
  let s = "";
  if (p.ctrl) s += "C-";
  if (p.alt) s += "A-";
  if (p.shift) s += "S-";
  if (p.meta) s += "M-";
  return s + p.key;
}

export const DEFAULT_KEYMAP: KeymapConfig = {
  prefix: "C-a",
  timeoutMs: 1500,
  prefixBindings: {
    // Splits
    "-": "split_down",
    s: "split_down",
    "\\": "split_right",
    v: "split_right",
    // Focus
    h: "focus_left",
    j: "focus_down",
    k: "focus_up",
    l: "focus_right",
    arrowleft: "focus_left",
    arrowdown: "focus_down",
    arrowup: "focus_up",
    arrowright: "focus_right",
    o: "focus_next",
    // Move / swap
    "S-h": "move_left",
    "S-j": "move_down",
    "S-k": "move_up",
    "S-l": "move_right",
    "S-arrowleft": "move_left",
    "S-arrowdown": "move_down",
    "S-arrowup": "move_up",
    "S-arrowright": "move_right",
    // Resize
    "C-h": "resize_left",
    "C-j": "resize_down",
    "C-k": "resize_up",
    "C-l": "resize_right",
    // Background / restore
    b: "background_pane",
    "S-b": "show_background",
    // Misc
    z: "toggle_zoom",
    x: "close_pane",
    d: "detach_pane",
    n: "new_window",
    e: "toggle_sidebar",
    "S-e": "toggle_editor",
    r: "reload_config",
    ",": "open_settings",
    p: "pick_shell",
  },
  // Global chords active *without* the prefix. These are the explicit, opt-in
  // exception to "never swallow keys outside prefix mode" (CLAUDE.md §9): each
  // carries a modifier and none maps to meaningful terminal input.
  //   - Win+O (and Ctrl+Shift+O as a fallback if the OS eats Win+O): insert a
  //     file path picked from the native dialog into the focused pane.
  //   - Ctrl+= / Ctrl+- / Ctrl+0: zoom the terminal font in / out / reset.
  globalBindings: {
    "M-o": "open_file_dialog",
    "C-S-o": "open_file_dialog",
    "C-=": "zoom_in",
    "C-+": "zoom_in",
    "C-S-=": "zoom_in",
    "C--": "zoom_out",
    "C-0": "zoom_reset",
  },
};

type Timer = ReturnType<typeof setTimeout>;

export class Keymap {
  private cfg: KeymapConfig;
  private inPrefix = false;
  private timer: Timer | null = null;
  /** Injected for tests; defaults to the real timers. */
  private readonly setTimer: (cb: () => void, ms: number) => Timer;
  private readonly clearTimer: (t: Timer) => void;

  constructor(
    cfg: KeymapConfig = DEFAULT_KEYMAP,
    timers?: {
      setTimer: (cb: () => void, ms: number) => Timer;
      clearTimer: (t: Timer) => void;
    },
  ) {
    this.cfg = cfg;
    this.setTimer = timers?.setTimer ?? ((cb, ms) => setTimeout(cb, ms));
    this.clearTimer = timers?.clearTimer ?? ((t) => clearTimeout(t));
  }

  get isInPrefix(): boolean {
    return this.inPrefix;
  }

  /** Replace the active configuration (e.g. on reload_config). */
  setConfig(cfg: KeymapConfig): void {
    this.cancelPrefix();
    this.cfg = cfg;
  }

  private armTimeout(): void {
    if (this.timer) this.clearTimer(this.timer);
    this.timer = this.setTimer(() => {
      this.inPrefix = false;
      this.timer = null;
    }, this.cfg.timeoutMs ?? 1500);
  }

  private cancelPrefix(): void {
    this.inPrefix = false;
    if (this.timer) {
      this.clearTimer(this.timer);
      this.timer = null;
    }
  }

  /** Core state machine. See CLAUDE.md §9 for the rules. */
  handleKey(ev: KeyEvent): KeyOutcome {
    // A lone modifier keydown (Shift/Ctrl/Alt/Meta by itself) must not resolve
    // prefix mode. Reaching a move (Shift+…) or resize (Ctrl+…) chord means
    // pressing the modifier *first*, which fires its own keydown; without this
    // guard that keydown would match no binding and silently cancel prefix mode
    // before the real key arrives, making every modifier-bearing binding
    // unreachable. While in prefix, swallow it and re-arm the timeout so the
    // window stays open; outside prefix, let it flow to the terminal untouched.
    if (isBareModifier(ev.key)) {
      if (this.inPrefix) {
        this.armTimeout();
        return { handled: true };
      }
      return { handled: false };
    }

    const chord = chordOf(ev);

    if (!this.inPrefix) {
      if (chord === this.cfg.prefix) {
        this.inPrefix = true;
        this.armTimeout();
        return { handled: true };
      }
      const global = this.cfg.globalBindings?.[chord];
      if (global) return { handled: true, action: global };
      // Outside prefix mode we never swallow keys (CLAUDE.md §9, §20).
      return { handled: false };
    }

    // In prefix mode: this keystroke resolves it either way.
    this.cancelPrefix();

    if (chord === this.cfg.prefix) {
      // Double prefix → send a literal prefix keystroke to the shell.
      return { handled: true, sendPrefix: true };
    }
    const action = this.lookupBinding(chord);
    if (action) return { handled: true, action };

    // Unrecognized key cancels prefix silently and is consumed.
    return { handled: true };
  }

  /**
   * Resolve a post-prefix chord to an action. Tries an exact match first, then
   * retries with the prefix's own modifiers removed.
   *
   * This is the difference between a keymap that works and one that feels
   * broken: the prefix is a chord like `C-a`, and people keep `Ctrl` held while
   * tapping the command key, so `s` actually arrives as `C-s`. Without this
   * fallback, every plain-key binding (`s`, `v`, `z`, `x`, `-`, …) silently
   * fails whenever Ctrl is still down. Exact bindings are matched first, so the
   * intentional `C-h`/`C-j`/`C-k`/`C-l` resize chords still win.
   */
  private lookupBinding(chord: string): string | undefined {
    const exact = this.cfg.prefixBindings[chord];
    if (exact !== undefined) return exact;

    const pre = parseChord(this.cfg.prefix);
    const c = parseChord(chord);
    const relaxed = buildChord({
      ctrl: c.ctrl && !pre.ctrl,
      alt: c.alt && !pre.alt,
      shift: c.shift && !pre.shift,
      meta: c.meta && !pre.meta,
      key: c.key,
    });
    return relaxed !== chord ? this.cfg.prefixBindings[relaxed] : undefined;
  }

  /** The raw bytes for the prefix keystroke, for double-prefix passthrough. */
  prefixBytes(): Uint8Array {
    return chordToBytes(this.cfg.prefix);
  }
}

/**
 * Translate a control chord like `"C-a"` into the byte it sends to a PTY.
 * Only the common `Ctrl+<letter>` case is needed for double-prefix passthrough.
 */
export function chordToBytes(chord: string): Uint8Array {
  const m = /^C-([a-z])$/.exec(chord);
  if (m) {
    // Ctrl+A => 0x01, Ctrl+B => 0x02, ...
    const code = m[1].charCodeAt(0) - 96;
    return new Uint8Array([code]);
  }
  // Fallback: send the last character literally.
  const ch = chord.slice(-1);
  return new TextEncoder().encode(ch);
}
