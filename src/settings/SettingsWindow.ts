/**
 * The Settings window: a custom, dark, centered in-app overlay (CLAUDE.md §14).
 *
 * It is keyboard-first (Escape closes; the controller binds <prefix> , to open
 * it) and applies changes live via the `onChange` callback. Sections: a
 * read-only keybindings reference, aliases, display (colours/fonts/divider),
 * terminal-info toggles with a live prompt preview, and the banner editor.
 *
 * Built fresh on each open from the controller's current settings, so it never
 * shows stale state. The DOM is imperative (no framework) to match the rest of
 * the app.
 */

import type { Settings, ColorKey } from "./settings";
import { PRESETS, MAX_DIR_SEGMENTS, clampDirSegments } from "./settings";
import type { PromptInfo, PromptToggle } from "../ipc/commands";
import { promptDir } from "../paths";
import { RemoteSection } from "./RemoteSection";

export interface KeymapView {
  prefix: string;
  prefixBindings: Record<string, string>;
  globalBindings: Record<string, string>;
}

export interface SettingsWindowDeps {
  /** Snapshot of the controller's current settings, taken on each open. */
  getSettings(): Settings;
  /** Called on every edit with the updated settings (controller persists + applies). */
  onChange(next: Settings): void;
  /** Keymap, for the read-only keybindings reference. */
  keymap: KeymapView;
  /** action name -> { label, category }. */
  actionInfo: Record<string, { label: string; category: string }>;
}

export type Section =
  | "keybindings"
  | "aliases"
  | "display"
  | "info"
  | "banner"
  | "remote";

const SECTION_META: { id: Section; label: string }[] = [
  { id: "keybindings", label: "⌨   Keybindings" },
  { id: "aliases", label: "⤳   Aliases" },
  { id: "display", label: "🎨  Display" },
  { id: "info", label: "ⓘ   Terminal Info" },
  { id: "banner", label: "▤   Banner" },
  { id: "remote", label: "⇄   Remote" },
];

const CATEGORY_ORDER = ["Panes", "Focus", "Move / Resize", "Font", "App", "Other"];

/** Stand-in working directory for the prompt preview: deep enough that both the
 * elided-middle and the last-N-segments settings visibly do something. */
const SAMPLE_DIR = "~/projects/project-x/mobile-app";

/** Segment count the "Last segments" mode starts on when it is picked. */
const DEFAULT_DIR_SEGMENTS = 2;

const SAMPLE_BANNER = [
  "\\e[1;32m+------------------------------------------+",
  "|   SplitView — keyboard-first terminal     |",
  "|   Ctrl+A then ,  opens Settings           |",
  "+------------------------------------------+\\e[0m",
].join("\n");

export class SettingsWindow {
  private root: HTMLElement | null = null;
  private current: Settings;
  private active: Section = "keybindings";
  private readonly panels = new Map<Section, HTMLElement>();
  private readonly navButtons = new Map<Section, HTMLButtonElement>();
  /** Built lazily and kept, so it can poll only while it is on screen. */
  private remoteSection: RemoteSection | null = null;

  constructor(private readonly deps: SettingsWindowDeps) {
    this.current = deps.getSettings();
  }

  get isOpen(): boolean {
    return this.root !== null;
  }

  /** Open Settings, optionally jumping straight to a section (the remote
   * indicator uses this to land on "Remote"). */
  open(section?: Section): void {
    if (this.root) {
      if (section) this.show(section);
      return;
    }
    this.current = clone(this.deps.getSettings());
    if (section) this.active = section;
    this.root = this.build();
    document.body.appendChild(this.root);
    this.show(this.active);
    this.navButtons.get(this.active)?.focus();
  }

  close(): void {
    if (!this.root) return;
    // Stop the Remote section polling the backend once it is off screen.
    this.remoteSection?.hide();
    this.remoteSection = null;
    this.root.remove();
    this.root = null;
    this.panels.clear();
    this.navButtons.clear();
  }

  toggle(): void {
    if (this.root) this.close();
    else this.open();
  }

  // --- internals ---------------------------------------------------------

  private emit(): void {
    this.deps.onChange(this.current);
  }

  private show(section: Section): void {
    this.active = section;
    for (const [id, panel] of this.panels)
      panel.classList.toggle("active", id === section);
    for (const [id, btn] of this.navButtons)
      btn.classList.toggle("active", id === section);
    // Only the visible section polls; everything else is static DOM.
    if (section === "remote") this.remoteSection?.show();
    else this.remoteSection?.hide();
  }

