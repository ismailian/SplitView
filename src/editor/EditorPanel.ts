/**
 * The editor panel: a code editor docked on the right of the window, for going
 * into a project and changing a file without leaving the terminal.
 *
 * Like the file sidebar it is not a pane: it has no PTY, it sits outside the
 * layout tree, and opening it only narrows the pane area. CodeMirror does the
 * editing (it is one `EditorView` with one `EditorState` per tab, so each file
 * keeps its own undo history, cursor and scroll position); this class owns the
 * tabs, the disk, and the panel around it. It keeps its tabs when hidden, so
 * closing the panel never loses anything.
 *
 * Files change under it all the time, because the terminal is right there. While
 * the panel is open the visible file is re-checked every couple of seconds: a
 * clean buffer just follows the disk, a dirty one says so in the status line and
 * the save asks before overwriting. The content-only changes this needs are
 * exactly what the sidebar's watch ignores, hence a stat rather than a watch.
 *
 * Keys: the panel's own chords are CodeMirror bindings on its editing surface,
 * so the document-level keymap still sees everything first and the prefix works
 * from here as it does anywhere (CLAUDE.md §9).
 */

import { basicSetup } from "codemirror";
import { indentWithTab, selectAll } from "@codemirror/commands";
import { indentUnit, LanguageDescription } from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import {
  Compartment,
  EditorState,
  Prec,
  Transaction,
  type Extension,
  type StateEffect,
  type Text,
  type TransactionSpec,
} from "@codemirror/state";
import { EditorView, keymap, type KeyBinding } from "@codemirror/view";

import {
  fileStamp,
  readTextFile,
  writeTextFile,
  type FileStamp,
} from "../ipc/commands";
import { attachEdgeDrag } from "../layout/edgeDrag";
import { baseName, compactPath, toShellPath } from "../paths";
import {
  changeBetween,
  decodeText,
  detectIndent,
  encodeText,
  tabLabels,
  type Eol,
} from "./model";
import { editorTheme } from "./theme";

export interface EditorPanelDeps {
  /** Hand focus back to the terminal. */
  onLeave(): void;
  /** The panel's width changed, so the pane area did: the terminals need
   * re-fitting (and their PTYs resizing with them, CLAUDE.md §20). */
  onResize(): void;
}

/** One open file. `state` and `scroll` are only current while it isn't showing:
 * for the active tab, the view holds them. */
interface Tab {
  path: string;
  state: EditorState;
  scroll: StateEffect<unknown> | null;
  /** The document as last read or saved; the buffer is dirty if it differs. */
  saved: Text;
  dirty: boolean;
  /** The version on disk that `saved` came from. */
  stamp: FileStamp;
  eol: Eol;
  bom: boolean;
  /** Language name, once its grammar has loaded. */
  language: string | null;
  /** A write is in flight. */
  saving: boolean;
  /** What is wrong between this buffer and the disk, if anything. */
  warning: string | null;
}

/** Width bounds and default, in px. The maximum is a share of the window, and
 * the stylesheet keeps a minimum for the terminals on top of that. */
const MIN_WIDTH = 280;
const MAX_WIDTH_RATIO = 0.8;
const DEFAULT_WIDTH_RATIO = 0.45;
/** How much one width chord moves the edge, in px. */
const WIDTH_STEP = 40;
const WIDTH_KEY = "splitview.editor.width";
/** How often the visible file is re-checked against the disk, in ms. */
const CHECK_INTERVAL_MS = 2000;
/** Long enough to read "saved", short enough not to linger. */
const NOTICE_MS = 2500;

/** Per-tab settings that change after the state is created. */
const language = new Compartment();
const indent = new Compartment();

export class EditorPanel {
  /** Stable root element; the controller parents it after the pane area. */
  readonly el: HTMLElement;
  private readonly tabsEl: HTMLElement;
  private readonly bodyEl: HTMLElement;
  private readonly emptyEl: HTMLElement;
  private readonly statusEl: HTMLElement;
  private readonly positionEl: HTMLElement;
  private readonly view: EditorView;
  private readonly extensions: Extension;

