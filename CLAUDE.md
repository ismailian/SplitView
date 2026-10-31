# CLAUDE.md — SplitView

> Context file for AI coding agents (and humans) working in this repository.
> Read this **before** writing code. It encodes the architecture, conventions,
> domain vocabulary, and footguns specific to SplitView. When something here
> conflicts with a generic instinct, follow this file.

---

## 1. What SplitView is

SplitView is a **minimalistic, keyboard-driven terminal emulator** for the
desktop, in the spirit of a tmux layout but as a native GUI app. The whole
point is fast, distraction-free terminal work where the UI gets out of the way:
thin dividers instead of chrome, every action reachable from the keyboard, and
terminal performance that feels native rather than "a terminal inside a web
page."

It is built with **Rust + Tauri**. Rust owns the pseudo-terminals (PTYs) and
process lifecycle; the webview frontend owns rendering, the split layout, and
the keymap.

### The features that define the product (do not regress these)

- **Split panes** horizontally and vertically.
- **Switch focus** between panes by keyboard.
- **Move / swap** panes around the layout by keyboard.
- **Zoom** a single pane to fill the window for focus, then restore.
- **Keyboard-only operation**: every one of the above is reachable without the
  mouse. The mouse is a convenience, never a requirement.
- **Thin separators**, never full borders around panes.
- A **minimal shell experience** (think tmux/plain bash, *not* PowerShell or
  cmd.exe) with a custom prompt of the form `` {user} @ {cwd} $ ``.
- A **custom terminal that still feels native**: real PTYs, real interactivity,
  full ANSI support, low input latency, GPU-accelerated rendering.

---

## 2. Design principles (the non-negotiables)

1. **Keyboard-first.** If you add a feature, you add a keybinding for it. A
   feature that only works with the mouse is incomplete.
2. **Minimal chrome.** No pane borders, no rounded corners, no drop shadows, no
   gradient toolbars. Panes are separated by a **1px line** and nothing more.
   Anything that adds visual weight needs a strong justification.
3. **Native feel beats fancy.** Latency and correctness of the terminal come
   first. Never trade input responsiveness or ANSI correctness for a visual
   nicety.
4. **The shell is the shell.** SplitView spawns a real login shell in a real
   PTY. We do not reimplement a shell, we do not parse the user's commands, and
   we do not intercept terminal keystrokes unless the user is in *prefix mode*
   (see §9).
5. **Rust owns processes, the frontend owns layout.** Keep that boundary clean.
   The frontend should never need to know how a PTY is spawned; the backend
   should never need to know how panes are arranged on screen.

---

## 3. Tech stack

| Layer | Choice | Notes |
|---|---|---|
| App shell | **Tauri 2.x** | Rust core process + system webview UI. Pin the latest stable 2.x; verify versions in `Cargo.toml` / `package.json` before assuming an API exists. |
| Backend language | **Rust** (2021 edition) | PTY management, IPC commands, app state. |
| PTY | **`portable-pty`** | Cross-platform PTY spawn/read/write/resize. The de-facto crate for this (from the WezTerm project). |
| Error handling | **`thiserror`** (+ `anyhow` for internal glue) | Command errors must be serializable to the frontend. |
| Frontend language | **TypeScript** | Strict mode on. |
| Frontend tooling | **Vite** | Tauri's default dev server / bundler. |
| Terminal rendering | **`@xterm/xterm`** + addons | `@xterm/addon-fit`, `@xterm/addon-webgl`, optionally `@xterm/addon-web-links`, `@xterm/addon-search`. Note the **scoped `@xterm/*` package names** — the old unscoped `xterm` packages are legacy. |
| Editor panel | **CodeMirror 6** (`codemirror`, `@codemirror/*`) | One `EditorView`, one `EditorState` per tab. Grammars come from `@codemirror/language-data` as lazy chunks. |
| Layout / UI | **Plain TypeScript** (no heavy framework) | The terminal surface and split layout are managed imperatively for predictable performance. Avoid introducing React/Vue for the terminal grid; re-render storms are the enemy here. A tiny reactive helper for non-terminal UI is acceptable if justified. |
| Config | **TOML** | User config at `~/.config/splitview/config.toml` (platform-appropriate dir). |
| Remote control | **`tokio` + `tokio-tungstenite` + `tokio-rustls`/`rustls` (ring) + `rcgen` + `mdns-sd`** | Opt-in encrypted WebSocket server; see §21. `ring` is the pinned crypto provider — the default `aws-lc-rs` needs cmake and nasm, which the Tauri toolchain does not already provide. |

**Primary platforms: macOS and Linux.** Windows is explicitly secondary and we
do **not** default to PowerShell or cmd.exe (see §18). If Windows is ever
supported it goes through a POSIX-style shell, not the Windows prompt.

---

## 4. Architecture overview

```
┌──────────────────────────────────────────────────────────────┐
│  Webview (Frontend, TypeScript)                                │
│                                                                │
│   Keymap ── dispatches ──▶ Layout tree ── renders ──▶ Panes    │
│     │                          │                        │      │
│     │                          │                  xterm.js per  │
│     │                          │                  leaf pane      │
│     │                          ▼                        │       │
│     │                    1px dividers                    │      │
│     └──────────── invoke() / Channel ────────────────────┘      │
│                              │                                  │
└──────────────────────────────┼─────────────────────────────────┘
                                │  Tauri IPC
┌──────────────────────────────┼─────────────────────────────────┐
│  Rust core (Backend)          ▼                                 │
│                                                                 │
│   Commands ──▶ AppState { panes: HashMap<PaneId, PtyHandle> }   │
│                       │                                         │
│            portable-pty: spawn shell in PTY                     │
│                       │                                         │
│        reader thread ─┴─▶ streams bytes ─▶ Channel ─▶ frontend  │
└─────────────────────────────────────────────────────────────────┘
```

