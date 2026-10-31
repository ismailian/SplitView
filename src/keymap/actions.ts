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
  /**
   * Close a pane by id with **no** confirmation dialog. The keymap never calls
   * this — `closePane` is its entry point, and it confirms first — but a remote
   * client must not be able to leave a modal on an unattended laptop, and it
   * does its own confirmation (`docs/remote-control-desktop.md` §5.4). No new
   * binding is added, so the remote still cannot reach anything the keyboard
   * cannot.
   */
  closePaneConfirmed(paneId: string): void;
  /** Pop the focused pane out into its own window (main window only). */
  detach(): void;
  /** Send a detached pane back into the main window (detached window only). */
  reattach(): void;
  /** Open a new top-level window with its own layout and shell. */
  newWindow(): void;
  /** Show/hide the file sidebar, rooted at the focused terminal's directory. */
  toggleSidebar(): void;
  /** Show/hide the editor panel on the right. */
  toggleEditor(): void;
  /** Push the focused pane out of the layout, leaving its shell running. */
  backgroundPane(): void;
  /** Open the picker listing backgrounded panes (restore / kill). */
  showBackground(): void;
  resize(dir: Direction): void;
  reloadConfig(): void;
  /** Pick a file from the native dialog and insert its path (Win+O). */
  openFileDialog(): void;
  /** Increase / decrease / reset the terminal font size. */
  zoomIn(): void;
  zoomOut(): void;
  zoomReset(): void;
  /** Open the Settings window. */
  openSettings(): void;
}

/** Human-readable label + grouping for each action, shown in Settings. */
export const ACTION_INFO: Record<string, { label: string; category: string }> = {
  split_down: { label: "Split horizontally (stacked)", category: "Panes" },
  split_right: { label: "Split vertically (side by side)", category: "Panes" },
  toggle_zoom: { label: "Toggle zoom (maximize pane)", category: "Panes" },
  close_pane: { label: "Close focused pane", category: "Panes" },
  detach_pane: { label: "Detach pane into its own window", category: "Panes" },
  reattach_pane: { label: "Reattach pane to the main window", category: "Panes" },
  background_pane: { label: "Background pane (keep it running)", category: "Panes" },
  show_background: { label: "Show backgrounded panes", category: "Panes" },

  focus_left: { label: "Focus pane left", category: "Focus" },
  focus_down: { label: "Focus pane down", category: "Focus" },
  focus_up: { label: "Focus pane up", category: "Focus" },
  focus_right: { label: "Focus pane right", category: "Focus" },
  focus_next: { label: "Cycle focus to next pane", category: "Focus" },

  move_left: { label: "Move pane left", category: "Move / Resize" },
  move_down: { label: "Move pane down", category: "Move / Resize" },
  move_up: { label: "Move pane up", category: "Move / Resize" },
  move_right: { label: "Move pane right", category: "Move / Resize" },
  resize_left: { label: "Resize pane left", category: "Move / Resize" },
  resize_down: { label: "Resize pane down", category: "Move / Resize" },
  resize_up: { label: "Resize pane up", category: "Move / Resize" },
  resize_right: { label: "Resize pane right", category: "Move / Resize" },

  zoom_in: { label: "Increase font size", category: "Font" },
  zoom_out: { label: "Decrease font size", category: "Font" },
  zoom_reset: { label: "Reset font size", category: "Font" },

  open_file_dialog: { label: "Insert file path (picker)", category: "App" },
  open_settings: { label: "Open Settings", category: "App" },
  reload_config: { label: "Reload config", category: "App" },
  new_window: { label: "Open a new window", category: "App" },
  toggle_sidebar: { label: "Toggle file sidebar", category: "App" },
  toggle_editor: { label: "Toggle editor panel", category: "App" },
};

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
  detach_pane: (a) => a.detach(),
  reattach_pane: (a) => a.reattach(),
  background_pane: (a) => a.backgroundPane(),
  show_background: (a) => a.showBackground(),
  new_window: (a) => a.newWindow(),
  toggle_sidebar: (a) => a.toggleSidebar(),
  toggle_editor: (a) => a.toggleEditor(),
  reload_config: (a) => a.reloadConfig(),

  open_file_dialog: (a) => a.openFileDialog(),
  zoom_in: (a) => a.zoomIn(),
  zoom_out: (a) => a.zoomOut(),
  zoom_reset: (a) => a.zoomReset(),
  open_settings: (a) => a.openSettings(),
};

/** Dispatch an action by name; unknown names are ignored. */
export function dispatchAction(name: string, app: AppActions): void {
  actionHandlers[name]?.(app);
}
