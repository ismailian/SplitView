/**
 * Backgrounded panes: the corner indicator + the picker overlay (CLAUDE.md §1).
 *
 * A backgrounded pane is one the user pushed out of the layout without killing
 * it — a dev server that should keep running but shouldn't take up screen space.
 * Its PTY, terminal and scrollback all stay alive in the controller; only its
 * place in the layout tree is gone.
 *
 * Two affordances, because an invisible running process is a trap:
 *   - a small always-on indicator showing how many panes are backgrounded, and
 *   - a keyboard-driven picker (`<prefix> B`) listing them with a live preview
 *     of each one's last output line, from which they are restored or killed.
 *
 * This module owns only the UI; the controller owns the panes themselves.
 */

export interface BackgroundEntry {
  id: string;
  /** Terminal title, or the shell's directory — whatever names it best. */
  title: string;
  /** Last non-empty output line, so a dev server is recognizable at a glance. */
  preview: string;
  /** When it was backgrounded (`Date.now()`), for the age column. */
  since: number;
}

export interface BackgroundPanelDeps {
  /** Snapshot of the backgrounded panes, newest last. */
  list(): BackgroundEntry[];
  /** Bring one back into the layout beside the focused pane. */
  restore(id: string): void;
  /** Kill its shell for good. */
  kill(id: string): void;
  /** Hand focus back to the terminal when the picker closes. */
  onClose(): void;
}

export class BackgroundPanel {
  /** The corner indicator; always mounted, hidden while nothing is backgrounded. */
  readonly indicator: HTMLElement;
  private overlay: HTMLElement | null = null;
  private listEl: HTMLElement | null = null;
  private entries: BackgroundEntry[] = [];
  private selected = 0;

  constructor(private readonly deps: BackgroundPanelDeps) {
    this.indicator = document.createElement("button");
    this.indicator.className = "bg-indicator";
    this.indicator.style.display = "none";
    this.indicator.title = "Backgrounded panes";
    this.indicator.addEventListener("click", () => this.open());
    this.update();
  }

  get isOpen(): boolean {
    return this.overlay !== null;
  }

  /** Re-read the count and refresh the indicator (and the list, if open). */
  update(): void {
    const n = this.deps.list().length;
    this.indicator.style.display = n > 0 ? "block" : "none";
    this.indicator.textContent = `▪ ${n} backgrounded`;
    if (this.overlay) this.renderList();
  }

  open(): void {
    if (this.overlay) return;
    this.selected = 0;
    this.overlay = this.build();
    document.body.appendChild(this.overlay);
    this.renderList();
  }

  close(): void {
    if (!this.overlay) return;
    this.overlay.remove();
    this.overlay = null;
    this.listEl = null;
    this.deps.onClose();
  }

  toggle(): void {
    if (this.overlay) this.close();
    else this.open();
  }

  /**
   * Handle a key while the picker is open. Every key is consumed — the picker is
   * modal — so the terminal never sees stray navigation keystrokes.
   */
  handleKey(ev: KeyboardEvent): boolean {
    if (!this.overlay) return false;
    const entry = this.entries[this.selected];

    switch (ev.key) {
      case "Escape":
      case "q":
        this.close();
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
        if (entry) {
          this.close();
          this.deps.restore(entry.id);
        }
        return true;
      case "x":
        if (entry) {
          this.deps.kill(entry.id);
          this.update();
          if (this.entries.length === 0) this.close();
        }
        return true;
      default: {
        // 1–9 restore directly, so a two-keystroke round trip is possible.
        const n = Number(ev.key);
        if (Number.isInteger(n) && n >= 1 && n <= this.entries.length) {
          const target = this.entries[n - 1];
          this.close();
          this.deps.restore(target.id);
        }
        return true;
      }
    }
  }

  // --- internals ---------------------------------------------------------

  private select(index: number): void {
    if (this.entries.length === 0) return;
    this.selected = Math.max(0, Math.min(this.entries.length - 1, index));
    this.paintSelection();
  }

  private build(): HTMLElement {
    const overlay = document.createElement("div");
    // Reuse the Settings overlay's backdrop + palette variables so both windows
    // look like the same app (CLAUDE.md §14).
    overlay.className = "sv-overlay bg-overlay";
    overlay.addEventListener("click", () => this.close());

    const win = document.createElement("div");
    win.className = "bg-window";
    win.addEventListener("click", (e) => e.stopPropagation());

    const title = document.createElement("div");
    title.className = "bg-titlebar";
    title.textContent = "Backgrounded panes";

    const list = document.createElement("div");
    list.className = "bg-list";
    this.listEl = list;

    const help = document.createElement("div");
    help.className = "bg-help";
    help.textContent = "j/k select · ⏎ restore · 1-9 restore · x kill · esc close";

    win.append(title, list, help);
    overlay.appendChild(win);
    return overlay;
  }

  private renderList(): void {
    if (!this.listEl) return;
    this.entries = this.deps.list();
    if (this.selected >= this.entries.length) {
      this.selected = Math.max(0, this.entries.length - 1);
    }

    const frag = document.createDocumentFragment();
    this.entries.forEach((entry, i) => {
      const row = document.createElement("div");
      row.className = "bg-row";
      row.append(
        cell("bg-index", `${i + 1}`),
        cell("bg-title", entry.title),
        cell("bg-preview", entry.preview),
        cell("bg-age", age(Date.now() - entry.since)),
      );
      row.addEventListener("click", () => {
        this.close();
        this.deps.restore(entry.id);
      });
      frag.appendChild(row);
    });
    if (this.entries.length === 0) {
      frag.appendChild(cell("bg-empty", "nothing is backgrounded"));
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

/** Compact age: `12s`, `4m`, `2h11m`. */
function age(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  return `${Math.floor(m / 60)}h${m % 60}m`;
}