### PTY lifecycle / data flow (read this carefully)

1. Frontend creates a leaf pane in the layout tree, mounts an xterm.js instance,
   and calls `spawn_pty(...)` passing a **Tauri `Channel<Vec<u8>>`** for output.
2. Backend opens a PTY (`portable_pty::native_pty_system().openpty(...)`),
   builds a `CommandBuilder` for the user's shell, spawns the child, stores a
   `PtyHandle` in `AppState` keyed by `PaneId`, and starts a **dedicated reader
   thread**.
3. The reader thread blocks on the PTY's reader, reads into a buffer
   (8–64 KB), and forwards each chunk over the `Channel`. The frontend writes
   chunks straight into `term.write(bytes)`.
4. Keystrokes in xterm.js fire `onData`, which calls `write_pty(pane_id, bytes)`;
   the backend writes to the PTY master.
5. Resize: the fit addon computes `{cols, rows}`; if they changed, the frontend
   calls `resize_pty(pane_id, cols, rows)`, which calls `master.resize(...)`.
6. On close, the backend kills the child, drops the master (which signals EOF to
   the reader thread so it exits), and removes the handle from `AppState`.

**Scrollback lives in xterm.js, not in Rust.** The backend is a dumb, fast pipe.

---

## 5. Repository structure

```
splitview/
├── CLAUDE.md                  ← this file
├── package.json
├── vite.config.ts
├── tsconfig.json
├── src/                       ← frontend (TypeScript)
│   ├── main.ts                ← app bootstrap + layout-window controller
│   ├── paths.ts               ← native ⇄ shell path helpers (TESTED)
│   ├── shellQuote.ts          ← POSIX-quote a path for the prompt
│   ├── terminal/
│   │   ├── Pane.ts            ← xterm.js wrapper + PTY wiring per leaf
│   │   ├── DetachedApp.ts     ← controller for a popped-out single-pane window
│   │   └── renderer.ts        ← webgl/canvas renderer setup, theme application
│   ├── layout/
│   │   ├── tree.ts            ← layout tree types + pure operations (TESTED)
│   │   ├── render.ts          ← maps tree -> DOM, draws dividers
│   │   ├── divider.ts         ← draggable 1px separators
│   │   └── edgeDrag.ts        ← drag handle shared by the side panels
│   ├── sidebar/
│   │   ├── FileTree.ts        ← terminal-styled filesystem sidebar (live, per-pane views)
│   │   └── model.ts           ← tree rows + watch set as pure functions (TESTED)
│   ├── editor/
│   │   ├── EditorPanel.ts     ← the code editor docked on the right (tabs, disk)
│   │   ├── model.ts           ← EOL/BOM round trip, indent detection (TESTED)
│   │   └── theme.ts           ← CodeMirror theme, from the CSS variables
│   ├── background/
│   │   └── BackgroundPanel.ts ← backgrounded-pane indicator + picker
│   ├── remote/                ← remote control, webview side (see §21)
│   │   ├── RemoteBridge.ts    ← publishes the roster, relays remote actions
│   │   ├── roster.ts          ← pane list -> flat remote roster (TESTED)
│   │   └── qr.ts              ← pairing QR encoder, no deps (TESTED)
│   ├── settings/              ← in-app Settings window (model + DOM)
│   │   └── RemoteSection.ts   ← the Remote panel: enable, pair, devices
│   ├── keymap/
│   │   ├── keymap.ts          ← prefix engine + binding registry
│   │   └── actions.ts         ← action handlers (split, focus, move, zoom...)
│   ├── ipc/
│   │   ├── commands.ts        ← thin typed wrappers over Tauri invoke()/Channel
│   │   └── windows.ts         ← window creation (new / detached windows)
│   ├── config/
│   │   └── config.ts          ← load/parse/validate TOML config
│   └── styles/
│       └── theme.css          ← CSS variables; divider + colors live here
│                                (+ settings.css, sidebar.css, editor.css,
│                                 background.css, remote.css)
└── src-tauri/                 ← backend (Rust)
    ├── Cargo.toml
    ├── tauri.conf.json
    ├── build.rs
    ├── capabilities/
    │   └── default.json       ← Tauri v2 permissions (expose only what we use)
    └── src/
        ├── main.rs            ← entrypoint, builder, invoke_handler
        ├── lib.rs
        ├── state.rs           ← AppState, PtyHandle, PaneId
        ├── pty.rs             ← spawn/read/write/resize/kill via portable-pty
        ├── shell.rs           ← shell detection + prompt injection (see §8)
        ├── watch.rs           ← file-sidebar filesystem watch (notify), debounced
        ├── files.rs           ← text file read/stat/save for the editor (TESTED)
        ├── commands.rs        ← #[tauri::command] functions
        └── remote/            ← remote control, backend side (see §21)
            ├── mod.rs         ← RemoteState: server, roster, devices, audit
            ├── protocol.rs    ← wire frames + messages, pure (TESTED)
            ├── server.rs      ← TLS + WebSocket accept loop (TESTED)
            ├── session.rs     ← one client: subscriptions, coalescing
            ├── auth.rs        ← device registry, token issue/verify (TESTED)
            ├── pairing.rs     ← pairing codes, QR payload, rate limit (TESTED)
            ├── identity.rs    ← self-signed cert + fingerprint (TESTED)
            └── discovery.rs   ← mDNS `_splitview._tcp` advertisement

docs/
├── remote-control-desktop.md  ← the normative remote design + wire spec
├── remote-control-mobile.md   ← the Flutter client's companion document
└── remote/fixtures/*.json     ← golden protocol vectors, shared with the client
```

