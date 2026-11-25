# CLAUDE.md — SplitView

> Context file for AI coding agents (and humans) working in this repository.
> Read this **before** writing code. It encodes the architecture, conventions,
> domain vocabulary, and footguns specific to SplitView. When something here
> conflicts with a generic instinct, follow this file.

---

## 1. What SplitView is

SplitView is a **minimalistic, keyboard-driven terminal emulator** for the
desktop, in the spirit of a tmux layout but as a native GUI app: fast,
distraction-free terminal work where the UI gets out of the way — thin dividers,
every action reachable from the keyboard, terminal performance that feels native.

Since 0.3 it is a **single native Rust binary**: **egui/eframe** (glow) draws
everything, **alacritty_terminal** emulates the terminal, **portable-pty** runs
real shells. (0.2.x was Tauri + xterm.js; that code is gone — see git history.)
The visual language follows SecureWire (`../SecureWire`): IDE-style frame with a
36 px top bar, a 24 px status bar, hairlines, one teal accent.

### The features that define the product (do not regress these)

- **Split panes** horizontally and vertically; **switch focus**, **move/swap**,
  **resize** and **zoom** panes — all by keyboard.
- **Keyboard-only operation**: the mouse is a convenience, never a requirement.
- **Thin separators** between panes (1 px by default), never full borders.
- A **minimal shell experience** (bash/zsh, Git Bash on Windows) with an injected
  prompt (`{user} @ {cwd} $` or the ParrotOS two-line style).
- A **real terminal**: real PTYs, full xterm emulation, mouse reporting, low
  input latency, no frames drawn while nothing changes.
- The **Explorer** sidebar and the **Editor** panel (§9).

---

## 2. Design principles (the non-negotiables)

1. **Keyboard-first.** A feature gets a keybinding (and a command-palette entry,
   which it gets for free by being in `keymap::ACTIONS`).
2. **Minimal chrome.** Panes are separated by a thin line. The top bar, status bar
   and per-pane title strips are the only chrome; pane strips can be turned off
   (Settings → Display). No pane borders, no gradients, one shadow (dialogs).
3. **Native feel beats fancy.** Latency and correctness of the terminal come first.
4. **The shell is the shell.** We spawn a real shell in a real PTY. We never parse
   the user's commands and never intercept keystrokes outside prefix mode (§9).
5. **Processes vs layout.** `hub.rs` owns PTYs and the remote server and knows
   nothing about splits; the layout lives in `sv_core::layout` and the app. Keep
   that boundary clean.
6. **Pay only for what is visible.** eframe runs **reactive**: frames come from
   input, PTY output (`Hub::repaint`) and explicit `request_repaint_after`
   deadlines. Nothing repaints on a timer while idle — no blinking cursors, no
   polling the disk from the UI thread.

---

## 3. Tech stack

| Layer | Choice | Notes |
|---|---|---|
| UI | **eframe/egui 0.36** (glow) | `eframe::App::ui(&mut self, ui, frame)`; panels are `egui::Panel::{top,bottom,left,right}(id)`, `CentralPanel::no_frame()`. Held at 0.36 because `egui-phosphor 0.14` needs it — never two egui versions (`cargo tree -i egui`). |
| Icons | **egui-phosphor 0.14** (regular) | |
| Terminal emulation | **alacritty_terminal 0.26** | `Term` + vte `Processor` behind a `FairMutex` (`crates/term`). |
| Lock | **parking_lot 0.12** | Must match alacritty_terminal's. |
| PTY | **portable-pty 0.8** | ConPTY on Windows. |
| FS watch | **notify 8** | Sidebar only (`watch.rs`). |
| File dialog | **rfd 0.16** | `Ctrl+Shift+O`, on a worker thread. |
| Config | **toml 1** + **serde_json** | `config.toml` (hand-edited), `settings.json` (Settings window). |
| Remote control | **tokio + tokio-tungstenite + tokio-rustls/rustls (ring) + rcgen + mdns-sd + qrcode** | Opt-in, §21. `ring` is pinned: `aws-lc-rs` needs cmake + nasm. |
| HTTP | **ureq 3** (rustls/ring) | `update.rs` (Windows): once per launch, reads `site/releases/latest.json`; installer comes from the GitHub release, verified by size + SHA-256. `claude.rs`: the usage limits. |
| Font | **JetBrains Mono NL** (bundled, OFL) | `fontFile` in config.toml overrides it for the terminal. |