  private tabs: Tab[] = [];
  private active: Tab | null = null;
  private open_ = false;
  private width = loadWidth();
  /** A passing message for the status line ("saved", a failed open). */
  private notice = "";
  private noticeTimer = 0;
  private checkTimer = 0;

  constructor(private readonly deps: EditorPanelDeps) {
    this.el = document.createElement("section");
    this.el.className = "editor";
    this.el.tabIndex = -1;
    this.el.style.display = "none";
    this.el.style.width = `${this.width}px`;

    this.tabsEl = document.createElement("div");
    this.tabsEl.className = "ed-tabs";
    this.bodyEl = document.createElement("div");
    this.bodyEl.className = "ed-body";
    this.emptyEl = document.createElement("div");
    this.emptyEl.className = "ed-empty";
    this.emptyEl.textContent =
      "No file open. Press e on a file in the sidebar, or run: splitview edit <file>";

    const footer = document.createElement("div");
    footer.className = "ed-footer";
    this.statusEl = document.createElement("span");
    this.statusEl.className = "ed-status";
    this.positionEl = document.createElement("span");
    this.positionEl.className = "ed-position";
    footer.append(this.statusEl, this.positionEl);

    // Mouse convenience only: Ctrl+Alt+Left/Right do the same from the keyboard
    // (CLAUDE.md §2.1, §18).
    const resizer = document.createElement("div");
    resizer.className = "ed-resizer";
    resizer.title = "Drag to resize (or Ctrl+Alt+Left / Right)";
    attachEdgeDrag(resizer, {
      width: () => this.el.getBoundingClientRect().width,
      sign: -1,
      resize: (px) => this.setWidth(px),
      done: () => saveWidth(this.width),
    });

    this.el.append(this.tabsEl, this.bodyEl, footer, resizer);

    this.extensions = [
      basicSetup,
      editorTheme,
      language.of([]),
      Prec.high(keymap.of(this.bindings())),
      keymap.of([indentWithTab]),
      // Below the defaults, so Escape first closes a completion popup or drops a
      // selection, and only then leaves.
      Prec.low(
        keymap.of([
          {
            key: "Escape",
            run: () => {
              this.deps.onLeave();
              return true;
            },
          },
        ]),
      ),
      EditorView.updateListener.of((update) => {
        if (update.docChanged) this.onDocChanged();
        if (update.docChanged || update.selectionSet) this.paintPosition();
      }),
    ];
    this.view = new EditorView({
      state: EditorState.create({ extensions: this.extensions }),
      parent: this.bodyEl,
    });
    this.bodyEl.appendChild(this.emptyEl);
    this.paint();
  }

  get isOpen(): boolean {
    return this.open_;
  }

  /** True while keyboard focus is anywhere in the panel, the search box included. */
  get isFocused(): boolean {
    return this.el.contains(document.activeElement);
  }

  get hasFiles(): boolean {
    return this.tabs.length > 0;
  }

  /** How many open files have changes that are not on disk. */
  get unsavedCount(): number {
    return this.tabs.filter((tab) => tab.dirty).length;
  }

  /** Show the panel, on whatever was open last, and take focus. */
  open(): void {
    this.show();
    this.focus();
    if (this.active) void this.check(this.active);
  }

  /** Hide the panel. Tabs and unsaved changes stay for next time. */
  close(): void {
    if (!this.open_) return;
    this.open_ = false;
    this.el.style.display = "none";
    window.clearInterval(this.checkTimer);
    this.deps.onResize();
  }

  focus(): void {
    if (this.active) this.view.focus();
    else this.el.focus();
  }

  /** The literal prefix (pressed twice) reaches the editor as what Ctrl+A means
   * there, since the first press never gets this far. */
  selectAll(): void {
    selectAll(this.view);
  }

  /** Terminal font size follows here, so zooming zooms both. */
  setFontSize(px: number): void {
    this.el.style.setProperty("--editor-font-size", `${px}px`);
    this.view.requestMeasure();
  }