---

## 6. Glossary / domain concepts

- **PTY (pseudo-terminal):** the kernel object pair (master/slave) that lets a
  GUI program talk to a shell as if it were a real terminal. The shell writes to
  the slave; we read/write the master.
- **Pane:** one terminal in the UI. Backed by exactly one PTY and one xterm.js
  instance. A pane is always a **leaf** in the layout tree.
- **Layout tree:** a binary tree describing the split arrangement. A node is
  either a `leaf` (a pane) or a `split` (two children + an orientation + a
  ratio). Identical mental model to tmux/i3.
- **Orientation:** described by the **divider line**, not the axis of motion:
  - **Horizontal split** → a *horizontal* line → panes stacked **top/bottom**.
  - **Vertical split** → a *vertical* line → panes side by side **left/right**.
  - Internally we use the unambiguous verbs **`split_down`** and
    **`split_right`** in code. UI-facing labels map: "split horizontally" =
    `split_down`, "split vertically" = `split_right`. (See §9 diagrams.)
- **Focus:** the active pane, which receives keyboard input and is visually
  indicated by the *adjacent divider*, never a heavy border.
- **Zoom:** temporarily expand the focused pane to fill the window, hiding its
  siblings, without destroying the tree. Toggling zoom restores the layout.
- **Prefix mode:** a tmux-style modal state entered by the prefix key, in which
  the next keystroke is interpreted as a SplitView command instead of being sent
  to the shell. This is how we avoid stealing keys from vim/emacs/etc.

---

## 7. Layout tree contract

`src/layout/tree.ts` holds the canonical model. Keep these as **pure
functions** with no DOM/IPC side effects so they can be unit-tested.

```ts
type PaneId = string;

type Node =
  | { kind: "leaf"; paneId: PaneId }
  | {
      kind: "split";
      orientation: "row" | "column"; // row = side-by-side (vertical divider)
                                      // column = stacked (horizontal divider)
      ratio: number;                  // 0..1, size of the first child
      first: Node;
      second: Node;
    };

interface Layout {
  root: Node | null;
  focused: PaneId | null;
  zoomed: PaneId | null; // non-null when a pane is zoomed
}
```

Required operations (all pure, all tested):

- `splitFocused(layout, dir: "down" | "right", newPaneId): Layout`
- `focusDirection(layout, dir: "up"|"down"|"left"|"right"): Layout`
- `focusNext(layout) / focusPrev(layout): Layout`
- `movePane(layout, dir): Layout`  — swap focused with neighbor in a direction
- `closeFocused(layout): { layout, removedPaneId }` — collapse the split,
  promote the sibling, keep ratios sane
- `toggleZoom(layout): Layout`
- `setRatio(layout, splitPathOrId, ratio): Layout` — for divider dragging

Invariants the tests must guard:

- A `split` node always has exactly two children.
- Closing a pane never leaves a `split` with a single child (the sibling is
  promoted in its place).
- `ratio` stays clamped to a sane range (e.g. `0.05..0.95`) so panes never
  collapse to zero.
- `focused` always points at an existing leaf (or null when empty).
- Zoom is purely presentational: the tree shape is unchanged while zoomed.

---

## 8. The terminal prompt (`{user} @ {cwd} $`) — important, easy to get wrong

**The terminal emulator does not draw the prompt. The shell does.** xterm.js
just renders whatever bytes the shell emits. So to get the prompt

```
{user} @ {working directory} $
```

SplitView must make the *shell* produce it, and it must do so **without
destroying the user's normal environment** (PATH, aliases, completions).

### Strategy: inject prompt at spawn, per shell

Detect the shell (`$SHELL`, falling back to `bash` → `zsh` → `sh`) and spawn it
with a custom init that sources the user's normal config first, then overrides
only the prompt variable.

- **bash:** spawn `bash --rcfile <tmp_rcfile>` (or set `PROMPT_COMMAND`), where
  `<tmp_rcfile>` does:
  ```bash
  [ -f "$HOME/.bashrc" ] && source "$HOME/.bashrc"
  PS1='\u @ \w \$ '     # \u=user  \w=cwd  \$ = $ (or # for root)
  ```
- **zsh:** point `ZDOTDIR` at a temp dir whose `.zshrc` sources the real one,
  then sets:
  ```zsh
  source "${HOME}/.zshrc" 2>/dev/null
  PROMPT='%n @ %~ %# '   # %n=user  %~=cwd  %#= % / #
  ```
- **fish / others:** documented best-effort; if we can't cleanly inject, fall
  back to the user's own prompt and log a warning rather than mangle it.

The prompt template is configurable (`prompt = "{user} @ {cwd} $ "` in config).
`shell.rs` owns translating that template into per-shell prompt syntax. Keep
that mapping in one place.

Which segments the prompt shows, and how much of the working directory it shows,
are Settings toggles passed to the shell as environment variables the prompt
hook re-reads on every prompt: `SV_SHOW_*` for the segments, and
`SV_DIR_SEGMENTS` for the directory — `0` = the whole path (middle elided once
it is deep), `n` = only the last `n` segments (`2` →
`project-x/mobile-app`). That translation lives in `shell.rs` too; the Settings
preview mirrors it in `promptDir` (`src/paths.ts`) and the two must stay in sync.

### Gotcha

