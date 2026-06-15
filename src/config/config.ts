/**
 * Load / parse / validate the TOML config (CLAUDE.md §3, §9).
 *
 * The parser is a small, dependency-free subset of TOML sufficient for our flat
 * config (string/number/bool values and `[section]` tables). It is pure and
 * testable. File I/O lives elsewhere; this module turns a TOML *string* into a
 * validated, clamped `Config`.
 */

import type { Bindings } from "../keymap/keymap";
import { MIN_RATIO, MAX_RATIO } from "../layout/tree";

export interface Config {
  /** Prompt template, e.g. `"{user} @ {cwd} $ "` (used when promptStyle="plain"). */
  prompt: string;
  /**
   * Which prompt the backend injects: `"parrot"` (default) reproduces the
   * iconic ParrotOS two-line prompt; `"plain"` uses the `prompt` template.
   */
  promptStyle: "parrot" | "plain";
  /** Prefix chord, e.g. `"C-a"`. */
  prefix: string;
  /** Optional shell override (else `$SHELL`). */
  shell?: string;
  /** Monospace font family for the terminal. */
  fontFamily: string;
  fontSize: number;
  /** How much a resize step changes a ratio. */
  resizeStep: number;
  /** Keybinding overrides: chord -> action name. */
  keybindings: Bindings;
}

export function defaultConfig(): Config {
  return {
    prompt: "{user} @ {cwd} $ ",
    promptStyle: "parrot",
    prefix: "C-a",
    fontFamily:
      'ui-monospace, "SF Mono", "JetBrains Mono", "Cascadia Code", Menlo, Consolas, monospace',
    fontSize: 14,
    resizeStep: 0.03,
    keybindings: {},
  };
}

type TomlValue = string | number | boolean;
type TomlTable = Record<string, TomlValue | Record<string, TomlValue>>;

/** Parse a tiny, flat subset of TOML. Throws on malformed input. */
export function parseToml(src: string): TomlTable {
  const root: TomlTable = {};
  let section: Record<string, TomlValue> | null = null;

  const lines = src.split(/\r?\n/);
  for (let i = 0; i < lines.length; i++) {
    const raw = stripComment(lines[i]).trim();
    if (raw === "") continue;

    const tableMatch = /^\[([A-Za-z0-9_.-]+)\]$/.exec(raw);
    if (tableMatch) {
      const name = tableMatch[1];
      const table: Record<string, TomlValue> = {};
      root[name] = table;
      section = table;
      continue;
    }

    const eq = raw.indexOf("=");
    if (eq < 0) throw new Error(`config: malformed line ${i + 1}: "${raw}"`);
    const key = unquoteKey(raw.slice(0, eq).trim());
    const value = parseValue(raw.slice(eq + 1).trim(), i + 1);
    if (section) section[key] = value;
    else root[key] = value;
  }
  return root;
}

/** TOML keys may be bare or quoted (`"C-x"` / `'C-x'`); strip the quotes. */
function unquoteKey(key: string): string {
  if (
    (key.startsWith('"') && key.endsWith('"')) ||
    (key.startsWith("'") && key.endsWith("'"))
  ) {
    return key.slice(1, -1);
  }
  return key;
}

function stripComment(line: string): string {
  // Remove `#` comments that are not inside a string.
  let inStr = false;
  let quote = "";
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (inStr) {
      if (c === quote) inStr = false;
    } else if (c === '"' || c === "'") {
      inStr = true;
      quote = c;
    } else if (c === "#") {
      return line.slice(0, i);
    }
  }
  return line;
}

function parseValue(token: string, lineNo: number): TomlValue {
  if (
    (token.startsWith('"') && token.endsWith('"')) ||
    (token.startsWith("'") && token.endsWith("'"))
  ) {
    return token.slice(1, -1);
  }
  if (token === "true") return true;
  if (token === "false") return false;
  const num = Number(token);
  if (!Number.isNaN(num)) return num;
  throw new Error(`config: bad value on line ${lineNo}: "${token}"`);
}

function asString(v: unknown, fallback: string): string {
  return typeof v === "string" ? v : fallback;
}
function asNumber(v: unknown, fallback: number): number {
  return typeof v === "number" ? v : fallback;
}

/** Validate + clamp a parsed table into a `Config`, filling defaults. */
export function validateConfig(table: TomlTable): Config {
  const def = defaultConfig();
  const cfg: Config = {
    prompt: asString(table.prompt, def.prompt),
    promptStyle: table.promptStyle === "plain" ? "plain" : "parrot",
    prefix: asString(table.prefix, def.prefix),
    shell: typeof table.shell === "string" ? table.shell : undefined,
    fontFamily: asString(table.fontFamily, def.fontFamily),
    fontSize: clampNumber(asNumber(table.fontSize, def.fontSize), 6, 72),
    resizeStep: clampNumber(
      asNumber(table.resizeStep, def.resizeStep),
      0.01,
      MAX_RATIO - MIN_RATIO,
    ),
    keybindings: {},
  };

  const binds = table.keybindings;
  if (binds && typeof binds === "object") {
    for (const [chord, action] of Object.entries(binds)) {
      if (typeof action === "string") cfg.keybindings[chord] = action;
    }
  }
  return cfg;
}

function clampNumber(n: number, lo: number, hi: number): number {
  if (Number.isNaN(n)) return lo;
  return Math.min(hi, Math.max(lo, n));
}

/** Convenience: parse + validate a TOML string into a `Config`. */
export function loadConfig(src: string): Config {
  return validateConfig(parseToml(src));
}
