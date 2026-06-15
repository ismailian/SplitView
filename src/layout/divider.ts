/**
 * Geometry for the draggable 1px separators (CLAUDE.md §14).
 *
 * The conversion from a pointer position to a new split ratio is pure so it can
 * be reasoned about and tested. DOM wiring lives in `render.ts`.
 */

import type { Orientation, Rect } from "./tree";

/**
 * Given a split's rectangle (in 0..1 space) and the pointer position within the
 * container, return the new ratio (size of the first child). Caller is expected
 * to clamp via `clampRatio`.
 */
export function ratioFromPointer(
  rect: Rect,
  orientation: Orientation,
  pointer: { x: number; y: number },
  container: { width: number; height: number },
): number {
  if (orientation === "row") {
    const localX = pointer.x / container.width - rect.x;
    return rect.w === 0 ? 0.5 : localX / rect.w;
  }
  const localY = pointer.y / container.height - rect.y;
  return rect.h === 0 ? 0.5 : localY / rect.h;
}
