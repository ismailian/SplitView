import { describe, it, expect } from "vitest";
import {
  type Layout,
  type Node,
  emptyLayout,
  singleLayout,
  splitFocused,
  focusDirection,
  focusNext,
  focusPrev,
  movePane,
  closeFocused,
  closePane,
  toggleZoom,
  setRatio,
  resizeFocused,
  leaves,
  computeRects,
  collectSplits,
  MIN_RATIO,
  MAX_RATIO,
} from "./tree";

/** Assert no split node has fewer/more than two real children. */
function assertSplitInvariants(node: Node | null): void {
  if (!node || node.kind === "leaf") return;
  expect(node.first).toBeDefined();
  expect(node.second).toBeDefined();
  expect(node.ratio).toBeGreaterThanOrEqual(MIN_RATIO);
  expect(node.ratio).toBeLessThanOrEqual(MAX_RATIO);
  assertSplitInvariants(node.first);
  assertSplitInvariants(node.second);
}

describe("splitFocused", () => {
  it("creates the first pane when the layout is empty", () => {
    const l = splitFocused(emptyLayout(), "right", "a");
    expect(l.root).toEqual({ kind: "leaf", paneId: "a" });
    expect(l.focused).toBe("a");
  });

  it("split right makes a row (side-by-side) and focus follows the new pane", () => {
    const l = splitFocused(singleLayout("a"), "right", "b");
    expect(l.root?.kind).toBe("split");
    if (l.root?.kind !== "split") throw new Error("expected split");
    expect(l.root.orientation).toBe("row");
    expect(leaves(l.root)).toEqual(["a", "b"]);
    expect(l.focused).toBe("b");
  });

  it("split down makes a column (stacked)", () => {
    const l = splitFocused(singleLayout("a"), "down", "b");
    if (l.root?.kind !== "split") throw new Error("expected split");
    expect(l.root.orientation).toBe("column");
  });

  it("splits the focused leaf, not some other leaf", () => {
    let l = splitFocused(singleLayout("a"), "right", "b"); // focus b
    l = splitFocused(l, "down", "c"); // split b
    expect(leaves(l.root)).toEqual(["a", "b", "c"]);
    expect(l.focused).toBe("c");
    assertSplitInvariants(l.root);
  });
});

describe("focusDirection", () => {
  // Layout:  a | b   (row split, a left, b right)
  const ab = splitFocused(singleLayout("a"), "right", "b"); // focused = b

  it("moves focus left to the neighbour", () => {
    expect(focusDirection(ab, "left").focused).toBe("a");
  });

  it("is a no-op when there is no neighbour", () => {
    expect(focusDirection(ab, "right").focused).toBe("b");
    expect(focusDirection(ab, "up").focused).toBe("b");
  });

  it("navigates a nested grid in all four directions", () => {
    // Build a 2x2 grid:
    //   a | b
    //   --+--
    //   c | d
    let l: Layout = singleLayout("a");
    l = splitFocused(l, "right", "b"); // a | b, focus b
    l = { ...l, focused: "a" };
    l = splitFocused(l, "down", "c"); // a/c | b, focus c
    l = { ...l, focused: "b" };
    l = splitFocused(l, "down", "d"); // a/c | b/d, focus d

    const at = (id: string) => ({ ...l, focused: id });
    expect(focusDirection(at("a"), "right").focused).toBe("b");
    expect(focusDirection(at("a"), "down").focused).toBe("c");
    expect(focusDirection(at("d"), "left").focused).toBe("c");
    expect(focusDirection(at("d"), "up").focused).toBe("b");
  });
});

describe("focusNext / focusPrev", () => {
  let l: Layout = singleLayout("a");
  l = splitFocused(l, "right", "b");
  l = splitFocused(l, "right", "c"); // leaves a, b, c ; focus c

  it("cycles forward with wraparound", () => {
    const order = leaves(l.root);
    expect(order.length).toBe(3);
    const start = { ...l, focused: order[order.length - 1] };
    expect(focusNext(start).focused).toBe(order[0]); // wrap
  });

  it("cycles backward with wraparound", () => {
    const order = leaves(l.root);
    const start = { ...l, focused: order[0] };
    expect(focusPrev(start).focused).toBe(order[order.length - 1]); // wrap
  });
});

describe("movePane", () => {
  it("swaps the focused pane with its neighbour and focus follows it", () => {
    const l = splitFocused(singleLayout("a"), "right", "b"); // a | b, focus b
    const moved = movePane(l, "left"); // swap b with a
    expect(leaves(moved.root)).toEqual(["b", "a"]);
    expect(moved.focused).toBe("b"); // focus follows the moved pane
  });

  it("is a no-op when there is no neighbour", () => {
    const l = splitFocused(singleLayout("a"), "right", "b");
    expect(movePane(l, "right")).toBe(l);
  });
});

