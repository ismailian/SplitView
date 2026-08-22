/**
 * Controller for a **detached** window: a single popped-out pane (CLAUDE.md §1).
 *
 * A detached window hosts exactly one terminal, adopted from an existing backend
 * PTY (the main window keeps the process alive and hands off the stream). There
 * is no split layout here — the whole point is a focused, single-terminal
 * window — so the layout-mutating actions are no-ops. `prefix d` reattaches the
 * pane to the main window; closing the window (X or `prefix x`) kills the shell.
 */

import "@xterm/xterm/css/xterm.css";
import "../styles/theme.css";
import "../styles/settings.css";

import { getCurrentWindow } from "@tauri-apps/api/window";
import type { Direction } from "../layout/tree";
import {
  closePty,
  onPtyExit,
  pickFile,
  requestReattach,
  stashHandoff,
  takeHandoff,
} from "../ipc/commands";
import { destroyCurrentWindow, openNewWindow } from "../ipc/windows";
import { Pane } from "./Pane";
import {
  Keymap,
  DEFAULT_KEYMAP,
  type KeyEvent,
  type KeymapConfig,
} from "../keymap/keymap";
import {
  dispatchAction,
  ACTION_INFO,
  type AppActions,
} from "../keymap/actions";
import type { Config } from "../config/config";
import {
  saveSettings,
  applyDisplay,
  type Settings,
} from "../settings/settings";
import { SettingsWindow } from "../settings/SettingsWindow";
import { shellQuote } from "../shellQuote";

/** Font-size bounds for the zoom-in/out actions, in px (mirrors `main.ts`). */
const FONT_MIN = 6;
const FONT_MAX = 48;

export class DetachedApp implements AppActions {
  private pane: Pane | null = null;
  private config: Config;
  private settings: Settings;
  private keymap: Keymap;
  private readonly settingsWindow: SettingsWindow;
  private fontSize: number;
  /** True while handing the pane back to main, so the close handler keeps the
   * PTY alive instead of killing it. */
  private reattaching = false;
  /** True once we've begun tearing the window down, so the close handler's
   * second pass (after killing the PTY) lets the window actually close. */
  private closing = false;

