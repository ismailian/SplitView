/**
 * Thin, typed wrappers over Tauri `invoke()` / `Channel` (CLAUDE.md §10).
 *
 * No other UI code should call `invoke` directly. Terminal output is arbitrary
 * bytes, so it is streamed as a `Channel<number[]>` (a JSON array of bytes is
 * byte-safe) and never round-tripped through a JSON *string*.
 */

import { invoke, Channel } from "@tauri-apps/api/core";
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";

/** A user-defined alias: typing `name` in the shell runs `command`. */
export interface Alias {
  name: string;
  command: string;
}

/** Which informational segments the prompt's first line shows. */
export interface PromptInfo {
  user: boolean;
  host: boolean;
  dir: boolean;
  branch: boolean;
  time: boolean;
  exitStatus: boolean;
  /**
   * How much of the working directory the `dir` segment shows: `0` = the whole
   * path (with the middle elided once it gets long), `n > 0` = only the last `n`
   * path segments (`2` → `project-x/mobile-app`). See `promptDir` in `paths.ts`.
   */
  dirSegments: number;
}

/** The boolean members of `PromptInfo` — i.e. everything that is a plain on/off
 * toggle, as opposed to `dirSegments`. */
export type PromptToggle = Exclude<keyof PromptInfo, "dirSegments">;

export interface SpawnOpts {
  cols?: number;
  rows?: number;
  cwd?: string;
  shell?: string;
  prompt?: string;
  /** `"parrot"` (default) or `"plain"`; selects the injected prompt. */
  promptStyle?: "parrot" | "plain";
  /** Aliases injected into the spawned shell. */
  aliases?: Alias[];
  /** Prompt info toggles (user/host/dir/branch/time/exit-status). */
  promptInfo?: PromptInfo;
  /** Banner printed once before the first prompt. */
  banner?: string;
}

export type PaneId = string;

/** Callback for a chunk of raw PTY output. */
export type OutputHandler = (bytes: Uint8Array) => void;

/**
 * Open a PTY, spawn the shell, and start streaming its output to `onOutput`.
 * Returns the backend-assigned pane id. Keep the returned `Channel` alive for
 * the life of the pane (the caller holds it via this closure).
 */
export async function spawnPty(
  opts: SpawnOpts,
  onOutput: OutputHandler,
): Promise<PaneId> {
  const channel = new Channel<number[]>();
  channel.onmessage = (msg) => onOutput(new Uint8Array(msg));
  return invoke<PaneId>("spawn_pty", { onOutput: channel, opts });
}

/**
 * Re-point an existing pane's PTY output to *this* window's channel. The PTY and
 * its shell keep running untouched; only the sink the backend reader streams to
 * is swapped. Used when a detached window adopts a popped-out pane, or when the
 * main window reattaches one. Mirrors `spawnPty`'s channel wiring.
 */
export async function attachPty(
  paneId: PaneId,
  onOutput: OutputHandler,
): Promise<void> {
  const channel = new Channel<number[]>();
  channel.onmessage = (msg) => onOutput(new Uint8Array(msg));
  await invoke("attach_pty", { paneId, onOutput: channel });
}

/** Write keystrokes/bytes to a pane's PTY. */
export async function writePty(paneId: PaneId, data: Uint8Array): Promise<void> {
  await invoke("write_pty", { paneId, data: Array.from(data) });
}

/** Resize a pane's PTY. Always paired with an xterm.js resize (CLAUDE.md §20). */
export async function resizePty(
  paneId: PaneId,
  cols: number,
  rows: number,
): Promise<void> {
  await invoke("resize_pty", { paneId, cols, rows });
}

/** Kill the shell and tear down the PTY. */
export async function closePty(paneId: PaneId): Promise<void> {
  await invoke("close_pty", { paneId });
}

/** List live pane ids (debugging/recovery). */
export async function listPanes(): Promise<PaneId[]> {
  return invoke<PaneId[]>("list_panes");
}