  /**
   * Open `path` in a tab (or switch to it, if it has one), show the panel and
   * take focus. A file that can't be opened leaves the reason in the status line.
   */
  async openFile(path: string): Promise<void> {
    try {
      const file = await readTextFile(path);
      const existing = this.tabs.find((tab) => tab.path === file.path);
      if (existing) {
        this.activate(existing);
      } else {
        const { text, eol, bom } = decodeText(file.content);
        const state = EditorState.create({
          doc: text,
          extensions: [
            this.extensions,
            indent.of(indentUnit.of(detectIndent(text))),
          ],
        });
        const tab: Tab = {
          path: file.path,
          state,
          scroll: null,
          saved: state.doc,
          dirty: false,
          stamp: file.stamp,
          eol,
          bom,
          language: null,
          saving: false,
          warning: null,
        };
        this.tabs.push(tab);
        this.activate(tab);
        this.loadLanguage(tab);
      }
      this.show();
      this.focus();
    } catch (e) {
      this.show();
      this.say(`${baseName(path)}: ${message(e)}`);
    }
  }

  // --- tabs --------------------------------------------------------------

  private activate(tab: Tab): void {
    if (tab !== this.active) {
      this.stash();
      this.active = tab;
      this.view.setState(tab.state);
      if (tab.scroll) this.view.dispatch({ effects: tab.scroll });
    }
    this.paint();
    void this.check(tab);
  }

  /** Move the active tab's live state out of the view. */
  private stash(): void {
    const tab = this.active;
    if (!tab) return;
    tab.state = this.view.state;
    tab.scroll = this.view.scrollSnapshot();
  }

  private stateOf(tab: Tab): EditorState {
    return tab === this.active ? this.view.state : tab.state;
  }

  /** Apply a change to a tab, showing or not. */
  private update(tab: Tab, spec: TransactionSpec): void {
    if (tab === this.active) this.view.dispatch(spec);
    else tab.state = tab.state.update(spec).state;
  }

  /** Move to the tab `delta` away, wrapping. */
  private cycle(delta: number): void {
    const tab = this.active;
    if (!tab || this.tabs.length < 2) return;
    const count = this.tabs.length;
    this.activate(this.tabs[(this.tabs.indexOf(tab) + delta + count) % count]);
  }

  /** Close a tab, asking first if it has unsaved changes. The last tab takes the
   * panel with it, which gives the terminals their width back. */
  private closeTab(tab: Tab): void {
    if (
      tab.dirty &&
      !window.confirm(`Discard unsaved changes to ${baseName(tab.path)}?`)
    ) {
      return;
    }
    const index = this.tabs.indexOf(tab);
    this.tabs.splice(index, 1);
    if (tab !== this.active) {
      this.paint();
      return;
    }
    this.active = null;
    const next = this.tabs[Math.min(index, this.tabs.length - 1)];
    if (next) {
      this.activate(next);
      this.focus();
    } else {
      this.view.setState(EditorState.create({ extensions: this.extensions }));
      this.paint();
      this.close();
      this.deps.onLeave();
    }
  }

  /** Load the file's grammar, by name. Grammars are separate chunks, so opening
   * the app costs nothing for languages that are never used. */
  private loadLanguage(tab: Tab): void {
    const description = LanguageDescription.matchFilename(
      languages,
      baseName(tab.path),
    );
    if (!description) return;
    description.load().then(
      (support) => {
        if (!this.tabs.includes(tab)) return;
        tab.language = description.name;
        this.update(tab, { effects: language.reconfigure(support) });
        this.paintPosition();
      },
      () => {
        // The chunk failed to load; the file is still editable as plain text.
      },
    );
  }

  // --- disk --------------------------------------------------------------

  /** Save the active file. If it changed on disk since it was read, ask first. */
  private async save(): Promise<void> {
    const tab = this.active;
    if (!tab || tab.saving) return;
    const doc = this.view.state.doc;
    const content = encodeText(doc.toString(), tab.eol, tab.bom);
    tab.saving = true;
    try {
      let stamp = await writeTextFile(tab.path, content, tab.stamp);
      if (!stamp) {
        const name = baseName(tab.path);
        if (!window.confirm(`${name} changed on disk. Overwrite it?`)) {
          this.say("not saved");
          return;
        }
        stamp = await writeTextFile(tab.path, content, null);
      }
      if (stamp) tab.stamp = stamp;
      tab.saved = doc;
      tab.warning = null;
      this.refreshDirty(tab);
      this.say("saved");
    } catch (e) {
      this.say(`not saved: ${message(e)}`);
    } finally {
      tab.saving = false;
    }
  }

