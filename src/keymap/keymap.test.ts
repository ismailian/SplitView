import { describe, it, expect } from "vitest";
import {
  Keymap,
  DEFAULT_KEYMAP,
  chordOf,
  chordToBytes,
  parseChord,
  type KeyEvent,
  type KeymapConfig,
} from "./keymap";

function key(
  k: string,
  mods: Partial<Omit<KeyEvent, "key">> = {},
): KeyEvent {
  return {
    key: k,
    ctrlKey: mods.ctrlKey ?? false,
    shiftKey: mods.shiftKey ?? false,
    altKey: mods.altKey ?? false,
    metaKey: mods.metaKey ?? false,
  };
}

describe("chordOf", () => {
  it("normalizes single characters and encodes modifiers", () => {
    expect(chordOf(key("a", { ctrlKey: true }))).toBe("C-a");
    expect(chordOf(key("H", { shiftKey: true }))).toBe("S-h");
    expect(chordOf(key("ArrowLeft"))).toBe("arrowleft");
    expect(chordOf(key("h", { ctrlKey: true }))).toBe("C-h");
  });
});

/** A Keymap whose timeout never fires unless we trigger it manually. */
function manualKeymap(cfg: KeymapConfig = DEFAULT_KEYMAP) {
  const timeouts: Array<() => void> = [];
  const km = new Keymap(cfg, {
    setTimer: (cb) => {
      timeouts.push(cb);
      return timeouts.length as unknown as ReturnType<typeof setTimeout>;
    },
    clearTimer: () => {},
  });
  return { km, fireTimeouts: () => timeouts.forEach((t) => t()) };
}

