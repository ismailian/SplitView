/**
 * Canonical layout model (CLAUDE.md §7).
 *
 * A binary tree of splits and leaves, identical in spirit to tmux/i3. Every
 * function here is **pure** — no DOM, no IPC — so the invariants can be unit
 * tested. The renderer reads this tree and reconciles the DOM elsewhere.
 */

export type PaneId = string;

/** `row` = side-by-side (a vertical divider); `column` = stacked (horizontal). */
export type Orientation = "row" | "column";

export type Direction = "up" | "down" | "left" | "right";

export type LeafNode = { kind: "leaf"; paneId: PaneId };

export type SplitNode = {
  kind: "split";
  orientation: Orientation;
  /** Size of the first child, 0..1. */
  ratio: number;
  first: Node;
  second: Node;
};

export type Node = LeafNode | SplitNode;

export interface Layout {
  root: Node | null;
  focused: PaneId | null;
  /** Non-null when a pane is zoomed. Purely presentational. */
  zoomed: PaneId | null;
}

/** Ratios are clamped so a pane can never collapse to zero (CLAUDE.md §7). */
export const MIN_RATIO = 0.05;
export const MAX_RATIO = 0.95;

/** A normalized rectangle in 0..1 space. */
export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** Path to a node: a sequence of child indices (0 = first, 1 = second). */
export type NodePath = ReadonlyArray<0 | 1>;

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

export function emptyLayout(): Layout {
  return { root: null, focused: null, zoomed: null };
}

export function leaf(paneId: PaneId): LeafNode {
  return { kind: "leaf", paneId };
}

/** A layout containing a single pane. */
export function singleLayout(paneId: PaneId): Layout {
  return { root: leaf(paneId), focused: paneId, zoomed: null };
}