**Platforms:** Windows, Linux, macOS. A POSIX shell is the default everywhere it
exists (Git Bash on Windows); PowerShell and cmd.exe are user-chosen profiles.

Rust edition 2024 (let-chains are used). Workspace: `crates/{core,term,app}`.

---

## 4. Architecture overview

```
┌──────────────────── UI thread (eframe, reactive) ─────────────────────┐
│ App ── windows: Vec<Workspace> (root viewport + immediate viewports)  │
│   │      Workspace { layout, background, focus, sidebar, editor }     │
│   │── keys::route ── Keymap (prefix engine) ── run_action             │
│   │── panes.rs: layout → rects → 1px dividers, title strips,          │
│   │             TerminalView per pane (sv_term)                       │
│   │── drain UiEvent channel each frame                                │
│   ▼                                                                   │
│ Arc<Hub> ─ ptys: { id → PtyHandle{writer, master, killer, term,…} }   │
│          ─ remote: RemoteState (server, roster, devices, pairing)     │
└────────────────────────────────────┬──────────────────────────────────┘
   per PTY: reader thread  ──────────┤ read → OscScanner → Term::advance
            (sv-pty-N)               │ → remote broadcast → ctx.repaint
            waiter thread (sv-wait-N)│ child.wait() → UiEvent::Exit
   remote: tokio runtime (2 workers, created on first enable)
```

### PTY lifecycle / data flow

1. `App::split_into` → `App::spawn_pane` → `Hub::spawn(id, size, ShellConfig)`:
   opens a PTY, builds the shell command (`shell.rs`), spawns it, creates the
   `sv_term::Terminal`, starts the **reader** and **waiter** threads, stores a
   `PtyHandle`. The layout gets a new leaf.
2. The **reader thread** blocks on the PTY, and for each chunk (≤ 64 KB):
   scans it for OSC 7 / OSC 5170 (`sv_core::osc`), feeds it to the emulator
   (`Terminal::advance`), fans it out to remote subscribers, and calls
   `ctx.request_repaint()`. egui coalesces repaints to one per frame.
3. The emulator answers terminal queries (DA, cursor position) through its event
   sink, which writes straight back to the PTY writer — from the reader thread.
4. Each frame, `TerminalView::show` draws the grid and turns keys/mouse/paste
   into bytes; the app writes them with `Hub::write`. When the pane's area
   changes size, the view resizes the grid (debounced 40 ms) and the app resizes
   the PTY — always together.
