/**
 * The webview half of remote control (CLAUDE.md §21,
 * `docs/remote-control-desktop.md` §5.2–§5.4).
 *
 * The backend owns the socket, the crypto and the device registry, and knows
 * nothing about layout. This module supplies the three things only the webview
 * has, and nothing else:
 *
 *   - **the roster** — titles, working directories, preview lines and pane
 *     order, published (debounced) whenever the layout changes,
 *   - **snapshots** — a pane's serialized xterm.js buffer, so a client that has
 *     just attached sees the screen before the live stream makes sense,
 *   - **layout actions** — creating and closing panes, relayed here as events
 *     and executed through the same operations the keymap uses.
 *
 * It also owns the two pieces of UI the feature needs on the desktop: the
 * always-visible indicator while the server is enabled, and the approval prompt
 * that stands between a device with the right pairing code and a token.
 *
 * Everything here is inert while the feature is off — including the roster,
 * which must never exist when nobody asked for remote control.
 */

import {
  onRemoteAction,
  onRemotePairingRequest,
  onRemoteSnapshotRequest,
  onRemoteStatusChanged,
  provideSnapshot,
  publishPanes,
  remotePairingRespond,
  remoteStatus,
  type RemotePaneInfo,
  type RemotePairingRequestEvent,
  type RemoteStatus,
} from "../ipc/commands";
import type { UnlistenFn } from "@tauri-apps/api/event";

/** How long to wait for the layout to settle before publishing a roster. */
const PUBLISH_DEBOUNCE_MS = 150;

/**
 * While a client is connected, republish on this interval even if nothing
 * changed: `preview` and the backend's output counters are what a phone uses to
 * show activity on tabs it is not streaming.
 */
const LIVE_REFRESH_MS = 2000;

/** What the bridge needs from whichever controller owns the panes. */
export interface RemoteBridgeDeps {
  /** This window's Tauri label. */
  windowLabel: string;
  /**
   * Every pane this window owns, in the order the phone should tab through
   * them — including backgrounded ones, which are still live terminals.
   */
  panes(): RemotePaneInfo[];
  /** The focused pane, or null. */
  focusedPaneId(): string | null;
  /** A pane's serialized xterm.js buffer and its size, or null if it is gone. */
  snapshot(paneId: string): { serialized: string; cols: number; rows: number } | null;
  /**
   * Create a pane, optionally in `cwd`. Undefined on a controller with no
   * layout to split (a detached window), in which case the request is ignored
   * and the client's create times out with `no_window`.
   */
  createPane?(cwd?: string): void;
  /** Close a pane **without** a confirmation dialog — see `closePaneConfirmed`. */
  closePane(paneId: string): void;
  /**
   * Whether a remotely created pane may land in this window. False for a
   * detached window, which has one pane and no layout to split: it publishes a
   * `focusedAt` of 0 so a real layout window always wins the target.
   */
  eligibleAsTarget?: boolean;
  /** Clicking the indicator opens the Remote section of Settings. */
  onIndicatorClick?(): void;
  /** Focus is returned to the terminal when the approval prompt closes. */
  onPromptClosed?(): void;
}

export class RemoteBridge {
  /** Corner indicator; mounted always, visible only while the server is on. */
  readonly indicator: HTMLElement;

  private status: RemoteStatus | null = null;
  private publishTimer: number | null = null;
  private refreshTimer: number | null = null;
  private readonly unlisteners: UnlistenFn[] = [];
  private prompt: HTMLElement | null = null;
  private pending: RemotePairingRequestEvent | null = null;
  /**
   * When this window was last focused. The backend sends a remotely created
   * pane to the window with the highest value, which is the one the user was
   * last using — the same window a keyboard split would have gone to.
   */
  private focusedAt = Date.now();
  private disposed = false;

  constructor(private readonly deps: RemoteBridgeDeps) {
    this.indicator = document.createElement("button");
    this.indicator.className = "remote-indicator";
    this.indicator.style.display = "none";
    this.indicator.title = "Remote control is enabled";
    this.indicator.addEventListener("click", () => this.deps.onIndicatorClick?.());
    window.addEventListener("focus", this.onWindowFocus);
  }