  private build(): HTMLElement {
    const overlay = h("div", { class: "sv-overlay" });
    overlay.onclick = () => this.close();

    const win = h("div", { class: "sv-window" });
    win.onclick = (e) => e.stopPropagation();

    const title = h(
      "div",
      { class: "sv-titlebar" },
      h("span", { class: "sv-dot" }),
      h("span", { class: "sv-title", textContent: "SplitView — Settings" }),
    );
    const closeBtn = h("button", {
      class: "sv-close",
      textContent: "✕",
      title: "Close (Esc)",
    });
    closeBtn.onclick = () => this.close();
    title.append(closeBtn);

    const nav = h("nav", { class: "sv-nav" });
    const content = h("main", { class: "sv-content" });

    for (const { id, label } of SECTION_META) {
      const btn = h("button", { class: "sv-nav-item", textContent: label });
      btn.onclick = () => this.show(id);
      this.navButtons.set(id, btn);
      nav.append(btn);

      const panel = this.buildSection(id);
      this.panels.set(id, panel);
      content.append(panel);
    }

    const body = h("div", { class: "sv-body" }, nav, content);
    const foot = h(
      "div",
      { class: "sv-foot" },
      h("span", {
        textContent: "Esc to close · run “splitview settings” in a terminal to reopen",
      }),
    );

    win.append(title, body, foot);
    overlay.append(win);
    return overlay;
  }

  private buildSection(id: Section): HTMLElement {
    switch (id) {
      case "keybindings":
        return this.buildKeybindings();
      case "aliases":
        return this.buildAliases();
      case "display":
        return this.buildDisplay();
      case "info":
        return this.buildInfo();
      case "banner":
        return this.buildBanner();
      case "remote":
        return this.buildRemote();
    }
  }

  /** The Remote section owns its own rendering and talks to the backend. */
  private buildRemote(): HTMLElement {
    const section = new RemoteSection({
      settings: () => this.current.remote,
      onChange: () => this.emit(),
    });
    this.remoteSection = section;
    return section.el;
  }

  // --- Keybindings (read-only) -------------------------------------------

  private buildKeybindings(): HTMLElement {
    const { keymap, actionInfo } = this.deps;
    const section = h("div", { class: "sv-section" });
    section.append(
      h("p", {
        class: "sv-note",
        textContent: `Prefix: ${formatChord(keymap.prefix)} — press the prefix, then the key. Press the prefix twice to send it to the shell.`,
      }),
    );

    section.append(
      this.bindingGroups("After prefix", keymap.prefixBindings, actionInfo),
    );
    if (Object.keys(keymap.globalBindings).length > 0) {
      section.append(
        this.bindingGroups("Global (no prefix)", keymap.globalBindings, actionInfo),
      );
    }
    return section;
  }

  private bindingGroups(
    heading: string,
    record: Record<string, string>,
    actionInfo: Record<string, { label: string; category: string }>,
  ): HTMLElement {
    // Collapse multiple chords bound to the same action into one row.
    const byAction = new Map<string, string[]>();
    for (const [chord, action] of Object.entries(record)) {
      const list = byAction.get(action) ?? [];
      list.push(chord);
      byAction.set(action, list);
    }
    const byCat = new Map<string, { chords: string[]; label: string }[]>();
    for (const [action, chords] of byAction) {
      const info = actionInfo[action];
      const cat = info?.category ?? "Other";
      const rows = byCat.get(cat) ?? [];
      rows.push({ chords, label: info?.label ?? action });
      byCat.set(cat, rows);
    }

    const wrap = h(
      "div",
      { class: "sv-keys" },
      h("h3", { class: "sv-subhead", textContent: heading }),
    );
    const cats = [...byCat.keys()].sort(
      (a, b) => orderIndex(a) - orderIndex(b),
    );
    for (const cat of cats) {
      const group = h("div", { class: "sv-key-group" });
      group.append(h("div", { class: "sv-key-cat", textContent: cat }));
      for (const row of byCat.get(cat) ?? []) {
        const keys = h("div", { class: "sv-key-chords" });
        row.chords
          .map(formatChord)
          .forEach((c) => keys.append(h("kbd", { class: "sv-kbd", textContent: c })));
        group.append(
          h(
            "div",
            { class: "sv-key-row" },
            keys,
            h("span", { class: "sv-key-label", textContent: row.label }),
          ),
        );
      }
      wrap.append(group);
    }
    return wrap;
  }

  // --- Aliases -----------------------------------------------------------

