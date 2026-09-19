/**
 * The file sidebar's tree shape, as pure functions — no DOM, no IPC — so the
 * part that decides what is on screen (and so what gets watched) is testable.
 *
 * The sidebar keeps one listing per directory, shared by every pane, and per
 * pane the set of open folders. The rows are derived from those two on every
 * render; nothing about the tree is stored twice.
 */

import type { DirEntryInfo } from "../ipc/commands";

/** What is known about one directory. */
export interface Listing {
  /** Sorted entries (as the backend returns them), or null until read. */
  entries: DirEntryInfo[] | null;
  /** The directory's parent, for "go up". */
  parent: string | null;
  /** The backend cut the list short. */
  truncated: boolean;
  /** Why the last read failed (gone, no permission), or null. */
  error: string | null;
}

/** One visible line of the tree. */
export interface Row {
  entry: DirEntryInfo;
  depth: number;
  /** For each ancestor level, whether that ancestor was the last of its group
   * (decides `│  ` vs `   `); the final entry decides `├─ ` vs `└─ `. */
  lastFlags: boolean[];
  /** A folder shown open. */
  open: boolean;
  /** An open folder whose entries haven't arrived yet, or couldn't be read. */
  status: "loading" | "error" | null;
}

export interface TreeShape {
  rows: Row[];
  /** Directories whose contents are on screen: the root and every visible open
   * folder, in tree order, minus unreadable ones. Exactly the set to watch. */
  shown: string[];
}

/**
 * Flatten the tree under `root`. Open folders nest their entries (or show as
 * loading until they are read); hidden entries — and anything open inside them —
 * are left out unless `showHidden`.
 */
export function buildTree(
  root: string,
  listings: ReadonlyMap<string, Listing>,
  open: ReadonlySet<string>,
  showHidden: boolean,
): TreeShape {
  const rows: Row[] = [];
  const shown: string[] = [];

  const walk = (dir: string, lastFlags: boolean[]): void => {
    const listing = listings.get(dir);
    if (listing?.error) return;
    shown.push(dir);
    if (!listing?.entries) return;
    const visible = showHidden
      ? listing.entries
      : listing.entries.filter((e) => !e.hidden);
    visible.forEach((entry, i) => {
      const flags = [...lastFlags, i === visible.length - 1];
      const isOpen = entry.isDir && open.has(entry.path);
      const child = isOpen ? listings.get(entry.path) : undefined;
      rows.push({
        entry,
        depth: lastFlags.length,
        lastFlags: flags,
        open: isOpen,
        status: !isOpen
          ? null
          : child?.error
            ? "error"
            : child?.entries
              ? null
              : "loading",
      });
      if (isOpen) walk(entry.path, flags);
    });
  };

  if (root) walk(root, []);
  return { rows, shown };
}

/** The `│  ├─ ` text drawn before a row's name. */
export function connectors(row: Row): string {
  let prefix = "";
  for (let d = 0; d < row.depth; d++) prefix += row.lastFlags[d] ? "   " : "│  ";
  return prefix + (row.lastFlags[row.depth] ? "└─ " : "├─ ");
}

/** True when a fresh read shows exactly what is already displayed, so the
 * re-read that a change notification triggered doesn't need a render. */
export function sameEntries(
  a: readonly DirEntryInfo[] | null,
  b: readonly DirEntryInfo[],
): boolean {
  if (!a || a.length !== b.length) return false;
  return a.every(
    (x, i) =>
      x.path === b[i].path &&
      x.isDir === b[i].isDir &&
      x.isSymlink === b[i].isSymlink &&
      x.hidden === b[i].hidden,
  );
}

/**
 * Where the cursor lands after the rows changed: on the same entry if it is
 * still there (rows appearing above it don't move it), else at the same
 * position, clamped — so deleting the selected file leaves the cursor on its
 * neighbour rather than jumping to the top.
 */
export function selectionIndex(
  rows: readonly Row[],
  selected: string | null,
  previous: number,
): number {
  if (rows.length === 0) return 0;
  if (selected !== null) {
    const i = rows.findIndex((r) => r.entry.path === selected);
    if (i >= 0) return i;
  }
  return Math.max(0, Math.min(rows.length - 1, previous));
}
