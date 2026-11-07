/**
 * The file sidebar: a terminal-styled filesystem tree (CLAUDE.md §1, §14).
 *
 * It follows the focused terminal. Whichever pane has focus, the tree shows that
 * pane's view, rooted at its shell's directory (reported over OSC 7), and
 * re-roots when the shell `cd`s. Each pane keeps its own view — open folders,
 * cursor, scroll position — for as long as the app runs, so switching away and
 * back finds the tree as it was left.
 *
 * It is live. While open, the directories on screen are watched by the backend
 * (`fs_watch*`, see `src-tauri/src/watch.rs`) and re-read when an entry in them
 * is created, removed or renamed — from the terminal or anywhere else. The cost
 * stays proportional to what is visible: only displayed directories are watched,
 * only the ones that changed are re-read, a re-read that changed nothing renders
 * nothing, and a render rebuilds only the rows that differ.
 *
 * Fully keyboard-driven — j/k to move, l/Enter to expand, `c` to `cd` the
 * terminal there, Enter on a file to insert its path at the prompt, `e` to open
 * it in the editor panel. The mouse is a convenience, never a requirement
 * (CLAUDE.md §2.1).
 *
 * Rendering is deliberately plain: monospace rows with `├──`/`└──` connectors
 * drawn as text, coloured from the theme's ANSI variables, so the sidebar reads
 * like `tree(1)` output rather than a file-manager widget. No framework — the
 * DOM is built imperatively like the rest of the app (CLAUDE.md §3).
 */

import {
  fsUnwatch,
  fsWatch,
  fsWatchDirs,
  readDir,
  type DirEntryInfo,
  type DirListing,
} from "../ipc/commands";
import { attachEdgeDrag } from "../layout/edgeDrag";
import { compactPath, toShellPath } from "../paths";
import {
  buildTree,
  connectors,
  sameEntries,
  selectionIndex,
  type Listing,
  type Row,
} from "./model";

export interface FileTreeDeps {
  /** Insert a (native) path at the focused terminal's prompt; the controller
   * converts and quotes it for that pane's shell. No newline. */
  onInsertPath(nativePath: string): void;
  /** Open a file in the editor panel. */
  onOpenFile(path: string): void;
  /** Run `cd <dir>` (a native path) in the focused terminal. */
  onChangeDir(nativeDir: string): void;
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
/** Listings kept before those no view refers to any more are dropped. */
const CACHE_LIMIT = 256;

/** A directory's listing plus the bookkeeping for re-reading it. */
interface DirState extends Listing {
  /** A read is in flight. */
  loading: boolean;
  /** A change arrived during that read, so read once more when it lands. */
  again: boolean;
  /** `FileTree.tick` when the latest read started. */
  started: number;
  /** `FileTree.tick` of the read whose result is stored; an older read that
   * lands late is discarded. */
  filled: number;
}

/** One pane's view of the tree, kept while the app runs. */
interface View {
  /** The cwd this view was last rooted at, as the shell reported it — so the
   * same directory reported again at the next prompt doesn't undo moving the
   * root with `u`. `undefined` until first rooted. */
  cwd: string | null | undefined;
  /** The directory at the top (native path), "" until the first read lands. */
  root: string;
  /** Open folders by path. Kept when an ancestor closes or the root moves, so
   * they come back open. */
  open: Set<string>;
  /** The entry under the cursor, by path, so it survives rows changing above it. */
  selected: string | null;
  scrollTop: number;
  /** Discards a slow root read that a newer one superseded. */
  seq: number;
}

export class FileTree {
  /** Stable root element; the controller parents it next to the layout. */
  readonly el: HTMLElement;
  private readonly rootEl: HTMLElement;
  private readonly bodyEl: HTMLElement;
  private readonly footerEl: HTMLElement;

  private open_ = false;
  private focused = false;
  private showHidden = false;
  /** Current width in px, restored from the last session. */
  private width = loadWidth();

  /** Listings by directory, shared by every view. */
  private readonly dirs = new Map<string, DirState>();
  /** Views by pane id. */
  private readonly views = new Map<string, View>();
  /** The view on screen, if any. */
  private view: View | null = null;
  /** The pane (and its cwd) to show — recorded while closed, applied on open. */
  private target: { key: string; cwd: string | null } = { key: "", cwd: null };

  private rows: Row[] = [];
  private index = 0;
  /** Directories on screen at the last render. */
  private shown: string[] = [];
  /** Row elements by entry path, with the text they were drawn from, so a
   * render reuses every row that looks the same. */
  private rowEls = new Map<string, { el: HTMLElement; sig: string }>();
  private selectedEl: HTMLElement | null = null;
  private renderFrame = 0;
  /** Monotonic counter ordering reads and watch requests (see `DirState`). */
  private tick = 0;