  /** Compare a tab with the disk: follow the file if the buffer is clean, warn if
   * it isn't. Runs on a timer, and whenever a tab comes into view. */
  private async check(tab: Tab): Promise<void> {
    if (tab.saving) return;
    const seen = tab.stamp;
    const current = await fileStamp(tab.path).catch(() => undefined);
    // Skip the answer if a save moved the file on while it was being asked.
    if (current === undefined || tab.saving || tab.stamp !== seen) return;

    if (!current) {
      this.warn(tab, "deleted on disk");
    } else if (current.modifiedMs === seen.modifiedMs && current.size === seen.size) {
      this.warn(tab, null);
    } else if (tab.dirty) {
      this.warn(tab, "changed on disk");
    } else {
      await this.reload(tab);
    }
  }

  /** Bring a clean buffer up to date with the file. Applied as the smallest
   * change, so the cursor and scroll position stay put, and kept out of the undo
   * history: Ctrl+Z undoes the user's edits, not someone else's. */
  private async reload(tab: Tab): Promise<void> {
    const seen = tab.stamp;
    try {
      const file = await readTextFile(tab.path);
      if (tab.dirty || tab.saving || tab.stamp !== seen) return;
      const { text, eol, bom } = decodeText(file.content);
      const change = changeBetween(this.stateOf(tab).doc.toString(), text);
      tab.stamp = file.stamp;
      tab.eol = eol;
      tab.bom = bom;
      tab.warning = null;
      if (change) {
        const spec = {
          changes: change,
          annotations: Transaction.addToHistory.of(false),
        };
        if (tab !== this.active) tab.scroll = null; // its position may be gone
        tab.saved = this.stateOf(tab).update(spec).newDoc;
        this.update(tab, spec);
      }
      this.paint();
    } catch (e) {
      this.warn(tab, message(e)); // it is no longer something we can open
    }
  }

  private warn(tab: Tab, warning: string | null): void {
    if (tab.warning === warning) return;
    tab.warning = warning;
    this.paintStatus();
  }

  // --- changes -----------------------------------------------------------

  private onDocChanged(): void {
    if (this.active) this.refreshDirty(this.active);
  }

  /** Update the dirty flag. Undoing back to the saved text makes a tab clean. */
  private refreshDirty(tab: Tab): void {
    const dirty = !this.stateOf(tab).doc.eq(tab.saved);
    if (dirty === tab.dirty) return;
    tab.dirty = dirty;
    this.paintTabs();
  }

  /** Show a passing message in the status line. */
  private say(text: string): void {
    this.notice = text;
    this.paintStatus();
    window.clearTimeout(this.noticeTimer);
    this.noticeTimer = window.setTimeout(() => {
      this.notice = "";
      this.paintStatus();
    }, NOTICE_MS);
  }

  // --- keyboard ----------------------------------------------------------

  /** The panel's own chords. Scoped to the search box too, so saving from it
   * works: it is still the same file. */
  private bindings(): KeyBinding[] {
    const scope = "editor search-panel";
    const run = (action: () => void) => (): boolean => {
      action();
      return true;
    };
    const closeActive = (): void => {
      if (this.active) this.closeTab(this.active);
    };
    return [
      { key: "Mod-s", scope, run: run(() => void this.save()) },
      { key: "Mod-w", scope, run: run(closeActive) },
      { key: "Ctrl-PageDown", scope, run: run(() => this.cycle(1)) },
      { key: "Ctrl-PageUp", scope, run: run(() => this.cycle(-1)) },
      {
        key: "Ctrl-Alt-ArrowLeft",
        scope,
        run: run(() => this.nudgeWidth(this.width + WIDTH_STEP)),
      },
      {
        key: "Ctrl-Alt-ArrowRight",
        scope,
        run: run(() => this.nudgeWidth(this.width - WIDTH_STEP)),
      },
    ];
  }

