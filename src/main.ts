/**
 * App bootstrap and controller (CLAUDE.md §4).
 *
 * Owns the layout tree, the live panes, the keymap, and the render loop, and
 * implements `AppActions` so the keymap can drive it. The frontend owns layout;
 * the backend owns processes — this file never spawns a PTY directly, it goes
 * through `ipc/commands.ts` via `Pane`.
 */

import "@xterm/xterm/css/xterm.css";
import "./styles/theme.css";

import {
  type Layout,
  type Direction,
  type NodePath,
  emptyLayout,
  singleLayout,
  splitFocused,
  focusDirection,
  focusNext,
  movePane,
  toggleZoom,
  closePane,
  setRatio,
  resizeFocused,
} from "./layout/tree";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { onPtyExit, pickFile } from "./ipc/commands";
import { renderLayout } from "./layout/render";
import { Pane } from "./terminal/Pane";
import {
  Keymap,
  DEFAULT_KEYMAP,
  type KeyEvent,
  type KeymapConfig,
} from "./keymap/keymap";
import { dispatchAction, type AppActions } from "./keymap/actions";
import { defaultConfig, type Config } from "./config/config";

/** Font-size bounds for the zoom-in/out actions, in px. */
const FONT_MIN = 6;
const FONT_MAX = 48;

class App implements AppActions {
  private layout: Layout = emptyLayout();
  private readonly panes = new Map<string, Pane>();
  private config: Config;
  private keymap: Keymap;
  private nextSeq = 0;
  /** Current terminal font size (zoom level); starts at the configured size. */
  private fontSize: number;

  constructor(
    private readonly container: HTMLElement,
    config: Config,
  ) {
    this.config = config;
    this.fontSize = config.fontSize;
    this.keymap = new Keymap(this.keymapConfig());
  }

  private keymapConfig(): KeymapConfig {
    // Defaults, with the user's prefix and any binding overrides applied.
    return {
      ...DEFAULT_KEYMAP,
      prefix: this.config.prefix || DEFAULT_KEYMAP.prefix,
      prefixBindings: {
        ...DEFAULT_KEYMAP.prefixBindings,
        ...this.config.keybindings,
      },
    };
  }

  async start(): Promise<void> {
    // Wire input synchronously, before any await, so keybindings always work
    // even if spawning the first PTY or registering the exit listener is slow
    // or fails.
    document.addEventListener("keydown", this.onKeyDown, true);
    window.addEventListener("resize", () => this.refitAll());

    // Register the exit listener before the first pane spawns so we never miss
    // a shell that exits immediately. A failure here must not break startup.
    void onPtyExit((id) => this.onPaneExited(id)).catch(() => {});

    const first = await this.newPane();
    this.layout = singleLayout(first.id ?? this.placeholderId());
    this.render();
    first.focus();
  }

  private placeholderId(): string {
    return `pane-local-${this.nextSeq++}`;
  }

  private async newPane(): Promise<Pane> {
    const pane = await Pane.create(this.config);
    // Keep a newly split pane at the current zoom level, not the configured one.
    if (this.fontSize !== this.config.fontSize) pane.setFontSize(this.fontSize);
    const id = pane.id;
    if (id) this.panes.set(id, pane);
    // Mouse clicks focus a pane's terminal directly; mirror that into the layout
    // so the next split/move/zoom acts on the pane the user is actually in.
    pane.onFocus = () => this.onPaneFocused(id);
    return pane;
  }

  /**
   * Sync `layout.focused` to a pane that gained DOM focus (e.g. a mouse click).
   * Re-renders to move the focus tint/divider, but does not call `focus()` again
   * — the pane already has DOM focus, so re-focusing would be redundant.
   */
  private onPaneFocused(id: string | null): void {
    if (!id || id === this.layout.focused || !this.panes.has(id)) return;
    this.layout = { ...this.layout, focused: id };
    this.render();
  }

  // --- key handling ------------------------------------------------------

  private onKeyDown = (ev: KeyboardEvent): void => {
    const desc: KeyEvent = {
      key: ev.key,
      ctrlKey: ev.ctrlKey,
      shiftKey: ev.shiftKey,
      altKey: ev.altKey,
      metaKey: ev.metaKey,
    };
    const outcome = this.keymap.handleKey(desc);
    if (!outcome.handled) return; // flows to xterm.js untouched

    ev.preventDefault();
    ev.stopPropagation();

    if ("sendPrefix" in outcome) {
      this.focusedPane()?.sendBytes(this.keymap.prefixBytes());
      return;
    }
    if (outcome.action) dispatchAction(outcome.action, this);
  };