describe("prefix engine", () => {
  it("does not swallow keys outside prefix mode", () => {
    const { km } = manualKeymap();
    const out = km.handleKey(key("h"));
    expect(out).toEqual({ handled: false });
  });

  it("enters prefix mode on the prefix chord and consumes it", () => {
    const { km } = manualKeymap();
    const out = km.handleKey(key("a", { ctrlKey: true }));
    expect(out.handled).toBe(true);
    expect(km.isInPrefix).toBe(true);
  });

  it("resolves a binding to an action and exits prefix mode", () => {
    const { km } = manualKeymap();
    km.handleKey(key("a", { ctrlKey: true })); // prefix
    const out = km.handleKey(key("z")); // zoom
    expect(out).toEqual({ handled: true, action: "toggle_zoom" });
    expect(km.isInPrefix).toBe(false);
  });

  it("maps split and focus bindings per the default keymap", () => {
    const { km } = manualKeymap();
    const press = (k: string, mods?: Partial<Omit<KeyEvent, "key">>) => {
      km.handleKey(key("a", { ctrlKey: true }));
      return km.handleKey(key(k, mods));
    };
    expect(press("-")).toMatchObject({ action: "split_down" });
    expect(press("\\")).toMatchObject({ action: "split_right" });
    expect(press("l")).toMatchObject({ action: "focus_right" });
    expect(press("l", { shiftKey: true })).toMatchObject({ action: "move_right" });
    expect(press("l", { ctrlKey: true })).toMatchObject({ action: "resize_right" });
  });

  it("still matches plain-key bindings when the prefix's Ctrl is held", () => {
    // Real-world: user holds Ctrl through "C-a" then taps s/v/z/x without
    // releasing, so those arrive as C-s / C-v / C-z / C-x.
    const { km } = manualKeymap();
    const pressHeld = (k: string) => {
      km.handleKey(key("a", { ctrlKey: true })); // prefix
      return km.handleKey(key(k, { ctrlKey: true })); // command key, Ctrl still down
    };
    expect(pressHeld("s")).toMatchObject({ action: "split_down" });
    expect(pressHeld("v")).toMatchObject({ action: "split_right" });
    expect(pressHeld("z")).toMatchObject({ action: "toggle_zoom" });
    expect(pressHeld("x")).toMatchObject({ action: "close_pane" });
  });

  it("keeps prefix mode when a lone modifier is pressed first", () => {
    // Reaching a move/resize chord means pressing Shift/Ctrl *before* the key.
    // That lone-modifier keydown must not cancel prefix mode.
    const { km } = manualKeymap();
    km.handleKey(key("a", { ctrlKey: true })); // prefix
    const mod = km.handleKey(key("Shift", { shiftKey: true }));
    expect(mod).toEqual({ handled: true });
    expect(km.isInPrefix).toBe(true);
  });

  it("resolves a move chord reached via a held Shift", () => {
    const { km } = manualKeymap();
    km.handleKey(key("a", { ctrlKey: true })); // prefix
    km.handleKey(key("Shift", { shiftKey: true })); // modifier pressed first
    expect(km.handleKey(key("L", { shiftKey: true }))).toMatchObject({
      action: "move_right",
    });
  });

  it("resolves a resize chord reached via a held Ctrl", () => {
    const { km } = manualKeymap();
    km.handleKey(key("a", { ctrlKey: true })); // prefix
    km.handleKey(key("Control", { ctrlKey: true })); // modifier pressed first
    expect(km.handleKey(key("l", { ctrlKey: true }))).toMatchObject({
      action: "resize_right",
    });
  });

  it("lets a lone modifier flow to the terminal outside prefix mode", () => {
    const { km } = manualKeymap();
    expect(km.handleKey(key("Shift", { shiftKey: true }))).toEqual({
      handled: false,
    });
  });

  it("prefers an exact Ctrl binding over the relaxed fallback", () => {
    // C-h is bound to resize_left; holding Ctrl must not collapse it to h.
    const { km } = manualKeymap();
    km.handleKey(key("a", { ctrlKey: true }));
    expect(km.handleKey(key("h", { ctrlKey: true }))).toMatchObject({
      action: "resize_left",
    });
  });

  it("sends a literal prefix on double-prefix", () => {
    const { km } = manualKeymap();
    km.handleKey(key("a", { ctrlKey: true }));
    const out = km.handleKey(key("a", { ctrlKey: true }));
    expect(out).toEqual({ handled: true, sendPrefix: true });
    expect(km.isInPrefix).toBe(false);
  });

  it("silently cancels on an unrecognized key", () => {
    const { km } = manualKeymap();
    km.handleKey(key("a", { ctrlKey: true }));
    const out = km.handleKey(key("q")); // not bound
    expect(out).toEqual({ handled: true });
    expect(km.isInPrefix).toBe(false);
  });

  it("cancels prefix mode when the timeout fires", () => {
    const { km, fireTimeouts } = manualKeymap();
    km.handleKey(key("a", { ctrlKey: true }));
    expect(km.isInPrefix).toBe(true);
    fireTimeouts();
    expect(km.isInPrefix).toBe(false);
    // After timeout, a plain key flows to the terminal again.
    expect(km.handleKey(key("z"))).toEqual({ handled: false });
  });

  it("honours config overrides via setConfig", () => {
    const { km } = manualKeymap();
    km.setConfig({
      prefix: "C-b",
      prefixBindings: { q: "close_pane" },
    });
    expect(km.handleKey(key("a", { ctrlKey: true }))).toEqual({
      handled: false,
    });
    km.handleKey(key("b", { ctrlKey: true })); // new prefix
    expect(km.handleKey(key("q"))).toMatchObject({ action: "close_pane" });
  });
});

describe("parseChord", () => {
  it("splits modifiers from the key, including a literal '-' key", () => {
    expect(parseChord("C-a")).toMatchObject({ ctrl: true, key: "a" });
    expect(parseChord("C-S-h")).toMatchObject({
      ctrl: true,
      shift: true,
      key: "h",
    });
    expect(parseChord("-")).toMatchObject({ ctrl: false, key: "-" });
    expect(parseChord("C--")).toMatchObject({ ctrl: true, key: "-" });
    expect(parseChord("arrowleft")).toMatchObject({ key: "arrowleft" });
  });
});

describe("chordToBytes", () => {
  it("encodes Ctrl+<letter> as the control byte", () => {
    expect(Array.from(chordToBytes("C-a"))).toEqual([1]);
    expect(Array.from(chordToBytes("C-b"))).toEqual([2]);
  });
});
