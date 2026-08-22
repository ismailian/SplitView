/**
 * The file sidebar: a terminal-styled filesystem tree (CLAUDE.md §1, §14).
 *
 * It opens on whatever directory the focused terminal is in (reported by the
 * shell over OSC 7) and is fully keyboard-driven — j/k to move, l/Enter to
 * expand, `c` to `cd` the terminal there, Enter on a file to insert its path at
 * the prompt. The mouse is a convenience, never a requirement (CLAUDE.md §2.1).
 *
 * Rendering is deliberately plain: monospace rows with `├──`/`└──` connectors
 * drawn as text, coloured from the theme's ANSI variables, so the sidebar reads
 * like `tree(1)` output rather than a file-manager widget. No framework — the
 * DOM is built imperatively like the rest of the app (CLAUDE.md §3).
 */

import { readDir, type DirEntryInfo } from "../ipc/commands";
import { compactPath, toShellPath } from "../paths";

export interface FileTreeDeps {
  /** Insert a path at the focused terminal's prompt (shell-quoted, no newline). */
  onInsertPath(shellPath: string): void;
  /** Run `cd <dir>` in the focused terminal. */
  onChangeDir(shellPath: string): void;
  /** Hand focus back to the terminal (Escape, or after acting on an entry). */
  onLeave(): void;
  /** The sidebar's width changed, so the pane area shrank/grew: the terminals
   * need re-fitting (and their PTYs resizing with them, CLAUDE.md §20). */
  onResize(): void;
}

/** Sidebar width bounds and default, in px. The minimum keeps the tree readable;
 * the maximum is a share of the window so the sidebar can never crowd out the
 * terminals it exists to serve. */
const MIN_WIDTH = 140;
const DEFAULT_WIDTH = 240;
const MAX_WIDTH_RATIO = 0.6;
/** How much one `<`/`>` keypress moves the edge, in px. */
const WIDTH_STEP = 20;
/** Where the chosen width persists (the sidebar owns this bit of UI state; the
 * Settings window has no business with it). */
const WIDTH_KEY = "splitview.sidebar.width";

/** One node of the tree. Children are loaded lazily, on first expand. */
interface FsNode {
  name: string;
  /** Absolute **native** path (as returned by the backend). */
  path: string;
  isDir: boolean;
  isSymlink: boolean;
  hidden: boolean;
  expanded: boolean;
  loading: boolean;
  error: string | null;
  children: FsNode[] | null;
}

/** A flattened, visible row: the node plus how to draw its connectors. */
interface Row {
  node: FsNode;
  depth: number;
  /** For each ancestor level, whether that ancestor was the last of its group
   * (decides `│  ` vs `   `); the final entry decides `├──` vs `└──`. */
  lastFlags: boolean[];
}

function nodeOf(e: DirEntryInfo): FsNode {
  return {
    name: e.name,
    path: e.path,
    isDir: e.isDir,
    isSymlink: e.isSymlink,
    hidden: e.hidden,
    expanded: false,
    loading: false,
    error: null,
    children: null,
  };
}

export class FileTree {
  /** Stable root element; the controller parents it next to the layout. */
  readonly el: HTMLElement;
  private readonly headerEl: HTMLElement;
  private readonly bodyEl: HTMLElement;
  private readonly footerEl: HTMLElement;

  private open_ = false;
  private focused = false;
  private rootPath = "";
  private parentPath: string | null = null;
  private roots: FsNode[] = [];
  private rows: Row[] = [];
  private selected = 0;
  private showHidden = false;
  private truncated = false;
  private error: string | null = null;
  /** Guards against a slow listing landing after the user re-rooted the tree. */
  private loadSeq = 0;
  /** Current width in px, restored from the last session. */
  private width = loadWidth();

