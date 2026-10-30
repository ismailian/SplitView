/**
 * Turning this window's panes into the flat roster a remote client sees
 * (`docs/remote-control-desktop.md` §5.2).
 *
 * Kept pure and DOM-free so the ordering rules — which decide what the phone's
 * tab strip looks like — can be tested without a browser or a backend, the same
 * way `layout/tree.ts` is.
 *
 * The phone is a **tab list**, not a layout: the split tree is flattened to the
 * order a person reads it in, and a pane's place in that order is the only
 * spatial information that survives.
 */

import type { RemotePaneInfo } from "../ipc/commands";

/** The minimum a pane has to expose to appear in the roster. */
export interface RosterPaneView {
  /** Terminal title (OSC 0/2), if the running program set one. */
  readonly title: string | null;
  /** Working directory the shell reported over OSC 7. */
  readonly cwd: string | null;
  /** Live terminal dimensions; the phone renders at these. */
  readonly term: { readonly cols: number; readonly rows: number };
  /** The last non-empty output line. */
  previewLine(): string;
}

/** Describe one pane for the roster. */
export function paneInfo(
  id: string,
  pane: RosterPaneView,
  backgrounded: boolean,
): RemotePaneInfo {
  return {
    id,
    title: pane.title,
    cwd: pane.cwd,
    cols: pane.term.cols,
    rows: pane.term.rows,
    preview: pane.previewLine(),
    backgrounded,
  };
}

/**
 * Build the roster: panes in layout order first, then backgrounded ones.
 *
 * Backgrounded panes are included because they are still live terminals with
 * live processes — a dev server you pushed out of the way is exactly the thing
 * you want to check from a phone. They are flagged so the client can say so.
 *
 * `order` may name panes this window no longer has (a pane closed between the
 * layout change and this call); those are skipped rather than published as
 * something the client could try to subscribe to.
 */
export function buildRoster(
  order: readonly string[],
  live: ReadonlyMap<string, RosterPaneView>,
  backgrounded: ReadonlyMap<string, RosterPaneView>,
): RemotePaneInfo[] {
  const out: RemotePaneInfo[] = [];
  for (const id of order) {
    const pane = live.get(id);
    if (pane) out.push(paneInfo(id, pane, false));
  }
  for (const [id, pane] of backgrounded) {
    // A pane cannot be in the layout and backgrounded at once; if some race
    // ever made it look that way, the layout entry is the truthful one.
    if (live.has(id)) continue;
    out.push(paneInfo(id, pane, true));
  }
  return out;
}
