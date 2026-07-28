/**
 * Window creation, kept out of the controllers (CLAUDE.md §10 — Tauri calls live
 * behind typed wrappers, not in random UI code).
 *
 * Every window runs the same `index.html`; the query string picks which
 * controller boots (see `main.ts`):
 *   - no query      → a full split-layout window (`App`)
 *   - `?cwd=…`      → same, with the first shell opened in that directory
 *   - `?pane=…`     → a detached window hosting one adopted pane (`DetachedApp`)
 */

import { WebviewWindow } from "@tauri-apps/api/webviewWindow";

/** Matches the main window's geometry in `tauri.conf.json`. */
const NEW_WINDOW = {
  title: "SplitView",
  width: 1024,
  height: 680,
  minWidth: 480,
  minHeight: 320,
};

let seq = 0;

/**
 * Open a new top-level SplitView window with its own layout and shell.
 *
 * The label must be unique for the app's lifetime *and* must match a pattern in
 * `capabilities/default.json` (`sv-win-*`), or the new window is denied the core
 * permissions it needs. Resolves once the OS window exists; rejects if creation
 * failed.
 */
export function openNewWindow(cwd?: string | null): Promise<void> {
  const label = `sv-win-${Date.now().toString(36)}-${seq++}`;
  const url = cwd ? `index.html?cwd=${encodeURIComponent(cwd)}` : "index.html";
  return new Promise((resolve, reject) => {
    const win = new WebviewWindow(label, { url, ...NEW_WINDOW });
    void win.once("tauri://created", () => resolve());
    void win.once("tauri://error", (e) => reject(new Error(String(e.payload))));
  });
}
