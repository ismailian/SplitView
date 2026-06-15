/**
 * Thin, typed wrappers over Tauri `invoke()` / `Channel` (CLAUDE.md §10).
 *
 * No other UI code should call `invoke` directly. Terminal output is arbitrary
 * bytes, so it is streamed as a `Channel<number[]>` (a JSON array of bytes is
 * byte-safe) and never round-tripped through a JSON *string*.
 */

import { invoke, Channel } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface SpawnOpts {
  cols?: number;
  rows?: number;
  cwd?: string;
  shell?: string;
  prompt?: string;
  /** `"parrot"` (default) or `"plain"`; selects the injected prompt. */
  promptStyle?: "parrot" | "plain";
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
