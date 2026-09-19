# Remote Control — Flutter Mobile App Implementation Plan

> Status: **proposal**. Nothing here is implemented yet.
> Companion document: [`remote-control-desktop.md`](./remote-control-desktop.md),
> which is the **normative** source for the wire protocol. Where this document
> describes a message, it is restating that spec from the client's side; if the
> two disagree, the desktop document wins.

---

## 1. What we are building

A Flutter app — **SplitView Remote** — that pairs with a desktop SplitView
instance over an encrypted LAN (or tunnelled) connection and gives you your
terminals on your phone.

The mental model is **tabs, not panes**. The desktop's split layout does not
exist on the phone. The app shows a flat, horizontally scrollable tab strip
where each tab is one live pane, and one terminal filling the screen below it.

Everything the app can do:

- **switch** between panes (tap a tab, or swipe the terminal),
- **create** a pane (`+`),
- **delete** a pane (close a tab),
- **type** into the visible pane,
- **read** its output in real time.

That is the entire feature set. It is deliberately small: a phone is a bad place
to manage a layout and a fine place to check on a build, restart a service, or
answer a prompt.

### Non-goals

- No split view, no layout tree, no move/zoom/detach/background/sidebar.
- No local terminal, no SSH client, no key management beyond the pairing token.
- No file browser, no settings sync, no theming engine.
- No offline mode. Disconnected means disconnected; we show it plainly.

---

## 2. Platform and packages

Flutter, targeting **iOS and Android**. (macOS/Windows desktop builds would work
but are pointless — the real app is right there.)

| Package | Purpose | Notes |
|---|---|---|
| `xterm` (xterm.dart) | terminal emulation + rendering | full ANSI, `TerminalView`, `onOutput` for input. The Dart counterpart of xterm.js. |
| `dart:io` `WebSocket.connect(..., customClient:)` | transport | **Not** `web_socket_channel`'s default constructor — we need a custom `HttpClient` to pin the certificate (§6.1). |
| `mobile_scanner` | QR scanning during pairing | |
| `flutter_secure_storage` | device token at rest | Keychain on iOS, EncryptedSharedPreferences on Android |
| `riverpod` | state management | plain `ChangeNotifier` would also do; the app is small |
| `freezed` + `json_serializable` | protocol message types | optional, but keeps the codec honest |
| `package_info_plus`, `device_info_plus` | `deviceName` / `platform` in the pairing request | |

No `xterm_flutter` forks, no WebView-based terminal. The whole point of the
desktop app is native-feeling terminal performance; the phone client should hold
the same line.

---

## 3. Repository placement

**A separate repository.** The Flutter toolchain, CI, and release cadence share
nothing with the Tauri app, and an `apps/mobile/` directory would drag Dart into
every `npm run` and `cargo` invocation in this repo.

What the two repos share is the protocol, and that is kept honest by:

- `docs/remote-control-desktop.md` in *this* repo as the single normative spec,
- `docs/remote/fixtures/*.json` in *this* repo as golden encode/decode vectors,
  copied into the Flutter repo and asserted by its codec tests (§10).

---

## 4. Project structure

```
lib/
├── main.dart
├── app.dart                     MaterialApp, routing, theme
├── protocol/
│   ├── frame.dart               binary frame encode/decode (§6.2)
│   ├── messages.dart            JSON control messages (freezed)
│   └── codec.dart               text/binary dispatch
├── transport/
│   ├── pinned_socket.dart       HttpClient with SHA-256 certificate pinning
│   ├── connection.dart          connect, hello/pair, heartbeat, reconnect
│   └── connection_state.dart    disconnected/connecting/pairing/ready/error
├── auth/
│   ├── pair_payload.dart        parse `splitview://pair?…`
│   └── device_store.dart        secure storage of {host, port, fingerprint,
│                                deviceId, token}
├── state/
│   ├── connection_controller.dart
│   └── panes_controller.dart    roster, active tab, unread flags
├── terminal/
│   └── pane_terminal.dart       one xterm.dart Terminal per pane + snapshot/
│                                resync handling
└── ui/
    ├── pair_screen.dart
    ├── terminal_screen.dart     tab strip + TerminalView + accessory row
    ├── tab_strip.dart
    ├── key_accessory.dart
    ├── connection_banner.dart
    └── settings_screen.dart
