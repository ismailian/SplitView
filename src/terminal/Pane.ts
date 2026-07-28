/**
 * One terminal pane: an xterm.js instance wired to a backend PTY (CLAUDE.md §4).
 *
 * A `Pane` owns a stable DOM node that the layout renderer *re-parents* rather
 * than recreating, so scrollback and the WebGL context survive layout changes
 * (CLAUDE.md §12, §20).
 */

import type { Terminal } from "@xterm/xterm";
import type { FitAddon } from "@xterm/addon-fit";
import type { SerializeAddon } from "@xterm/addon-serialize";
import { createTerminal, themeFromCss } from "./renderer";
import type { Config } from "../config/config";
import {
  spawnPty,
  attachPty,
  writePty,
  resizePty,
  closePty,
  type PaneId,
  type Alias,
  type PromptInfo,
} from "../ipc/commands";

/** Per-spawn extras layered on top of the base `Config` (CLAUDE.md §4). */
export interface SpawnExtras {
  /** Working directory for the new shell (inherited from a split). */
  cwd?: string;
  /** Aliases to inject into the shell. */
  aliases?: Alias[];
  /** Text shown at the top of the pane before the first prompt. */
  banner?: string;
  /** Prompt info toggles. */
  promptInfo?: PromptInfo;
}

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
  private readonly serializer: SerializeAddon;
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
  /**
   * Called when the shell runs the injected `splitview` app command, which
   * emits a private OSC sequence carrying its arguments (e.g. `"settings"`).
   * Lets the user drive SplitView from the terminal (CLAUDE.md §1).
   */
  onAppCommand: ((cmd: string) => void) | null = null;
  /** Latest working directory reported by the shell via OSC 7, if any. */
  private currentCwd: string | null = null;
  /** Latest terminal title (OSC 0/2), used to name a backgrounded pane. */
  private currentTitle: string | null = null;

  private constructor(
    el: HTMLDivElement,
    term: Terminal,
    fit: FitAddon,
    serializer: SerializeAddon,
  ) {
    this.el = el;
    this.term = term;
    this.fit = fit;
    this.serializer = serializer;
  }

  /**
   * Build the DOM node + xterm.js instance and wire every handler that is
   * independent of *how* the PTY is obtained (OSC parsing, focus forwarding,
   * key interception, keystrokes → PTY). Both `create` (spawns a fresh PTY) and
   * `attach` (adopts an existing one) start here.
   */
  private static mount(cfg: Config): Pane {
    const el = document.createElement("div");
    el.className = "pane";

    const { term, fit, serialize } = createTerminal(el, cfg);
    const pane = new Pane(el, term, fit, serialize);

    // OSC 7: the shell reports its cwd each prompt so a new split inherits it.
    // OSC 5170: SplitView's private app-command channel — the injected
    // `splitview` shell function emits it (e.g. `splitview settings`).
    term.parser.registerOscHandler(7, (data) => {
      const p = parseOsc7(data);
      if (p) pane.currentCwd = p;
      return true;
    });
    term.parser.registerOscHandler(5170, (data) => {
      if (!pane.disposed) pane.onAppCommand?.(data.trim());
      return true;
    });

    // OSC 0/2: whatever the running program calls itself. Used to name the pane
    // in the backgrounded-panes picker.
    term.onTitleChange((title) => {
      pane.currentTitle = title.trim() || null;
    });

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

    // Keystrokes -> PTY. xterm.js buffers/flushes on its own; we don't add a
    // per-keystroke loop on top. Guarded by `paneId` so it is inert until the
    // PTY is spawned/attached below.
    term.onData((data) => {
      if (pane.paneId) void writePty(pane.paneId, encode(data));
    });

    // Size to the container before the PTY starts so it opens at the right size.
    pane.safeFit();
    pane.lastCols = term.cols;
    pane.lastRows = term.rows;

    return pane;
  }

  /** Create a pane, mount xterm.js, and spawn its PTY. */
  static async create(cfg: Config, spawn: SpawnExtras = {}): Promise<Pane> {
    const pane = Pane.mount(cfg);

    const id = await spawnPty(
      {
        cols: pane.term.cols,
        rows: pane.term.rows,
        cwd: spawn.cwd,
        shell: cfg.shell,
        prompt: cfg.prompt,
        promptStyle: cfg.promptStyle,
        aliases: spawn.aliases,
        promptInfo: spawn.promptInfo,
        banner: spawn.banner,
      },
      (bytes) => {
        if (!pane.disposed) pane.term.write(bytes);
      },
    );
    pane.paneId = id;

    return pane;
  }

  /**
   * Adopt an **existing** backend PTY (identified by `existingId`) into a fresh
   * terminal — the destination side of a detach/reattach. The prior contents are
   * restored from `serialized` (an `@xterm/addon-serialize` dump) *before* the
   * live stream is attached, so scrollback survives the move. `attach_pty` swaps
   * the backend reader's sink to this window's channel; the PTY never restarts.
   */
  static async attach(
    cfg: Config,
    existingId: PaneId,
    serialized: string,
  ): Promise<Pane> {
    const pane = Pane.mount(cfg);

    if (serialized) pane.term.write(serialized);
    pane.paneId = existingId;

    await attachPty(existingId, (bytes) => {
      if (!pane.disposed) pane.term.write(bytes);
    });

    return pane;
  }

  get id(): PaneId | null {
    return this.paneId;
  }

  /** Latest cwd reported by the shell (OSC 7), for inheriting on split. */
  get cwd(): string | null {
    return this.currentCwd;
  }

  /** Latest terminal title (OSC 0/2), if the running program set one. */
  get title(): string | null {
    return this.currentTitle;
  }

  /**
   * The last non-empty output line, for previewing a backgrounded pane. Lines
   * belonging to the injected two-line prompt are skipped — a backgrounded dev
   * server should be identified by what it printed, not by the prompt frame.
   */
  previewLine(): string {
    if (this.disposed) return "";
    const buf = this.term.buffer.active;
    const from = buf.baseY + buf.cursorY;
    for (let y = from; y >= 0 && y > from - 200; y--) {
      const text = buf.getLine(y)?.translateToString(true).trim() ?? "";
      if (text === "" || text.startsWith("┌") || text.startsWith("└")) continue;
      return text;
    }
    return "";
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

  /**
   * Re-read the theme from the CSS variables and apply the font family/size,
   * then refit. Called when the user changes Display settings so live panes
   * update without being recreated (CLAUDE.md §12, §14).
   */
  applyDisplay(fontFamily: string, fontSize: number): void {
    if (this.disposed) return;
    this.term.options.theme = themeFromCss();
    this.term.options.fontFamily = fontFamily;
    this.term.options.fontSize = fontSize;
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

  /**
   * Serialize the terminal buffer (screen + scrollback) to a string that a
   * fresh terminal can `write()` to reproduce it. Used to carry history across
   * a detach/reattach; call it *before* `detachDispose()`.
   */
  serialize(): string {
    if (this.disposed) return "";
    return this.serializer.serialize();
  }

  /**
   * Tear down the terminal and DOM node **without** killing the PTY, so the
   * pane's shell keeps running and can be adopted by another window via
   * `Pane.attach`. This is the detach/reattach path; contrast `dispose()`,
   * which also kills the PTY.
   */
  detachDispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    if (this.resizeTimer !== null) window.clearTimeout(this.resizeTimer);
    this.paneId = null;
    this.term.dispose();
    this.el.remove();
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

/**
 * Parse an OSC 7 payload (`file://host/path`) into the bare path. The host is
 * discarded; the path is percent-decoded best-effort. Returns null if it isn't
 * a `file://` URL. On Windows the path arrives in msys form (`/c/Users/...`);
 * the backend converts it to a native path when spawning the next pane.
 */
function parseOsc7(data: string): string | null {
  if (!data.startsWith("file://")) return null;
  const rest = data.slice("file://".length);
  const slash = rest.indexOf("/");
  if (slash < 0) return null;
  const path = rest.slice(slash);
  try {
    return decodeURIComponent(path);
  } catch {
    return path; // malformed escape; use the raw path
  }
}
