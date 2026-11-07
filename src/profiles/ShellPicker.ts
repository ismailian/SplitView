/**
 * The shell-profile picker (CLAUDE.md §9): a small keyboard-driven list of the
 * shells installed on this machine.
 *
 * Two uses:
 *   - **First run** — no default has been chosen yet (a fresh install), or the
 *     chosen shell can no longer be started. Nothing is spawned until the user
 *     picks, and the pick becomes the default.
 *   - **`<prefix> p`** — open a pane running a different shell beside the
 *     focused one (Enter), or change the default for new panes (`d`).
 *
 * Like the backgrounded-panes picker it is modal while open — every key is
 * consumed, so nothing leaks into the terminal underneath — and it reuses that
 * picker's look. This module owns only the UI; the controller spawns panes and
 * persists the default.
 */

import type { ShellProfile } from "../ipc/commands";

export type PickerMode = "first-run" | "open";

export interface PickerOptions {
  mode: PickerMode;
  profiles: ShellProfile[];
  /** The current default's id, marked in the list and selected initially. */
  defaultId: string | null;
  /** Shown above the list, e.g. why the previous shell failed to start. */
  message?: string;
}

/** What the user picked, and what they want done with it. */
export interface PickerResult {
  profile: ShellProfile;
  /** `open`: a pane running it. `default`: use it for new panes from now on. */
  action: "open" | "default";
}

export class ShellPicker {
  private overlay: HTMLElement | null = null;
  private listEl: HTMLElement | null = null;
  private opts: PickerOptions | null = null;
  private selected = 0;
  private resolve: ((r: PickerResult | null) => void) | null = null;

  get isOpen(): boolean {
    return this.overlay !== null;
  }

  /**
   * Show the picker. Resolves with the choice, or `null` if it was dismissed —
   * which first-run mode never is: there, Escape takes the highlighted shell for
   * this session without saving it, so the window is never left without one.
   */
  open(opts: PickerOptions): Promise<PickerResult | null> {
    this.finish(null); // a picker already open is superseded
    this.opts = opts;
    const at = opts.profiles.findIndex((p) => p.id === opts.defaultId);
    this.selected = at >= 0 ? at : 0;
    this.overlay = this.build(opts);
    document.body.appendChild(this.overlay);
    this.renderList();
    return new Promise((resolve) => {
      this.resolve = resolve;
    });
  }

  close(): void {
    this.finish(null);
  }

  /** Handle a key while open. Every key is consumed. */
  handleKey(ev: KeyboardEvent): boolean {
    const opts = this.opts;
    if (!this.overlay || !opts) return false;
    const profile = opts.profiles[this.selected];
    const firstRun = opts.mode === "first-run";

    switch (ev.key) {
      case "Escape":
      case "q":
        if (firstRun && profile) this.finish({ profile, action: "open" });
        else if (!firstRun) this.finish(null);
        return true;
      case "j":
      case "ArrowDown":
        this.select(this.selected + 1);
        return true;
      case "k":
      case "ArrowUp":
        this.select(this.selected - 1);
        return true;
      case "Enter":
      case " ":
        if (profile) this.finish({ profile, action: firstRun ? "default" : "open" });
        return true;
      case "d":
        if (profile) this.finish({ profile, action: "default" });
        return true;
      default: {
        // 1–9 pick directly.
        const n = Number(ev.key);
        if (Number.isInteger(n) && n >= 1 && n <= opts.profiles.length) {
          this.finish({
            profile: opts.profiles[n - 1],
            action: firstRun ? "default" : "open",
          });
        }
        return true;
      }
    }
  }

  // --- internals ---------------------------------------------------------

  private finish(result: PickerResult | null): void {
    this.overlay?.remove();
    this.overlay = null;
    this.listEl = null;
    this.opts = null;
    const resolve = this.resolve;
    this.resolve = null;
    resolve?.(result);
  }

  private select(index: number): void {
    const n = this.opts?.profiles.length ?? 0;
    if (n === 0) return;
    this.selected = Math.max(0, Math.min(n - 1, index));
    this.paintSelection();
  }

  private build(opts: PickerOptions): HTMLElement {
    const firstRun = opts.mode === "first-run";
    const overlay = document.createElement("div");
    overlay.className = "sv-overlay bg-overlay";
    // First run has no "nothing" to fall back to, so the backdrop click that
    // dismisses other pickers does nothing here.
    overlay.addEventListener("click", () => {
      if (!firstRun) this.finish(null);
    });

    const win = document.createElement("div");
    win.className = "bg-window";
    win.addEventListener("click", (e) => e.stopPropagation());

    const title = document.createElement("div");
    title.className = "bg-titlebar";
    title.textContent = firstRun ? "Choose your default shell" : "Shells";
    win.append(title);

    const note = opts.message ?? (firstRun
      ? "Pick the shell new panes start with. You can change it later with the prefix then p, or in Settings → Shell."
      : undefined);
    if (note) {
      const msg = document.createElement("div");
      msg.className = "picker-message";
      if (opts.message) msg.classList.add("picker-error");
      msg.textContent = note;
      win.append(msg);
    }

    const list = document.createElement("div");
    list.className = "bg-list";
    this.listEl = list;

    const help = document.createElement("div");
    help.className = "bg-help";
    help.textContent = firstRun
      ? "j/k select · ⏎ or 1-9 use as default · esc use once"
      : "j/k select · ⏎ or 1-9 open in a new pane · d make default · esc close";

    win.append(list, help);
    overlay.appendChild(win);
    return overlay;
  }

  private renderList(): void {
    const opts = this.opts;
    if (!this.listEl || !opts) return;
    const frag = document.createDocumentFragment();
    opts.profiles.forEach((profile, i) => {
      const row = document.createElement("div");
      row.className = "bg-row";
      row.append(
        cell("bg-index", `${i + 1}`),
        cell("bg-title", profile.name),
        cell("bg-preview", profile.program),
        cell("bg-age", profile.id === opts.defaultId ? "default" : ""),
      );
      row.addEventListener("click", () => {
        this.finish({
          profile,
          action: opts.mode === "first-run" ? "default" : "open",
        });
      });
      frag.appendChild(row);
    });
    if (opts.profiles.length === 0) {
      frag.appendChild(cell("bg-empty", "no shells found"));
    }
    this.listEl.replaceChildren(frag);
    this.paintSelection();
  }

  private paintSelection(): void {
    if (!this.listEl) return;
    const rows = this.listEl.children;
    for (let i = 0; i < rows.length; i++) {
      rows[i].classList.toggle("selected", i === this.selected);
    }
  }
}

function cell(className: string, text: string): HTMLElement {
  const el = document.createElement("span");
  el.className = className;
  el.textContent = text;
  return el;
}

/**
 * The profile a new pane should run, given the saved default: that profile if
 * it is still installed, else `null` — the caller then asks (first run, or the
 * shell was uninstalled since). Pure, so the decision is testable.
 */
export function resolveDefault(
  profiles: ShellProfile[],
  defaultId: string | null,
): ShellProfile | null {
  if (defaultId === null) return null;
  return profiles.find((p) => p.id === defaultId) ?? null;
}