```

---

## 5. UX specification

### 5.1 Screens

**Pair.** Shown when no saved device exists.
- Big "Scan QR" button → `mobile_scanner`.
- "Enter manually" → host, port, pairing code. (Fingerprint cannot be typed;
  manual entry falls back to showing the fingerprint on the desktop and asking
  the user to confirm the first 8 characters shown in the app. Explicitly a
  second-class path.)
- After scanning: *"Waiting for approval on your laptop…"* — the desktop
  requires an explicit approval (desktop doc §7.2), so the app must show that
  state rather than appearing to hang.
- On success, store the device and go to Terminal.

**Terminal.** The main screen, and where the app lives.

```
┌──────────────────────────────────────────┐
│ ● project-x   npm run dev•   pane-9   +  │  ← tab strip, scrolls horizontally
├──────────────────────────────────────────┤
│                                          │
│  me @ project-x $ npm run dev            │
│                                          │
│    ➜  Local:   http://localhost:5173/    │  ← TerminalView, fills the screen
│                                          │
│                                          │
├──────────────────────────────────────────┤
│ esc │ tab │ ctrl │ alt │ ← ↓ ↑ → │ ^C │ …│  ← key accessory row
└──────────────────────────────────────────┘
```

**Settings.** Connection status and host, "Disconnect", "Forget this laptop",
font size, and a "Fit terminal to phone" toggle (disabled unless the server
advertises the `resize` feature — desktop doc §10.1).

### 5.2 Tab strip

- One tab per pane, in roster order. Label priority: terminal title (OSC 0/2) →
  basename of `cwd` → pane id. Truncated to ~14 characters.
- A small dot on a tab whose `seq` has advanced since it was last visible —
  this is the "something happened over there" signal, and it costs nothing
  because the roster already carries `seq` (desktop doc §6.2).
- The active tab is marked with a 1px accent underline. No pills, no shadows —
  the desktop app's visual restraint (CLAUDE.md §14) carries over.
- `+` at the end creates a pane. It shows a spinner until the new pane appears
  in the roster, then selects it.
- Long-press a tab → sheet with **Close pane** (with confirmation) and
  **Copy working directory**.
- Swiping the terminal horizontally moves between tabs. This must **not** fight
  terminal apps that use horizontal input; restrict the gesture to an edge-drag
  so `vim` and friends stay usable.

### 5.3 Key accessory row

This is the single detail that decides whether the app is usable. Mobile
keyboards have no `Esc`, no `Ctrl`, no arrows, and terminals need all three.

Row (scrollable): `Esc` `Tab` `Ctrl` `Alt` `↑` `↓` `←` `→` `^C` `^D` `^Z` `|`
`/` `-` `~`.

- `Ctrl` and `Alt` are **sticky modifiers**: tap to arm, next key is modified,
  auto-disarms. Double-tap to lock.
- Arrows send the correct sequences and respect application-cursor mode, which
  xterm.dart already tracks.
- `^C` is its own key because it is the single most-pressed thing on a phone
  terminal and burying it behind a sticky modifier is cruel.
- Long-press `Ctrl` → grid of the remaining control characters.

### 5.4 Connection states

A thin banner above the tab strip, only when something is wrong:

| State | Banner |
|---|---|
| connecting | *"Connecting to ismail-laptop…"* with a progress line |
| reconnecting | *"Reconnecting… (attempt 3)"* |
| disconnected | *"Disconnected"* + **Retry** |
| unauthorized / revoked | *"This laptop revoked access"* + **Pair again** |
| proto mismatch | *"Update SplitView on your laptop"* (or the app) |

Never a modal. The terminal stays visible with its last known contents, dimmed.

---

## 6. Transport and protocol client

### 6.1 Certificate pinning

The desktop uses a self-signed certificate; the QR carries its SHA-256. The
client must verify that fingerprint and refuse anything else — "accept any
certificate" is not an acceptable fallback, ever, including in debug builds.

```dart
final client = HttpClient(context: SecurityContext(withTrustedRoots: false))
  ..badCertificateCallback = (cert, host, port) {
    final digest = sha256.convert(cert.der).bytes;
    return _constantTimeEquals(digest, expectedFingerprint);
  };

final socket = await WebSocket.connect(
  'wss://$host:$port/remote',
  customClient: client,
);
```

`SecurityContext(withTrustedRoots: false)` ensures a public CA cannot be
substituted, and the comparison is constant-time. The fingerprint comes from the
QR at pairing time and is stored alongside the token.

### 6.2 Frames

Binary frames carry PTY bytes; text frames carry JSON. This mirrors the desktop
exactly, and exists because JSON-encoding terminal output corrupts non-UTF-8
sequences.

```dart
// offset 0: opcode (0x01 output, 0x02 input)
// offset 1: pane id length
// offset 2: pane id (ASCII)
// offset 2+n: raw bytes