Setting `PS1` as an *environment variable* before spawning does **not** work —
the user's rc file runs *after* and overwrites it. You must inject *after* the
rc file runs. That is the whole reason for the `--rcfile` / `ZDOTDIR` dance.

On **macOS**, spawn a **login shell** (e.g. include `-l`) or source the login
profile so PATH and Homebrew are populated; otherwise users get a broken
environment.

---

## 9. Keyboard shortcuts

SplitView uses a **prefix model** like tmux to avoid stealing keys from
terminal programs. Default prefix is **`Ctrl+A`** (configurable). Press the
prefix, then a command key. Pressing the prefix **twice** sends a literal
prefix keystroke to the shell.

The keymap is **data-driven**: bindings live in a registry and are overridable
from config. Don't hardcode key handling inside action functions.

### Split terminology, made unambiguous

```
split VERTICALLY (vertical divider, `split_right`)
+--------+--------+
|        |        |
| pane A | pane B |          A is focused, B is new
|        |        |
+--------+--------+

split HORIZONTALLY (horizontal divider, `split_down`)
+-----------------+
|     pane A      |
+-----------------+          A is focused, B is new
|     pane B      |
+-----------------+
```

### Default keymap

| Action | Default binding (after prefix) | Internal action |
|---|---|---|
| Split horizontally (stacked) | `-` or `s` | `split_down` |
| Split vertically (side by side) | `\` or `v` | `split_right` |
| Focus left / down / up / right | `h` / `j` / `k` / `l` or arrows | `focus_dir` |
| Cycle focus to next pane | `o` | `focus_next` |
| Move/swap pane left/down/up/right | `Shift+h/j/k/l` or `Shift+arrows` | `move_pane` |
| Toggle zoom (fullscreen focus) | `z` | `toggle_zoom` |
| Resize focused pane | `Ctrl+h/j/k/l` (held in prefix) | `resize_pane` |
| Close focused pane | `x` (with confirm) | `close_pane` |
| Detach pane into its own window | `d` | `detach_pane` |
| Background pane (keeps running) | `b` | `background_pane` |
| List / restore backgrounded panes | `Shift+b` | `show_background` |
| Open a new window | `n` | `new_window` |
| Toggle the file sidebar | `e` | `toggle_sidebar` |
| Toggle the editor panel | `Shift+e` | `toggle_editor` |
| Open Settings | `,` | `open_settings` |
| Reload config | `r` | `reload_config` |
| Send literal prefix to shell | press prefix again | — |

### Backgrounded panes

`background_pane` removes the focused pane from the layout tree **without
killing anything**: the PTY, the xterm.js instance and its scrollback stay alive,
just hidden (`display:none`), so a dev server keeps running and keeps buffering
output. The picker (`show_background`) lists them with a live preview of each
one's last output line; restoring splices the pane back beside the focused one.
Because an invisible running process is a trap, a small corner indicator shows
the count whenever any exist, closing the window kills them, and closing the
*last visible* pane restores the newest one instead of leaving nothing.

### The file sidebar

A `tree(1)`-styled column left of the pane area, outside the layout tree (it is
not a pane and has no PTY). It follows the focused terminal: it shows that pane's
directory — the cwd the shell reports over OSC 7 — and re-roots when focus moves
or the shell `cd`s. Each pane has its own view (open folders, cursor, scroll),
kept in memory while the app runs, so switching panes and back finds the tree as
it was. It is live: while open, the backend watches exactly the directories on
screen (`fs_watch*`, `watch.rs`) and the tree re-reads the ones where an entry was
created, removed or renamed — content writes are ignored, bursts are coalesced
(~120 ms), and only changed rows are re-rendered. It is navigated with `j`/`k`, `l`/`h` to
fold, Enter to expand a directory or insert a file's path at the prompt, `c` to
`cd` the terminal there, `u` to go up, `.` for hidden files, `r` to reload,
`<`/`>` to narrow/widen the column (`=` resets it; the edge can also be dragged,
and the width persists), Escape to return to the terminal. Paths are converted to
the shell's form (`C:\Users\x` → `/c/Users/x` for msys) before being typed; see
`src/paths.ts`. Resizing it resizes the pane area, so the terminals are refit
(and their PTYs with them).

### The editor panel

A code editor docked right of the pane area, outside the layout tree (like the
sidebar, it is not a pane and has no PTY), for changing a file without leaving
the terminal. CodeMirror does the editing; `EditorPanel` owns the tabs, the disk
and the panel. Open a file with `e` on it in the sidebar (or double-click), or
`splitview edit <file>` from a shell, relative to that shell's directory. With no
file open, `<prefix> E` sends you to the sidebar to pick one. It exists only in
layout windows, not detached ones.

Its own chords: `Ctrl+S` save, `Ctrl+W` close tab, `Ctrl+PageUp/PageDown` switch
tab, `Ctrl+Alt+Left/Right` resize (the edge can also be dragged, and the width
persists), `Ctrl+F` find and replace, Escape to return to the terminal. They are
CodeMirror bindings on its editing surface, so the document-level keymap still
sees every key first and the prefix works as it does anywhere. The cost is that
`Ctrl+A` is the prefix here too: pressing it twice selects all, because the
literal prefix goes to whatever has focus, never to a shell behind the editor.

Hiding the panel loses nothing; tabs and unsaved changes stay. Closing a dirty tab
or a window with dirty tabs asks first. A file changes under an open buffer all
the time, because the terminal is right there, so the visible file is stat'ed
every 2 s while the panel is open: a clean buffer follows the disk (as a minimal
change, outside the undo history, leaving the cursor alone), and a dirty one shows
a warning. Saving is conditional on the `FileStamp` the buffer was read at, so
overwriting someone else's change takes an explicit yes. It polls because the
sidebar's watch deliberately ignores content writes. Only UTF-8 text under 8 MB
opens; anything else is refused rather than risk saving it back corrupted.

### Implementation notes

- The keymap engine listens at the document level. **It must not swallow keys
  unless prefix mode is active** (or the chord is an explicit non-prefix global
  binding the user opted into). Outside prefix mode, keys flow to the focused
  xterm.js instance untouched.
- Prefix mode has a short timeout; an unrecognized key cancels it silently.
- Resize/move operate on the focused pane and mutate the layout tree, then
  re-render. Resizing must also `resize_pty` for the affected leaves.

---

## 10. IPC contract (frontend ⇄ backend)

Keep all Tauri calls behind typed wrappers in `src/ipc/commands.ts`. Never call
`invoke` from random UI code.

### Commands (Rust `#[tauri::command]`)