/**
 * Stash a pane's serialized xterm buffer so the window it is moving to can pick
 * it up via `takeHandoff`. Carries scrollback across a detach/reattach without
 * smuggling a large buffer through the detached window's URL.
 */
export async function stashHandoff(paneId: PaneId, data: string): Promise<void> {
  await invoke("stash_handoff", { paneId, data });
}

/** Take (and clear) a pane's stashed serialized buffer, or `null` if none. */
export async function takeHandoff(paneId: PaneId): Promise<string | null> {
  return invoke<string | null>("take_handoff", { paneId });
}

/** One entry of a directory listing (see the `read_dir` command). */
export interface DirEntryInfo {
  /** File name only. */
  name: string;
  /** Absolute native path; pass it straight back to `readDir` to expand it. */
  path: string;
  isDir: boolean;
  isSymlink: boolean;
  /** Dot-file (or hidden/system on Windows). */
  hidden: boolean;
}

/** A directory listing: the directory, its parent, and its sorted entries. */
export interface DirListing {
  path: string;
  parent: string | null;
  entries: DirEntryInfo[];
  /** The directory had more entries than the backend returns; the list is cut. */
  truncated: boolean;
}

/**
 * List a directory for the file sidebar. `path` may be the msys-form cwd the
 * shell reported over OSC 7 (`/c/Users/…`); the backend normalizes it and
 * returns native paths. Omit it to list the user's home directory.
 */
export async function readDir(path?: string | null): Promise<DirListing> {
  return invoke<DirListing>("read_dir", { path: path ?? null });
}

/**
 * Start this window's file-sidebar watch. `onChange` receives batches of
 * directories whose listing changed (an entry was created, removed or renamed),
 * spelled exactly as they were passed to `fsWatchDirs`. Replaces any earlier
 * watch of this window, and watches nothing until `fsWatchDirs` says what to.
 */
export async function fsWatch(onChange: (dirs: string[]) => void): Promise<void> {
  const channel = new Channel<string[]>();
  channel.onmessage = onChange;
  await invoke("fs_watch", { onChange: channel });
}

/** Set the directories the sidebar displays — its root and open folders, as the
 * native paths `readDir` returned. Only the difference reaches the OS. */
export async function fsWatchDirs(dirs: string[]): Promise<void> {
  await invoke("fs_watch_dirs", { dirs });
}

/** Stop this window's sidebar watch, releasing every OS watch it held. */
export async function fsUnwatch(): Promise<void> {
  await invoke("fs_unwatch");
}

/** Which version of a file this is. Compared for equality, never ordered. */
export interface FileStamp {
  modifiedMs: number;
  size: number;
}

/** A file opened for the editor panel. */
export interface TextFile {
  /** Native path, normalized by the backend (msys spellings are accepted). */
  path: string;
  content: string;
  stamp: FileStamp;
}

/** Open a text file. Rejects with a readable reason for a binary, non-UTF-8 or
 * oversized file. */
export async function readTextFile(path: string): Promise<TextFile> {
  return invoke<TextFile>("read_text_file", { path });
}

/** A file's current stamp, or `null` if it no longer exists. */
export async function fileStamp(path: string): Promise<FileStamp | null> {
  return invoke<FileStamp | null>("file_stamp", { path });
}

/**
 * Save a file, provided it is still at `expected`. Returns the new stamp, or
 * `null` if the file changed on disk and nothing was written; pass `null` for
 * `expected` to overwrite regardless.
 */
export async function writeTextFile(
  path: string,
  content: string,
  expected: FileStamp | null,
): Promise<FileStamp | null> {
  return invoke<FileStamp | null>("write_text_file", { path, content, expected });
}

/**
 * Show the OS-native file picker and return the selected absolute path, or
 * `null` if the user cancelled. The dialog is driven entirely by the backend
 * `pick_file` command (CLAUDE.md §10 — no `invoke` outside this module).
 */
export async function pickFile(): Promise<string | null> {
  return invoke<string | null>("pick_file");
}