export function clampRatio(r: number): number {
  if (Number.isNaN(r)) return 0.5;
  return Math.min(MAX_RATIO, Math.max(MIN_RATIO, r));
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/** All pane ids, left-to-right / top-to-bottom (in-order). */
export function leaves(node: Node | null): PaneId[] {
  if (!node) return [];
  if (node.kind === "leaf") return [node.paneId];
  return [...leaves(node.first), ...leaves(node.second)];
}

export function hasPane(layout: Layout, paneId: PaneId): boolean {
  return leaves(layout.root).includes(paneId);
}

function firstLeaf(node: Node): PaneId {
  let cur = node;
  while (cur.kind === "split") cur = cur.first;
  return cur.paneId;
}

/** Compute the normalized rectangle of every leaf, given the root rect. */
export function computeRects(
  node: Node | null,
  rect: Rect = { x: 0, y: 0, w: 1, h: 1 },
  out: Map<PaneId, Rect> = new Map(),
): Map<PaneId, Rect> {
  if (!node) return out;
  if (node.kind === "leaf") {
    out.set(node.paneId, rect);
    return out;
  }
  if (node.orientation === "row") {
    const firstW = rect.w * node.ratio;
    computeRects(node.first, { ...rect, w: firstW }, out);
    computeRects(
      node.second,
      { x: rect.x + firstW, y: rect.y, w: rect.w - firstW, h: rect.h },
      out,
    );
  } else {
    const firstH = rect.h * node.ratio;
    computeRects(node.first, { ...rect, h: firstH }, out);
    computeRects(
      node.second,
      { x: rect.x, y: rect.y + firstH, w: rect.w, h: rect.h - firstH },
      out,
    );
  }
  return out;
}

export interface SplitInfo {
  path: NodePath;
  orientation: Orientation;
  ratio: number;
  rect: Rect;
}

/** Enumerate every split with its path and rectangle — for divider rendering. */
export function collectSplits(
  node: Node | null,
  rect: Rect = { x: 0, y: 0, w: 1, h: 1 },
  path: NodePath = [],
  out: SplitInfo[] = [],
): SplitInfo[] {
  if (!node || node.kind === "leaf") return out;
  out.push({ path, orientation: node.orientation, ratio: node.ratio, rect });
  if (node.orientation === "row") {
    const firstW = rect.w * node.ratio;
    collectSplits(node.first, { ...rect, w: firstW }, [...path, 0], out);
    collectSplits(
      node.second,
      { x: rect.x + firstW, y: rect.y, w: rect.w - firstW, h: rect.h },
      [...path, 1],
      out,
    );
  } else {
    const firstH = rect.h * node.ratio;
    collectSplits(node.first, { ...rect, h: firstH }, [...path, 0], out);
    collectSplits(
      node.second,
      { x: rect.x, y: rect.y + firstH, w: rect.w, h: rect.h - firstH },
      [...path, 1],
      out,
    );
  }
  return out;
}

// ---------------------------------------------------------------------------
// Structural transforms (all return new trees; inputs are never mutated)
// ---------------------------------------------------------------------------

/** Replace the leaf with `paneId` by running `fn` over it. */
function mapLeaf(node: Node, paneId: PaneId, fn: (l: LeafNode) => Node): Node {
  if (node.kind === "leaf") {
    return node.paneId === paneId ? fn(node) : node;
  }
  return {
    ...node,
    first: mapLeaf(node.first, paneId, fn),
    second: mapLeaf(node.second, paneId, fn),
  };
}

/**
 * Split the focused pane. `down` stacks the new pane below (a horizontal
 * divider, `column`); `right` places it to the side (a vertical divider,
 * `row`). Focus follows to the new pane, which is the keyboard-first default:
 * you split in order to start working in the new pane.
 */
export function splitFocused(
  layout: Layout,
  dir: "down" | "right",
  newPaneId: PaneId,
): Layout {
  const orientation: Orientation = dir === "down" ? "column" : "row";

  // Splitting an empty layout just creates the first pane.
  if (!layout.root || !layout.focused) {
    return singleLayout(newPaneId);
  }

  const root = mapLeaf(layout.root, layout.focused, (l) => ({
    kind: "split",
    orientation,
    ratio: 0.5,
    first: l,
    second: leaf(newPaneId),
  }));

  return { ...layout, root, focused: newPaneId };
}

function rangesOverlap(a1: number, a2: number, b1: number, b2: number): boolean {
  return Math.max(a1, b1) < Math.min(a2, b2);
}

function center(r: Rect): { x: number; y: number } {
  return { x: r.x + r.w / 2, y: r.y + r.h / 2 };
}

/** The neighbouring pane in a direction, or null if there is none. */
export function neighbor(layout: Layout, dir: Direction): PaneId | null {
  if (!layout.root || !layout.focused) return null;
  const rects = computeRects(layout.root);
  const cur = rects.get(layout.focused);
  if (!cur) return null;

  const horizontal = dir === "left" || dir === "right";
  const cc = center(cur);
  let best: PaneId | null = null;
  let bestDist = Infinity;

  for (const [id, r] of rects) {
    if (id === layout.focused) continue;
    const overlap = horizontal
      ? rangesOverlap(r.y, r.y + r.h, cur.y, cur.y + cur.h)
      : rangesOverlap(r.x, r.x + r.w, cur.x, cur.x + cur.w);
    if (!overlap) continue;

    const rc = center(r);
    let onSide = false;
    let dist = 0;
    switch (dir) {
      case "left":
        onSide = rc.x < cc.x;
        dist = cc.x - rc.x;
        break;
      case "right":
        onSide = rc.x > cc.x;
        dist = rc.x - cc.x;
        break;
      case "up":
        onSide = rc.y < cc.y;
        dist = cc.y - rc.y;
        break;
      case "down":
        onSide = rc.y > cc.y;
        dist = rc.y - cc.y;
        break;
    }
    if (!onSide) continue;
    if (dist < bestDist) {
      bestDist = dist;
      best = id;
    }
  }
  return best;
}

/** Move focus to the adjacent pane in a direction (no-op if none). */
export function focusDirection(layout: Layout, dir: Direction): Layout {
  const next = neighbor(layout, dir);
  return next ? { ...layout, focused: next } : layout;
}

function cycleFocus(layout: Layout, delta: 1 | -1): Layout {
  const ids = leaves(layout.root);
  if (ids.length === 0 || !layout.focused) return layout;
  const idx = ids.indexOf(layout.focused);
  if (idx < 0) return layout;
  const next = (idx + delta + ids.length) % ids.length;
  return { ...layout, focused: ids[next] };
}

export function focusNext(layout: Layout): Layout {
  return cycleFocus(layout, 1);
}

export function focusPrev(layout: Layout): Layout {
  return cycleFocus(layout, -1);
}

/** Swap two pane ids wherever they appear in the tree. */
function swapPanes(node: Node, a: PaneId, b: PaneId): Node {
  if (node.kind === "leaf") {
    if (node.paneId === a) return leaf(b);
    if (node.paneId === b) return leaf(a);
    return node;
  }
  return {
    ...node,
    first: swapPanes(node.first, a, b),
    second: swapPanes(node.second, a, b),
  };
}

/**
 * Swap the focused pane with its neighbour in a direction. Focus follows the
 * pane that moved.
 */
export function movePane(layout: Layout, dir: Direction): Layout {
  if (!layout.root || !layout.focused) return layout;
  const other = neighbor(layout, dir);
  if (!other) return layout;
  return { ...layout, root: swapPanes(layout.root, layout.focused, other) };
}

/**
 * Remove a node by path, promoting its sibling into the parent's place so a
 * split never ends up with a single child.
 */
function removeAtPath(node: Node, path: NodePath): Node | null {
  if (path.length === 0) return null; // removing the root
  if (node.kind === "leaf") return node; // shouldn't happen
  const [head, ...rest] = path;
  if (rest.length === 0) {
    // Drop `head`, promote the sibling.
    return head === 0 ? node.second : node.first;
  }
  const child = head === 0 ? node.first : node.second;
  const replaced = removeAtPath(child, rest);
  if (replaced === null) return head === 0 ? node.second : node.first;
  return head === 0 ? { ...node, first: replaced } : { ...node, second: replaced };
}

function pathToLeaf(node: Node, paneId: PaneId, acc: (0 | 1)[] = []): NodePath | null {
  if (node.kind === "leaf") return node.paneId === paneId ? acc : null;
  const left = pathToLeaf(node.first, paneId, [...acc, 0]);
  if (left) return left;
  return pathToLeaf(node.second, paneId, [...acc, 1]);
}

/**
 * Close an arbitrary pane by id, collapse its split, and promote the sibling.
 * If the removed pane held focus, focus moves to a leaf of the promoted
 * sibling; otherwise focus is left where it is. Zoom is cleared if the zoomed
 * pane was the one removed. Returns the layout unchanged if `paneId` isn't a
 * leaf in the tree.
 *
 * This handles panes whose shell exited on its own (`pty-exit`), which may not
 * be the focused pane — see `closeFocused` for the keyboard-driven case.
 */
export function closePane(
  layout: Layout,
  paneId: PaneId,
): {
  layout: Layout;
  removedPaneId: PaneId | null;
} {
  if (!layout.root) return { layout, removedPaneId: null };
  const path = pathToLeaf(layout.root, paneId);
  if (path === null) return { layout, removedPaneId: null };

  // Root leaf: the layout becomes empty.
  if (path.length === 0) {
    return {
      layout: { root: null, focused: null, zoomed: null },
      removedPaneId: paneId,
    };
  }

  const newRoot = removeAtPath(layout.root, path);
  // Only the focused pane closing needs to relocate focus. The sibling that
  // took this pane's slot is at the parent path.
  let nextFocus = layout.focused;
  if (layout.focused === paneId) {
    const parentPath = path.slice(0, -1);
    const sibling = nodeAtPath(newRoot, parentPath);
    nextFocus = sibling ? firstLeaf(sibling) : null;
  }

  return {
    layout: {
      root: newRoot,
      focused: nextFocus,
      zoomed: layout.zoomed === paneId ? null : layout.zoomed,
    },
    removedPaneId: paneId,
  };
}

/**
 * Close the focused pane, collapse its split, and promote the sibling. Focus
 * moves to a leaf of the promoted sibling; zoom is cleared if the zoomed pane
 * was the one removed.
 */
export function closeFocused(layout: Layout): {
  layout: Layout;
  removedPaneId: PaneId | null;
} {
  if (!layout.focused) return { layout, removedPaneId: null };
  return closePane(layout, layout.focused);
}

function nodeAtPath(node: Node | null, path: NodePath): Node | null {
  let cur = node;
  for (const step of path) {
    if (!cur || cur.kind === "leaf") return null;
    cur = step === 0 ? cur.first : cur.second;
  }
  return cur;
}

/** Toggle zoom on the focused pane. The tree shape is left untouched. */
export function toggleZoom(layout: Layout): Layout {
  if (layout.zoomed) return { ...layout, zoomed: null };
  if (!layout.focused) return layout;
  return { ...layout, zoomed: layout.focused };
}

/**
 * Resize the focused pane by nudging the nearest ancestor split with the
 * matching orientation. The divider moves in the pressed direction:
 * `right`/`down` grow the first child, `left`/`up` shrink it.
 */
export function resizeFocused(
  layout: Layout,
  dir: Direction,
  step: number,
): Layout {
  if (!layout.root || !layout.focused) return layout;
  const wantRow = dir === "left" || dir === "right";
  const path = pathToLeaf(layout.root, layout.focused);
  if (!path) return layout;

  for (let len = path.length - 1; len >= 0; len--) {
    const ancestorPath = path.slice(0, len);
    const node = nodeAtPath(layout.root, ancestorPath);
    if (
      node &&
      node.kind === "split" &&
      node.orientation === (wantRow ? "row" : "column")
    ) {
      const delta = dir === "right" || dir === "down" ? step : -step;
      return setRatio(layout, ancestorPath, node.ratio + delta);
    }
  }
  return layout;
}

/** Set the ratio of the split addressed by `path`. No-op if it isn't a split. */
export function setRatio(layout: Layout, path: NodePath, ratio: number): Layout {
  if (!layout.root) return layout;
  const target = nodeAtPath(layout.root, path);
  if (!target || target.kind !== "split") return layout;

  const clamped = clampRatio(ratio);
  const update = (node: Node, p: NodePath): Node => {
    if (p.length === 0) {
      return node.kind === "split" ? { ...node, ratio: clamped } : node;
    }
    if (node.kind === "leaf") return node;
    const [head, ...rest] = p;
    return head === 0
      ? { ...node, first: update(node.first, rest) }
      : { ...node, second: update(node.second, rest) };
  };
  return { ...layout, root: update(layout.root, path) };
}