| Command | Signature (logical) | Purpose |
|---|---|---|
| `spawn_pty` | `(channel: Channel<Vec<u8>>, opts: SpawnOpts) -> Result<PaneId>` | Open PTY, spawn shell, start reader thread streaming to `channel`. |
| `write_pty` | `(pane_id: PaneId, data: Vec<u8>) -> Result<()>` | Write keystrokes/bytes to the PTY master. |
| `resize_pty` | `(pane_id: PaneId, cols: u16, rows: u16) -> Result<()>` | Resize the PTY. |
| `close_pty` | `(pane_id: PaneId) -> Result<()>` | Kill child, drop master, remove handle. |
| `list_panes` | `() -> Result<Vec<PaneId>>` | Mostly for debugging/recovery. |
| `read_dir` | `(path: Option<String>) -> Result<DirListing>` | Directory listing for the file sidebar. Accepts msys paths (normalized), sorts dirs-first, caps the entry count. |
| `fs_watch` | `(window, on_change: Channel<Vec<String>>) -> Result<()>` | Start the calling window's sidebar watch; batches of changed directories arrive on the channel. Dropped automatically when the window is destroyed. |
| `fs_watch_dirs` | `(window, dirs: Vec<String>) -> Result<()>` | Replace the watched set (the sidebar's root + open folders); only the delta reaches the OS. |
| `fs_unwatch` | `(window) -> Result<()>` | Stop the window's watch (sidebar closed). |
| `read_text_file` | `(path: String) -> Result<TextFile>` | Open a file for the editor. Refuses binary, non-UTF-8 and oversized files. |
| `file_stamp` | `(path: String) -> Result<Option<FileStamp>>` | A file's version, `None` if it is gone. |
| `write_text_file` | `(path: String, content: String, expected: Option<FileStamp>) -> Result<Option<FileStamp>>` | Atomic save. `None` back means the file is no longer at `expected` and nothing was written; `expected: None` overwrites. |
| `publish_panes` | `(snapshot: WindowPanes) -> Result<()>` | Publish this window's pane roster for remote clients. **Ignored while remote control is off** (§21). |
| `provide_snapshot` | `(request_id: u64, pane_id, serialized, cols, rows) -> Result<()>` | Answer a `remote-snapshot-request` with a serialized xterm.js buffer. |
| `remote_status` | `() -> Result<RemoteStatus>` | Server state, address, fingerprint, devices, sessions, live pairing code. |
| `remote_enable` | `(config: RemoteConfig) -> Result<RemoteStatus>` | Generate/load the TLS identity, bind, advertise over mDNS. |
| `remote_disable` | `() -> Result<RemoteStatus>` | Stop accepting, end every session, forget the roster. |
| `remote_pairing_start` | `() -> Result<RemoteStatus>` | Issue a single-use pairing code (120 s). |
| `remote_pairing_cancel` | `() -> Result<RemoteStatus>` | Drop the live pairing code. |
| `remote_pairing_respond` | `(request_id: u64, approve: bool) -> Result<()>` | The answer to a `remote-pairing-request`. Nothing is issued until this says `true`. |
| `remote_devices` | `() -> Result<Vec<DeviceInfo>>` | Paired devices, newest first. |
| `remote_revoke_device` | `(device_id: String) -> Result<RemoteStatus>` | Forget a device **and** cut any session using it. |
| `remote_disconnect_session` | `(session_id: u64) -> Result<RemoteStatus>` | End one live session without revoking its device. |

### Events (backend → frontend)

| Event | Payload | Purpose |
|---|---|---|
| `pty-exit` | `PaneId` | A pane's shell exited on its own; close the pane. |
| `reattach-pane` | `{ paneId, owner }` | A detached window asking its owner to adopt its pane back. |
| `remote-action` | `{ windowLabel, action, paneId?, cwd? }` | Addressed: create or close a pane on a remote client's behalf (§21). |
| `remote-snapshot-request` | `{ windowLabel, requestId, paneId }` | Addressed: serialize a pane for a client that just attached. |
| `remote-pairing-request` | `{ requestId, deviceName, platform, address }` | Addressed to the target window: show the approval prompt. |
| `remote-status-changed` | `RemoteStatus` | Broadcast: the server was enabled/disabled, or a session came or went. |

`SpawnOpts` includes optional `cwd`, optional `shell` override, and the prompt
template (or the backend reads it from loaded config).

### Streaming

Use a **Tauri `Channel<Vec<u8>>`** per pane for PTY → frontend output. Prefer
this over `emit`/event-per-chunk: it is the efficient path and keeps ordering.
Do **not** emit one IPC message per byte.

### Data must be byte-safe

Terminal output is arbitrary bytes, not UTF-8 text. Pass it as **byte arrays**
(`Vec<u8>` / `Uint8Array`). Do **not** round-trip terminal data through a JSON
string — it will corrupt non-UTF-8 sequences and break rendering.

---

## 11. Rust backend conventions

- `rustfmt` and `clippy` are required; CI fails on warnings. Run `cargo fmt` and
  `cargo clippy --all-targets -- -D warnings` before finishing.
- **No `unwrap()` / `expect()` / `panic!` in command handlers or the reader
  thread.** A panic in the core process can take down the app. Return a typed
  `Error` that serializes to the frontend.
- Define one error enum with `thiserror`, `impl serde::Serialize`, so commands
  return `Result<T, Error>` and the frontend gets a structured reason.
- `AppState` example:
  ```rust
  pub struct PtyHandle {
      writer: Box<dyn std::io::Write + Send>,
      master: Box<dyn portable_pty::MasterPty + Send>,
      child:  Box<dyn portable_pty::Child + Send>,
  }
  pub struct AppState {
      pub panes: std::sync::Mutex<std::collections::HashMap<PaneId, PtyHandle>>,
  }
  ```
- **Locking discipline:** never hold the `panes` lock across a blocking PTY read
  or a `Channel` send. Clone out what you need (the writer is needed under lock
  only briefly for `write_pty`).
- **Reader thread:** one per PTY, `std::thread::spawn`. It reads into a buffer
  and sends chunks to the `Channel`. It exits when the read returns EOF (master
  dropped) — which is how `close_pty` stops it. Don't leak threads.
- **Drop order on close:** kill the child first, then drop the master, then
  remove from the map. Otherwise you risk zombies or a reader thread spinning.
- Register exactly the commands you implement in `invoke_handler!` and grant
  exactly the matching permissions in `capabilities/` (see §15).

---

## 12. Frontend conventions

- TypeScript `strict` mode. No `any` without a written reason.
- The **layout tree module is pure** and framework-free; the renderer reads the
  tree and reconciles the DOM. Keep IPC and DOM out of `layout/tree.ts`.
- One xterm.js instance per leaf. **Reuse** instances where possible across
  re-renders; don't tear down and recreate a terminal on every layout change
  (it loses scrollback and flickers). Re-parent the DOM node instead.
- After any layout change that resizes a pane, call the fit addon and then
  `resize_pty` with the new `{cols, rows}` — but only if they actually changed.
- All keyboard handling goes through `keymap/`. UI components don't add their
  own ad-hoc `keydown` listeners that could fight the prefix engine.

---

## 13. Rendering & performance rules

These exist because "a terminal in a webview" can feel sluggish if done
naively. Treat them as hard requirements.

- Use the **WebGL renderer addon** (`@xterm/addon-webgl`); fall back to the
  canvas renderer if WebGL is unavailable. Apply it once per terminal at setup.
- **Stream in chunks**, write straight to `term.write()` — xterm.js buffers and
  flushes on its own animation frame. Don't add your own per-keystroke flush
  loop on top.
- **Debounce resize** (~30–60 ms) and divider-drag-driven resizes. Sending a
  PTY resize on every mousemove pixel is wasteful and causes reflow storms.
- Don't keep terminal scrollback in Rust or in app state — xterm.js owns it.
- Avoid layout thrash: batch DOM reads/writes when reconciling the split tree.
- Keep the focused-pane indicator cheap (a 1px divider color change / subtle
  tint), never a re-layout.

---

## 14. Visual / styling rules

- **Separators are 1px lines.** No borders around panes, no rounded corners, no
  shadows. The divider is the only thing between two panes.
- All colors live as **CSS variables** in `styles/theme.css`
  (`--divider`, `--divider-active`, `--bg`, `--fg`, plus the 16 ANSI colors).
  Don't hardcode hex values in components.
- The **active pane** is indicated subtly: a slightly brighter color on the
  divider adjacent to it, or a faint background tint — *not* a thick highlighted
  border. When in doubt, less.
- Inactive panes may be dimmed very slightly; this must be optional and off by
  default if it hurts readability.
- Font is a user-configurable monospace family with a sensible mono default. Let
  xterm.js own font metrics; don't fight it with CSS.
- The window itself stays minimal — no busy toolbars. A thin/zero title bar is
  preferred where the platform allows it.

---

## 15. Tauri capabilities / security

Tauri v2 uses an explicit permission model. **Expose only what SplitView
needs.** In `src-tauri/capabilities/default.json`, allow exactly the custom
commands (`spawn_pty`, `write_pty`, `resize_pty`, `close_pty`, `list_panes`) and
nothing broad. Do not enable filesystem/shell plugin permissions the app
doesn't use. Spawning the shell happens through our own `portable-pty` code, not
through an over-broad plugin grant. When you add a command, add its permission
in the same change or the call will be rejected at runtime.

**Capabilities are scoped by window label.** `default.json` lists the label
patterns it applies to (`main`, `detached-*`, `sv-win-*`). A window opened at
runtime whose label matches none of them gets no core permissions, and the
failure is silent-ish: the window opens but `emit`/`listen`/`close` are denied.
If you introduce a new window label, add its pattern there in the same change.

The remote-control commands (§21) are app-local like the rest, so they need no
extra grant — but the **events** they depend on do travel over `core:event`,
which `core:default` provides *per listed label*. A window whose label matches
nothing in `default.json` never hears `remote-action`,
`remote-snapshot-request` or `remote-pairing-request`, and remote control will
quietly skip it. Remote control adds no plugin permissions and no filesystem
grant: `read_dir`, `pick_file` and the editor's file commands are **never**
exposed to a remote client.

**Closing a window needs `core:window:allow-destroy`, not just `allow-close`.**
As soon as a window registers an `onCloseRequested` listener — every SplitView
window does, to kill its shells first — Tauri prevents the native close and
leaves the decision to the frontend. `close()` from inside that handler only
re-enters it; only `destroy()` actually closes the window (see
`destroyCurrentWindow` in `src/ipc/windows.ts`).

---

## 16. Build, dev, and test commands

```bash
# install frontend deps
npm install

# run the app in dev (hot-reloads frontend, rebuilds Rust on change)
npm run tauri dev

# production bundle
npm run tauri build

# ---- quality gates (run before finishing any change) ----

# Rust
cargo fmt --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test  --manifest-path src-tauri/Cargo.toml

# Frontend
npm run typecheck   # tsc --noEmit
npm run lint        # eslint
npm run test        # vitest (layout tree, keymap)
```

---

## 17. Testing strategy

- **Layout tree (`layout/tree.ts`)** — the highest-value tests. Cover: splitting
  in both orientations, focus traversal in all four directions, `focus_next`
  wraparound, moving/swapping panes, closing a pane (sibling promotion + ratio
  sanity), zoom toggle leaving tree shape intact, ratio clamping. Use `vitest`.
- **Keymap** — prefix enter/exit, timeout cancel, double-prefix passthrough,
  binding lookup, config overrides.
- **Rust PTY (`pty.rs`)** — spawn a trivial command (e.g. `echo hello`), assert
  the bytes arrive over the channel; test resize doesn't error; test that
  `close_pty` actually reaps the child and the reader thread exits.
- **E2E (optional)** — `tauri-driver` / WebDriver smoke test: launch, split,
  focus-switch, zoom, close. Keep it small; it's a smoke test, not the main
  coverage.

---

## 18. Non-goals / constraints (do not "helpfully" add these)

- **No PowerShell or cmd.exe default.** SplitView targets a minimal POSIX shell
  experience. The default shell is the user's `$SHELL` (bash/zsh/fish), not a
  Windows prompt.
- **Not a persistent multiplexer server.** Unlike tmux, v1 does not detach /
  reattach sessions that survive the app closing. Panes live with the window.
  Remote *viewing and control* is in scope (§21) and session *persistence* is
  not: a phone can watch and drive the panes that exist, but closing the window
  still kills them and there is nothing to reattach to afterwards.
- **No heavy theming engine, no plugin system, no AI features in the terminal**
  in v1. Keep the surface small.
- **No mouse-only features.** Anything you can do with the mouse must also have a
  keybinding (the reverse is fine).
- **No full borders / decorative chrome.** See §13–14.

---

## 19. Common tasks ("how do I…")

**Add a new keybinding/action**
1. Add the action handler in `keymap/actions.ts`.
2. Register the default binding in the keymap registry.
3. Expose it as a config-overridable key.
4. If it mutates layout, go through `layout/tree.ts` (and add a tree test).

**Add a new IPC command**
1. Implement `#[tauri::command]` in `src-tauri/src/commands.rs`.
2. Register it in the `invoke_handler!` in `main.rs`.
3. Grant its permission in `capabilities/default.json`.
4. Add a typed wrapper in `src/ipc/commands.ts`.

**Add a layout operation**
1. Implement it as a pure function in `layout/tree.ts`.
2. Add `vitest` cases including the invariants in §7.
3. Wire it to a keybinding (§19, first recipe).

**Change the prompt format**
- Edit the template in config; the per-shell translation lives in
  `src-tauri/src/shell.rs` (§8). Don't scatter prompt logic elsewhere.

---

## 20. Footguns (things that have bitten this codebase or will)

- **PS1 set via env gets overwritten** by the user's rc file. Inject *after* rc
  runs (`--rcfile` / `ZDOTDIR`). See §8.
- **Stealing terminal keys.** If the keymap swallows keys outside prefix mode,
  vim/emacs/tmux-inside-SplitView break. Only intercept in prefix mode.
- **JSON-encoding terminal bytes** corrupts non-UTF-8 output. Use byte arrays /
  the `Channel<Vec<u8>>`. See §10.
- **Blocking the core process.** PTY reads are blocking — they must be on
  dedicated threads, never on the async command path or under the state lock.
- **A window that won't close.** With an `onCloseRequested` listener registered,
  `close()` only re-runs that handler; the window is gone only after `destroy()`,
  which needs `core:window:allow-destroy` in the capability. Get this wrong and
  `exit` in the last pane leaves a black window whose X button does nothing.
- **Zombie children / leaked reader threads.** Kill child → drop master → remove
  handle, in that order. EOF is what stops the reader thread.
- **Resize mismatch.** If xterm.js cols/rows and the PTY size drift apart, output
  wraps wrong and full-screen TUIs corrupt. Always resize both, together.
- **macOS non-login shell** → missing PATH/Homebrew. Spawn a login shell.
- **No locale = garbled line editing.** A GUI app often has no `LANG` (always on
  Windows, on macOS from the Dock), and `--rcfile` skips the profile that sets
  one, so bash runs in the "C" locale and measures the prompt's `└──╼` in bytes
  (12) instead of columns (4). Every full-line redraw — dropping the highlight
  after a bracketed paste, history recall — then lands 8 columns off and
  duplicates text. `ensure_utf8_locale` in `shell.rs` sets `LC_CTYPE` when no
  locale variable is set; keep it.
- **Watching a directory on Windows locks its ancestors.** ReadDirectoryChangesW
  holds a handle, and a directory can't be renamed or deleted while a handle
  inside it is open, so per-folder watches break `mv` and `git checkout` in the
  terminal. `watch.rs` watches only the outermost directory (recursively) on
  Windows and filters events back down; don't "simplify" it to one watch per
  open folder.
- **The editor buffer only holds `\n`.** CodeMirror normalizes line endings on
  the way in, so saving `doc.toString()` as-is rewrites every line of a CRLF file
  and makes a one-line edit a whole-file diff. `decodeText`/`encodeText` in
  `src/editor/model.ts` carry the file's EOL and BOM across; go through them.
- **Recreating terminals on layout change** loses scrollback and flickers.
  Re-parent the existing xterm.js DOM node instead.
- **Remote input bypasses the webview entirely.** A remote keystroke goes
  straight to the PTY via the same write path `write_pty` uses; it is never
  echoed locally. The desktop terminal and the phone converge because the PTY
  echoes on the *read* side, which both are subscribed to. If you ever add local
  echo on either side you get every character twice.
- **The roster must never be published when remote control is off.** A list of
  every terminal you have open, with titles, working directories and a preview
  of each one's output, is exactly the thing that should not exist unless
  someone asked for the feature. `publish_panes` refuses while the server is
  off, `RemoteBridge` skips the call, and `disable` drops what was there.
- **A pane's `seq` counts chunks, not bytes**, and is only ever compared against
  itself. Do not present it as a byte count or persist it across a restart.
- **`Notify::notify_waiters()` only wakes a waiter that has already polled.** A
  session killed between registration and its first `select!` would never hear
  it, so revocation uses `notify_one()`, which leaves a permit behind.

---

## 21. Remote control

An **opt-in** encrypted WebSocket server that lets a paired phone see the live
panes, attach to one, type into it, and create or close panes. The full design
and the normative wire protocol are in
[`docs/remote-control-desktop.md`](docs/remote-control-desktop.md); the Flutter
client's view of the same protocol is in `docs/remote-control-mobile.md`, and
`docs/remote/fixtures/` holds the golden vectors both sides assert against.
**The desktop document wins if the two ever disagree.**

Three rules keep the architecture the same as it was without the feature:

1. **Input goes straight to the PTY.** Remote keystrokes call the same write
   path `write_pty` does; they never pass through a webview.
2. **Output fans out in the backend.** `state::OutputSink` is `{ local, remote,
   seq }`: the owning window's `Channel` (swapped by `attach_pty`, semantics
   unchanged) plus a `tokio::sync::broadcast` for remote subscribers. `send` is
   synchronous and non-blocking on both paths, so the reader thread in `pty.rs`
   keeps its shape and can never be stalled by the network — a receiver that
   falls behind is *lagged*, told to resync, and loses history. With nothing
   subscribed the remote path costs one atomic load per chunk.
