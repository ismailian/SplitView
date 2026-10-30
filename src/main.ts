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
import "./styles/sidebar.css";
import "./styles/background.css";
import "./styles/remote.css";

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
  leaves,
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
import { destroyCurrentWindow, openNewWindow } from "./ipc/windows";
import { renderLayout } from "./layout/render";
import { Pane } from "./terminal/Pane";
import { DetachedApp } from "./terminal/DetachedApp";
import { shellQuote } from "./shellQuote";
import { baseName } from "./paths";
import { FileTree } from "./sidebar/FileTree";
import {
  BackgroundPanel,
  type BackgroundEntry,
} from "./background/BackgroundPanel";
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
  remoteConfigOf,
  type Settings,
} from "./settings/settings";
import { SettingsWindow } from "./settings/SettingsWindow";
import { RemoteBridge } from "./remote/RemoteBridge";
import { buildRoster } from "./remote/roster";
import { remoteEnable, type RemotePaneInfo } from "./ipc/commands";

/** Font-size bounds for the zoom-in/out actions, in px. */
const FONT_MIN = 6;
const FONT_MAX = 48;

/** A pane pushed out of the layout with its shell still running (<prefix> b). */
interface BackgroundedPane {
  pane: Pane;
  /** When it was backgrounded, for the picker's age column. */
  since: number;
}

class App implements AppActions {
  private layout: Layout = emptyLayout();
  private readonly panes = new Map<string, Pane>();
  private config: Config;
  private settings: Settings;
  private keymap: Keymap;
  private readonly settingsWindow: SettingsWindow;
  private readonly fileTree: FileTree;
  private readonly backgroundPanel: BackgroundPanel;
  /** Publishes this window's panes to remote clients and relays their actions;
   * inert while remote control is off (CLAUDE.md §21). */
  private readonly remote: RemoteBridge;
  /** Panes running outside the layout, oldest first (insertion order). */
  private readonly backgrounded = new Map<string, BackgroundedPane>();
  private nextSeq = 0;
  /** Detached windows we spawned, keyed by pane id → window label, so we can
   * forget one on reattach and close the rest when this window exits. */
  private readonly detachedWindows = new Map<string, string>();
  /** This window's Tauri label, so reattach requests can be addressed to us. */
  private readonly windowLabel = getCurrentWindow().label;
  /** The pane area; a sibling of the sidebar, and what the renderer positions
   * panes inside (percentages are relative to it, not the whole window). */
  private readonly container: HTMLElement;
  /** Current terminal font size (zoom level); starts at the configured size. */
  private fontSize: number;
  /** True once this window is tearing down, so the close handler's second pass
   * (after the backgrounded shells are killed) lets it actually close. */
  private closing = false;

  constructor(
    root: HTMLElement,
    config: Config,
    settings: Settings,
    /** Directory for this window's first shell (a new window inherits the cwd
     * of the pane it was opened from). */
    private readonly initialCwd?: string,
  ) {
    this.config = config;
    this.settings = settings;
    this.fontSize = settings.display.fontSize;
    this.keymap = new Keymap(this.keymapConfig());

    this.container = document.createElement("div");
    this.container.className = "layout-root";
    this.fileTree = new FileTree({
      // A path is inserted without a newline so the user can edit or prepend a
      // command; `cd` is sent with one, because there is nothing to edit.
      onInsertPath: (path) => this.insertIntoTerminal(shellQuote(path)),
      onChangeDir: (path) => this.insertIntoTerminal(`cd ${shellQuote(path)}\r`),
      onLeave: () => this.focusedPane()?.focus(),
      // Resizing the sidebar resizes the pane area with it.
      onResize: () => this.refitAll(),
    });
    this.backgroundPanel = new BackgroundPanel({
      list: () => this.backgroundEntries(),
      restore: (id) => this.restoreBackgrounded(id),
      kill: (id) => this.killBackgrounded(id),
      onClose: () => this.focusedPane()?.focus(),
    });
    this.remote = new RemoteBridge({
      windowLabel: this.windowLabel,
      panes: () => this.remotePanes(),
      focusedPaneId: () => this.layout.focused,
      snapshot: (id) => this.remoteSnapshot(id),
      createPane: (cwd) => this.splitInto("right", cwd),
      closePane: (id) => this.closePaneConfirmed(id),
      onIndicatorClick: () => this.settingsWindow.open("remote"),
      onPromptClosed: () => this.focusedPane()?.focus(),
    });

    // Sidebar to the left of the pane area; the indicators float over both.
    root.append(
      this.fileTree.el,
      this.container,
      this.backgroundPanel.indicator,
      this.remote.indicator,
    );

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

    // A detached window asking to send its pane back into our layout. The event
    // reaches every window, so ignore requests addressed to a different one.
    void onReattachPane((id, owner) => {
      if (owner && owner !== this.windowLabel) return;
      void this.onReattach(id);
    }).catch(() => {});

    // If this window closes while detached windows or backgrounded shells are
    // still alive, take them with it — nothing should linger with no window and
    // no way to reach it (CLAUDE.md §20). The teardown is deferred until the
    // kills are actually issued, because a window that vanishes first leaves
    // orphaned shells behind; `closing` makes a second close request (an
    // impatient X while the kills are in flight) a no-op rather than a re-run.
    void getCurrentWindow()
      .onCloseRequested(async (event) => {
        if (this.closing) return;
        event.preventDefault();
        this.closing = true;
        this.closeDetachedWindows();
        await this.killAllBackgrounded();
        // `destroy`, not `close`: a close request would land right back in this
        // handler and the window would never actually go away.
        await destroyCurrentWindow();
      })
      .catch(() => {});

    // Remote control: pick up the current status and start listening. A
    // failure here must not stop the window from being a terminal.
    void this.remote
      .start()
      .then(() => this.autoEnableRemote())
      .catch(() => {});

    const first = await this.newPane(this.initialCwd);
    this.layout = singleLayout(first.id ?? this.placeholderId());
    this.render();
    first.focus();
  }