  /** True while the approval prompt is up; the controller must not steal keys. */
  get isPromptOpen(): boolean {
    return this.prompt !== null;
  }

  /** Whether a remote client could be watching right now. */
  get isEnabled(): boolean {
    return this.status?.enabled ?? false;
  }

  /**
   * Subscribe to the backend's remote events and take the current status.
   *
   * Failures are swallowed: remote control is optional, and a window that
   * cannot reach these events must still be a working terminal.
   */
  async start(): Promise<void> {
    const listeners = await Promise.allSettled([
      onRemoteStatusChanged((status) => this.onStatus(status)),
      onRemoteSnapshotRequest((request) => {
        if (request.windowLabel !== this.deps.windowLabel) return;
        this.onSnapshotRequest(request.requestId, request.paneId);
      }),
      onRemoteAction((action) => {
        // Addressed, like `reattach-pane`: every window hears it, one acts.
        if (action.windowLabel !== this.deps.windowLabel) return;
        this.onAction(action.action, action.paneId ?? null, action.cwd ?? null);
      }),
      onRemotePairingRequest((request) => this.showPrompt(request)),
    ]);
    for (const result of listeners) {
      if (result.status === "fulfilled") this.unlisteners.push(result.value);
    }

    try {
      this.onStatus(await remoteStatus());
    } catch {
      // The backend refused; treat remote control as off.
    }
  }

  /**
   * Publish this window's roster, debounced. Cheap and safe to call from every
   * render; a no-op while the feature is off.
   */
  publish(): void {
    if (this.disposed || !this.isEnabled) return;
    if (this.publishTimer !== null) window.clearTimeout(this.publishTimer);
    this.publishTimer = window.setTimeout(() => {
      this.publishTimer = null;
      void this.publishNow();
    }, PUBLISH_DEBOUNCE_MS);
  }

  /** Tear down listeners, timers and any open prompt. */
  dispose(): void {
    this.disposed = true;
    window.removeEventListener("focus", this.onWindowFocus);
    if (this.publishTimer !== null) window.clearTimeout(this.publishTimer);
    this.stopRefresh();
    this.closePrompt();
    for (const unlisten of this.unlisteners) unlisten();
    this.unlisteners.length = 0;
  }

  // --- internals ---------------------------------------------------------

  private onWindowFocus = (): void => {
    this.focusedAt = Date.now();
    // The target window changed, so the backend needs to hear about it.
    this.publish();
  };

  private async publishNow(): Promise<void> {
    if (!this.isEnabled) return;
    try {
      await publishPanes({
        windowLabel: this.deps.windowLabel,
        focusedAt: this.deps.eligibleAsTarget === false ? 0 : this.focusedAt,
        focusedPaneId: this.deps.focusedPaneId(),
        panes: this.deps.panes(),
      });
    } catch {
      // The roster is refreshed on a timer and on the next layout change.
    }
  }

  private onStatus(status: RemoteStatus): void {
    const wasEnabled = this.isEnabled;
    this.status = status;
    this.paintIndicator();

    if (status.enabled && status.sessions.length > 0) this.startRefresh();
    else this.stopRefresh();

    // Freshly enabled: the backend has an empty roster and is waiting for one.
    if (status.enabled && !wasEnabled) void this.publishNow();
    else if (status.enabled) this.publish();
  }

  private paintIndicator(): void {
    const status = this.status;
    if (!status?.enabled) {
      this.indicator.style.display = "none";
      return;
    }
    const sessions = status.sessions.length;
    this.indicator.style.display = "block";
    this.indicator.classList.toggle("live", sessions > 0);
    this.indicator.textContent =
      sessions > 0 ? `◉ remote · ${sessions}` : "◌ remote";
    this.indicator.title =
      sessions > 0
        ? `${sessions} device${sessions === 1 ? "" : "s"} connected — ${status.host ?? ""}:${status.port}`
        : `Remote control is on — ${status.host ?? ""}:${status.port}`;
  }

  private startRefresh(): void {
    if (this.refreshTimer !== null) return;
    this.refreshTimer = window.setInterval(() => void this.publishNow(), LIVE_REFRESH_MS);
  }