5. The **waiter thread** blocks on `child.wait()`; when the shell exits it sends
   `UiEvent::Exit(id)` and the pane closes (ConPTY's read never hits EOF while
   the master lives, so read-EOF can't be the exit signal).
6. `Hub::close(id)`: kill the child, drop the master (EOF ends the reader),
   remove the handle.

**Scrollback lives in the emulator** (`sv_term`, 10 000 lines per pane).

---

## 5. Repository structure

```
splitview/
├── CLAUDE.md, README.md, config.example.toml
├── Cargo.toml                 ← workspace, shared deps, profiles
├── .cargo/config.toml         ← rust-lld on Windows MSVC
├── crates/
│   ├── core/  splitview-core (lib sv_core) — pure, no I/O, no egui; TESTED
│   │   ├── layout.rs          ← layout tree + pure operations (§7)
│   │   ├── keymap.rs          ← prefix engine, default bindings, ACTIONS table
│   │   ├── config.rs          ← config.toml → validated Config
│   │   ├── paths.rs           ← native ⇄ msys paths, OSC 7, prompt_dir, compact_path
│   │   ├── quote.rs           ← ShellFamily, quoting/`cd` per shell
│   │   ├── osc.rs             ← streaming OSC 7 / 5170 scanner
│   │   ├── tree.rs            ← sidebar rows + watch set
│   │   ├── editor.rs          ← EOL/BOM round trip, indent, minimal change, find
│   │   ├── highlight.rs       ← one-pass syntax highlighter (per-language table)
│   │   ├── git.rs             ← `git status --porcelain=v2` parser, row lookup
│   │   ├── update.rs          ← update manifest, version order, URL/size/hash checks
│   │   ├── scratch.rs         ← scratchpad document, hit tests, per-project file name
│   │   └── fuzzy.rs           ← command-palette matching
│   ├── term/  splitview-term (lib sv_term)
│   │   ├── lib.rs             ← Terminal, preview_line, url_at
│   │   ├── view.rs            ← TerminalView egui widget (paint, keys, mouse, selection)
│   │   ├── input.rs           ← xterm key/mouse/paste encoding
│   │   └── snapshot.rs        ← ANSI dump of scrollback+screen (remote attach)
│   └── app/   splitview (bin)
│       ├── main.rs            ← args (--version, --shot), NativeOptions
│       ├── app.rs             ← App, Workspace, Pane; events; run_action; windows
│       ├── hub.rs             ← PTY registry + threads, UiEvent, remote runtime
│       ├── keys.rs            ← egui key → chord, input routing (keymap first)
│       ├── panes.rs           ← split area: rects, dividers, title strips, views
│       ├── chrome.rs          ← top bar, status bar, side-panel hosting, edge drag
│       ├── sidebar.rs         ← Explorer (reads off-thread, watch, keys, rows)
│       ├── editor.rs          ← Editor panel (tabs, TextEdit + highlighter, find)
│       ├── scratchpad.rs      ← Scratchpad panel (notes, ink, shapes; autosave)
│       ├── claude.rs          ← Claude Code sessions + usage limits (status bar)
│       ├── dialogs.rs         ← modals: shell picker, background, confirms,
│       │                        palette, pairing approval, toast
│       ├── settings.rs        ← Settings model + JSON persistence
│       ├── settings_ui.rs     ← Settings window (Display, Keys, Aliases, …, Remote)
│       ├── theme.rs           ← tokens, visuals, fonts, Skin (only place colors live)
│       ├── widgets.rs         ← small painter widgets (badge, switch, kbd, …)
│       ├── shell.rs           ← shell detection + prompt injection (§8)
│       ├── profiles.rs        ← installed shells (TESTED)
│       ├── files.rs           ← text read/stat/save for the editor (TESTED)
│       ├── watch.rs           ← sidebar filesystem watch (TESTED)
│       ├── git.rs             ← runs `git status` for the sidebar (TESTED)
│       ├── store.rs           ← config dir paths, config.toml loading
│       ├── shot.rs            ← --shot scenes + PNG writer
│       ├── update.rs          ← launch-time update check, installer download/run (Windows)
│       └── remote/            ← remote control (§21)
├── packaging/
│   ├── icons/                 ← splitview.ico/.png
│   └── windows/               ← splitview.iss, build-installer.ps1, sign-windows.ps1
└── docs/
    ├── remote-control-desktop.md, remote-control-mobile.md
    ├── remote/fixtures/*.json ← golden protocol vectors (asserted by protocol.rs)
    └── design/                ← reference screenshots
```

Dependency direction: `app → term`, `app → core`. `core` depends only on `serde`
and `toml`. Never name a package `core`.

---

## 6. Glossary / domain concepts

- **PTY:** the master/slave pair that lets us talk to a shell as a terminal.
- **Pane:** one terminal: one PTY + one `sv_term::Terminal` + one `TerminalView`.
  Always a leaf in the layout tree. `PaneId` is a `u64`; remote clients see
  `pane-<n>`.
- **Workspace / window:** one OS window with its own layout, background list,
  sidebar and editor. The first is the root viewport; others (`<prefix> n`, and
  detached panes `<prefix> d`) are egui immediate viewports in the same process,
  so moving a pane between windows is just moving a `PaneId`.
- **Layout tree:** binary tree of splits and leaves (§7).
- **Orientation:** named after the divider: `Row` = side by side (vertical
  divider, `split_right`); `Column` = stacked (horizontal divider, `split_down`).
- **Focus:** the active pane (keyboard input), shown by the accent on its title
  strip and its interior divider edges. Separately, `Workspace::focus` says
  whether the keyboard belongs to the terminal, the sidebar or the editor.
- **Zoom:** the focused pane fills the area; tree unchanged.
- **Prefix mode:** after the prefix chord (default `Ctrl+A`) the next key is a
  SplitView command. This is how we avoid stealing keys from vim/emacs/tmux.
- **Skin:** colors derived from the user's Display settings (terminal palette,
  dividers); chrome colors are fixed tokens in `theme::tok`.