  constructor(private readonly deps: FileTreeDeps) {
    this.el = document.createElement("aside");
    this.el.className = "sidebar";
    this.el.tabIndex = 0;
    this.el.style.display = "none";
    this.el.style.width = `${this.width}px`;

    this.headerEl = document.createElement("div");
    this.headerEl.className = "ft-header";
    this.bodyEl = document.createElement("div");
    this.bodyEl.className = "ft-body";
    this.footerEl = document.createElement("div");
    this.footerEl.className = "ft-footer";
    this.footerEl.textContent =
      "j/k move · l/h fold · ⏎ open · c cd · u up · . hidden · r reload · </> width · esc term";

    // Drag handle on the sidebar's edge. Mouse convenience only — `<`/`>`/`=`
    // do the same from the keyboard (CLAUDE.md §2.1, §18).
    const resizer = document.createElement("div");
    resizer.className = "ft-resizer";
    resizer.title = "Drag to resize (or < / > / = from the tree)";
    this.attachResize(resizer);

    this.el.append(this.headerEl, this.bodyEl, this.footerEl, resizer);

    this.el.addEventListener("focus", () => {
      this.focused = true;
      this.el.classList.add("focused");
    });
    this.el.addEventListener("blur", () => {
      this.focused = false;
      this.el.classList.remove("focused");
    });

    // A window that shrank can leave a previously fine width taking the whole
    // screen and squeezing the panes to nothing; re-clamp against the new size.
    window.addEventListener("resize", () => {
      if (this.open_) this.setWidth(this.width);
    });
  }

  get isOpen(): boolean {
    return this.open_;
  }

  /** True while the sidebar itself holds keyboard focus. */
  get isFocused(): boolean {
    return this.focused;
  }

  /**
   * Show the sidebar rooted at `cwd` (the focused terminal's directory; msys
   * form is fine — the backend normalizes it) and take focus. Re-opening always
   * re-roots, so toggling it is how you follow the terminal to a new directory.
   */
  open(cwd: string | null): void {
    this.open_ = true;
    this.el.style.display = "flex";
    this.setWidth(this.width); // re-clamp: the window may have shrunk since
    void this.setRoot(cwd);
    this.el.focus();
  }

  close(): void {
    this.open_ = false;
    this.el.style.display = "none";
  }

  /** Focus the sidebar without re-rooting it (it is already open). */
  focus(): void {
    this.el.focus();
  }

  // --- width -------------------------------------------------------------

  /**
   * Set the sidebar's width (clamped), and tell the controller so the terminals
   * re-fit. Not persisted here: a drag calls this once per frame, so the store
   * is written when the gesture ends (or immediately for a keypress).
   */
  private setWidth(px: number): void {
    const max = Math.max(MIN_WIDTH, window.innerWidth * MAX_WIDTH_RATIO);
    const next = Math.round(Math.max(MIN_WIDTH, Math.min(max, px)));
    if (next === this.width) return;
    this.width = next;
    this.el.style.width = `${next}px`;
    this.deps.onResize();
  }

  /** Keyboard resize (`<` / `>` / `=`), persisted right away. */
  private nudgeWidth(px: number): void {
    this.setWidth(px);
    saveWidth(this.width);
  }

  /**
   * Pointer drag on the edge handle. Moves are coalesced to one width change per
   * animation frame so a drag doesn't re-fit every terminal per mousemove
   * (CLAUDE.md §13), and the listeners live on `window` so the drag survives the
   * pointer leaving the handle's few pixels.
   */
  private attachResize(handle: HTMLElement): void {
    handle.addEventListener("pointerdown", (ev) => {
      ev.preventDefault(); // keep focus where it is; start no text selection
      handle.classList.add("active");

      const startX = ev.clientX;
      const startWidth = this.el.getBoundingClientRect().width;
      const prevCursor = document.body.style.cursor;
      const prevSelect = document.body.style.userSelect;
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";

      let frame = 0;
      let pending: number | null = null;
      const flush = (): void => {
        frame = 0;
        if (pending !== null) this.setWidth(pending);
      };
      const move = (e: PointerEvent): void => {
        pending = startWidth + (e.clientX - startX);
        if (!frame) frame = requestAnimationFrame(flush);
      };
      const up = (e: PointerEvent): void => {
        window.removeEventListener("pointermove", move);
        window.removeEventListener("pointerup", up);
        if (frame) cancelAnimationFrame(frame);
        this.setWidth(startWidth + (e.clientX - startX)); // commit the last move
        saveWidth(this.width);
        handle.classList.remove("active");
        document.body.style.cursor = prevCursor;
        document.body.style.userSelect = prevSelect;
      };
      window.addEventListener("pointermove", move);
      window.addEventListener("pointerup", up);
    });
  }

  // --- loading -----------------------------------------------------------