  /**
   * Start the remote server at launch, if the user asked for that.
   *
   * Only the main window does it: enabling rebinds the socket, so a second
   * window doing the same would restart a server that is already running and
   * drop whoever was connected.
   */
  private autoEnableRemote(): void {
    if (this.windowLabel !== "main") return;
    if (!this.settings.remote.enableAtStartup || this.remote.isEnabled) return;
    void remoteEnable(remoteConfigOf(this.settings.remote)).catch(() => {
      // The port is taken or the identity could not be written; Settings shows
      // the real reason when the user goes looking.
    });
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
    this.wirePane(pane, id);
    return pane;
  }

  /** Connect a pane's callbacks to this controller (new and reattached panes). */
  private wirePane(pane: Pane, id: string | null): void {
    // Mouse clicks focus a pane's terminal directly; mirror that into the layout
    // so the next split/move/zoom acts on the pane the user is actually in.
    pane.onFocus = () => this.onPaneFocused(id);
    // The injected `splitview` shell command reaches us as an app command.
    pane.onAppCommand = (cmd) => this.onAppCommand(cmd);
    // The sidebar follows the focused shell into whatever directory it `cd`s to.
    pane.onCwdChange = () => {
      if (id !== null && id === this.layout.focused) this.syncSidebar();
    };
  }

  /** Point the file sidebar at the focused pane: its own view, its directory.
   * Cheap when neither changed, so it runs after every focus change. */
  private syncSidebar(): void {
    this.fileTree.follow(this.layout.focused, this.focusedPane()?.cwd ?? null);
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
    this.syncSidebar();
  }

  // --- key handling ------------------------------------------------------