---

## 7. Layout tree contract

`crates/core/src/layout.rs` is the canonical model. Pure functions only.

```rust
pub enum Node { Leaf(PaneId), Split { orientation, ratio: f32, first: Box<Node>, second: Box<Node> } }
pub struct Layout { root: Option<Node>, focused: Option<PaneId>, zoomed: Option<PaneId> }
```

Operations (all return a new `Layout`): `split_focused`, `focus_direction`,
`focus_next/prev`, `focus`, `move_pane`, `close_pane`/`close_focused`,
`toggle_zoom`, `resize_focused`, `set_ratio(path, r)`, `insert_beside`, plus
`leaves`, `rects`, `splits`, `neighbor`.

Invariants the tests guard: a split has exactly two children; closing never
leaves a one-child split (the sibling is promoted); `ratio ∈ [0.05, 0.95]`;
`focused` is a leaf or `None` when empty; zoom never changes the tree.

---

## 8. The terminal prompt — injected by the shell, not drawn by us

The emulator only renders bytes. To get the prompt, SplitView makes the *shell*
produce it without destroying the user's environment (`shell.rs` — keep all
prompt logic there):

- **bash:** `bash --rcfile <tmp>`; the rcfile sources `~/.bashrc` first, then
  sets `PS1`/`PROMPT_COMMAND`.
- **zsh:** `ZDOTDIR` → a temp dir whose `.zshrc` sources the real one, then sets
  `PROMPT`.
- **PowerShell:** `-NoExit -EncodedCommand <init>` (profile runs first; encoded
  because `Restricted` policy refuses script files).
- **cmd.exe:** `PROMPT` env var plus `/K <init.cmd>` (must end with `@echo on`).
- **fish / others:** best-effort; leave the user's prompt alone.

Every injected shell also gets: aliases from Settings, the banner, `SV_SHOW_*` /
`SV_DIR_SEGMENTS` env toggles (re-read each prompt), an **OSC 7** hook reporting
the cwd, and a `splitview` function emitting the private **OSC 5170** (`splitview
edit <file>`, `splitview settings`). The reader thread picks both out of the
stream (`sv_core::osc`).

The Settings preview mirrors the directory rendering in
`sv_core::paths::prompt_dir`; keep it in sync with `__sv_compact` /
`__sv_tail_segs` in `shell.rs`.

### Which shell: profiles

`profiles::detect()` lists installed shells. With no saved default
(`settings.shell.defaultProfile`) the first window shows the picker before
spawning anything; a failed spawn brings the picker back with the reason, never
an empty window. A `shell` in config.toml outranks the saved choice. Each pane
remembers its `ShellFamily` for quoting typed paths (`sv_core::quote`).

### Gotchas

- Setting `PS1` in the environment does not work — the rc file overwrites it.
- On macOS spawn a login shell, or PATH/Homebrew are missing.
- `ensure_utf8_locale` sets `LC_CTYPE` when no locale is set (a GUI app often has
  none), or bash mis-measures the prompt and line editing goes wrong. Keep it.
