import { describe, it, expect } from "vitest";
import {
  parseToml,
  validateConfig,
  loadConfig,
  defaultConfig,
} from "./config";

describe("parseToml", () => {
  it("parses scalars, strings, sections and comments", () => {
    const t = parseToml(`
      # a comment
      prompt = "{user} @ {cwd} $ "
      fontSize = 16          # trailing comment
      cursorBlink = true

      [keybindings]
      "C-x" = "close_pane"
    `);
    expect(t.prompt).toBe("{user} @ {cwd} $ ");
    expect(t.fontSize).toBe(16);
    expect(t.cursorBlink).toBe(true);
    expect(t.keybindings).toEqual({ "C-x": "close_pane" });
  });

  it("ignores `#` inside quoted strings", () => {
    const t = parseToml(`prompt = "a # b"`);
    expect(t.prompt).toBe("a # b");
  });

  it("throws on malformed lines", () => {
    expect(() => parseToml("not a pair")).toThrow();
  });
});

describe("validateConfig", () => {
  it("fills defaults for missing keys", () => {
    const cfg = validateConfig({});
    expect(cfg).toEqual(defaultConfig());
  });

  it("clamps fontSize and resizeStep into range", () => {
    expect(validateConfig({ fontSize: 1000 }).fontSize).toBe(72);
    expect(validateConfig({ fontSize: 1 }).fontSize).toBe(6);
    expect(validateConfig({ resizeStep: 5 }).resizeStep).toBeLessThanOrEqual(0.9);
  });

  it("collects keybinding overrides", () => {
    const cfg = validateConfig({ keybindings: { "C-x": "close_pane" } });
    expect(cfg.keybindings).toEqual({ "C-x": "close_pane" });
  });
});

describe("loadConfig", () => {
  it("parses and validates a full config string", () => {
    const cfg = loadConfig(`
      prefix = "C-b"
      fontSize = 18
      [keybindings]
      q = "toggle_zoom"
    `);
    expect(cfg.prefix).toBe("C-b");
    expect(cfg.fontSize).toBe(18);
    expect(cfg.keybindings.q).toBe("toggle_zoom");
  });
});