describe("closeFocused", () => {
  it("promotes the sibling and never leaves a single-child split", () => {
    let l: Layout = singleLayout("a");
    l = splitFocused(l, "right", "b"); // a | b, focus b
    const { layout, removedPaneId } = closeFocused(l);
    expect(removedPaneId).toBe("b");
    expect(layout.root).toEqual({ kind: "leaf", paneId: "a" });
    expect(layout.focused).toBe("a");
    assertSplitInvariants(layout.root);
  });

  it("keeps the rest of the tree intact when closing a nested pane", () => {
    let l: Layout = singleLayout("a");
    l = splitFocused(l, "right", "b"); // a | b
    l = { ...l, focused: "b" };
    l = splitFocused(l, "down", "c"); // a | (b/c), focus c
    const { layout, removedPaneId } = closeFocused(l);
    expect(removedPaneId).toBe("c");
    expect(leaves(layout.root)).toEqual(["a", "b"]);
    assertSplitInvariants(layout.root);
  });

  it("empties the layout when the last pane is closed", () => {
    const { layout, removedPaneId } = closeFocused(singleLayout("a"));
    expect(removedPaneId).toBe("a");
    expect(layout.root).toBeNull();
    expect(layout.focused).toBeNull();
  });

  it("clears zoom if the zoomed pane was the one removed", () => {
    let l: Layout = singleLayout("a");
    l = splitFocused(l, "right", "b"); // focus b
    l = toggleZoom(l); // zoom b
    expect(l.zoomed).toBe("b");
    const { layout } = closeFocused(l);
    expect(layout.zoomed).toBeNull();
  });
});

describe("closePane (by id)", () => {
  it("closes a non-focused pane and leaves focus untouched", () => {
    let l: Layout = singleLayout("a");
    l = splitFocused(l, "right", "b"); // a | b, focus b (stays focused)
    const { layout, removedPaneId } = closePane(l, "a"); // close the unfocused one
    expect(removedPaneId).toBe("a");
    expect(layout.root).toEqual({ kind: "leaf", paneId: "b" });
    expect(layout.focused).toBe("b"); // focus did not move
    assertSplitInvariants(layout.root);
  });

  it("relocates focus when the closed pane was focused", () => {
    let l: Layout = singleLayout("a");
    l = splitFocused(l, "right", "b"); // a | b, focus b
    const { layout, removedPaneId } = closePane(l, "b");
    expect(removedPaneId).toBe("b");
    expect(layout.focused).toBe("a");
  });

  it("is a no-op for an id that isn't in the tree", () => {
    const l = splitFocused(singleLayout("a"), "right", "b");
    const { layout, removedPaneId } = closePane(l, "zzz");
    expect(removedPaneId).toBeNull();
    expect(layout).toBe(l);
  });
});

describe("toggleZoom", () => {
  it("toggles zoom without changing the tree shape", () => {
    const l = splitFocused(singleLayout("a"), "right", "b");
    const before = JSON.stringify(l.root);
    const zoomed = toggleZoom(l);
    expect(zoomed.zoomed).toBe("b");
    expect(JSON.stringify(zoomed.root)).toBe(before);
    const restored = toggleZoom(zoomed);
    expect(restored.zoomed).toBeNull();
    expect(JSON.stringify(restored.root)).toBe(before);
  });
});

describe("setRatio", () => {
  it("clamps the ratio to a sane range", () => {
    const l = splitFocused(singleLayout("a"), "right", "b");
    expect(setRatioValue(l, 0.5)).toBeCloseTo(0.5);
    expect(setRatioValue(l, 5)).toBe(MAX_RATIO);
    expect(setRatioValue(l, -1)).toBe(MIN_RATIO);
  });

  function setRatioValue(l: Layout, r: number): number {
    const out = setRatio(l, [], r);
    if (out.root?.kind !== "split") throw new Error("expected split");
    return out.root.ratio;
  }
});

describe("resizeFocused", () => {
  it("moves the nearest matching split's divider in the pressed direction", () => {
    const l = splitFocused(singleLayout("a"), "right", "b"); // row split @0.5
    const grown = resizeFocused(l, "right", 0.1);
    if (grown.root?.kind !== "split") throw new Error("expected split");
    expect(grown.root.ratio).toBeCloseTo(0.6);
    const shrunk = resizeFocused(l, "left", 0.1);
    if (shrunk.root?.kind !== "split") throw new Error("expected split");
    expect(shrunk.root.ratio).toBeCloseTo(0.4);
  });

  it("ignores axes with no matching ancestor split", () => {
    const l = splitFocused(singleLayout("a"), "right", "b"); // only a row split
    expect(resizeFocused(l, "up", 0.1)).toBe(l);
  });
});

describe("geometry helpers", () => {
  it("computeRects partitions the unit square by ratio", () => {
    const l = setRatio(splitFocused(singleLayout("a"), "right", "b"), [], 0.3);
    const rects = computeRects(l.root);
    expect(rects.get("a")).toEqual({ x: 0, y: 0, w: 0.3, h: 1 });
    expect(rects.get("b")).toEqual({ x: 0.3, y: 0, w: 0.7, h: 1 });
  });

  it("collectSplits reports each split with its path", () => {
    let l: Layout = singleLayout("a");
    l = splitFocused(l, "right", "b");
    l = splitFocused(l, "down", "c");
    const splits = collectSplits(l.root);
    expect(splits.length).toBe(2);
    expect(splits[0].path).toEqual([]); // root split
  });
});
