/**
 * Map the layout tree to the DOM and draw the 1px dividers (CLAUDE.md §4, §12).
 *
 * Panes are positioned absolutely from the tree's computed rectangles and are
 * **re-parented, never recreated**, so xterm.js keeps its scrollback and WebGL
 * context across layout changes. Dividers are cheap and rebuilt each pass.
 */

import {
  clampRatio,
  collectSplits,
  computeRects,
  type Layout,
  type NodePath,
  type Rect,
} from "./tree";
import { ratioFromPointer } from "./divider";
import type { Pane } from "../terminal/Pane";

export interface RenderDeps {
  container: HTMLElement;
  panes: Map<string, Pane>;
  /** Called when a divider drag changes a split's ratio. */
  onRatioChange: (path: NodePath, ratio: number) => void;
}

function pct(n: number): string {
  return `${n * 100}%`;
}

function placePane(pane: Pane, rect: Rect, focused: boolean): void {
  const s = pane.el.style;
  s.position = "absolute";
  s.left = pct(rect.x);
  s.top = pct(rect.y);
  s.width = pct(rect.w);
  s.height = pct(rect.h);
  s.display = "block";
  pane.el.classList.toggle("focused", focused);
}

/** Reconcile the DOM to `layout`. Returns the panes that are currently visible. */
export function renderLayout(layout: Layout, deps: RenderDeps): void {
  const { container, panes } = deps;

  // Ensure every pane element is a child of the container exactly once.
  for (const pane of panes.values()) {
    if (pane.el.parentElement !== container) container.appendChild(pane.el);
  }

  // Remove old dividers (panes are kept).
  container
    .querySelectorAll(".divider")
    .forEach((d) => d.remove());

  if (!layout.root) {
    for (const pane of panes.values()) pane.el.style.display = "none";
    return;
  }

  // Zoom is purely presentational: show only the zoomed pane, full-bleed.
  if (layout.zoomed && panes.has(layout.zoomed)) {
    for (const [id, pane] of panes) {
      if (id === layout.zoomed) {
        placePane(pane, { x: 0, y: 0, w: 1, h: 1 }, true);
        pane.refit();
      } else {
        pane.el.style.display = "none";
      }
    }
    return;
  }

  const rects = computeRects(layout.root);
  for (const [id, pane] of panes) {
    const rect = rects.get(id);
    if (!rect) {
      pane.el.style.display = "none";
      continue;
    }
    placePane(pane, rect, id === layout.focused);
    pane.refit();
  }

  drawDividers(layout, deps);
}

function drawDividers(layout: Layout, deps: RenderDeps): void {
  const { container } = deps;
  for (const split of collectSplits(layout.root)) {
    const el = document.createElement("div");
    el.className = `divider divider-${split.orientation}`;

    if (split.orientation === "row") {
      const x = split.rect.x + split.rect.w * split.ratio;
      el.style.left = pct(x);
      el.style.top = pct(split.rect.y);
      el.style.height = pct(split.rect.h);
    } else {
      const y = split.rect.y + split.rect.h * split.ratio;
      el.style.top = pct(y);
      el.style.left = pct(split.rect.x);
      el.style.width = pct(split.rect.w);
    }

    attachDrag(el, split.path, split.orientation, split.rect, deps);
    container.appendChild(el);
  }
}

function attachDrag(
  el: HTMLElement,
  path: NodePath,
  orientation: "row" | "column",
  rect: Rect,
  deps: RenderDeps,
): void {
  el.addEventListener("pointerdown", (ev) => {
    ev.preventDefault();
    el.classList.add("active");

    // A ratio change re-renders the layout, which rebuilds *every* divider —
    // so the element we started on is detached after the first move. Listen on
    // `window` (not `el`) and skip pointer capture, so the drag survives that
    // rebuild instead of dying after one step.
    const bounds = deps.container.getBoundingClientRect();
    const cursor = orientation === "row" ? "col-resize" : "row-resize";
    const prevCursor = document.body.style.cursor;
    const prevSelect = document.body.style.userSelect;
    document.body.style.cursor = cursor;
    document.body.style.userSelect = "none";

    // Coalesce moves to one re-render per animation frame (CLAUDE.md §13).
    let frame = 0;
    let pending: number | null = null;
    const flush = (): void => {
      frame = 0;
      if (pending !== null) deps.onRatioChange(path, pending);
    };
    const ratioAt = (e: PointerEvent): number =>
      clampRatio(
        ratioFromPointer(
          rect,
          orientation,
          { x: e.clientX - bounds.left, y: e.clientY - bounds.top },
          { width: bounds.width, height: bounds.height },
        ),
      );
    const move = (e: PointerEvent): void => {
      pending = ratioAt(e);
      if (!frame) frame = requestAnimationFrame(flush);
    };
    const up = (e: PointerEvent): void => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      if (frame) cancelAnimationFrame(frame);
      deps.onRatioChange(path, ratioAt(e)); // commit the final position
      document.body.style.cursor = prevCursor;
      document.body.style.userSelect = prevSelect;
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  });
}
