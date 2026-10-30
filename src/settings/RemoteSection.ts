/**
 * The Settings window's **Remote** section
 * (`docs/remote-control-desktop.md` §9).
 *
 * This is where a terminal emulator becomes a remote shell server, so the panel
 * is built to make that legible rather than to hide it: the switch says what it
 * turns on, the address and certificate fingerprint are shown in full, pairing
 * is a deliberate two-step with a countdown, and every paired device and live
 * session can be cut from here.
 *
 * It follows the rest of the app's visual rules — thin lines, no chrome
 * (CLAUDE.md §14) — and is keyboard-reachable like everything else: `<prefix> ,`
 * then the section, or clicking the corner indicator.
 */

import {
  remoteDisable,
  remoteDisconnectSession,
  remoteEnable,
  remotePairingCancel,
  remotePairingStart,
  remoteRevokeDevice,
  remoteStatus,
  type RemoteStatus,
} from "../ipc/commands";
import { qrMatrix, qrSvg, QrTooLongError } from "../remote/qr";
import { clampPort, remoteConfigOf, type RemoteSettings } from "./settings";

/** How often the panel re-reads status while it is visible. Drives the pairing
 * countdown, the session list, and the "last seen" column. */
const POLL_MS = 1000;

export interface RemoteSectionDeps {
  /** The stored remote settings; edited in place, like the other sections. */
  settings(): RemoteSettings;
  /** Called after any edit so the controller persists them. */
  onChange(): void;
}

export class RemoteSection {
  readonly el: HTMLElement;
  private status: RemoteStatus | null = null;
  private timer: number | null = null;
  private busy = false;
  /** Set while an action is in flight, shown instead of a silent no-op. */
  private error: string | null = null;

  constructor(private readonly deps: RemoteSectionDeps) {
    this.el = document.createElement("div");
    this.el.className = "sv-section sv-remote";
    this.render();
  }

  /** Begin polling; called when the section becomes visible. */
  show(): void {
    void this.refresh();
    if (this.timer === null) {
      this.timer = window.setInterval(() => void this.refresh(), POLL_MS);
    }
  }

  /** Stop polling; called when another section is shown or Settings closes. */
  hide(): void {
    if (this.timer === null) return;
    window.clearInterval(this.timer);
    this.timer = null;
  }

  // --- data --------------------------------------------------------------

  private async refresh(): Promise<void> {
    try {
      this.status = await remoteStatus();
      this.error = null;
    } catch (e) {
      this.status = null;
      this.error = describe(e);
    }
    this.render();
  }