  private async setRoot(path: string | null): Promise<void> {
    const seq = ++this.loadSeq;
    this.error = null;
    try {
      const listing = await readDir(path);
      if (seq !== this.loadSeq) return; // superseded by a newer navigation
      this.rootPath = listing.path;
      this.parentPath = listing.parent;
      this.roots = listing.entries.map(nodeOf);
      this.truncated = listing.truncated;
      this.selected = 0;
    } catch (e) {
      if (seq !== this.loadSeq) return;
      this.rootPath = path ?? "";
      this.parentPath = null;
      this.roots = [];
      this.truncated = false;
      this.error = message(e);
    }
    this.render();
  }

  /** Load a directory node's children, then re-render in place. */
  private async loadChildren(node: FsNode): Promise<void> {
    if (node.children || node.loading) return;
    node.loading = true;
    node.error = null;
    this.render();
    try {
      const listing = await readDir(node.path);
      node.children = listing.entries.map(nodeOf);
    } catch (e) {
      node.children = [];
      node.error = message(e);
    }
    node.loading = false;
    this.render();
  }

  /** Reload the current root, dropping every expansion. */
  refresh(): void {
    void this.setRoot(this.rootPath || null);
  }

  // --- keyboard ----------------------------------------------------------

  /**
   * Handle a key while the sidebar has focus. Returns true if it was consumed;
   * anything else is left for the keymap, so the prefix still works from here
   * (CLAUDE.md §9 — the sidebar must not become a keyboard dead end).
   */
  handleKey(ev: KeyboardEvent): boolean {
    if (ev.ctrlKey || ev.altKey || ev.metaKey) return false;
    const row = this.rows[this.selected];

    switch (ev.key) {
      case "j":
      case "ArrowDown":
        this.select(this.selected + 1);
        return true;
      case "k":
      case "ArrowUp":
        this.select(this.selected - 1);
        return true;
      case "g":
      case "Home":
        this.select(0);
        return true;
      case "G":
      case "End":
        this.select(this.rows.length - 1);
        return true;
      case "PageDown":
        this.select(this.selected + 10);
        return true;
      case "PageUp":
        this.select(this.selected - 10);
        return true;
      case "l":
      case "ArrowRight":
        if (row?.node.isDir) this.expand(row.node);
        return true;
      case "h":
      case "ArrowLeft":
        this.collapseOrParent();
        return true;
      case "Enter":
        if (row) this.activate(row.node);
        return true;
      case "c":
        if (row) this.changeDir(row.node);
        return true;
      case "u":
      case "Backspace":
        this.goUp();
        return true;
      case ".":
        this.showHidden = !this.showHidden;
        this.render();
        return true;
      case "r":
        this.refresh();
        return true;
      case "<":
        this.nudgeWidth(this.width - WIDTH_STEP);
        return true;
      case ">":
        this.nudgeWidth(this.width + WIDTH_STEP);
        return true;
      case "=":
        this.nudgeWidth(DEFAULT_WIDTH);
        return true;
      case "q":
      case "Escape":
        this.deps.onLeave();
        return true;
      default:
        return false;
    }
  }

  private select(index: number): void {
    if (this.rows.length === 0) return;
    this.selected = Math.max(0, Math.min(this.rows.length - 1, index));
    this.paintSelection();
  }

  private expand(node: FsNode): void {
    if (!node.isDir) return;
    node.expanded = true;
    if (!node.children) void this.loadChildren(node);
    else this.render();
  }

  /** `h`: collapse an expanded directory, else jump to the parent row. */
  private collapseOrParent(): void {
    const row = this.rows[this.selected];
    if (!row) return;
    if (row.node.isDir && row.node.expanded) {
      row.node.expanded = false;
      this.render();
      return;
    }
    for (let i = this.selected - 1; i >= 0; i--) {
      if (this.rows[i].depth < row.depth) {
        this.select(i);
        return;
      }
    }
  }

  /** Enter: toggle a directory, or insert a file's path at the prompt. */
  private activate(node: FsNode): void {
    if (node.isDir) {
      if (node.expanded) {
        node.expanded = false;
        this.render();
      } else {
        this.expand(node);
      }
      return;
    }
    this.deps.onInsertPath(toShellPath(node.path));
  }

  /** `c`: cd the terminal into this directory (or a file's directory). */
  private changeDir(node: FsNode): void {
    const dir = node.isDir ? node.path : dirNameOf(node.path, this.rootPath);
    this.deps.onChangeDir(toShellPath(dir));
  }

