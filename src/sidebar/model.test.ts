import { describe, it, expect } from "vitest";
import type { DirEntryInfo } from "../ipc/commands";
import {
  buildTree,
  connectors,
  sameEntries,
  selectionIndex,
  type Listing,
  type Row,
} from "./model";

function dir(path: string, hidden = false): DirEntryInfo {
  const name = path.slice(path.lastIndexOf("/") + 1);
  return { name, path, isDir: true, isSymlink: false, hidden };
}

function file(path: string, hidden = false): DirEntryInfo {
  return { ...dir(path, hidden), isDir: false };
}

function listing(entries: DirEntryInfo[] | null, error: string | null = null): Listing {
  return { entries, parent: null, truncated: false, error };
}

/** Rows as they read on screen. */
function drawn(rows: Row[]): string[] {
  return rows.map((r) => connectors(r) + r.entry.name);
}

describe("buildTree", () => {
  const listings = new Map<string, Listing>([
    ["/p", listing([dir("/p/.git", true), dir("/p/src"), file("/p/README.md")])],
    ["/p/src", listing([file("/p/src/main.ts"), file("/p/src/util.ts")])],
    ["/p/.git", listing([file("/p/.git/HEAD")])],
  ]);

  it("lists the root with folders closed, and watches only the root", () => {
    const { rows, shown } = buildTree("/p", listings, new Set(), false);
    expect(drawn(rows)).toEqual(["├─ src", "└─ README.md"]);
    expect(shown).toEqual(["/p"]);
  });

  it("nests an open folder's entries under it and watches that folder too", () => {
    const { rows, shown } = buildTree("/p", listings, new Set(["/p/src"]), false);
    expect(drawn(rows)).toEqual([
      "├─ src",
      "│  ├─ main.ts",
      "│  └─ util.ts",
      "└─ README.md",
    ]);
    expect(rows[0].open).toBe(true);
    expect(shown).toEqual(["/p", "/p/src"]);
  });

  it("hides hidden entries, and folders open inside them, unless asked", () => {
    const open = new Set(["/p/.git"]);
    expect(buildTree("/p", listings, open, false).shown).toEqual(["/p"]);

    const { rows, shown } = buildTree("/p", listings, open, true);
    expect(drawn(rows)).toEqual(["├─ .git", "│  └─ HEAD", "├─ src", "└─ README.md"]);
    expect(shown).toEqual(["/p", "/p/.git"]);
  });

  it("keeps a closed folder's open subfolders out of the rows and the watch set", () => {
    // `/p/src/ui` stays remembered as open, but `/p/src` itself is closed.
    const { rows, shown } = buildTree("/p", listings, new Set(["/p/src/ui"]), false);
    expect(rows).toHaveLength(2);
    expect(shown).toEqual(["/p"]);
  });

  it("marks open folders that are loading or unreadable, and watches only readable ones", () => {
    const partial = new Map<string, Listing>([
      ["/p", listing([dir("/p/lib"), dir("/p/secret")])],
      ["/p/secret", listing([], "permission denied")],
    ]);
    const { rows, shown } = buildTree("/p", partial, new Set(["/p/lib", "/p/secret"]), false);
    expect(rows.map((r) => r.status)).toEqual(["loading", "error"]);
    // `/p/lib` is watched before its first read lands, so no change is missed.
    expect(shown).toEqual(["/p", "/p/lib"]);
  });

  it("draws no rail under the last folder", () => {
    const tail = new Map<string, Listing>([
      ["/p", listing([dir("/p/a"), dir("/p/b")])],
      ["/p/b", listing([file("/p/b/x")])],
    ]);
    const { rows } = buildTree("/p", tail, new Set(["/p/b"]), false);
    expect(drawn(rows)).toEqual(["├─ a", "└─ b", "   └─ x"]);
  });

  it("shows nothing for an unresolved root, but asks to watch an unread one", () => {
    expect(buildTree("", listings, new Set(), false)).toEqual({ rows: [], shown: [] });
    expect(buildTree("/q", listings, new Set(), false)).toEqual({ rows: [], shown: ["/q"] });
  });
});

describe("sameEntries", () => {
  const a = [dir("/p/src"), file("/p/x")];

  it("matches an identical re-read", () => {
    expect(sameEntries(a, [dir("/p/src"), file("/p/x")])).toBe(true);
  });

  it("notices additions, renames and flag flips", () => {
    expect(sameEntries(null, [])).toBe(false);
    expect(sameEntries(a, [...a, file("/p/y")])).toBe(false);
    expect(sameEntries(a, [dir("/p/src"), file("/p/z")])).toBe(false);
    expect(sameEntries(a, [dir("/p/src"), file("/p/x", true)])).toBe(false);
  });
});

describe("selectionIndex", () => {
  const rowsOf = (...paths: string[]): Row[] =>
    buildTree("/p", new Map([["/p", listing(paths.map((p) => file(p)))]]), new Set(), false)
      .rows;

  it("stays on the same entry when rows appear above it", () => {
    expect(selectionIndex(rowsOf("/p/a", "/p/b", "/p/c"), "/p/c", 1)).toBe(2);
  });

  it("keeps the position when the selected entry disappears", () => {
    expect(selectionIndex(rowsOf("/p/a", "/p/c"), "/p/b", 1)).toBe(1);
    expect(selectionIndex(rowsOf("/p/a"), "/p/b", 1)).toBe(0);
  });

  it("is 0 for an empty tree", () => {
    expect(selectionIndex([], "/p/a", 3)).toBe(0);
  });
});
