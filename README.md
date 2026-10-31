# SplitView

A minimalistic, keyboard-driven terminal emulator — tmux-style split panes as a
native GUI app. Built with **Rust + Tauri 2** (PTYs, process lifecycle) and
**TypeScript + xterm.js** (rendering, split layout, keymap).

See **[CLAUDE.md](./CLAUDE.md)** for the full architecture, conventions, and
domain vocabulary. This README is just the quick start.

## Features

- Split panes horizontally (`split_down`) and vertically (`split_right`)
- Keyboard focus switching, pane move/swap, and zoom — all without the mouse
- A tmux-style **prefix** model (default `Ctrl+A`) so terminal keys are never stolen
- Real PTYs via `portable-pty`, full ANSI, WebGL-accelerated rendering
- Thin 1px dividers, no decorative chrome
- A file sidebar (`<prefix> e`) and a built-in **code editor** docked on the right
  (`<prefix> E`): tabs, syntax highlighting, find and replace, and saves that
  won't overwrite a change made from the terminal. Press `e` on a file in the
  sidebar, or run `splitview edit <file>`
- **Remote control** (opt-in, off by default): pair a phone over an encrypted,
  certificate-pinned connection and watch, type into, or open and close panes
  from it. Settings → Remote. See
  [`docs/remote-control-desktop.md`](./docs/remote-control-desktop.md).

## Layout

```
src/                  frontend (TypeScript)
  layout/tree.ts      pure, tested layout tree (the core model)
  keymap/             prefix engine + action registry
  terminal/           xterm.js wrapper + renderer
  editor/             the code editor panel (CodeMirror)
  ipc/commands.ts     typed wrappers over Tauri invoke()/Channel
  config/config.ts    TOML config load/parse/validate
  remote/             remote-control bridge, roster, pairing QR encoder
src-tauri/src/        backend (Rust)
  pty.rs shell.rs     PTY spawn/read + per-shell prompt injection
  files.rs            text file read/save for the editor
  commands.rs         the IPC surface
  state.rs error.rs   app state + serializable errors
  remote/             TLS WebSocket server, protocol, pairing, mDNS
```

## Develop

```bash
npm install
npm run tauri dev          # run the app with hot reload
```

## Quality gates

```bash
# Frontend
npm run typecheck          # tsc --noEmit
npm run lint               # eslint
npm run test               # vitest (layout tree, keymap, config)

# Rust
cargo fmt   --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test  --manifest-path src-tauri/Cargo.toml
```

## Build

```bash
npm run tauri build              # full installer bundle
npm run tauri build -- --no-bundle   # compile the release binary only
```

## Configuration

Copy [`config.example.toml`](./config.example.toml) to the platform config dir
(e.g. `~/.config/splitview/config.toml`) and edit the prompt, prefix key, font,
and keybindings.

> Note: Tauri's `compression` feature is intentionally disabled in
> `src-tauri/Cargo.toml` because the `brotli` crate it pulls in fails to compile
> on the current Rust toolchain. Embedded assets are stored uncompressed.
