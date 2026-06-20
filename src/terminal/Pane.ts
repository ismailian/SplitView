/**
 * One terminal pane: an xterm.js instance wired to a backend PTY (CLAUDE.md §4).
 *
 * A `Pane` owns a stable DOM node that the layout renderer *re-parents* rather
 * than recreating, so scrollback and the WebGL context survive layout changes
 * (CLAUDE.md §12, §20).
 */

import type { Terminal } from "@xterm/xterm";
import type { FitAddon } from "@xterm/addon-fit";
import { createTerminal } from "./renderer";
import type { Config } from "../config/config";
import {
  spawnPty,
  writePty,
  resizePty,
  closePty,
  type PaneId,
} from "../ipc/commands";

const RESIZE_DEBOUNCE_MS = 40;

/**
 * What Ctrl+Shift+Enter sends: Esc + CR (the Meta/Alt+Enter sequence). Apps that
 * support multi-line input (readline-/Ink-based REPLs) treat this as "insert a
 * newline" rather than "submit", which a plain CR would do (CLAUDE.md §1).
 */
const CTRL_SHIFT_ENTER = encode("\x1b\r");

export class Pane {
  /** Stable wrapper element; the renderer moves this around the DOM. */
  readonly el: HTMLDivElement;
  readonly term: Terminal;
  private readonly fit: FitAddon;
  private paneId: PaneId | null = null;
  private resizeTimer: number | null = null;
  private lastCols = 0;
  private lastRows = 0;
  private disposed = false;
  /**
   * Called when this pane gains focus (mouse click or programmatic). Lets the
   * controller keep `layout.focused` in sync with the terminal that actually
   * has DOM focus, so a subsequent split targets the right pane.
   */
  onFocus: (() => void) | null = null;

  private constructor(el: HTMLDivElement, term: Terminal, fit: FitAddon) {
    this.el = el;
    this.term = term;
    this.fit = fit;
  }

  /** Create a pane, mount xterm.js, and spawn its PTY. */
  static async create(cfg: Config, cwd?: string): Promise<Pane> {
    const el = document.createElement("div");
    el.className = "pane";

    const { term, fit } = createTerminal(el, cfg);
    const pane = new Pane(el, term, fit);

    // xterm focuses its helper textarea on click; that `focusin` bubbles to the
    // wrapper. Forward it so the controller can sync `layout.focused` to the
    // pane the user actually clicked into (CLAUDE.md §9 keyboard-first, but the
    // mouse is still a valid way to pick the focused pane).
    el.addEventListener("focusin", () => {
      if (!pane.disposed) pane.onFocus?.();
    });

    // Intercept two keys before xterm's defaults (CLAUDE.md §1):
    //  - Ctrl+Shift+Enter -> Esc+Enter, so multi-line apps insert a newline
    //    instead of submitting (xterm would otherwise send a plain CR).
    //  - Ctrl+V -> return false so the webview's native paste fires on xterm's
    //    helper textarea, which xterm turns into a correct bracketed paste.
    //    Otherwise xterm sends the literal-next control byte and nothing pastes.
    term.attachCustomKeyEventHandler((ev) => {
      if (ev.type !== "keydown") return true;
      const plainCtrl = ev.ctrlKey && !ev.altKey && !ev.metaKey;
      if (plainCtrl && ev.shiftKey && ev.key === "Enter") {
        pane.sendBytes(CTRL_SHIFT_ENTER);
        return false;
      }
      if (plainCtrl && !ev.shiftKey && ev.key.toLowerCase() === "v") {
        return false; // allow the native paste
      }
      return true;
    });

    // Size to the container before spawning so the PTY starts at the right size.
    pane.safeFit();
    const { cols, rows } = term;
    pane.lastCols = cols;
    pane.lastRows = rows;

    const id = await spawnPty(
      {
        cols,
        rows,
        cwd,
        shell: cfg.shell,
        prompt: cfg.prompt,
        promptStyle: cfg.promptStyle,
      },
      (bytes) => {
        if (!pane.disposed) term.write(bytes);
      },
    );
    pane.paneId = id;

    // Keystrokes -> PTY. xterm.js buffers/flushes on its own; we don't add a
    // per-keystroke loop on top.
    term.onData((data) => {
      if (pane.paneId) void writePty(pane.paneId, encode(data));
    });

    return pane;
  }

  get id(): PaneId | null {
    return this.paneId;
  }

  /** Send raw bytes to the PTY (used for double-prefix passthrough). */
  sendBytes(bytes: Uint8Array): void {
    if (this.paneId) void writePty(this.paneId, bytes);
  }

  /** Send UTF-8 text to the PTY as if typed (e.g. an inserted file path). */
  sendText(text: string): void {
    this.sendBytes(encode(text));
  }

  /**
   * Change the terminal's font size, then re-fit so the PTY tracks the new
   * cols/rows (CLAUDE.md §13, §20). Used by the zoom-in/out actions.
   */
  setFontSize(px: number): void {
    if (this.disposed) return;
    this.term.options.fontSize = px;
    this.refit();
  }

  focus(): void {
    this.term.focus();
  }

  private safeFit(): void {
    try {
      this.fit.fit();
    } catch {
      // Element not visible / zero-size yet; ignore until next layout pass.
    }
  }

  /**
   * Re-fit after a layout change and, only if the dimensions actually changed,
   * resize the PTY. Debounced to avoid reflow storms (CLAUDE.md §13).
   */
  refit(): void {
    if (this.disposed) return;
    this.safeFit();
    const { cols, rows } = this.term;
    if (cols === this.lastCols && rows === this.lastRows) return;
    this.lastCols = cols;
    this.lastRows = rows;

    if (this.resizeTimer !== null) window.clearTimeout(this.resizeTimer);
    this.resizeTimer = window.setTimeout(() => {
      this.resizeTimer = null;
      if (this.paneId) void resizePty(this.paneId, cols, rows);
    }, RESIZE_DEBOUNCE_MS);
  }

  /** Kill the PTY and dispose the terminal. */
  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    if (this.resizeTimer !== null) window.clearTimeout(this.resizeTimer);
    const id = this.paneId;
    this.paneId = null;
    this.term.dispose();
    this.el.remove();
    if (id) await closePty(id);
  }
}

function encode(s: string): Uint8Array {
  return new TextEncoder().encode(s);
}