  private stopRefresh(): void {
    if (this.refreshTimer === null) return;
    window.clearInterval(this.refreshTimer);
    this.refreshTimer = null;
  }

  private onSnapshotRequest(requestId: number, paneId: string): void {
    const snapshot = this.deps.snapshot(paneId);
    void provideSnapshot(
      requestId,
      paneId,
      snapshot?.serialized ?? "",
      snapshot?.cols ?? 0,
      snapshot?.rows ?? 0,
    ).catch(() => {
      // The session times out after 3s and sends an empty screen instead.
    });
  }

  private onAction(
    action: "create_pane" | "close_pane",
    paneId: string | null,
    cwd: string | null,
  ): void {
    if (action === "create_pane") {
      this.deps.createPane?.(cwd ?? undefined);
    } else if (action === "close_pane" && paneId) {
      // No confirmation dialog: the phone did its own, and an unattended
      // laptop must not end up with a modal nobody can answer.
      this.deps.closePane(paneId);
    }
    // The new (or missing) pane reaches the client in the next roster publish.
    this.publish();
  }

  // --- pairing approval --------------------------------------------------

  /**
   * The last line of defence: a device presented a valid pairing code, and
   * nothing is issued until the person at the laptop says yes.
   */
  private showPrompt(request: RemotePairingRequestEvent): void {
    this.closePrompt();
    this.pending = request;

    const overlay = document.createElement("div");
    overlay.className = "sv-overlay remote-prompt-overlay";
    // Clicking away is a "no", not a dismissal: leaving the request hanging
    // would leave the phone waiting forever.
    overlay.addEventListener("click", () => this.respond(false));

    const win = document.createElement("div");
    win.className = "remote-prompt";
    win.addEventListener("click", (e) => e.stopPropagation());

    const title = document.createElement("div");
    title.className = "remote-prompt-title";
    title.textContent = "Pair a device?";

    const body = document.createElement("div");
    body.className = "remote-prompt-body";
    const name = document.createElement("strong");
    name.textContent = request.deviceName;
    body.append(
      name,
      document.createTextNode(
        ` (${request.platform}) at ${request.address} wants to control SplitView.`,
      ),
    );

    const warn = document.createElement("div");
    warn.className = "remote-prompt-warn";
    warn.textContent =
      "It will be able to read and type into every terminal in this app.";

    const row = document.createElement("div");
    row.className = "remote-prompt-actions";
    const deny = document.createElement("button");
    deny.className = "sv-btn sv-btn-ghost";
    deny.textContent = "Deny  (Esc)";
    deny.addEventListener("click", () => this.respond(false));
    const allow = document.createElement("button");
    allow.className = "sv-btn remote-allow";
    allow.textContent = "Allow  (Y)";
    allow.addEventListener("click", () => this.respond(true));
    row.append(deny, allow);

    win.append(title, body, warn, row);
    overlay.append(win);
    document.body.appendChild(overlay);
    this.prompt = overlay;
    // Focus lands on Deny: the safe answer is the one you get by pressing Enter.
    deny.focus();
  }

  /**
   * Keys while the prompt is open. The controller routes every keystroke here
   * first, so nothing leaks into the terminal underneath.
   */
  handleKey(ev: KeyboardEvent): boolean {
    if (!this.prompt) return false;
    const key = ev.key.toLowerCase();
    if (key === "escape" || key === "n") {
      this.respond(false);
      return true;
    }
    if (key === "y") {
      this.respond(true);
      return true;
    }
    // Tab and Enter belong to the buttons; everything else is swallowed so a
    // stray keystroke cannot reach the shell behind the prompt.
    return key !== "tab" && key !== "enter";
  }

  private respond(approve: boolean): void {
    const request = this.pending;
    this.closePrompt();
    if (!request) return;
    void remotePairingRespond(request.requestId, approve).catch(() => {
      // The session gives up on its own if the answer never arrives.
    });
  }

  private closePrompt(): void {
    if (!this.prompt) return;
    this.prompt.remove();
    this.prompt = null;
    this.pending = null;
    this.deps.onPromptClosed?.();
  }
}