3. **Anything that touches layout goes through the frontend.** `create_pane` and
   `close_pane` are relayed as addressed `remote-action` events into the window
   that owns the pane, which uses the existing layout operations. Rust still
   does not know what a split is.

What lives where:

- **Only the webview knows** titles, working directories, preview lines, pane
  order and scrollback. It publishes the first four as a flat roster
  (`publish_panes`, debounced ~150 ms, plus a 2 s refresh while a session is
  live) and answers `remote-snapshot-request` with a `SerializeAddon` dump.
- **Only the backend knows** the socket, the TLS identity, the device registry,
  the pairing codes and the per-pane output counters.

The phone is a **tab list, not a layout**: one flat roster, one subscription at
a time, no split/move/zoom/detach/background/sidebar. `src/layout/tree.ts` must
not change for a remote feature — if it wants to, that is the design smell.

### Security posture

Enabling this turns a terminal emulator into a remote shell server, and the code
is arranged to make that hard to do by accident:

- **Off by default**, and not restored at launch unless the user explicitly asks
  (Settings → Remote → "Turn on at startup").
- **TLS with a pinned self-signed certificate.** No CA. The client learns the
  SHA-256 out of band from the pairing QR and refuses anything else.
- **Pairing is two factors and a person**: a single-use 8-character Crockford
  base32 code (120 s, 5 attempts, then burned), *and* an explicit desktop
  approval prompt that is not skippable. With no window to ask, the answer is no.
- **Tokens** are 256 bits of CSPRNG output, stored only as a SHA-256, compared
  in constant time, and revocable — revoking also cuts the live session.
- **Bounded**: 4 concurrent sessions, 30-minute idle timeout, 10 connection
  attempts per IP per minute, one served path (`/remote`).
- **Remote resize is off** by default: one PTY has one size, and reflowing the
  laptop's window because someone picked up a phone is behaviour people
  uninstall software over.
- **The filesystem commands (`read_dir`, `pick_file`, `read_text_file`,
  `file_stamp`, `write_text_file`) are never exposed.**
- Every pair, approval, rejection, connect, disconnect and revoke appends to
  `remote/audit.log` next to the identity, and an indicator is visible whenever
  the server is on, with a distinct state while a session is live.

### Reachability

LAN plus mDNS out of the box; **Tailscale or any other overlay is the
recommended path** for reaching a laptop you are not next to (scan the QR or
type the tailnet address — mDNS will not cross a subnet). A relay is explicitly
out of scope. One limit no code fixes: a sleeping laptop is a dead server.

---

_When in doubt: keyboard-first, thin lines, real PTYs, low latency. Keep the UI
out of the way._