/**
 * Subscribe to backend `pty-exit` events, fired when a pane's shell process
 * exits on its own (e.g. the user runs `exit`). The handler receives the dead
 * pane's id so the caller can close it. Returns an unlisten function.
 */
export async function onPtyExit(
  handler: (paneId: PaneId) => void,
): Promise<UnlistenFn> {
  return listen<PaneId>("pty-exit", (event) => handler(event.payload));
}

/** Cross-window signal a detached window sends to ask the window it came from to
 * adopt its pane back into the layout. */
interface ReattachRequest {
  paneId: PaneId;
  /** Label of the window the pane was detached from. Events are broadcast to
   * every window, so with several layout windows open (<prefix> n) the request
   * has to name its destination or two windows would fight over the pane. */
  owner: string | null;
}

/** Layout window: listen for a detached window asking to send its pane home. */
export async function onReattachPane(
  handler: (paneId: PaneId, owner: string | null) => void,
): Promise<UnlistenFn> {
  return listen<ReattachRequest>("reattach-pane", (event) =>
    handler(event.payload.paneId, event.payload.owner ?? null),
  );
}

/** Detached window: ask `owner` to reattach this pane into its layout. */
export async function requestReattach(
  paneId: PaneId,
  owner: string | null,
): Promise<void> {
  await emit("reattach-pane", { paneId, owner } satisfies ReattachRequest);
}

// --- remote control (docs/remote-control-desktop.md) ------------------------
//
// The webview publishes what only it knows (the pane roster, a pane's screen)
// and relays what only it may do (anything that touches the layout tree). The
// backend owns the socket, the crypto and the device registry. See
// `src/remote/RemoteBridge.ts`.

/** Which interfaces the server binds: every one, or loopback for tunnelling. */
export type BindMode = "lan" | "loopback";

/** The server's user-facing configuration, sent when enabling it. */
export interface RemoteConfig {
  bind: BindMode;
  /** `0` asks the OS for an ephemeral port. */
  port: number;
  /** Allow a client to resize a pane, which reflows this window too. */
  allowResize: boolean;
}

/** A paired device. The token itself never leaves the backend. */
export interface RemoteDevice {
  id: string;
  name: string;
  platform: string;
  /** Unix seconds. */
  pairedAt: number;
  lastSeen: number;
}

/** One connected client. */
export interface RemoteSession {
  id: number;
  deviceId: string;
  deviceName: string;
  address: string;
  connectedAt: number;
}

/** The live pairing code, its countdown, and the payload the QR encodes. */
export interface RemotePairing {
  code: string;
  secondsLeft: number;
  payload: string;
}

/** Everything the Settings "Remote" section renders. */
export interface RemoteStatus {
  enabled: boolean;
  bind: BindMode;
  allowResize: boolean;
  port: number;
  /** The address to type into the phone; null while the server is off. */
  host: string | null;
  hostName: string;
  /** `AB:CD:…`, present once an identity exists. */
  fingerprint: string | null;
  sessions: RemoteSession[];
  devices: RemoteDevice[];
  pairing: RemotePairing | null;
}

/** One pane, as this window publishes it to the backend's roster. */
export interface RemotePaneInfo {
  id: PaneId;
  /** OSC 0/2 title. */
  title: string | null;
  /** OSC 7 working directory. */
  cwd: string | null;
  cols: number;
  rows: number;
  /** The pane's last non-empty output line. */
  preview: string;
  backgrounded: boolean;
}

/** This window's snapshot of itself, for the remote roster. */
export interface WindowPanes {
  windowLabel: string;
  /** Monotonic; the backend targets the window with the highest value. */
  focusedAt: number;
  focusedPaneId: PaneId | null;
  panes: RemotePaneInfo[];
}

/** A layout change a remote client asked for, addressed to one window. */
export interface RemoteActionEvent {
  windowLabel: string;
  action: "create_pane" | "close_pane";
  paneId?: PaneId | null;
  cwd?: string | null;
}

