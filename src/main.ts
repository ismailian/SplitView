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
import "./styles/settings.css";

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
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import {
  onPtyExit,
  onReattachPane,
  pickFile,
  stashHandoff,
  takeHandoff,
} from "./ipc/commands";
import { renderLayout } from "./layout/render";
import { Pane } from "./terminal/Pane";
import { DetachedApp } from "./terminal/DetachedApp";
import { shellQuote } from "./shellQuote";
import {
  Keymap,
  DEFAULT_KEYMAP,
  type KeyEvent,
  type KeymapConfig,
} from "./keymap/keymap";
import {
  dispatchAction,
  ACTION_INFO,
  type AppActions,
} from "./keymap/actions";
import { defaultConfig, type Config } from "./config/config";
import {
  loadSettings,
  saveSettings,
  applyDisplay,
  type Settings,
} from "./settings/settings";
import { SettingsWindow } from "./settings/SettingsWindow";

/** Font-size bounds for the zoom-in/out actions, in px. */
const FONT_MIN = 6;
const FONT_MAX = 48;

class App implements AppActions {
  private layout: Layout = emptyLayout();
  private readonly panes = new Map<string, Pane>();
  private config: Config;
  private settings: Settings;
  private keymap: Keymap;
  private readonly settingsWindow: SettingsWindow;
  private nextSeq = 0;
  /** Detached windows we spawned, keyed by pane id → window label, so we can
   * forget one on reattach and close the rest when the main window exits. */
  private readonly detachedWindows = new Map<string, string>();
  /** Current terminal font size (zoom level); starts at the configured size. */
  private fontSize: number;

  constructor(
    private readonly container: HTMLElement,
    config: Config,
    settings: Settings,
  ) {
    this.config = config;
    this.settings = settings;
    this.fontSize = settings.display.fontSize;
    this.keymap = new Keymap(this.keymapConfig());
    this.settingsWindow = new SettingsWindow({
      getSettings: () => this.settings,
      onChange: (next) => this.onSettingsChanged(next),
      keymap: {
        prefix: this.config.prefix || DEFAULT_KEYMAP.prefix,
        prefixBindings: this.keymapConfig().prefixBindings,
        globalBindings: DEFAULT_KEYMAP.globalBindings ?? {},
      },
      actionInfo: ACTION_INFO,
    });
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

    // A detached window asking to send its pane back into our layout.
    void onReattachPane((id) => this.onReattach(id)).catch(() => {});

    // If the main window closes while detached windows are still open, close
    // them too so their shells don't linger with no window (CLAUDE.md §20).
    void getCurrentWindow()
      .onCloseRequested(() => this.closeDetachedWindows())
      .catch(() => {});

    const first = await this.newPane();
    this.layout = singleLayout(first.id ?? this.placeholderId());
    this.render();
    first.focus();
  }

  private placeholderId(): string {
    return `pane-local-${this.nextSeq++}`;
  }

  private async newPane(cwd?: string): Promise<Pane> {
    const pane = await Pane.create(this.effectiveConfig(), {
      cwd,
      aliases: this.settings.aliases,
      banner: this.settings.banner,
      promptInfo: this.settings.promptInfo,
    });
    // Keep a newly split pane at the current zoom level, not the base size.
    if (this.fontSize !== this.settings.display.fontSize)
      pane.setFontSize(this.fontSize);
    const id = pane.id;
    if (id) this.panes.set(id, pane);
    // Mouse clicks focus a pane's terminal directly; mirror that into the layout
    // so the next split/move/zoom acts on the pane the user is actually in.
    pane.onFocus = () => this.onPaneFocused(id);
    // The injected `splitview` shell command reaches us as an app command.
    pane.onAppCommand = (cmd) => this.onAppCommand(cmd);
    return pane;
  }