  /** Run a backend call, then re-read status. Errors are shown, never swallowed. */
  private async act(run: () => Promise<RemoteStatus | void>): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    this.render();
    try {
      const status = await run();
      if (status) this.status = status;
      this.error = null;
    } catch (e) {
      this.error = describe(e);
    } finally {
      this.busy = false;
    }
    await this.refresh();
  }

  private async setEnabled(on: boolean): Promise<void> {
    const settings = this.deps.settings();
    await this.act(() =>
      on ? remoteEnable(remoteConfigOf(settings)) : remoteDisable(),
    );
  }

  /**
   * Apply an edit. While the server is running, configuration changes take
   * effect by restarting it — which is also why the UI says connected devices
   * will reconnect.
   */
  private async applyEdit(): Promise<void> {
    this.deps.onChange();
    if (this.status?.enabled) await this.setEnabled(true);
    else this.render();
  }

  // --- rendering ---------------------------------------------------------

  private render(): void {
    const settings = this.deps.settings();
    const status = this.status;
    const frag = document.createDocumentFragment();

    frag.append(
      note(
        "Lets a paired phone see your panes, type into them, and open or close them. " +
          "Everything is encrypted and every device has to be approved here first.",
      ),
    );
    if (this.error) frag.append(banner(this.error));

    // --- the switch
    frag.append(
      toggleRow(
        "Enable remote control",
        status?.enabled
          ? "The server is running. A paired device can reach every terminal in this app."
          : "Off. Nothing is listening and no pane roster leaves this machine.",
        status?.enabled ?? false,
        this.busy,
        (on) => void this.setEnabled(on),
      ),
    );

    // --- where it listens
    const lan = chip("Whole network", settings.bind === "lan", () => {
      settings.bind = "lan";
      void this.applyEdit();
    });
    const loopback = chip("This machine only", settings.bind === "loopback", () => {
      settings.bind = "loopback";
      void this.applyEdit();
    });
    frag.append(
      field(
        "Reachable from",
        row(lan, loopback),
        settings.bind === "lan"
          ? "Anything on your Wi-Fi can try to connect (it still needs to pair)."
          : "Loopback only — reach it through Tailscale, WireGuard or an SSH tunnel.",
      ),
    );

    const port = document.createElement("input");
    port.className = "sv-input sv-num";
    port.type = "number";
    port.min = "0";
    port.max = "65535";
    port.value = String(settings.port);
    port.disabled = this.busy;
    port.onchange = () => {
      settings.port = clampPort(port.value);
      port.value = String(settings.port);
      void this.applyEdit();
    };
    frag.append(field("Port", port, "0 lets the system pick a free one."));

    // --- what to tell the phone
    if (status?.enabled) {
      frag.append(
        readOnly("Address", `${status.host ?? "?"}:${status.port}`),
        readOnly("This machine", status.hostName),
        readOnly(
          "Certificate fingerprint",
          status.fingerprint ?? "—",
          "The phone pins this. Check the first characters match what it shows.",
        ),
      );
    }

    // --- pairing
    frag.append(this.pairingBlock(status));

    // --- devices and sessions
    frag.append(this.devicesBlock(status));
    frag.append(this.sessionsBlock(status));

    // --- the two remaining switches
    frag.append(
      toggleRow(
        "Turn on at startup",
        "Starts the server every time SplitView launches. Leave off unless you " +
          "want it always reachable.",
        settings.enableAtStartup,
        this.busy,
        (on) => {
          settings.enableAtStartup = on;
          this.deps.onChange();
          this.render();
        },
      ),
      toggleRow(
        "Allow remote resize",
        "Lets the phone fit a terminal to its screen — which reflows that pane " +
          "in this window too. Connected devices reconnect when this changes.",
        settings.allowResize,
        this.busy,
        (on) => {
          settings.allowResize = on;
          void this.applyEdit();
        },
      ),
    );

    this.el.replaceChildren(frag);
  }

  private pairingBlock(status: RemoteStatus | null): HTMLElement {
    const block = document.createElement("div");
    block.className = "sv-remote-block";
    block.append(subhead("Pair a device"));

    if (!status?.enabled) {
      block.append(empty("Turn remote control on first."));
      return block;
    }

    const pairing = status.pairing;
    if (!pairing) {
      const start = button("＋  Pair a device", "sv-btn", () =>
        void this.act(() => remotePairingStart()),
      );
      start.disabled = this.busy;
      block.append(
        note("Opens a code and a QR for two minutes. Scan it with SplitView Remote."),
        start,
      );
      return block;
    }

    // A live code: QR, the same thing as text, and a countdown.
    const qr = document.createElement("div");
    qr.className = "sv-qr";
    try {
      qr.innerHTML = qrSvg(qrMatrix(pairing.payload));
    } catch (e) {
      qr.textContent =
        e instanceof QrTooLongError
          ? "The pairing link is too long to show as a QR — type it in instead."
          : "Could not draw the QR — type the details in instead.";
      qr.classList.add("sv-qr-failed");
    }

    const code = document.createElement("div");
    code.className = "sv-pair-code";
    code.textContent = `${pairing.code.slice(0, 4)} ${pairing.code.slice(4)}`;

    const left = document.createElement("div");
    left.className = "sv-pair-countdown";
    left.textContent = `expires in ${pairing.secondsLeft}s`;

    const manual = document.createElement("div");
    manual.className = "sv-pair-manual";
    manual.textContent = `or enter ${status.host ?? "?"} : ${status.port} and this code`;

    const cancel = button("Cancel", "sv-btn sv-btn-ghost", () =>
      void this.act(() => remotePairingCancel()),
    );

    const panel = document.createElement("div");
    panel.className = "sv-pair";
    const side = document.createElement("div");
    side.className = "sv-pair-side";
    side.append(code, left, manual, cancel);
    panel.append(qr, side);
    block.append(panel);
    return block;
  }

  private devicesBlock(status: RemoteStatus | null): HTMLElement {
    const block = document.createElement("div");
    block.className = "sv-remote-block";
    block.append(subhead("Paired devices"));

    const devices = status?.devices ?? [];
    if (devices.length === 0) {
      block.append(empty("No devices are paired."));
      return block;
    }
    for (const device of devices) {
      const row = document.createElement("div");
      row.className = "sv-remote-row";
      row.append(
        cell("sv-remote-name", device.name),
        cell("sv-remote-meta", device.platform),
        cell("sv-remote-meta", `last seen ${ago(device.lastSeen)}`),
        button("Revoke", "sv-icon-btn sv-danger", () =>
          void this.act(() => remoteRevokeDevice(device.id)),
        ),
      );
      block.append(row);
    }
    block.append(
      note("Revoking forgets the device and disconnects it immediately."),
    );
    return block;
  }

  private sessionsBlock(status: RemoteStatus | null): HTMLElement {
    const block = document.createElement("div");
    block.className = "sv-remote-block";
    block.append(subhead("Connected now"));

    const sessions = status?.sessions ?? [];
    if (sessions.length === 0) {
      block.append(empty("Nothing is connected."));
      return block;
    }
    for (const session of sessions) {
      const row = document.createElement("div");
      row.className = "sv-remote-row";
      row.append(
        cell("sv-remote-name", session.deviceName),
        cell("sv-remote-meta", session.address),
        cell("sv-remote-meta", `for ${ago(session.connectedAt)}`),
        button("Disconnect", "sv-icon-btn", () =>
          void this.act(() => remoteDisconnectSession(session.id)),
        ),
      );
      block.append(row);
    }
    return block;
  }
}