/** A request for a pane's serialized screen, addressed to one window. */
export interface RemoteSnapshotRequestEvent {
  windowLabel: string;
  requestId: number;
  paneId: PaneId;
}

/** A device asking to pair, for the desktop approval prompt. */
export interface RemotePairingRequestEvent {
  requestId: number;
  deviceName: string;
  platform: string;
  address: string;
}

/**
 * Publish this window's pane roster. Debounced by the caller; ignored by the
 * backend while remote control is off, so a roster of every terminal you have
 * open never exists unless you asked for the feature.
 */
export async function publishPanes(snapshot: WindowPanes): Promise<void> {
  await invoke("publish_panes", { snapshot });
}

/** Answer a `remote-snapshot-request` with a serialized xterm.js buffer. */
export async function provideSnapshot(
  requestId: number,
  paneId: PaneId,
  serialized: string,
  cols: number,
  rows: number,
): Promise<void> {
  await invoke("provide_snapshot", { requestId, paneId, serialized, cols, rows });
}

/** Current remote-control status (server, devices, sessions, pairing code). */
export async function remoteStatus(): Promise<RemoteStatus> {
  return invoke<RemoteStatus>("remote_status");
}

/** Turn remote control on. Generates the TLS identity on first use. */
export async function remoteEnable(config: RemoteConfig): Promise<RemoteStatus> {
  return invoke<RemoteStatus>("remote_enable", { config });
}

/** Turn it off: stop accepting, end every session, forget the roster. */
export async function remoteDisable(): Promise<RemoteStatus> {
  return invoke<RemoteStatus>("remote_disable");
}

/** Issue a single-use pairing code (120 s). */
export async function remotePairingStart(): Promise<RemoteStatus> {
  return invoke<RemoteStatus>("remote_pairing_start");
}

/** Drop the live pairing code. */
export async function remotePairingCancel(): Promise<RemoteStatus> {
  return invoke<RemoteStatus>("remote_pairing_cancel");
}

/** Answer a pairing prompt. Nothing is issued until this says `true`. */
export async function remotePairingRespond(
  requestId: number,
  approve: boolean,
): Promise<void> {
  await invoke("remote_pairing_respond", { requestId, approve });
}

/** Paired devices, newest first. */
export async function remoteDevices(): Promise<RemoteDevice[]> {
  return invoke<RemoteDevice[]>("remote_devices");
}

/** Forget a device and cut any session using it. */
export async function remoteRevokeDevice(deviceId: string): Promise<RemoteStatus> {
  return invoke<RemoteStatus>("remote_revoke_device", { deviceId });
}

/** End one live session without revoking its device. */
export async function remoteDisconnectSession(
  sessionId: number,
): Promise<RemoteStatus> {
  return invoke<RemoteStatus>("remote_disconnect_session", { sessionId });
}

/** Listen for layout changes a remote client asked for. */
export async function onRemoteAction(
  handler: (action: RemoteActionEvent) => void,
): Promise<UnlistenFn> {
  return listen<RemoteActionEvent>("remote-action", (e) => handler(e.payload));
}

/** Listen for requests to serialize a pane for a newly attached client. */
export async function onRemoteSnapshotRequest(
  handler: (request: RemoteSnapshotRequestEvent) => void,
): Promise<UnlistenFn> {
  return listen<RemoteSnapshotRequestEvent>("remote-snapshot-request", (e) =>
    handler(e.payload),
  );
}

/** Listen for devices asking to pair. */
export async function onRemotePairingRequest(
  handler: (request: RemotePairingRequestEvent) => void,
): Promise<UnlistenFn> {
  return listen<RemotePairingRequestEvent>("remote-pairing-request", (e) =>
    handler(e.payload),
  );
}

/** Listen for the server being enabled/disabled or a session coming or going. */
export async function onRemoteStatusChanged(
  handler: (status: RemoteStatus) => void,
): Promise<UnlistenFn> {
  return listen<RemoteStatus>("remote-status-changed", (e) => handler(e.payload));
}