  /** Watch IPC runs strictly in order: start, set, stop can't cross. */
  private watchOps: Promise<void> = Promise.resolve();
  /** The watch set the screen wants, and the last one the backend accepted
   * (as `\n`-joined keys, null for none). */
  private wantDirs: string[] = [];
  private wantKey: string | null = null;
  private sentKey: string | null = null;
  private watched = new Set<string>();
  private pumpQueued = false;

  constructor(private readonly deps: FileTreeDeps) {
    this.el = document.createElement("aside");
    this.el.className = "sidebar";
    this.el.tabIndex = 0;
    this.el.style.display = "none";
    this.el.style.width = `${this.width}px`;

    const header = document.createElement("div");
    header.className = "ft-header";
    this.rootEl = span("ft-root", "");
    header.appendChild(this.rootEl);
    this.bodyEl = document.createElement("div");
    this.bodyEl.className = "ft-body";
    this.footerEl = document.createElement("div");
    this.footerEl.className = "ft-footer";
    this.footerEl.textContent =
      "j/k move · l/h fold · ⏎ insert · e edit · c cd · u up · . hidden · r reload · </> width · esc term";

    // Drag handle on the sidebar's edge. Mouse convenience only — `<`/`>`/`=`
    // do the same from the keyboard (CLAUDE.md §2.1, §18).
    const resizer = document.createElement("div");
    resizer.className = "ft-resizer";
    resizer.title = "Drag to resize (or < / > / = from the tree)";
    attachEdgeDrag(resizer, {
      width: () => this.el.getBoundingClientRect().width,
      sign: 1,
      resize: (px) => this.setWidth(px),
      done: () => saveWidth(this.width),
    });

    this.el.append(header, this.bodyEl, this.footerEl, resizer);

    this.el.addEventListener("focus", () => {
      this.focused = true;
      this.el.classList.add("focused");
    });
    this.el.addEventListener("blur", () => {
      this.focused = false;
      this.el.classList.remove("focused");
    });

    // Mouse convenience: click selects (and toggles a folder), double-click
    // opens a file in the editor. One listener for every row, so rows carry no
    // handlers of their own. All of it is reachable from the keyboard too.
    this.bodyEl.addEventListener("mousedown", (ev) => {
      const row = this.rowAt(ev.target);
      if (!row) return;
      ev.preventDefault(); // keep focus on the sidebar
      this.el.focus();
      this.select(this.rows.indexOf(row));
      if (row.entry.isDir) this.activate(row);
    });
    this.bodyEl.addEventListener("dblclick", (ev) => {
      const row = this.rowAt(ev.target);
      if (row && !row.entry.isDir) this.deps.onOpenFile(row.entry.path);
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
   * Show the sidebar on pane `key`'s view, rooted at that shell's `cwd` (msys
   * form is fine — the backend normalizes it), start watching, and take focus.
   */
  open(key: string | null, cwd: string | null): void {
    this.target = { key: key ?? "", cwd };
    if (!this.open_) {
      this.open_ = true;
      this.el.style.display = "flex";
      this.setWidth(this.width); // re-clamp: the window may have shrunk since
      this.enqueueWatch(async () => {
        await fsWatch((dirs) => this.onDirsChanged(dirs)).catch(() => {});
        this.resetWatched();
      });
    }
    this.sync();
    this.el.focus();
  }

  /** Hide the sidebar and release its watch; every view is kept for next time. */
  close(): void {
    if (!this.open_) return;
    this.open_ = false;
    this.el.style.display = "none";
    if (this.view) this.view.scrollTop = this.bodyEl.scrollTop;
    this.view = null;
    this.wantKey = null;
    this.enqueueWatch(async () => {
      await fsUnwatch().catch(() => {});
      this.resetWatched();
    });
  }

  /** Focus the sidebar without re-rooting it (it is already open). */
  focus(): void {
    this.el.focus();
  }

  /**
   * The focused pane changed, or its shell changed directory: show that pane's
   * view. Cheap when neither did, so the controller can call it after every
   * layout change. While closed it only records the target.
   */
  follow(key: string | null, cwd: string | null): void {
    this.target = { key: key ?? "", cwd };
    if (this.open_) this.sync();
  }

  /** A pane is gone for good (closed, killed, detached): drop its view. */
  forget(key: string): void {
    const view = this.views.get(key);
    if (!view) return;
    this.views.delete(key);
    view.seq++; // its in-flight root read has nowhere to land
    if (view === this.view) this.view = null;
  }

  /** Bring the screen in line with `target`. */
  private sync(): void {
    const { key, cwd } = this.target;
    let view = this.views.get(key);
    if (!view) {
      view = {
        cwd: undefined,
        root: "",
        open: new Set(),
        selected: null,
        scrollTop: 0,
        seq: 0,
      };
      this.views.set(key, view);
    }
    if (view !== this.view) {
      if (this.view) this.view.scrollTop = this.bodyEl.scrollTop;
      this.view = view;
      this.render();
      this.bodyEl.scrollTop = view.scrollTop;
    }
    if (cwd !== view.cwd) {
      view.cwd = cwd;
      this.reroot(view, cwd);
    }
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

  // --- loading -----------------------------------------------------------

  private dir(path: string): DirState {
    let state = this.dirs.get(path);
    if (!state) {
      state = {
        entries: null,
        parent: null,
        truncated: false,
        error: null,
        loading: false,
        again: false,
        started: -1,
        filled: -1,
      };
      this.dirs.set(path, state);
    }
    return state;
  }

  /**
   * Root `view` at `path` — a shell cwd, or a native path when moving up. The
   * root's native spelling is only known once the backend has read it, so the
   * listing is filed under what it reports.
   */
  private reroot(view: View, path: string | null): void {
    const seq = ++view.seq;
    const tick = this.tick++;
    const moveTo = (root: string): void => {
      if (view.root !== root) {
        view.root = root;
        view.selected = null;
        view.scrollTop = 0;
        if (view === this.view) this.bodyEl.scrollTop = 0;
      }
      if (view === this.view) this.render();
    };
    readDir(path).then(
      (listing) => {
        if (seq !== view.seq) return; // superseded by a newer navigation
        const state = this.dir(listing.path);
        state.started = Math.max(state.started, tick);
        this.fill(state, listing, tick);
        moveTo(listing.path);
      },
      (e: unknown) => {
        if (seq !== view.seq) return;
        const root = path ?? "";
        this.fail(this.dir(root), message(e), tick);
        moveTo(root);
      },
    );
  }

  /**
   * (Re-)read one directory. A request while a read is in flight queues exactly
   * one more read, however many changes arrive meanwhile.
   */
  private load(path: string): void {
    const state = this.dir(path);
    if (state.loading) {
      state.again = true;
      return;
    }
    state.loading = true;
    const tick = (state.started = this.tick++);
    readDir(path)
      .then(
        (listing) => this.fill(state, listing, tick),
        (e: unknown) => this.fail(state, message(e), tick),
      )
      .then((changed) => {
        if (changed) this.scheduleRender();
      })
      .finally(() => {
        state.loading = false;
        if (state.again) {
          state.again = false;
          this.load(path);
        }
      });
  }

  /** Store a read unless a newer one already landed; true if the display changes. */
  private fill(state: DirState, listing: DirListing, tick: number): boolean {
    if (tick < state.filled) return false;
    state.filled = tick;
    const changed =
      state.error !== null ||
      state.truncated !== listing.truncated ||
      !sameEntries(state.entries, listing.entries);
    state.parent = listing.parent;
    if (changed) {
      state.entries = listing.entries;
      state.truncated = listing.truncated;
      state.error = null;
    }
    return changed;
  }

  /** Record a failed read (the directory went away, or can't be read). */
  private fail(state: DirState, error: string, tick: number): boolean {
    if (tick < state.filled) return false;
    state.filled = tick;
    const changed = state.error !== error;
    state.entries = [];
    state.truncated = false;
    state.error = error;
    return changed;
  }

  /** `r`: re-read everything on screen (and retry an unreadable root). */
  refresh(): void {
    const view = this.view;
    if (!view) return;
    const root = this.dirs.get(view.root);
    if (!root || root.error) this.reroot(view, view.root || view.cwd || null);
    for (const dir of this.shown) this.load(dir);
  }

  // --- watching ----------------------------------------------------------

  private enqueueWatch(op: () => Promise<void>): void {
    this.watchOps = this.watchOps.then(op).catch(() => {});
  }

  /** The backend's watch set is empty (a watch just started or stopped). */
  private resetWatched(): void {
    this.watched = new Set();
    this.sentKey = null;
  }

  /** The screen shows `dirs`: make that the watch set. Coalesced — a burst of
   * renders sends one update, with whatever the screen shows by then. */
  private wantWatch(dirs: string[]): void {
    const key = dirs.join("\n");
    if (key === this.wantKey) return;
    this.wantKey = key;
    this.wantDirs = dirs;
    if (this.pumpQueued) return;
    this.pumpQueued = true;
    this.enqueueWatch(() => this.pumpWatch());
  }

  private async pumpWatch(): Promise<void> {
    this.pumpQueued = false;
    const key = this.wantKey;
    const dirs = this.wantDirs;
    if (!this.open_ || key === null || key === this.sentKey) return;
    const tick = this.tick++;
    await fsWatchDirs(dirs).catch(() => {});
    const before = this.watched;
    this.watched = new Set(dirs);
    this.sentKey = key;
    // A directory that just came into view wasn't watched a moment ago, so its
    // cached listing may be stale — unless a read started after the watch did.
    for (const dir of dirs) {
      const state = this.dirs.get(dir);
      if (!before.has(dir) && (!state || state.started < tick)) this.load(dir);
    }
  }

  /** The backend saw entries change in these directories. */
  private onDirsChanged(dirs: string[]): void {
    if (!this.open_) return;
    for (const dir of dirs) if (this.watched.has(dir)) this.load(dir);
  }

  // --- keyboard ----------------------------------------------------------

  /**
   * Handle a key while the sidebar has focus. Returns true if it was consumed;
   * anything else is left for the keymap, so the prefix still works from here
   * (CLAUDE.md §9 — the sidebar must not become a keyboard dead end).
   */
  handleKey(ev: KeyboardEvent): boolean {
    if (ev.ctrlKey || ev.altKey || ev.metaKey) return false;
    const row = this.rows[this.index];

    switch (ev.key) {
      case "j":
      case "ArrowDown":
        this.select(this.index + 1);
        return true;
      case "k":
      case "ArrowUp":
        this.select(this.index - 1);
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
        this.select(this.index + 10);
        return true;
      case "PageUp":
        this.select(this.index - 10);
        return true;
      case "l":
      case "ArrowRight":
        if (row) this.setOpen(row.entry, true);
        return true;
      case "h":
      case "ArrowLeft":
        this.collapseOrParent();
        return true;
      case "Enter":
        if (row) this.activate(row);
        return true;
      case "e":
        if (row && !row.entry.isDir) this.deps.onOpenFile(row.entry.path);
        return true;
      case "c":
        if (row) this.changeDir(row.entry);
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
    if (this.rows.length === 0 || !this.view) return;
    this.index = Math.max(0, Math.min(this.rows.length - 1, index));
    this.view.selected = this.rows[this.index].entry.path;
    this.paintSelection(true);
  }

  /** Open or close a folder. Opening one that isn't cached shows it loading;
   * the read follows once it is watched (see `pumpWatch`). */
  private setOpen(entry: DirEntryInfo, open: boolean): void {
    const view = this.view;
    if (!view || !entry.isDir || open === view.open.has(entry.path)) return;
    if (open) view.open.add(entry.path);
    else view.open.delete(entry.path);
    this.render();
  }

  /** `h`: close an open folder, else jump to the parent row. */
  private collapseOrParent(): void {
    const row = this.rows[this.index];
    if (!row) return;
    if (row.open) {
      this.setOpen(row.entry, false);
      return;
    }
    for (let i = this.index - 1; i >= 0; i--) {
      if (this.rows[i].depth < row.depth) {
        this.select(i);
        return;
      }
    }
  }

  /** Enter: toggle a folder, or insert a file's path at the prompt. */
  private activate(row: Row): void {
    if (row.entry.isDir) this.setOpen(row.entry, !row.open);
    else this.deps.onInsertPath(row.entry.path);
  }

  /** `c`: cd the terminal into this directory (or a file's directory). The tree
   * follows on its own once the shell reports the new cwd. */
  private changeDir(entry: DirEntryInfo): void {
    const dir = entry.isDir ? entry.path : dirNameOf(entry.path, this.view?.root ?? "");
    this.deps.onChangeDir(dir);
  }

  /** `u`: re-root one directory up. */
  private goUp(): void {
    const view = this.view;
    const parent = view ? this.dirs.get(view.root)?.parent : null;
    if (view && parent) this.reroot(view, parent);
  }

  // --- rendering ---------------------------------------------------------

  /** Render on the next frame; many reads landing together render once. */
  private scheduleRender(): void {
    if (this.renderFrame || !this.open_) return;
    this.renderFrame = requestAnimationFrame(() => {
      this.renderFrame = 0;
      this.render();
    });
  }

  private render(): void {
    if (this.renderFrame) {
      cancelAnimationFrame(this.renderFrame);
      this.renderFrame = 0;
    }
    const view = this.view;
    const root = view?.root ?? "";
    const rootState = this.dirs.get(root);

    const title = root ? compactPath(toShellPath(root), 30) : "…";
    if (this.rootEl.textContent !== title) {
      this.rootEl.textContent = title;
      this.rootEl.title = root;
    }

    const { rows, shown } = view
      ? buildTree(root, this.dirs, view.open, this.showHidden)
      : { rows: [], shown: [] };
    this.rows = rows;
    this.shown = shown;
    this.index = selectionIndex(rows, view?.selected ?? null, this.index);
    if (view) view.selected = rows[this.index]?.entry.path ?? null;

    const nodes: HTMLElement[] = [];
    const rowEls = new Map<string, { el: HTMLElement; sig: string }>();
    for (const row of rows) {
      const prefix = connectors(row);
      const { entry } = row;
      const sig = `${prefix}|${entry.isDir}${entry.isSymlink}${entry.hidden}|${row.status}`;
      const prev = this.rowEls.get(entry.path);
      const item = prev?.sig === sig ? prev : { el: rowElement(row, prefix), sig };
      rowEls.set(entry.path, item);
      nodes.push(item.el);
    }
    this.rowEls = rowEls;

    if (!rootState || (rootState.entries === null && !rootState.error)) {
      nodes.push(span("ft-note", "…"));
    } else if (rootState.error) {
      nodes.push(span("ft-note ft-error", rootState.error));
    } else if (rows.length === 0) {
      nodes.push(span("ft-note", this.showHidden ? "empty" : "empty (. shows hidden)"));
    }
    if (rootState?.truncated) nodes.push(span("ft-note", "… too many entries"));

    patchChildren(this.bodyEl, nodes);
    this.paintSelection(false);
    if (this.open_) this.wantWatch(shown);
    if (this.dirs.size > CACHE_LIMIT) this.trimCache();
  }

  /** Move the highlight, touching only the two rows involved. Scrolls only for a
   * cursor move — a background re-read must not yank a scrolled tree around. */
  private paintSelection(scroll: boolean): void {
    const row = this.rows[this.index];
    const el = row ? (this.rowEls.get(row.entry.path)?.el ?? null) : null;
    if (el !== this.selectedEl) {
      this.selectedEl?.classList.remove("selected");
      el?.classList.add("selected");
      this.selectedEl = el;
    }
    if (scroll) el?.scrollIntoView({ block: "nearest" });
  }

  /** The row a mouse event landed on, if any. */
  private rowAt(target: EventTarget | null): Row | undefined {
    const el = target instanceof Element ? target.closest<HTMLElement>(".ft-row") : null;
    const path = el?.dataset.path;
    return path === undefined ? undefined : this.rows.find((r) => r.entry.path === path);
  }

  /** Drop listings no view refers to (a pane closed, a root moved on). */
  private trimCache(): void {
    const keep = new Set<string>(this.watched);
    for (const view of this.views.values()) {
      keep.add(view.root);
      for (const dir of view.open) keep.add(dir);
    }
    for (const dir of this.dirs.keys()) if (!keep.has(dir)) this.dirs.delete(dir);
  }
}

/** Build one row's element. Its `data-path` is how clicks find the row. */
function rowElement(row: Row, prefix: string): HTMLElement {
  const el = document.createElement("div");
  el.className = "ft-row";
  el.dataset.path = row.entry.path;
  el.appendChild(span("ft-tree", prefix));

  const { entry } = row;
  const classes = ["ft-name"];
  if (entry.isDir) classes.push("ft-dir");
  if (entry.hidden) classes.push("ft-hidden");
  if (entry.isSymlink) classes.push("ft-link");
  el.appendChild(span(classes.join(" "), entry.name + (entry.isDir ? "/" : "")));

  if (row.status === "loading") el.appendChild(span("ft-note", " …"));
  if (row.status === "error") el.appendChild(span("ft-note ft-error", " !"));
  return el;
}

/**
 * Make `parent`'s children exactly `nodes`, touching only what differs: children
 * no longer wanted are removed, then missing or misplaced ones inserted. A
 * re-read that adds one file costs one insertion, not a rebuild of the list.
 */
function patchChildren(parent: HTMLElement, nodes: readonly HTMLElement[]): void {
  const keep = new Set<Node>(nodes);
  for (let child = parent.firstChild; child; ) {
    const next = child.nextSibling;
    if (!keep.has(child)) parent.removeChild(child);
    child = next;
  }
  let cursor = parent.firstChild;
  for (const node of nodes) {
    if (node === cursor) cursor = cursor.nextSibling;
    else parent.insertBefore(node, cursor);
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