  private onKeyDown = (ev: KeyboardEvent): void => {
    // The pairing prompt outranks everything: a keystroke meant for it must
    // never reach a terminal a stranger is asking to watch.
    if (this.remote.isPromptOpen) {
      if (this.remote.handleKey(ev)) {
        ev.preventDefault();
        ev.stopPropagation();
      }
      return;
    }

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

    // The backgrounded-panes picker is modal: it consumes every key, so none
    // leak into the terminal underneath.
    if (this.backgroundPanel.isOpen) {
      ev.preventDefault();
      ev.stopPropagation();
      this.backgroundPanel.handleKey(ev);
      return;
    }

    // The sidebar gets first refusal while it holds focus, but only for the keys
    // it uses — anything else falls through to the keymap, so the prefix (and
    // every action) still works from inside the tree (CLAUDE.md §9). Not while a
    // prefix is pending, though: that keystroke is a SplitView command, and the
    // tree must not eat it.
    if (
      !this.keymap.isInPrefix &&
      this.fileTree.isFocused &&
      this.fileTree.handleKey(ev)
    ) {
      ev.preventDefault();
      ev.stopPropagation();
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
    // Debounced, and a no-op while remote control is off.
    this.remote.publish();
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
    this.syncSidebar();
  }

  // --- AppActions --------------------------------------------------------

  split(dir: "down" | "right"): void {
    // Inherit the focused pane's working directory so the new shell opens where
    // the user is (reported by the shell via OSC 7; see Pane).
    this.splitInto(dir, this.focusedPane()?.cwd ?? undefined);
  }

  /**
   * Split, opening the new shell in `cwd` (falling back to the focused pane's
   * directory). The remote's `create_pane` lands here with the directory the
   * phone was looking at, which is the split it would have made itself.
   */
  private splitInto(dir: "down" | "right", cwd?: string): void {
    const where = cwd ?? this.focusedPane()?.cwd ?? undefined;
    void this.newPane(where).then((pane) => {
      const id = pane.id;
      if (!id) return;
      this.commit(
        this.layout.root ? splitFocused(this.layout, dir, id) : singleLayout(id),
      );
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
    this.closePaneConfirmed(target);
  }

  /**
   * Close a pane with no confirmation. The keymap always goes through
   * `closePane`, which asks first; this is the entry point for a caller that
   * has already confirmed — today, a remote client, which cannot be allowed to
   * pop a modal on a laptop nobody is sitting at.
   */
  closePaneConfirmed(paneId: string): void {
    if (!this.panes.has(paneId) && !this.backgrounded.has(paneId)) return;
    if (this.backgrounded.has(paneId)) {
      this.killBackgrounded(paneId);
      return;
    }
    this.removePane(paneId);
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
        this.fileTree.forget(id);
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
   * Open another top-level SplitView window, with its own layout, keymap and
   * first shell — opened in the focused pane's directory, the same inheritance
   * a split gets. Windows are independent: closing this one does not close them.
   */
  newWindow(): void {
    void openNewWindow(this.focusedPane()?.cwd).catch(() => {
      // The OS refused the window; nothing to clean up, the layout is untouched.
    });
  }

  /**
   * Cycle the file sidebar: closed → open (on the focused pane's view, rooted at
   * its terminal's directory, reported over OSC 7) → focused-from-the-terminal →
   * closed. The middle step matters because the sidebar loses focus as soon as
   * you insert a path or `cd`, and the same key should take you back to the tree
   * rather than closing it. Once open, the tree follows focus and `cd` by itself
   * (`syncSidebar`). Opening/closing changes the pane area's width, so the
   * terminals are refit (CLAUDE.md §20 — xterm and the PTY must resize together).
   */
  toggleSidebar(): void {
    if (!this.fileTree.isOpen) {
      this.fileTree.open(this.layout.focused, this.focusedPane()?.cwd ?? null);
    } else if (!this.fileTree.isFocused) {
      this.fileTree.focus(); // already open, just go there — no resize
      return;
    } else {
      this.fileTree.close();
      this.focusedPane()?.focus();
    }
    this.refitAll();
  }

  /**
   * Push the focused pane out of the layout **without killing it**: the PTY, the
   * terminal and its scrollback stay alive, hidden, until it is restored from the
   * picker (`<prefix> B`). This is the "keep the dev server running, give me back
   * the screen" case. Requires ≥2 panes — backgrounding the only pane would empty
   * the window.
   */
  backgroundPane(): void {
    const id = this.layout.focused;
    const pane = id ? this.panes.get(id) : undefined;
    if (!id || !pane || this.panes.size < 2) return;

    const { layout } = closePane(this.layout, id);
    this.panes.delete(id);
    this.backgrounded.set(id, { pane, since: Date.now() });
    // Hidden, but still mounted: the terminal keeps consuming its PTY stream, so
    // nothing is lost while it is out of sight.
    pane.el.style.display = "none";
    pane.el.classList.remove("focused");
    this.backgroundPanel.update();
    this.commit(layout);
  }

  /** Open the picker listing backgrounded panes. */
  showBackground(): void {
    this.backgroundPanel.open();
  }

  /** Snapshot for the picker: newest last, named and previewed live. */
  private backgroundEntries(): BackgroundEntry[] {
    return [...this.backgrounded].map(([id, { pane, since }]) => ({
      id,
      title: pane.title ?? (pane.cwd ? baseName(pane.cwd) : id),
      preview: pane.previewLine(),
      since,
    }));
  }

  /**
   * Bring a backgrounded pane back beside the focused one. Zoom is cleared first
   * — restoring into a zoomed layout would put the pane somewhere invisible.
   */
  private restoreBackgrounded(id: string): void {
    const entry = this.backgrounded.get(id);
    if (!entry) return;
    this.backgrounded.delete(id);
    this.panes.set(id, entry.pane);
    entry.pane.el.style.display = "block";
    this.backgroundPanel.update();

    const base = this.layout.zoomed ? { ...this.layout, zoomed: null } : this.layout;
    this.commit(
      base.root ? splitFocused(base, "right", id) : singleLayout(id),
    );
  }

  // --- remote control ----------------------------------------------------

  /**
   * This window's panes for the remote roster: layout order first (what the
   * phone tabs through), then backgrounded ones, which are still live
   * terminals and still worth reaching from a phone.
   */
  private remotePanes(): RemotePaneInfo[] {
    const backgrounded = new Map<string, Pane>();
    for (const [id, entry] of this.backgrounded) backgrounded.set(id, entry.pane);
    return buildRoster(leaves(this.layout.root), this.panes, backgrounded);
  }

  /** A pane's current screen, for a client that has just attached. */
  private remoteSnapshot(
    paneId: string,
  ): { serialized: string; cols: number; rows: number } | null {
    const pane = this.panes.get(paneId) ?? this.backgrounded.get(paneId)?.pane;
    if (!pane) return null;
    return {
      serialized: pane.serialize(),
      cols: pane.term.cols,
      rows: pane.term.rows,
    };
  }

  /** Kill a backgrounded pane's shell for good (picker `x`). */
  private killBackgrounded(id: string): void {
    const entry = this.backgrounded.get(id);
    if (!entry) return;
    this.backgrounded.delete(id);
    this.fileTree.forget(id);
    void entry.pane.dispose();
    this.backgroundPanel.update();
    this.remote.publish();
  }

  /** Kill every backgrounded shell (this window is going away). Resolves once
   * each kill has been issued, so the window can close without orphaning them. */
  private async killAllBackgrounded(): Promise<void> {
    const kills = [...this.backgrounded.values()].map(({ pane }) =>
      pane.dispose().catch(() => {}),
    );
    this.backgrounded.clear();
    this.backgroundPanel.update();
    await Promise.all(kills);
  }

  /**
   * A pane's shell exited on its own (e.g. the user ran `exit`). Close it with
   * no confirmation. Ignore unknown ids — the pane may already be gone because
   * the user closed it manually (which itself triggers a `pty-exit`).
   */
  private onPaneExited(id: string): void {
    // A backgrounded shell can exit too (the dev server crashed): drop it from
    // the list rather than leaving a dead entry in the picker.
    if (this.backgrounded.has(id)) {
      this.killBackgrounded(id);
      return;
    }
    if (!this.panes.has(id)) return;
    this.removePane(id);
  }

  /**
   * Remove a pane by id: prune the layout tree, dispose its terminal, and — if
   * it was the last pane — close the window, since an empty terminal emulator
   * has nothing left to show.
   *
   * Unless something is backgrounded: closing the window would silently kill
   * those shells, so the most recently backgrounded pane is restored into the
   * empty layout instead, putting the user back in front of what is still
   * running.
   */
  private removePane(id: string): void {
    const { layout, removedPaneId } = closePane(this.layout, id);
    if (removedPaneId) {
      const pane = this.panes.get(removedPaneId);
      this.panes.delete(removedPaneId);
      this.fileTree.forget(removedPaneId);
      void pane?.dispose();
    }
    this.commit(layout);
    if (layout.root) return;

    const newest = [...this.backgrounded.keys()].pop();
    if (newest) this.restoreBackgrounded(newest);
    else void getCurrentWindow().close();
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
    this.wirePane(pane, id);
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
        // `owner` tells the detached window which layout window to send the pane
        // back to — there can be several (<prefix> n).
        url: `index.html?pane=${encodeURIComponent(id)}&owner=${encodeURIComponent(this.windowLabel)}`,
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
    else if (word === "files" || word === "tree") this.toggleSidebar();
    else if (word === "window" || word === "new") this.newWindow();
    else if (word === "bg" || word === "background") this.showBackground();
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
    void pickFile()
      .then((path) => {
        if (path) this.insertIntoTerminal(shellQuote(path));
      })
      .catch(() => {
        // Dialog failed/unavailable; nothing to insert.
      });
  }

  /**
   * Type `text` into the focused terminal and give it focus — the shared path for
   * the file picker and the sidebar's insert/`cd`. The terminal takes focus
   * because the point of inserting text is to act on it.
   */
  private insertIntoTerminal(text: string): void {
    const pane = this.focusedPane();
    if (!pane) return;
    pane.focus();
    pane.sendText(text);
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

  // The query string decides which controller boots (see `ipc/windows.ts`):
  // `?pane=<id>` is a detached window hosting a single popped-out pane (with
  // `owner` naming the window to send it back to); otherwise this is a full
  // layout window, optionally opening its first shell in `?cwd=<dir>`.
  const params = new URLSearchParams(location.search);
  const detachedId = params.get("pane");
  if (detachedId) {
    const owner = params.get("owner");
    await new DetachedApp(container, config, settings, detachedId, owner).start();
  } else {
    await new App(container, config, settings, params.get("cwd") ?? undefined).start();
  }
}

void bootstrap();
