/**
 * Settings model, persistence, and live application (CLAUDE.md §3).
 *
 * Settings extend the base `Config` with everything the Settings window edits:
 * display (colours/fonts/divider), prompt-info toggles, aliases, and the
 * banner. They persist to `localStorage` (the webview owns this state; the
 * backend is told the relevant pieces at spawn time) and apply live by writing
 * the CSS variables in `styles/theme.css` — the single source of colour truth.
 */

import type { Alias, BindMode, PromptInfo, RemoteConfig } from "../ipc/commands";
import { defaultConfig } from "../config/config";

/** Everything under the "Display" section. Colours are `#rrggbb` hex. */
export interface DisplaySettings {
  fontFamily: string;
  fontSize: number;
  /** Terminal background. */
  bg: string;
  /** Terminal foreground (default text). */
  fg: string;
  /** Cursor colour. */
  cursor: string;
  /** Accent green: typed commands + the ANSI green slot. */
  accent: string;
  /** Idle divider colour. */
  divider: string;
  /** Divider colour next to the focused pane / on hover. */
  dividerActive: string;
  /** Divider thickness in px. */
  dividerSize: number;
}

/**
 * Remote control, as Settings remembers it (`docs/remote-control-desktop.md`
 * §7.3). The server itself is never on unless the backend says so — this is the
 * configuration it is started *with*, plus whether to start it at all.
 */
export interface RemoteSettings {
  /**
   * Re-enable the server on launch. Off by default and deliberately opt-in:
   * turning a terminal emulator into a remote shell server should be something
   * you did, not something you inherited from last week.
   */
  enableAtStartup: boolean;
  /** `lan` (reachable from the network) or `loopback` (tunnel it yourself). */
  bind: BindMode;
  /** `0` asks the OS for an ephemeral port. */
  port: number;
  /** Let a client resize a pane, which reflows this window too. Off by default. */
  allowResize: boolean;
}

export interface Settings {
  display: DisplaySettings;
  promptInfo: PromptInfo;
  aliases: Alias[];
  banner: string;
  remote: RemoteSettings;
}

/** The subset of `RemoteSettings` the backend needs to start the server. */
export function remoteConfigOf(settings: RemoteSettings): RemoteConfig {
  return {
    bind: settings.bind,
    port: settings.port,
    allowResize: settings.allowResize,
  };
}

/** Default listening port; mirrors `DEFAULT_PORT` in `src-tauri/src/remote`. */
export const DEFAULT_REMOTE_PORT = 8765;

const STORAGE_KEY = "splitview.settings";

/** Defaults mirror `styles/theme.css` and `defaultConfig()` exactly. */
export function defaultSettings(): Settings {
  const cfg = defaultConfig();
  return {
    display: {
      fontFamily: cfg.fontFamily,
      fontSize: cfg.fontSize,
      bg: "#0c1212",
      fg: "#cfd8d4",
      cursor: "#4ce675",
      accent: "#4ce675",
      divider: "#2a4640",
      dividerActive: "#4a8473",
      dividerSize: 2,
    },
    promptInfo: {
      user: true,
      host: true,
      dir: true,
      branch: true,
      time: false,
      exitStatus: false,
      // 0 = show the whole path (middle elided when long).
      dirSegments: 0,
    },
    aliases: [],
    banner: "",
    remote: {
      enableAtStartup: false,
      bind: "lan",
      port: DEFAULT_REMOTE_PORT,
      allowResize: false,
    },
  };
}

/** Clamp a stored/edited port to a usable range; `0` means "any free port". */
export function clampPort(value: unknown): number {
  const n = Math.floor(Number(value));
  if (!Number.isFinite(n) || n < 0 || n > 65535) return DEFAULT_REMOTE_PORT;
  // Ports below 1024 need privileges we do not have and do not want.
  if (n > 0 && n < 1024) return DEFAULT_REMOTE_PORT;
  return n;
}

/** The colour-valued keys of `DisplaySettings` (everything a preset sets). */
export type ColorKey =
  | "bg"
  | "fg"
  | "cursor"
  | "accent"
  | "divider"
  | "dividerActive";