  private buildAliases(): HTMLElement {
    const section = h("div", { class: "sv-section" });
    section.append(
      h("p", {
        class: "sv-note",
        textContent:
          "Type the alias name in any terminal to run its command. Applies to newly opened terminals.",
      }),
    );
    const list = h("div", { class: "sv-alias-list" });
    const render = (): void => {
      list.replaceChildren();
      this.current.aliases.forEach((_, i) => list.append(this.aliasRow(i, render)));
      if (this.current.aliases.length === 0)
        list.append(h("p", { class: "sv-empty", textContent: "No aliases yet." }));
    };
    render();

    const add = h("button", { class: "sv-btn", textContent: "+  Add alias" });
    add.onclick = () => {
      this.current.aliases.push({ name: "", command: "" });
      this.emit();
      render();
    };
    section.append(list, add);
    return section;
  }

  private aliasRow(index: number, rerender: () => void): HTMLElement {
    const alias = this.current.aliases[index];
    const name = h("input", {
      class: "sv-input sv-alias-name",
      type: "text",
      value: alias.name,
      placeholder: "name",
      spellcheck: false,
    });
    name.oninput = () => {
      const clean = name.value.replace(/[^A-Za-z0-9_-]/g, "");
      name.value = clean;
      this.current.aliases[index].name = clean;
      this.emit();
    };
    const arrow = h("span", { class: "sv-alias-arrow", textContent: "→" });
    const cmd = h("input", {
      class: "sv-input sv-alias-cmd",
      type: "text",
      value: alias.command,
      placeholder: "command to run",
      spellcheck: false,
    });
    cmd.oninput = () => {
      this.current.aliases[index].command = cmd.value;
      this.emit();
    };
    const del = h("button", {
      class: "sv-icon-btn",
      textContent: "✕",
      title: "Remove",
    });
    del.onclick = () => {
      this.current.aliases.splice(index, 1);
      this.emit();
      rerender();
    };
    return h("div", { class: "sv-alias-row" }, name, arrow, cmd, del);
  }

  // --- Display -----------------------------------------------------------