  private focusedPane(): Pane | undefined {
    return this.layout.focused
      ? this.panes.get(this.layout.focused)
      : undefined;
  }

  private render(): void {
    renderLayout(this.layout, {
      container: this.container,
      panes: this.panes,
      onRatioChange: (path, ratio) => this.onRatioChange(path, ratio),
    });
  }

  private refitAll(): void {
    for (const pane of this.panes.values()) pane.refit();
  }

  private onRatioChange(path: NodePath, ratio: number): void {
    this.layout = setRatio(this.layout, path, ratio);
    this.render();
  }

  private commit(layout: Layout): void {
    this.layout = layout;
    this.render();
    this.focusedPane()?.focus();
  }

  // --- AppActions --------------------------------------------------------

  split(dir: "down" | "right"): void {
    void this.newPane().then((pane) => {
      const id = pane.id;
      if (!id) return;
      this.commit(splitFocused(this.layout, dir, id));
    });
  }

  focus(dir: Direction): void {
    this.commit(focusDirection(this.layout, dir));
  }

  focusNext(): void {
    this.commit(focusNext(this.layout));
  }

  move(dir: Direction): void {
    this.commit(movePane(this.layout, dir));
  }

  toggleZoom(): void {
    this.commit(toggleZoom(this.layout));
  }

  resize(dir: Direction): void {
    this.commit(resizeFocused(this.layout, dir, this.config.resizeStep));
  }

  closePane(): void {
    const target = this.layout.focused;
    if (!target) return;
    if (!window.confirm("Close this pane?")) return;
    this.removePane(target);
  }

  /**
   * A pane's shell exited on its own (e.g. the user ran `exit`). Close it with
   * no confirmation. Ignore unknown ids — the pane may already be gone because
   * the user closed it manually (which itself triggers a `pty-exit`).
   */
  private onPaneExited(id: string): void {
    if (!this.panes.has(id)) return;
    this.removePane(id);
  }

  /**
   * Remove a pane by id: prune the layout tree, dispose its terminal, and — if
   * it was the last pane — close the window, since an empty terminal emulator
   * has nothing left to show.
   */
  private removePane(id: string): void {
    const { layout, removedPaneId } = closePane(this.layout, id);
    if (removedPaneId) {
      const pane = this.panes.get(removedPaneId);
      this.panes.delete(removedPaneId);
      void pane?.dispose();
    }
    this.commit(layout);

    if (!layout.root) void getCurrentWindow().close();
  }

  reloadConfig(): void {
    // Re-apply config-derived state. File reloading would hook in here; for now
    // we re-derive the keymap (picks up programmatic config changes).
    this.keymap.setConfig(this.keymapConfig());
    this.render();
  }

  /**
   * Win+O: open the native file picker and insert the chosen path into the
   * focused pane (shell-quoted, no trailing newline — the user can edit/run it).
   * The dialog runs in the backend; a cancel resolves to null and is a no-op.
   */
  openFileDialog(): void {
    const pane = this.focusedPane();
    if (!pane) return;
    void pickFile()
      .then((path) => {
        if (!path) return;
        pane.focus();
        pane.sendText(shellQuote(path));
      })
      .catch(() => {
        // Dialog failed/unavailable; nothing to insert.
      });
  }

  zoomIn(): void {
    this.applyFontSize(this.fontSize + 1);
  }

  zoomOut(): void {
    this.applyFontSize(this.fontSize - 1);
  }

  zoomReset(): void {
    this.applyFontSize(this.config.fontSize);
  }

  /** Clamp and apply a new font size to every live pane. */
  private applyFontSize(px: number): void {
    const next = Math.max(FONT_MIN, Math.min(FONT_MAX, Math.round(px)));
    if (next === this.fontSize) return;
    this.fontSize = next;
    for (const pane of this.panes.values()) pane.setFontSize(next);
  }
}

/**
 * POSIX-quote a path for insertion at the shell prompt. Strings made only of
 * safe characters are inserted verbatim; anything with a space, backslash
 * (Windows paths) or shell metacharacter is wrapped in single quotes with any
 * embedded single quote escaped, so the path survives intact.
 */
function shellQuote(path: string): string {
  if (path === "") return "''";
  if (/^[A-Za-z0-9_@%+=:,./-]+$/.test(path)) return path;
  return "'" + path.replace(/'/g, "'\\''") + "'";
}

async function bootstrap(): Promise<void> {
  const container = document.getElementById("app");
  if (!container) throw new Error("missing #app container");
  const config = defaultConfig();
  document.documentElement.style.setProperty(
    "--font-family",
    config.fontFamily,
  );
  const app = new App(container, config);
  await app.start();
}

void bootstrap();