  constructor(
    private readonly container: HTMLElement,
    config: Config,
    settings: Settings,
    private readonly paneId: string,
    /** Label of the layout window this pane came from; `reattach` sends it back
     * there rather than to whichever window happens to be listening. */
    private readonly owner: string | null = null,
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

  /** Defaults, with the user's prefix and overrides — but `d` reattaches here. */
  private keymapConfig(): KeymapConfig {
    return {
      ...DEFAULT_KEYMAP,
      prefix: this.config.prefix || DEFAULT_KEYMAP.prefix,
      prefixBindings: {
        ...DEFAULT_KEYMAP.prefixBindings,
        ...this.config.keybindings,
        // In a detached window, prefix-d sends the pane back to the main window.
        d: "reattach_pane",
      },
    };
  }

  async start(): Promise<void> {
    document.addEventListener("keydown", this.onKeyDown, true);
    window.addEventListener("resize", () => this.pane?.refit());

    // If this pane's shell exits on its own (`exit`), close the window.
    void onPtyExit((id) => {
      if (id === this.paneId) void this.destroyWindow();
    }).catch(() => {});

    // The OS close button (or the main window closing us) kills the PTY — unless
    // we're mid-reattach, which hands the shell back alive (CLAUDE.md §20).
    void getCurrentWindow()
      .onCloseRequested(async (event) => {
        if (this.reattaching || this.closing) return; // let the close proceed
        event.preventDefault();
        this.closing = true;
        await closePty(this.paneId).catch(() => {});
        await this.destroyWindow();
      })
      .catch(() => {});

    // Adopt the existing PTY, restoring the scrollback the main window stashed.
    const serialized = (await takeHandoff(this.paneId)) ?? "";
    const pane = await Pane.attach(this.effectiveConfig(), this.paneId, serialized);
    pane.onAppCommand = (cmd) => this.onAppCommand(cmd);
    if (this.fontSize !== this.settings.display.fontSize)
      pane.setFontSize(this.fontSize);
    this.pane = pane;

    // Fill the window (there is only ever one pane; no dividers).
    const s = pane.el.style;
    s.position = "absolute";
    s.left = "0";
    s.top = "0";
    s.width = "100%";
    s.height = "100%";
    pane.el.classList.add("focused");
    this.container.appendChild(pane.el);
    pane.refit();
    pane.focus();
  }

  // --- key handling (mirrors the main controller) ------------------------

  private onKeyDown = (ev: KeyboardEvent): void => {
    if (this.settingsWindow.isOpen) {
      if (ev.key === "Escape") {
        ev.preventDefault();
        this.settingsWindow.close();
        this.pane?.focus();
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
    if (!outcome.handled) return;

    ev.preventDefault();
    ev.stopPropagation();

    if ("sendPrefix" in outcome) {
      this.pane?.sendBytes(this.keymap.prefixBytes());
      return;
    }
    if (outcome.action) dispatchAction(outcome.action, this);
  };

  private effectiveConfig(): Config {
    return {
      ...this.config,
      fontFamily: this.settings.display.fontFamily,
      fontSize: this.settings.display.fontSize,
    };
  }

  // --- AppActions --------------------------------------------------------
  // A detached window is single-pane, so the layout actions do nothing.

  split(_dir: "down" | "right"): void {}
  focus(_dir: Direction): void {}
  focusNext(): void {}
  move(_dir: Direction): void {}
  toggleZoom(): void {}
  resize(_dir: Direction): void {}
  detach(): void {}

  // Backgrounding and the file sidebar belong to a layout window: this window is
  // one pane with nowhere to hide it and no layout to shrink. Reattach it first
  // (`prefix d`) to use those.
  toggleSidebar(): void {}
  backgroundPane(): void {}
  showBackground(): void {}

  /** Open a fresh layout window, starting in this terminal's directory. */
  newWindow(): void {
    void openNewWindow(this.pane?.cwd).catch(() => {});
  }

  /** Send this pane back into the main window's layout, then close this window
   * without killing the PTY (the main window adopts it). */
  reattach(): void {
    const pane = this.pane;
    if (!pane || this.reattaching) return;
    this.reattaching = true;
    const serialized = pane.serialize();
    void (async () => {
      try {
        await stashHandoff(this.paneId, serialized);
        await requestReattach(this.paneId, this.owner);
        pane.detachDispose(); // drop the terminal, keep the PTY alive
        await this.destroyWindow();
      } catch {
        this.reattaching = false; // failed; stay put
      }
    })();
  }

  /** Close the terminal (kills the shell). Routed through the window close so
   * the same PTY-killing path runs as the OS close button. */
  closePane(): void {
    if (!window.confirm("Close this terminal?")) return;
    void getCurrentWindow().close();
  }

  reloadConfig(): void {
    this.keymap.setConfig(this.keymapConfig());
  }

  openSettings(): void {
    this.settingsWindow.open();
  }

  openFileDialog(): void {
    const pane = this.pane;
    if (!pane) return;
    void pickFile()
      .then((path) => {
        if (!path) return;
        pane.focus();
        pane.sendText(shellQuote(path));
      })
      .catch(() => {});
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

  private applyFontSize(px: number): void {
    const next = Math.max(FONT_MIN, Math.min(FONT_MAX, Math.round(px)));
    if (next === this.fontSize) return;
    this.fontSize = next;
    this.pane?.setFontSize(next);
  }

  private onAppCommand(cmd: string): void {
    const word = cmd.trim().toLowerCase().split(/\s+/)[0];
    if (word === "settings" || word === "config") this.settingsWindow.open();
    else if (word === "reload") this.reloadConfig();
    else if (word === "window" || word === "new") this.newWindow();
  }

  private onSettingsChanged(next: Settings): void {
    this.settings = next;
    saveSettings(next);
    applyDisplay(next.display);
    this.fontSize = next.display.fontSize;
    this.pane?.applyDisplay(next.display.fontFamily, next.display.fontSize);
  }

  /** Close this window for real. `destroy`, not `close`: a close request only
   * re-enters our own close handler, and the window would never go away
   * (CLAUDE.md §20 — see `destroyCurrentWindow`). */
  private async destroyWindow(): Promise<void> {
    this.closing = true;
    await destroyCurrentWindow();
  }
}
