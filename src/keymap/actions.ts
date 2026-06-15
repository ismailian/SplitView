/**
 * Action handlers (CLAUDE.md §9, §19).
 *
 * The keymap is data-driven: bindings map a chord to an *action name*, and this
 * table maps an action name to a behaviour. Key handling never lives inside the
 * action functions themselves. Layout-mutating actions go through
 * `layout/tree.ts`.
 */

import type { Direction } from "../layout/tree";

/** The surface the app controller exposes to actions. */
export interface AppActions {
  split(dir: "down" | "right"): void;
  focus(dir: Direction): void;
  focusNext(): void;
  move(dir: Direction): void;
  toggleZoom(): void;
  closePane(): void;
  resize(dir: Direction): void;
  reloadConfig(): void;
  /** Pick a file from the native dialog and insert its path (Win+O). */
  openFileDialog(): void;
  /** Increase / decrease / reset the terminal font size. */
  zoomIn(): void;
  zoomOut(): void;
  zoomReset(): void;
}

export type ActionHandler = (a: AppActions) => void;

export const actionHandlers: Record<string, ActionHandler> = {
  split_down: (a) => a.split("down"),
  split_right: (a) => a.split("right"),

  focus_left: (a) => a.focus("left"),
  focus_down: (a) => a.focus("down"),
  focus_up: (a) => a.focus("up"),
  focus_right: (a) => a.focus("right"),
  focus_next: (a) => a.focusNext(),

  move_left: (a) => a.move("left"),
  move_down: (a) => a.move("down"),
  move_up: (a) => a.move("up"),
  move_right: (a) => a.move("right"),

  resize_left: (a) => a.resize("left"),
  resize_down: (a) => a.resize("down"),
  resize_up: (a) => a.resize("up"),
  resize_right: (a) => a.resize("right"),

  toggle_zoom: (a) => a.toggleZoom(),
  close_pane: (a) => a.closePane(),
  reload_config: (a) => a.reloadConfig(),

  open_file_dialog: (a) => a.openFileDialog(),
  zoom_in: (a) => a.zoomIn(),
  zoom_out: (a) => a.zoomOut(),
  zoom_reset: (a) => a.zoomReset(),
};

/** Dispatch an action by name; unknown names are ignored. */
export function dispatchAction(name: string, app: AppActions): void {
  actionHandlers[name]?.(app);
}