// --- small DOM helpers ----------------------------------------------------

function note(text: string): HTMLElement {
  const el = document.createElement("p");
  el.className = "sv-note";
  el.textContent = text;
  return el;
}

function banner(text: string): HTMLElement {
  const el = document.createElement("p");
  el.className = "sv-note sv-remote-error";
  el.textContent = text;
  return el;
}

function subhead(text: string): HTMLElement {
  const el = document.createElement("h3");
  el.className = "sv-subhead";
  el.textContent = text;
  return el;
}

function empty(text: string): HTMLElement {
  const el = document.createElement("p");
  el.className = "sv-empty";
  el.textContent = text;
  return el;
}

function cell(className: string, text: string): HTMLElement {
  const el = document.createElement("span");
  el.className = className;
  el.textContent = text;
  return el;
}

function row(...children: HTMLElement[]): HTMLElement {
  const el = document.createElement("div");
  el.className = "sv-choice";
  el.append(...children);
  return el;
}

function button(
  label: string,
  className: string,
  onClick: () => void,
): HTMLButtonElement {
  const el = document.createElement("button");
  el.className = className;
  el.textContent = label;
  el.onclick = onClick;
  return el;
}

function chip(label: string, active: boolean, onClick: () => void): HTMLButtonElement {
  const el = button(label, "sv-chip", onClick);
  el.classList.toggle("active", active);
  return el;
}

/** A labelled row, matching the other Settings sections. */
function field(label: string, control: HTMLElement, hint?: string): HTMLElement {
  const text = document.createElement("div");
  text.className = "sv-field-text";
  text.append(cell("sv-label", label));
  if (hint) text.append(cell("sv-hint", hint));
  const wrap = document.createElement("div");
  // A plain div, not a <label>: clicking a label activates its first control,
  // which here would silently flip the bind mode.
  wrap.className = "sv-field";
  wrap.append(text, control);
  return wrap;
}

function readOnly(label: string, value: string, hint?: string): HTMLElement {
  const el = document.createElement("code");
  el.className = "sv-mono";
  el.textContent = value;
  return field(label, el, hint);
}

function toggleRow(
  label: string,
  desc: string,
  checked: boolean,
  disabled: boolean,
  onToggle: (on: boolean) => void,
): HTMLElement {
  const input = document.createElement("input");
  input.className = "sv-switch-input";
  input.type = "checkbox";
  input.checked = checked;
  input.disabled = disabled;
  input.onchange = () => onToggle(input.checked);

  const track = document.createElement("span");
  track.className = "sv-switch-track";
  const sw = document.createElement("span");
  sw.className = "sv-switch";
  sw.append(input, track);

  const text = document.createElement("div");
  text.className = "sv-toggle-text";
  text.append(cell("sv-toggle-label", label), cell("sv-toggle-desc", desc));

  const el = document.createElement("label");
  el.className = "sv-toggle-row";
  el.append(text, sw);
  return el;
}

/** Compact age from a unix-seconds timestamp: `12s`, `4m`, `2h11m`, `3d`. */
function ago(unixSeconds: number): string {
  if (!unixSeconds) return "never";
  const s = Math.max(0, Math.floor(Date.now() / 1000 - unixSeconds));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h${m % 60}m`;
  return `${Math.floor(h / 24)}d`;
}

/** Render a backend error as something a person can act on. */
function describe(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}