/** A handful of ready-made palettes for the Display section's preset buttons. */
export const PRESETS: { name: string; colors: Partial<Record<ColorKey, string>> }[] = [
  {
    name: "Parrot",
    colors: {
      bg: "#0c1212",
      fg: "#cfd8d4",
      cursor: "#4ce675",
      accent: "#4ce675",
      divider: "#2a4640",
      dividerActive: "#4a8473",
    },
  },
  {
    name: "Midnight",
    colors: {
      bg: "#0b0f1a",
      fg: "#c7d0e0",
      cursor: "#7aa2f7",
      accent: "#7aa2f7",
      divider: "#222a3d",
      dividerActive: "#3b4a6b",
    },
  },
  {
    name: "Carbon",
    colors: {
      bg: "#101010",
      fg: "#d6d6d6",
      cursor: "#e6b450",
      accent: "#e6b450",
      divider: "#2a2a2a",
      dividerActive: "#4a4a4a",
    },
  },
  {
    name: "Matrix",
    colors: {
      bg: "#000800",
      fg: "#9bf59b",
      cursor: "#39ff14",
      accent: "#39ff14",
      divider: "#0f3d0f",
      dividerActive: "#1f7a1f",
    },
  },
];

/** Upper bound on the prompt's trailing-directory-segment count. Past a handful
 * of segments you are asking for the whole path, which is what `0` means. */
export const MAX_DIR_SEGMENTS = 9;

/** Clamp a stored/edited segment count to `0..MAX_DIR_SEGMENTS` (0 = full path).
 * It ends up in the spawned shell's environment, so it must be a plain integer. */
export function clampDirSegments(value: unknown): number {
  const n = Math.floor(Number(value));
  if (!Number.isFinite(n) || n <= 0) return 0;
  return Math.min(n, MAX_DIR_SEGMENTS);
}

/** Keep only well-formed `{ name, command }` alias entries. */
function sanitizeAliases(value: unknown): Alias[] {
  if (!Array.isArray(value)) return [];
  const out: Alias[] = [];
  for (const item of value) {
    if (
      item &&
      typeof item === "object" &&
      typeof (item as Alias).name === "string" &&
      typeof (item as Alias).command === "string"
    ) {
      out.push({ name: (item as Alias).name, command: (item as Alias).command });
    }
  }
  return out;
}

/** Load settings from `localStorage`, merging over defaults so missing or
 * stale fields are filled in (and a corrupt store falls back cleanly). */
export function loadSettings(): Settings {
  const def = defaultSettings();
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return def;
    const parsed = JSON.parse(raw) as Partial<Settings>;
    const promptInfo = { ...def.promptInfo, ...(parsed.promptInfo ?? {}) };
    return {
      display: { ...def.display, ...(parsed.display ?? {}) },
      promptInfo: {
        ...promptInfo,
        dirSegments: clampDirSegments(promptInfo.dirSegments),
      },
      aliases: parsed.aliases ? sanitizeAliases(parsed.aliases) : def.aliases,
      banner: typeof parsed.banner === "string" ? parsed.banner : def.banner,
      remote: sanitizeRemote(def.remote, parsed.remote),
    };
  } catch {
    return def;
  }
}

/** Fill in and clamp stored remote settings, defaulting anything unusable. */
function sanitizeRemote(
  def: RemoteSettings,
  stored: Partial<RemoteSettings> | undefined,
): RemoteSettings {
  const merged = { ...def, ...(stored ?? {}) };
  return {
    // A stored value that is not exactly `true` leaves the server off: the
    // safe reading of a corrupt settings blob is "do not open a socket".
    enableAtStartup: merged.enableAtStartup === true,
    bind: merged.bind === "loopback" ? "loopback" : "lan",
    port: clampPort(merged.port),
    allowResize: merged.allowResize === true,
  };
}

/** Persist settings. Failures (e.g. storage disabled) are non-fatal. */
export function saveSettings(settings: Settings): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(settings));
  } catch {
    // Storage unavailable; settings simply won't persist this session.
  }
}

/**
 * Apply display settings live by writing the CSS variables the rest of the app
 * reads (`styles/theme.css`). Pane terminals re-read these via `themeFromCss()`
 * separately; this only touches the document-level custom properties.
 */
export function applyDisplay(d: DisplaySettings): void {
  const root = document.documentElement.style;
  root.setProperty("--bg", d.bg);
  root.setProperty("--fg", d.fg);
  root.setProperty("--cursor", d.cursor);
  root.setProperty("--ansi-2", d.accent);
  root.setProperty("--divider", d.divider);
  root.setProperty("--divider-active", d.dividerActive);
  root.setProperty("--divider-size", `${d.dividerSize}px`);
  root.setProperty("--font-family", d.fontFamily);
}