  private buildDisplay(): HTMLElement {
    const d = this.current.display;
    const section = h("div", { class: "sv-section" });

    // Presets
    const presets = h("div", { class: "sv-presets" });
    const colorInputs = new Map<ColorKey, HTMLInputElement>();
    const hexInputs = new Map<ColorKey, HTMLInputElement>();
    for (const preset of PRESETS) {
      const chip = h("button", { class: "sv-chip", textContent: preset.name });
      chip.onclick = () => {
        for (const [k, v] of Object.entries(preset.colors)) {
          if (typeof v !== "string") continue;
          const key = k as ColorKey;
          d[key] = v;
          const ci = colorInputs.get(key);
          if (ci) ci.value = v;
          const hi = hexInputs.get(key);
          if (hi) hi.value = v;
        }
        this.emit();
      };
      presets.append(chip);
    }
    section.append(
      this.field("Palette presets", presets, "Quick-start themes; tweak below."),
    );

    // Font
    const fontFamily = h("input", {
      class: "sv-input",
      type: "text",
      value: d.fontFamily,
      spellcheck: false,
    });
    fontFamily.oninput = () => {
      d.fontFamily = fontFamily.value;
      this.emit();
    };
    section.append(this.field("Font family", fontFamily));

    const fontSize = numberInput(d.fontSize, 6, 48, (v) => {
      d.fontSize = v;
      this.emit();
    });
    section.append(this.field("Font size (px)", fontSize));

    // Colours
    const colorField = (label: string, key: ColorKey): void => {
      const picker = h("input", { class: "sv-color", type: "color", value: d[key] });
      const hex = h("input", {
        class: "sv-input sv-hex",
        type: "text",
        value: d[key],
        spellcheck: false,
      });
      colorInputs.set(key, picker);
      hexInputs.set(key, hex);
      const apply = (val: string): void => {
        if (!/^#[0-9a-fA-F]{6}$/.test(val)) return;
        const lower = val.toLowerCase();
        d[key] = lower;
        picker.value = lower;
        hex.value = lower;
        this.emit();
      };
      picker.oninput = () => apply(picker.value);
      hex.onchange = () => apply(hex.value.trim());
      const wrap = h("div", { class: "sv-color-wrap" }, picker, hex);
      section.append(this.field(label, wrap));
    };
    colorField("Background", "bg");
    colorField("Foreground", "fg");
    colorField("Cursor", "cursor");
    colorField("Accent (green)", "accent");
    colorField("Divider", "divider");
    colorField("Divider (active)", "dividerActive");

    const dividerSize = numberInput(d.dividerSize, 1, 8, (v) => {
      d.dividerSize = v;
      this.emit();
    });
    section.append(this.field("Divider thickness (px)", dividerSize));

    return section;
  }

  // --- Terminal Info -----------------------------------------------------

  private buildInfo(): HTMLElement {
    const p = this.current.promptInfo;
    const section = h("div", { class: "sv-section" });
    section.append(
      h("p", {
        class: "sv-note",
        textContent:
          "Choose what the prompt shows. Applies to newly opened terminals.",
      }),
    );

    const preview = h("div", { class: "sv-prompt-preview" });
    renderPromptPreview(preview, p);
    section.append(preview);

    const items: { key: PromptToggle; label: string; desc: string }[] = [
      { key: "user", label: "User", desc: "Your username" },
      { key: "host", label: "Computer name", desc: "The machine's hostname" },
      { key: "dir", label: "Working directory", desc: "Current folder" },
      { key: "branch", label: "Git branch", desc: "Branch name inside a repo" },
      { key: "time", label: "Clock", desc: "Current time of day" },
      { key: "exitStatus", label: "Exit status", desc: "✗ + code when a command fails" },
    ];
    const toggles = h("div", { class: "sv-toggles" });
    for (const it of items) {
      toggles.append(
        toggleRow(it.label, it.desc, p[it.key], (on) => {
          p[it.key] = on;
          renderPromptPreview(preview, p);
          this.emit();
        }),
      );
    }
    section.append(toggles);
    section.append(this.dirSegmentsField(p, preview));
    return section;
  }

  /**
   * How much of the working directory the prompt shows: the whole path (long
   * ones keep their first two segments, `...`, and the last), or just the last
   * N segments — counted from the end, since the end is the part you are
   * actually working in.
   */
  private dirSegmentsField(p: PromptInfo, preview: HTMLElement): HTMLElement {
    const full = h("button", { class: "sv-chip", textContent: "Full path" });
    const last = h("button", { class: "sv-chip", textContent: "Last segments" });
    const count = h("input", {
      class: "sv-input sv-num sv-seg-count",
      type: "number",
      min: "1",
      max: String(MAX_DIR_SEGMENTS),
      value: String(p.dirSegments || DEFAULT_DIR_SEGMENTS),
      title: "How many trailing segments to show",
    });

    const apply = (segments: number): void => {
      p.dirSegments = clampDirSegments(segments);
      sync();
      renderPromptPreview(preview, p);
      this.emit();
    };
    const sync = (): void => {
      const on = p.dirSegments > 0;
      full.classList.toggle("active", !on);
      last.classList.toggle("active", on);
      count.disabled = !on;
      if (on) count.value = String(p.dirSegments);
    };

    full.onclick = () => apply(0);
    last.onclick = () => apply(Number(count.value) || DEFAULT_DIR_SEGMENTS);
    count.oninput = () => {
      const v = Number(count.value);
      if (Number.isInteger(v) && v >= 1 && v <= MAX_DIR_SEGMENTS) apply(v);
    };
    sync();

    // A plain div, not `field()`: that wraps the row in a `<label>`, and a click
    // anywhere in a label activates its first control — here the "Full path"
    // button, which would silently reset the setting.
    return h(
      "div",
      { class: "sv-field" },
      h(
        "div",
        { class: "sv-field-text" },
        h("span", { class: "sv-label", textContent: "Directory detail" }),
        h("span", {
          class: "sv-hint",
          textContent: `${SAMPLE_DIR} → ${promptDir(SAMPLE_DIR, 1)} at 1 segment`,
        }),
      ),
      h("div", { class: "sv-choice" }, full, last, count),
    );
  }

  // --- Banner ------------------------------------------------------------

  private buildBanner(): HTMLElement {
    const section = h("div", { class: "sv-section" });
    section.append(
      h("p", {
        class: "sv-note",
        textContent:
          "Shown at the top of every new terminal, before the first prompt. Use \\e[1;32m…\\e[0m for colour.",
      }),
    );
    const ta = h("textarea", {
      class: "sv-textarea",
      rows: 8,
      spellcheck: false,
      placeholder: "Welcome to SplitView 🦜",
    });
    ta.value = this.current.banner;
    ta.oninput = () => {
      this.current.banner = ta.value;
      this.emit();
    };
    const sample = h("button", { class: "sv-btn", textContent: "Insert sample" });
    sample.onclick = () => {
      ta.value = SAMPLE_BANNER;
      this.current.banner = SAMPLE_BANNER;
      this.emit();
    };
    const clear = h("button", {
      class: "sv-btn sv-btn-ghost",
      textContent: "Clear",
    });
    clear.onclick = () => {
      ta.value = "";
      this.current.banner = "";
      this.emit();
    };
    section.append(ta, h("div", { class: "sv-row" }, sample, clear));
    return section;
  }

  /** A labelled field row used across Display. */
  private field(label: string, control: HTMLElement, hint?: string): HTMLElement {
    const text = h("div", { class: "sv-field-text" }, h("span", {
      class: "sv-label",
      textContent: label,
    }));
    if (hint) text.append(h("span", { class: "sv-hint", textContent: hint }));
    return h("label", { class: "sv-field" }, text, control);
  }
}

// --- module helpers ------------------------------------------------------

/** Tiny hyperscript: create an element, assign props, append children. */
function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Partial<Omit<HTMLElementTagNameMap[K], "className">> & { class?: string } = {},
  ...children: (Node | string)[]
): HTMLElementTagNameMap[K] {
  const { class: className, ...rest } = props;
  const el = document.createElement(tag);
  if (className) el.className = className;
  Object.assign(el, rest);
  for (const child of children) el.append(child);
  return el;
}