  // --- width -------------------------------------------------------------

  /** Show the panel if it is hidden. */
  private show(): void {
    if (this.open_) return;
    this.open_ = true;
    this.el.style.display = "flex";
    this.setWidth(this.width); // re-clamp: the window may have shrunk since
    this.checkTimer = window.setInterval(() => {
      if (this.active && !document.hidden) void this.check(this.active);
    }, CHECK_INTERVAL_MS);
    this.deps.onResize();
    this.view.requestMeasure();
  }

  private setWidth(px: number): void {
    const max = Math.max(MIN_WIDTH, window.innerWidth * MAX_WIDTH_RATIO);
    const next = Math.round(Math.max(MIN_WIDTH, Math.min(max, px)));
    if (next === this.width) return;
    this.width = next;
    this.el.style.width = `${next}px`;
    this.deps.onResize();
  }

  private nudgeWidth(px: number): void {
    this.setWidth(px);
    saveWidth(this.width);
  }

  // --- rendering ---------------------------------------------------------

  private paint(): void {
    const showing = this.active !== null;
    this.view.dom.style.display = showing ? "" : "none";
    this.emptyEl.style.display = showing ? "none" : "block";
    this.paintTabs();
    this.paintStatus();
    this.paintPosition();
  }

  /** Rebuild the tab row. There are a handful of tabs and this runs when one
   * opens, closes or turns dirty, never per keystroke. */
  private paintTabs(): void {
    const labels = tabLabels(this.tabs.map((tab) => tab.path));
    this.tabsEl.replaceChildren(
      ...this.tabs.map((tab, i) => this.tabElement(tab, labels[i])),
    );
    this.tabsEl
      .querySelector(".ed-tab.active")
      ?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }

  private tabElement(tab: Tab, label: string): HTMLElement {
    const el = document.createElement("button");
    el.className = tab === this.active ? "ed-tab active" : "ed-tab";
    el.tabIndex = -1; // tabs are switched with Ctrl+PageUp/PageDown
    el.title = tab.path;
    el.textContent = tab.dirty ? `● ${label}` : label;

    const close = document.createElement("span");
    close.className = "ed-tab-close";
    close.textContent = "×";
    el.appendChild(close);

    el.addEventListener("mousedown", (ev) => {
      ev.preventDefault(); // keep focus in the editor
      if (ev.target === close || ev.button === 1) {
        this.closeTab(tab);
      } else {
        this.activate(tab);
        this.focus();
      }
    });
    return el;
  }

  private paintStatus(): void {
    const tab = this.active;
    const warning = tab?.warning ?? null;
    this.statusEl.textContent =
      this.notice ||
      warning ||
      (tab ? compactPath(toShellPath(tab.path), 60) : "");
    this.statusEl.classList.toggle("warning", !this.notice && warning !== null);
    this.statusEl.title = tab?.path ?? "";
  }

  private paintPosition(): void {
    const tab = this.active;
    if (!tab) {
      this.positionEl.textContent = "";
      return;
    }
    const { state } = this.view;
    const head = state.selection.main.head;
    const line = state.doc.lineAt(head);
    const parts = [
      `Ln ${line.number}, Col ${head - line.from + 1}`,
      tab.eol === "\n" ? "LF" : "CRLF",
    ];
    if (tab.language) parts.push(tab.language);
    this.positionEl.textContent = parts.join(" · ");
  }
}

/** Last chosen width, or a share of the window if none/garbage is stored. */
function loadWidth(): number {
  try {
    const raw = Number(localStorage.getItem(WIDTH_KEY));
    if (Number.isFinite(raw) && raw >= MIN_WIDTH) return raw;
  } catch {
    // Storage unavailable; the default width is fine.
  }
  return Math.round(window.innerWidth * DEFAULT_WIDTH_RATIO);
}

/** Persist the width. Failures (storage disabled) are non-fatal. */
function saveWidth(px: number): void {
  try {
    localStorage.setItem(WIDTH_KEY, String(px));
  } catch {
    // Storage unavailable; the width simply won't survive a restart.
  }
}

function message(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return "cannot open file";
}