Uint8List encodeInput(String paneId, Uint8List bytes) { … }
({int opcode, String paneId, Uint8List data}) decodeFrame(Uint8List frame) { … }
```

Output bytes go **straight** into `terminal.write()` — decoded as bytes, not
re-encoded through a Dart `String`. xterm.dart accepts a `String`, so the
codepath uses a streaming UTF-8 decoder that tolerates split multi-byte
sequences across frame boundaries (`Utf8Decoder(allowMalformed: true)` over a
chunked conversion sink, one decoder instance **per pane**, reset on snapshot).
Getting this wrong shows up as mojibake the first time someone `cat`s a file
with box-drawing characters.

Input from `terminal.onOutput` is UTF-8 encoded and sent as a binary frame. No
local echo — the PTY echoes, exactly as it does for the desktop client.

### 6.3 Connection lifecycle

```
launch
  └─ saved device? ── no ──▶ Pair screen
         │ yes
         ▼
   TLS connect (pinned)
         ▼
   hello {proto, deviceId, token, client}
         ▼
   welcome {features}  ──▶ panes {…}  ──▶ render tabs
         ▼
   subscribe {activePaneId}
         ▼
   snapshot  ──▶ terminal.write(serialized)
         ▼
   live binary frames
```

- **Heartbeat.** `ping` every 20 s; no `pong` within 10 s → treat as dead and
  reconnect. Mobile networks drop sockets silently and often.
- **Reconnect.** Exponential backoff 1, 2, 4, 8, 16, 30 s, capped, with jitter.
  Reset on a successful `welcome`.
- **App lifecycle.** On `AppLifecycleState.paused`, close the socket cleanly
  (iOS will kill it anyway). On `resumed`, reconnect, re-`subscribe` to the
  active tab, and `requestSnapshot` — the desktop still holds the real
  scrollback, so a fresh snapshot restores full fidelity.
- **`resync {paneId}`** from the server means the stream was cut for
  backpressure reasons. Response: clear the terminal and `requestSnapshot`.

### 6.4 One subscription at a time

The app subscribes to the **visible pane only**. Switching tabs sends
`unsubscribe` for the old pane and `subscribe` for the new one, and the snapshot
that comes back reconstructs the screen.

This is a real advantage of the tab model rather than a limitation: it bounds
mobile data usage to a single pane's output, keeps battery cost low, and still
gives perfect fidelity on switch because the desktop owns the scrollback. Background
tabs stay informative via `preview` and `seq` in the roster.

Per-pane `Terminal` objects are kept in memory (cheap) so switching back is
instant while the snapshot loads, but their contents are considered stale until
the snapshot arrives.

---

## 7. Terminal sizing

The desktop refuses remote resizes by default (desktop doc §10.1), so the phone
renders at the pane's own `cols`/`rows` from the roster:

1. Compute the font size that makes `cols` fit the screen width, using
   xterm.dart's cell metrics for the chosen family.
2. Clamp to a minimum legible size (~7 pt). Below that, stop shrinking and allow
   **horizontal scrolling** instead of rendering something unreadable.
3. Landscape orientation is supported and usually fixes the problem outright;
   the app should not lock orientation.
4. If the server advertises the `resize` feature and the user enables **Fit
   terminal to phone**, send `resize {paneId, cols, rows}` computed from the
   phone's viewport — with a one-time warning that this reflows the laptop
   window too.

A 120-column pane on a phone in portrait is genuinely small. That is honest, and
better than silently reflowing the user's laptop while they are not looking.

---

## 8. Pairing implementation

QR payload (produced by the desktop):

```
splitview://pair?h=192.168.1.20&p=8765&f=<cert-sha256-base64url>&c=<code>&v=1
```

Client steps:

1. Parse and validate: `v` must be supported, `f` must decode to 32 bytes, `h`
   and `p` must be well-formed. Reject anything else with a clear message rather
   than attempting a connection.
2. Connect with the pin from `f`.
3. Send `pair {proto, code, deviceName, platform}` where `deviceName` is the
   device's own name (e.g. *"Ismail's iPhone"*) — this is what the desktop shows
   in its approval prompt, so it must be recognisable.
4. Show *"Waiting for approval on your laptop…"*, with a cancel button.
5. On `paired {deviceId, token}`, write `{host, port, fingerprint, deviceId,
   token}` to `flutter_secure_storage` and proceed.
6. On `pairing_rejected` / `pairing_expired` / `rate_limited`, return to the Pair
   screen with the specific reason.

**Storage rules.** The token never leaves secure storage, is never logged, never
appears in an error message, and is not included in any crash report. On
"Forget this laptop", delete the entry and close the socket.

**Manual entry** stores the fingerprint the server presents on first connect
after the user confirms its first 8 characters against the desktop's Settings
screen. This is trust-on-first-use with human verification, and it is explicitly
weaker than the QR path; the UI should say so in one line.

---

## 9. State management

Two controllers, both `riverpod` notifiers:

**`ConnectionController`** — owns the socket, the handshake, the heartbeat and
backoff. Exposes `ConnectionState` and a `Stream<ServerMessage>`. Nothing else
in the app touches the socket.

**`PanesController`** — consumes `panes` rosters and owns:

```dart
class PanesState {
  final List<RemotePane> panes;     // roster order
  final String? activePaneId;
  final Map<String, int> seenSeq;   // for the unread dot
  final Set<String> pendingCreates;
  final bool creating;
}
```

Roster diffing drives the UI: a pane that disappears closes its tab (selecting
the neighbour); a pane that appears while a create is pending becomes the active
tab.

`PaneTerminal` holds the xterm.dart `Terminal`, the per-pane UTF-8 decoder, and
the "stale until snapshot" flag. It is created lazily on first view and disposed
when its pane leaves the roster.

---

## 10. Testing

- **`protocol/` codec** — the highest-value tests in the app. Round-trip every
  message type; assert the **golden fixtures from the desktop repo**
  (`docs/remote/fixtures/`) decode to the expected values. This is what stops
  the two implementations drifting.
- **UTF-8 chunk boundaries** — feed a multi-byte sequence split across two
  frames and assert the terminal renders one character, not two replacements.
  This bug is guaranteed if untested.
- **`transport/pinned_socket.dart`** — a wrong fingerprint is rejected; a right
  one is accepted; a public-CA certificate for the same host is rejected.
- **`PanesController`** — roster diffing: pane added, removed, renamed, reordered;
  active-tab selection when the active pane disappears; pending-create
  correlation and its 5 s timeout.
- **Reconnect** — backoff sequence, reset on success, resubscribe on resume.
- **Widget tests** — tab strip renders and selects; accessory row sticky
  modifiers arm, apply once, and disarm.
- **Manual smoke matrix** — real device on Wi-Fi and on cellular-over-Tailscale;
  `vim`, `htop`, `less`, a dev server with a busy log, and a `yes` flood to
  exercise the resync path.

---

## 11. Milestones

**M1 — Protocol + transport.** Codec, pinned socket, connection lifecycle, no
UI beyond a debug screen that dumps messages. Runs against the desktop's **M3**
(plaintext loopback, no auth) via a port forward. Golden fixtures passing.

**M2 — Single terminal.** One hardcoded pane: subscribe, snapshot, live output,
keyboard input, correct sizing. This is the moment the feature either feels
right or does not.

**M3 — Tabs.** Roster, tab strip, switching with subscribe/unsubscribe, create,
close, unread dots.

**M4 — Pairing.** QR scanning, pin verification, approval waiting state, secure
storage, reconnect with token, "forget this laptop". Requires desktop **M4**.

**M5 — Keyboard and polish.** Accessory row with sticky modifiers, connection
banners, app-lifecycle handling, landscape, settings screen, font sizing.

**M6 — Release.** Icons, store metadata, TestFlight / internal track, crash
reporting configured to exclude terminal contents and tokens.

M1–M2 are the risky ones; M3 onward is ordinary app work.

---

## 12. Risks

- **xterm.dart fidelity.** It is good but not xterm.js. Full-screen TUIs
  (`htop`, `vim`, `tmux` inside a pane) are the stress case and should be
  smoke-tested early — ideally during M2, not M5, because a rendering shortfall
  would change the plan.
- **iOS background socket policy.** The socket will die when backgrounded. The
  design accepts this and treats resume-as-reconnect; anyone expecting a
  persistent session will be disappointed and the UI should not pretend
  otherwise.
- **Keyboard and IME interaction.** Software keyboards, autocorrect, and
  predictive text fight terminals. Disable autocorrect, capitalisation, and
  predictive input on the terminal's input surface.
- **Privacy of terminal contents.** Screenshots, crash reports, and the app
  switcher preview can all leak whatever is on screen. Obscure the terminal in
  the app switcher (`FLAG_SECURE` on Android, a blur overlay on iOS) and never
  include buffer contents in diagnostics.
- **Protocol drift between two repos.** Mitigated by golden fixtures and a
  version check in `hello`/`welcome`, but it needs discipline: a protocol change
  lands in the desktop repo's spec and fixtures *first*.