  /** Base config with the live Display font folded in (CLAUDE.md §12). */
  private effectiveConfig(): Config {
    return {
      ...this.config,
      fontFamily: this.settings.display.fontFamily,
      fontSize: this.settings.display.fontSize,
    };
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
    // While Settings is open, the keymap must not fire — keys belong to the
    // window's inputs. Escape closes it and returns focus to the terminal.
    if (this.settingsWindow.isOpen) {
      if (ev.key === "Escape") {
        ev.preventDefault();
        this.settingsWindow.close();
        this.focusedPane()?.focus();
      }
      return;
    }

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
    // Inherit the focused pane's working directory so the new shell opens where
    // the user is (reported by the shell via OSC 7; see Pane).
    const cwd = this.focusedPane()?.cwd ?? undefined;
    void this.newPane(cwd).then((pane) => {
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
   * Pop the focused pane out into its own window. The PTY keeps running in the
   * backend; we serialize the pane's buffer, hand it off, open a `detached-*`
   * window that adopts the live stream (see `DetachedApp`), then drop the pane
   * from our layout — but only after the window is created, so a failure can't
   * strand the PTY. Requires ≥2 panes: popping the only pane would empty (and
   * close) the main window.
   */
  detach(): void {
    const id = this.layout.focused;
    const pane = id ? this.panes.get(id) : undefined;
    if (!id || !pane || this.panes.size < 2) return;

    const serialized = pane.serialize();
    void (async () => {
      try {
        await stashHandoff(id, serialized);
        await this.openDetachedWindow(id);
        // The new window has adopted the stream: relinquish the pane locally
        // (keep the PTY alive via detachDispose, not dispose).
        const { layout } = closePane(this.layout, id);
        this.panes.delete(id);
        pane.detachDispose();
        this.commit(layout);
      } catch {
        // Detach failed before the window took over; leave the pane in place.
      }
    })();
  }

  /** No-op in the main window — reattach is initiated *by* detached windows and
   * arrives here as the `reattach-pane` event (see `onReattach`). */
  reattach(): void {}

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

  /**
   * A detached window asked to send its pane back. Adopt the existing PTY into a
   * fresh terminal (restoring the scrollback it stashed), then splice it into
   * the layout beside the focused pane. Ignore ids we somehow already own.
   */
  private async onReattach(id: string): Promise<void> {
    if (this.panes.has(id)) return;
    this.detachedWindows.delete(id);

    const serialized = (await takeHandoff(id)) ?? "";
    const pane = await Pane.attach(this.effectiveConfig(), id, serialized);
    if (this.fontSize !== this.settings.display.fontSize)
      pane.setFontSize(this.fontSize);
    pane.onFocus = () => this.onPaneFocused(id);
    pane.onAppCommand = (cmd) => this.onAppCommand(cmd);
    this.panes.set(id, pane);

    // Splice beside the focused pane, inheriting focus (or seed an empty layout).
    const layout = this.layout.root
      ? splitFocused(this.layout, "right", id)
      : singleLayout(id);
    this.commit(layout);
  }

  /**
   * Create the detached window for pane `id`. Resolves once the OS window exists
   * (or rejects on failure) so `detach` only relinquishes the pane on success.
   * The label carries a sequence so re-detaching a reattached pane can't collide
   * with a stale label.
   */
  private openDetachedWindow(id: string): Promise<void> {
    // Labels must be unique for the app's lifetime, and a reattached pane keeps
    // its id — so tag the label with a fresh sequence to avoid colliding with a
    // stale (closed) window's label.
    const label = `detached-${id}-${this.nextSeq++}`;
    return new Promise((resolve, reject) => {
      const win = new WebviewWindow(label, {
        url: `index.html?pane=${encodeURIComponent(id)}`,
        title: `SplitView — ${id}`,
        width: 720,
        height: 480,
        minWidth: 320,
        minHeight: 200,
      });
      win.once("tauri://created", () => {
        this.detachedWindows.set(id, label);
        resolve();
      });
      win.once("tauri://error", (e) => reject(new Error(String(e.payload))));
    });
  }

  /** Close every detached window we spawned (main window is closing). */
  private closeDetachedWindows(): void {
    for (const label of this.detachedWindows.values()) {
      void WebviewWindow.getByLabel(label).then((win) => win?.close());
    }
    this.detachedWindows.clear();
  }

  reloadConfig(): void {
    // Re-apply config-derived state. File reloading would hook in here; for now
    // we re-derive the keymap (picks up programmatic config changes).
    this.keymap.setConfig(this.keymapConfig());
    this.render();
  }

  /** Open the Settings window (<prefix> , or `splitview settings`). */
  openSettings(): void {
    this.settingsWindow.open();
  }

  /**
   * Handle an app command from the terminal: the injected `splitview` shell
   * function emits a private OSC sequence carrying its arguments. Only the first
   * word is significant; unknown commands are ignored.
   */
  private onAppCommand(cmd: string): void {
    const word = cmd.trim().toLowerCase().split(/\s+/)[0];
    if (word === "settings" || word === "config") this.settingsWindow.open();
    else if (word === "reload") this.reloadConfig();
  }

  /**
   * Persist edited settings and apply the display changes live: update the CSS
   * variables, then re-theme every pane (CLAUDE.md §14). Alias/banner/prompt-info
   * changes take effect on the next new terminal.
   */
  private onSettingsChanged(next: Settings): void {
    this.settings = next;
    saveSettings(next);
    applyDisplay(next.display);
    this.fontSize = next.display.fontSize;
    for (const pane of this.panes.values())
      pane.applyDisplay(next.display.fontFamily, next.display.fontSize);
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

async function bootstrap(): Promise<void> {
  const container = document.getElementById("app");
  if (!container) throw new Error("missing #app container");
  const config = defaultConfig();
  const settings = loadSettings();
  // Apply the saved display settings (colours, font, divider) before the first
  // pane mounts so it renders with the right theme from the start.
  applyDisplay(settings.display);

  // A `?pane=<id>` query marks a detached window: it hosts a single popped-out
  // pane (adopted from the backend) rather than the full split layout.
  const detachedId = new URLSearchParams(location.search).get("pane");
  if (detachedId) {
    await new DetachedApp(container, config, settings, detachedId).start();
  } else {
    await new App(container, config, settings).start();
  }
}

void bootstrap();