function numberInput(
  value: number,
  min: number,
  max: number,
  onChange: (v: number) => void,
): HTMLInputElement {
  const input = h("input", {
    class: "sv-input sv-num",
    type: "number",
    value: String(value),
    min: String(min),
    max: String(max),
  });
  input.oninput = () => {
    const v = Number(input.value);
    if (!Number.isNaN(v) && v >= min && v <= max) onChange(v);
  };
  return input;
}

function toggleRow(
  label: string,
  desc: string,
  checked: boolean,
  onToggle: (on: boolean) => void,
): HTMLElement {
  const input = h("input", { class: "sv-switch-input", type: "checkbox", checked });
  input.onchange = () => onToggle(input.checked);
  const sw = h("span", { class: "sv-switch" }, input, h("span", { class: "sv-switch-track" }));
  const text = h(
    "div",
    { class: "sv-toggle-text" },
    h("span", { class: "sv-toggle-label", textContent: label }),
    h("span", { class: "sv-toggle-desc", textContent: desc }),
  );
  return h("label", { class: "sv-toggle-row" }, text, sw);
}

/** A coloured prompt segment for the live preview. */
function seg(text: string, color: string): HTMLSpanElement {
  const s = document.createElement("span");
  s.textContent = text;
  s.style.color = color;
  return s;
}

function renderPromptPreview(container: HTMLElement, p: PromptInfo): void {
  const red = "#ff5555";
  const white = "#d3dad6";
  const blue = "#5aa2ff";
  const yellow = "#ffc234";
  const cyan = "#34e2e2";
  const green = "#4ce675";
  container.replaceChildren();

  const line1 = document.createElement("div");
  line1.append(seg("┌", red));
  if (p.user || p.host) {
    const uh = p.user && p.host ? "user@host" : p.user ? "user" : "host";
    line1.append(seg("─[", red), seg(uh, white), seg("]", red));
  }
  if (p.dir) {
    const dir = promptDir(SAMPLE_DIR, p.dirSegments);
    line1.append(seg("─[", red), seg(dir, blue), seg("]", red));
  }
  if (p.branch) line1.append(seg("─[", red), seg("main", yellow), seg("]", red));
  if (p.time) line1.append(seg("─[", red), seg("14:22:07", cyan), seg("]", red));
  if (p.exitStatus) line1.append(seg("─[", red), seg("✗ 1", red), seg("]", red));

  const line2 = document.createElement("div");
  line2.append(seg("└──╼ ", red), seg("$", green));
  container.append(line1, line2);
}

/** Format a canonical chord (`"C-a"`, `"S-l"`, `"arrowleft"`, `"-"`) for display. */
function formatChord(chord: string): string {
  let rest = chord;
  let mods = "";
  const take = (tok: string, label: string): void => {
    if (rest.startsWith(tok)) {
      rest = rest.slice(tok.length);
      mods += label;
    }
  };
  take("C-", "Ctrl+");
  take("A-", "Alt+");
  take("S-", "Shift+");
  take("M-", "Win+");
  const named: Record<string, string> = {
    arrowleft: "←",
    arrowright: "→",
    arrowup: "↑",
    arrowdown: "↓",
    " ": "Space",
  };
  const key = named[rest] ?? (rest.length === 1 ? rest.toUpperCase() : rest);
  return mods + key;
}

function orderIndex(category: string): number {
  const i = CATEGORY_ORDER.indexOf(category);
  return i < 0 ? CATEGORY_ORDER.length : i;
}

function clone(settings: Settings): Settings {
  return JSON.parse(JSON.stringify(settings)) as Settings;
}
