/**
 * xterm.js terminal construction: WebGL renderer with a canvas fallback, and
 * theme derived from the CSS variables in `styles/theme.css` (CLAUDE.md §13–14).
 *
 * Colors are never hardcoded here — they are read from CSS custom properties so
 * the theme stays in one place.
 */

import { Terminal, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { SerializeAddon } from "@xterm/addon-serialize";
import type { Config } from "../config/config";

function cssVar(name: string, fallback: string): string {
  const v = getComputedStyle(document.documentElement)
    .getPropertyValue(name)
    .trim();
  return v || fallback;
}

/** Build the xterm theme from CSS variables (the 16 ANSI colors + bg/fg). */
export function themeFromCss(): ITheme {
  return {
    background: cssVar("--bg", "#0c1212"),
    foreground: cssVar("--fg", "#cfd8d4"),
    cursor: cssVar("--cursor", "#4ce675"),
    cursorAccent: cssVar("--bg", "#0c1212"),
    selectionBackground: cssVar("--selection", "#173a33"),
    black: cssVar("--ansi-0", "#1d1f21"),
    red: cssVar("--ansi-1", "#cc6666"),
    green: cssVar("--ansi-2", "#b5bd68"),
    yellow: cssVar("--ansi-3", "#f0c674"),
    blue: cssVar("--ansi-4", "#81a2be"),
    magenta: cssVar("--ansi-5", "#b294bb"),
    cyan: cssVar("--ansi-6", "#8abeb7"),
    white: cssVar("--ansi-7", "#c5c8c6"),
    brightBlack: cssVar("--ansi-8", "#666666"),
    brightRed: cssVar("--ansi-9", "#d54e53"),
    brightGreen: cssVar("--ansi-10", "#b9ca4a"),
    brightYellow: cssVar("--ansi-11", "#e7c547"),
    brightBlue: cssVar("--ansi-12", "#7aa6da"),
    brightMagenta: cssVar("--ansi-13", "#c397d8"),
    brightCyan: cssVar("--ansi-14", "#70c0b1"),
    brightWhite: cssVar("--ansi-15", "#eaeaea"),
  };
}

export interface TerminalBundle {
  term: Terminal;
  fit: FitAddon;
  /** Serializes the buffer (incl. scrollback) so a pane survives moving windows. */
  serialize: SerializeAddon;
}

/**
 * Create a `Terminal` + fit addon and attach it to `el`. Tries the WebGL
 * renderer once, falling back to canvas if WebGL is unavailable or its context
 * is lost.
 */
export function createTerminal(el: HTMLElement, cfg: Config): TerminalBundle {
  const term = new Terminal({
    fontFamily: cfg.fontFamily,
    fontSize: cfg.fontSize,
    cursorBlink: true,
    allowProposedApi: true,
    scrollback: 10000,
    theme: themeFromCss(),
    macOptionIsMeta: true,
  });

  const fit = new FitAddon();
  term.loadAddon(fit);
  const serialize = new SerializeAddon();
  term.loadAddon(serialize);
  term.open(el);

  try {
    const webgl = new WebglAddon();
    webgl.onContextLoss(() => webgl.dispose());
    term.loadAddon(webgl);
  } catch {
    // WebGL unavailable — xterm.js falls back to its DOM/canvas renderer.
  }

  return { term, fit, serialize };
}