- **msys paths.** Git Bash reports `$PWD` like `/c/x`, `/tmp/x` or `D:/x`. The
  OSC 7 hook always prefixes a `/`; `sv_core::paths::msys_to_native` maps drive
  paths, `/tmp` (the user's temp dir) and other mounts (under the msys root
  derived from the shell's path, `msys_root`). Without this the sidebar can't
  follow a `cd /tmp`.

---

## 9. Keyboard

Prefix model like tmux. Default prefix **`Ctrl+A`** (configurable). Prefix twice
sends a literal prefix to the focused terminal (in the editor it selects all).

Input routing (`keys::route`, at the start of each window's frame, before any
widget reads input): pairing prompt > modal > keymap (+ the sidebar's own keys
when it has focus and no prefix is pending) > widgets. Whatever the keymap
consumes is **removed** from the frame's events, together with the `Text` event
the same keystroke produced. Outside prefix mode, keys flow to the focused widget
untouched. Ctrl+C/V/X arrive from winit as Copy/Paste/Cut events: Ctrl+C copies
when there is a selection, else sends `^C`.

### Default keymap (after the prefix)

| Action | Keys | Name |
|---|---|---|
| Split stacked / side by side | `-` `s` / `\` `v` | `split_down` / `split_right` |
| Focus | `h j k l`, arrows; `o` next | `focus_*`, `focus_next` |
| Move/swap | `Shift+h/j/k/l`, `Shift+arrows` | `move_*` |
| Resize | `Ctrl+h/j/k/l` | `resize_*` |
| Zoom | `z` | `toggle_zoom` |
| Close (confirms) | `x` | `close_pane` |
| Detach / new window | `d` / `n` | `detach_pane` / `new_window` |
| Background / list | `b` / `Shift+b` | `background_pane` / `show_background` |
| Explorer / Editor / Scratchpad | `e` / `Shift+e` / `w` | `toggle_sidebar` / `toggle_editor` / `toggle_scratchpad` |
| Settings / shell picker | `,` / `p` | `open_settings` / `pick_shell` |
| Palette / reload config | `Shift+p` / `r` | `command_palette` / `reload_config` |

Global (no prefix, all carry a modifier): `Ctrl+Shift+P` palette, `Ctrl+Shift+O`
insert a picked file path, `Ctrl+=`/`Ctrl+-`/`Ctrl+0` font size. Terminal-local:
`Shift+PageUp/PageDown/Home/End` scroll, `Ctrl+Shift+Enter` sends ESC CR,
`Ctrl+click` opens a URL. Adding an action: add it to `keymap::ACTIONS`, a default
binding, and a branch in `App::run_action` — palette and Settings → Keybindings
pick it up automatically.

### The Explorer (sidebar)

Left of the panes, outside the layout tree. Follows the focused pane's cwd (OSC
7), one view per pane (open folders, cursor). Directory reads run on worker
threads; while open, exactly the directories on screen are watched (`watch.rs`),
changes are debounced (~120 ms) and only changed directories re-read. Keys: `j/k`,
`l/h` (open/fold; `l` on a file opens it), `Enter` (toggle dir / insert path),
`c` cd, `e` edit, `y` insert path, `o` reveal in the OS file manager,
`m`/`Shift+m` next/previous changed row, `u` root up, `.` hidden files, `r` reload,
`<`/`>`/`=` width, `g/G`, `Esc` back. The edge drags; width persists.

Inside a git work tree the rows show `git status` (coloured name + letter,
folders a dot) and a recap row shows branch, ahead/behind and totals. It shells
out to the user's `git` with `--no-optional-locks` (never take `index.lock`
under a `git commit` typed next door) and `CREATE_NO_WINDOW` on Windows, off
thread, at most every 750 ms: on root change, watch events, `r`, and every
prompt of the focused pane (OSC 7), which is how commits/checkouts show up.

### The Editor

Right of the panes. egui `TextEdit` with a cached, highlighted `LayoutJob`
(re-laid out only when the text hash or font changes), a line-number gutter,
find/replace (`Ctrl+F`), `Ctrl+S` save, `Ctrl+W` close tab, `Alt+Z` wrap lines,
`Ctrl+PageUp/PageDown` tabs, `Ctrl+Alt+←/→` width, `Esc` back. Tab inserts the
file's detected indent; Enter keeps the line's indent. Only UTF-8 text under
4 MB opens. A watcher thread stats the visible file every 2 s and wakes the UI
**only if it changed**: a clean buffer follows the disk (minimal change), a dirty
one shows a warning; saving is conditional on the stamp it was read at.

### The Scratchpad

Right of the panes (outermost when the editor is open too), toggled with
`<prefix> w` or the top bar. Notes, pen strokes and shapes on a pannable
canvas; one document per project — the focused pane's git work tree, else its
cwd (`sv_core::scratch::project_root`) — stored as
`<config dir>/scratchpads/<fnv-1a of the path>.json` (`file_name`; never change
the hash, it would orphan every pad). Autosaved ~0.8 s after the last change and
on exit; a file that doesn't parse is copied to `.bak`, never overwritten. Keys
on the pad: `Enter` new note, `v t p l a r o x` tools, `Ctrl+Z` undo (memory only),
select tool: drag moves (grab a shape anywhere in its box), arrows nudge, `Delete` removes,
`Esc` leaves a note, then the pad. Settings → Panes sets which of Explorer /
Editor / Scratchpad a new window opens with.

### Claude usage (status bar)

`claude.rs`: a `sv-claude` thread reads `~/.claude/sessions/<pid>.json` every
5 s (live pids only), the context from the tail of each session's transcript,
and — while a session runs — the 5-hour/weekly limits from
`api.anthropic.com/api/oauth/usage` every 2 min with the token in
`~/.claude/.credentials.json` (undocumented endpoint: expect it to change). It
emits `UiEvent::Claude` only on change; the meters are hidden when no session
runs.

---

## 10. Hub API (process side)

`Hub` (in `Arc`) is shared by the UI, reader/waiter threads and remote sessions:
`spawn`, `write`, `resize` (PTY), `resize_both` (grid + PTY, remote), `close`,
`close_all`, `subscribe` (remote output), `subscribe_exits`, `terminal`, `seqs`,
`emit(UiEvent)` (send + repaint), `repaint`, `runtime()` (lazy tokio).

`UiEvent`: `Exit`, `Cwd`, `AppCommand`, `FsChanged`, `Listing`, `FilePicked`, `Claude`,
`Remote(RemoteUi::{Status, Pairing, Action})`.

Terminal bytes are **bytes** end to end (`&[u8]`), never round-tripped through
`String`.

---

## 11. Rust conventions

- `cargo fmt --all` and `cargo clippy --release --workspace --all-targets -- -D
  warnings` must be clean.
- **No `unwrap`/`expect`/`panic!` outside tests** in anything that runs per frame
  or on a PTY/remote thread. Return typed errors (`error.rs`, `thiserror`).
- **Locking discipline:** never hold the PTY registry lock across a blocking
  read or a send. The writer has its own lock and is never held while taking the
  terminal lock (the emulator writes replies from inside `advance`).
- **The UI thread never blocks:** directory reads, file dialogs, disk polling
  and remote work run elsewhere. (Opening/saving a file in the editor is a
  synchronous read/write of a small file — acceptable.)
- **Drop order on close:** kill the child, then drop the master, then forget it.
- Colors only from `theme.rs` (`tok::*` for chrome, `Skin` for the terminal).
- Release builds only (`--release` for build, test and clippy); the release
  profile is thin-LTO, 1 codegen unit, stripped, `panic = "abort"`.

---

## 12. UI conventions

- Region order per window: top bar → status bar → Explorer (left) → Scratchpad
  (right) → Editor (right) → central split area → overlays (modals, toast).
- One `TerminalView` per pane, kept for the pane's lifetime (no recreation on
  layout change — the widget state is just ids and caches).
- Long lists use `ScrollArea::show_rows` (Explorer).
- Modals take the whole keyboard of their window (`keys::take_all`) except the
  palette and Settings, which consume only the keys they use so text fields work.
- `widgets::*` for small painter-drawn widgets; dialogs via `dialogs::dialog`
  (scrim + centered frame on one Foreground layer).

---

## 13. Performance rules (hard requirements)

- **Reactive only.** No `request_repaint()` per frame. A widget that wants a
  future frame uses `request_repaint_after` with a real deadline. Every
  per-frame `request_repaint` loop found so far was a bug (focus requests while
  the window is in the background; a blinking text cursor).
- **Measure:** `SPLITVIEW_FRAME_STATS=<file>` appends frames-per-5s with egui's
  repaint causes. Targets: 0 frames/s idle with a terminal focused; < 1 % of a
  core idle with sidebar + editor open; ~40 MB private memory.
- Text runs: ASCII runs are drawn as one string per style run; anything else is
  placed per cell so fallback glyphs can't shift columns. egui caches galleys
  across frames.
- Resize is debounced 40 ms; grid and PTY change together.
- PTY reads are 64 KB; output is coalesced into whole frames by egui.

---

## 14. Visual rules

- Tokens: SecureWire's (`bg-0…bg-4`, `line`, `tx-0…tx-3`, accent teal, ok/warn/err/info).
- Pane divider: `Skin.divider` (1–4 px, default 1); the focused pane's interior
  edges and hovered/dragged dividers use `Skin.divider_active`.
- Pane title strip (24 px, optional): shell/title, cwd, size, hover tools; the
  focused pane's strip has the terminal background and a 2 px accent top line.
- Inactive-pane dimming is optional and off by default.
- The terminal cursor and the editor cursor do not blink.

---

## 15. Security

- Remote control is the only network surface and is off by default (§21).
- Program-supplied text (titles) is sanitized before it is drawn outside the
  terminal (`hub::sanitize`). OSC 52 (clipboard) and color queries are not
  honoured. Pasted text has ESC stripped and is bracketed when the app asked.
- URLs opened by `Ctrl+click` must be `http(s)://` and are passed as a single
  argument (never through a shell).
- The filesystem (sidebar/editor) is never exposed to remote clients.

---

## 16. Build, test, package

```bash
cargo build --release -p splitview
cargo test  --release --workspace
cargo clippy --release --workspace --all-targets -- -D warnings
cargo fmt --all
powershell -File packaging\windows\build-installer.ps1   # dist\SplitView_<v>_x64-setup.exe
splitview --shot out.png --scene main                    # screenshots (see README)
```

Tests and screenshot runs should set `SPLITVIEW_CONFIG_DIR` to a scratch
directory: config.toml, settings.json **and** the remote identity/audit log all
follow it.

---

## 17. Testing strategy

- `sv_core`: layout invariants, keymap state machine, config validation, path
  and quoting rules, OSC scanner (split chunks, bounds), tree rows, editor model,
  highlighter (spans contiguous, no panics on unterminated input).
- `sv_term`: emulation basics, DA replies, preview line, URL detection, ANSI
  snapshot round trip.
- App: settings persistence, key→chord mapping, a **real PTY round trip**
  (`hub::tests`), shell injection, profiles, files, watch, remote protocol
  (golden fixtures), auth, pairing, TLS pinning over loopback.
- UI changes: verify with `--shot` scenes (and, for input paths, by driving a
  test window — only ever by PID, never "all processes named splitview").

---

## 18. Non-goals

- No PowerShell/cmd **by default** (they are profiles the user picks).
- Not a persistent multiplexer: panes live with the process.
- No plugin system, no AI features in the terminal, no heavy theming engine.
- No mouse-only features.

---

## 19. Common tasks

- **New action:** `keymap::ACTIONS` + default binding + `App::run_action` branch.
- **New layout op:** pure function in `layout.rs` + tests, then an action.
- **New setting:** field in `settings.rs` (with `#[serde(default)]` defaults and
  clamping in `sanitized`), UI in `settings_ui.rs`, apply in `App::apply_settings`.
- **Prompt change:** `shell.rs` only.

---

## 20. Footguns

- **Stealing terminal keys.** Only intercept in prefix mode (or the explicit
  modifier-carrying globals). Remove the consumed key's `Text` event too.
- **Per-frame repaint loops** burn a core in the background (§13).
- **Ctrl+C/X/V never arrive as key presses.** egui-winit turns them into
  Copy/Cut/Paste (Paste only if the clipboard has text — otherwise *nothing*).
  `keys::route` treats a C/X/V release with no seen press as the chord while a
  prefix is pending, so `Ctrl+A, Ctrl+V` splits with any clipboard.
- **Dividers are registered after the panes.** egui gives a drag to the topmost
  widget; the terminals sense drags for selection and would steal the grab band.
- **A folder that leaves the watch set goes stale.** `Sidebar::rebuild` re-reads
  every directory that becomes shown again; don't trust a cached listing.
- **egui focus only lands while the OS window is focused** — request it then, once.
- **Resize mismatch.** Grid and PTY must change together (`out.resized` → `Hub::resize`).
- **Zombie children / leaked threads.** Kill → drop master → forget; the waiter
  thread reaps; the reader exits on EOF.
- **ConPTY never EOFs** while the master lives: exit comes from `child.wait()`.
- **ConPTY titles are the exe path** (`…\bash.exe`): show the shell name instead.
- **msys `/tmp` and friends** (§8).
- **Watching a directory on Windows locks its ancestors** — `watch.rs` watches only
  the outermost directory recursively on Windows; don't "simplify" it.
- **The editor buffer only holds `\n`** — always go through `decode_text`/`encode_text`.
- **Remote input bypasses the UI.** It goes straight to the PTY; the echo comes
  back through the reader. Never add local echo.
- **The roster must never be published while remote control is off.**
- **A pane's `seq` counts chunks**, not bytes.
- **`Notify::notify_waiters()` only wakes a waiter that already polled** —
  revocation uses `notify_one()`.
- **Enabling remote control binds a LAN port** and on Windows triggers a firewall
  prompt the first time — tests should prefer loopback.

---

## 21. Remote control

An **opt-in** encrypted WebSocket server: a paired phone sees the live panes,
attaches to one, types into it, creates or closes panes. Normative design and
wire protocol: [`docs/remote-control-desktop.md`](docs/remote-control-desktop.md)
(the Flutter client's view: `docs/remote-control-mobile.md`; golden vectors in
`docs/remote/fixtures/`, asserted by `remote/protocol.rs`). The desktop document
wins if they disagree. Its file paths predate 0.3; see the mapping at its top.

Rules:

1. **Input goes straight to the PTY** (`Hub::write`).
2. **Output fans out in the reader thread**: the emulator, plus a
   `tokio::sync::broadcast` for subscribers. A slow receiver is lagged and
   resynced, never waited on. With nothing subscribed it costs one atomic load.
3. **Layout changes go through the UI**: `create_pane`/`close_pane` arrive as
   `UiEvent::Remote(RemoteUi::Action)` addressed to a window label (`main`,
   `sv-win-N`, `detached-N`) and use the normal layout operations.
4. **Snapshots** (a client attaching) are read directly from the emulator
   (`Terminal::snapshot_ansi`) on a blocking task.
5. **The roster** (titles, cwd, size, preview line, background flag per window)
   is published by the app — debounced ~150 ms, plus every 2 s while a session
   is live — and **only while the server is on**.

Security posture (unchanged): off by default and not restored at launch unless
asked; TLS with a pinned self-signed certificate (fingerprint via the pairing
QR); pairing = single-use 8-char code (120 s, 5 attempts) **and** a desktop
approval prompt that needs a click (Esc rejects); 256-bit tokens stored as
SHA-256, constant-time compare, revocable (cuts live sessions); 4 sessions,
30-min idle timeout, 10 attempts/IP/min, one path (`/remote`); remote resize is
a setting; every pair/approve/reject/connect/disconnect/revoke goes to
`remote/audit.log`; the top bar shows an indicator while the server is on, with a
dot while a session is live.

Reachability: LAN + mDNS (`_splitview._tcp`); Tailscale or another overlay for
anything further. A sleeping laptop is a dead server.

---

_When in doubt: keyboard-first, thin lines, real PTYs, no idle frames._
