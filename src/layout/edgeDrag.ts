/**
 * Pointer drag on a side panel's edge handle, shared by the file sidebar and the
 * editor panel. Moves are coalesced to one width change per animation frame so a
 * drag doesn't re-fit every terminal per mousemove (CLAUDE.md §13), and the
 * listeners live on `window` so the drag survives the pointer leaving the
 * handle's few pixels.
 */

export interface EdgeDrag {
  /** The panel's width when the drag starts, in px. */
  width(): number;
  /** Which way widens the panel: 1 if its edge is on the right, -1 if on the left. */
  sign: 1 | -1;
  /** Apply a width. Called at most once per frame, and once more on release. */
  resize(px: number): void;
  /** The gesture ended; persist the result. */
  done(): void;
}

export function attachEdgeDrag(handle: HTMLElement, drag: EdgeDrag): void {
  handle.addEventListener("pointerdown", (ev) => {
    ev.preventDefault(); // keep focus where it is; start no text selection
    handle.classList.add("active");

    const startX = ev.clientX;
    const startWidth = drag.width();
    const widthAt = (x: number): number => startWidth + drag.sign * (x - startX);
    const prevCursor = document.body.style.cursor;
    const prevSelect = document.body.style.userSelect;
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";

    let frame = 0;
    let pending: number | null = null;
    const flush = (): void => {
      frame = 0;
      if (pending !== null) drag.resize(pending);
    };
    const move = (e: PointerEvent): void => {
      pending = widthAt(e.clientX);
      if (!frame) frame = requestAnimationFrame(flush);
    };
    const up = (e: PointerEvent): void => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      if (frame) cancelAnimationFrame(frame);
      drag.resize(widthAt(e.clientX)); // commit the last move
      drag.done();
      handle.classList.remove("active");
      document.body.style.cursor = prevCursor;
      document.body.style.userSelect = prevSelect;
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  });
}