  /** `u`: re-root one directory up. */
  private goUp(): void {
    if (this.parentPath) void this.setRoot(this.parentPath);
  }

  // --- rendering ---------------------------------------------------------

  private visibleChildren(nodes: FsNode[]): FsNode[] {
    return this.showHidden ? nodes : nodes.filter((n) => !n.hidden);
  }

  private flatten(nodes: FsNode[], lastFlags: boolean[], out: Row[]): void {
    const visible = this.visibleChildren(nodes);
    visible.forEach((node, i) => {
      const isLast = i === visible.length - 1;
      const flags = [...lastFlags, isLast];
      out.push({ node, depth: lastFlags.length, lastFlags: flags });
      if (node.isDir && node.expanded && node.children) {
        this.flatten(node.children, flags, out);
      }
    });
  }

  private render(): void {
    this.headerEl.replaceChildren(
      span("ft-root", compactPath(toShellPath(this.rootPath), 30)),
    );

    this.rows = [];
    this.flatten(this.roots, [], this.rows);
    if (this.selected >= this.rows.length) {
      this.selected = Math.max(0, this.rows.length - 1);
    }

    const frag = document.createDocumentFragment();
    for (const [i, row] of this.rows.entries()) {
      frag.appendChild(this.rowElement(row, i));
    }
    if (this.error) frag.appendChild(span("ft-note ft-error", this.error));
    else if (this.rows.length === 0) {
      frag.appendChild(
        span("ft-note", this.showHidden ? "empty" : "empty (. shows hidden)"),
      );
    }
    if (this.truncated) frag.appendChild(span("ft-note", "… too many entries"));

    this.bodyEl.replaceChildren(frag);
    this.paintSelection();
  }

  private rowElement(row: Row, index: number): HTMLElement {
    const el = document.createElement("div");
    el.className = "ft-row";

    // `│  ` for each ancestor still having siblings below, then the connector.
    let prefix = "";
    for (let d = 0; d < row.depth; d++) prefix += row.lastFlags[d] ? "   " : "│  ";
    prefix += row.lastFlags[row.depth] ? "└─ " : "├─ ";
    el.appendChild(span("ft-tree", prefix));

    const node = row.node;
    const classes = ["ft-name"];
    if (node.isDir) classes.push("ft-dir");
    if (node.hidden) classes.push("ft-hidden");
    if (node.isSymlink) classes.push("ft-link");
    const label = node.name + (node.isDir ? "/" : "");
    el.appendChild(span(classes.join(" "), label));

    if (node.loading) el.appendChild(span("ft-note", " …"));
    if (node.error) el.appendChild(span("ft-note ft-error", " !"));

    // Mouse convenience: click selects (and toggles a directory), double-click
    // activates a file. Everything here is reachable from the keyboard too.
    el.addEventListener("mousedown", (ev) => {
      ev.preventDefault(); // keep focus on the sidebar
      this.el.focus();
      this.select(index);
      if (node.isDir) this.activate(node);
    });
    el.addEventListener("dblclick", () => {
      if (!node.isDir) this.activate(node);
    });

    return el;
  }

  private paintSelection(): void {
    const children = this.bodyEl.children;
    for (let i = 0; i < children.length; i++) {
      children[i].classList.toggle("selected", i === this.selected);
    }
    const sel = children[this.selected];
    if (sel instanceof HTMLElement) sel.scrollIntoView({ block: "nearest" });
  }
}

/** Last chosen sidebar width, or the default if none/garbage is stored. */
function loadWidth(): number {
  try {
    const raw = Number(localStorage.getItem(WIDTH_KEY));
    if (Number.isFinite(raw) && raw >= MIN_WIDTH) return raw;
  } catch {
    // Storage unavailable; the default width is fine.
  }
  return DEFAULT_WIDTH;
}

/** Persist the width. Failures (storage disabled) are non-fatal. */
function saveWidth(px: number): void {
  try {
    localStorage.setItem(WIDTH_KEY, String(px));
  } catch {
    // Storage unavailable; the width simply won't survive a restart.
  }
}

function span(className: string, text: string): HTMLSpanElement {
  const el = document.createElement("span");
  el.className = className;
  el.textContent = text;
  return el;
}

/** The directory part of a native path, falling back to the tree's root. */
function dirNameOf(path: string, fallback: string): string {
  const cut = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return cut > 0 ? path.slice(0, cut) : fallback;
}

function message(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return "cannot read directory";
}
