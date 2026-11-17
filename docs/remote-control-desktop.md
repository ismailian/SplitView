# Remote Control — Desktop Implementation Plan

> Status: **implemented** (M1–M5; the optional relay, M6, is not).
> Companion document: [`remote-control-mobile.md`](./remote-control-mobile.md).
> This document is the **normative** source for the wire protocol; the mobile
> document describes the client's view of it and must not diverge.
>
> Where the implementation deviates from the original proposal, this document
> has been amended to describe **what the code does** — see §5.3 (the snapshot
> guarantee), §6.2 (subscription limits) and §14 (the open questions, now
> answered). Golden protocol vectors live in `docs/remote/fixtures/`.
>
> **Since 0.3 (egui):** the wire protocol, pairing, auth and TLS are unchanged,
> but the host side moved. Read the Tauri-era paths below as:
> `src-tauri/src/remote/*` → `crates/app/src/remote/*`; `state.rs`/`pty.rs`/
> `commands.rs` → `crates/app/src/hub.rs`; "the webview" / `src/main.ts` /
> `RemoteBridge.ts` → the egui app (`crates/app/src/app.rs`), which publishes the
> roster itself; Tauri events → `UiEvent::Remote`. Snapshots are no longer asked
> of a window: the emulator lives in the backend, so `request_snapshot` reads
> `Terminal::snapshot_ansi()` directly (same message, same guarantee). Tauri
> capabilities (§7.4) no longer exist — there is no IPC surface to grant.
>
> **Not verified in a running GUI.** Everything below is covered by unit and
> integration tests (including a real loopback TLS handshake with certificate
> pinning), but no phone has ever connected to this server — there is no client
> yet. Treat the end-to-end flow as untested.

---

## 1. What we are building

SplitView gains an opt-in **remote control** mode. When enabled, the desktop app
runs a small encrypted server. A paired phone connects to it and can:

- see the list of live panes,
- attach to one pane and see its output in real time,
- type into it,
- create a new pane,
- close a pane.

That is the whole feature surface. The phone is a **tab-based** client: one flat
list of panes, one visible at a time. It does **not** mirror the split layout,
does not move or zoom panes, and does not know the layout tree exists.

### Goals

- Real-time, byte-accurate terminal I/O in both directions.
- Zero change to how the desktop app behaves when the feature is off.
- Encrypted, mutually authenticated transport with an explicit pairing step.
- The remote client is "just another frontend" — it drives the same actions the
  keymap drives, so it cannot do anything a keyboard user cannot.

### Non-goals (v1)

- **Headless panes.** A pane created from the phone is a real pane in a real
  desktop window. We do not spawn panes that exist only for the remote client.
  (That would require the backend to own scrollback — see §5.3.)
- **Layout control from the phone.** No split direction, move, zoom, detach,
  background, sidebar, or settings.
- **Session persistence.** CLAUDE.md §18 still holds: panes die with the window.
  This is a remote *view*, not a multiplexer server.
- **Internet reachability out of the box.** See §8.

---

## 2. What the codebase already gives us

Grounding, so the plan is concrete rather than aspirational:

| Capability | Where | Status |
|---|---|---|
| Stable pane ids, unique across all windows (`pane-N`) | `src-tauri/src/commands.rs` (`next_pane_id`) | reuse as-is |
| Byte-safe PTY read loop on a dedicated thread | `src-tauri/src/pty.rs` | reuse as-is |
| `write_pty` / `resize_pty` / `close_pty` as pure backend ops | `src-tauri/src/commands.rs` | reuse as-is |
| Shell-exit signal (`pty-exit` event) | `src-tauri/src/pty.rs` | reuse as-is |
| Scrollback snapshot for a fresh viewer (`SerializeAddon`) | `src/terminal/Pane.ts` (`serialize`), `stash_handoff`/`take_handoff` | reuse the mechanism |
| One-line live preview per pane | `src/terminal/Pane.ts` (`previewLine`) | reuse as-is |
| Action dispatch by name | `src/keymap/actions.ts` (`dispatchAction`) | reuse as-is |
| Output sink is **single-consumer** | `src-tauri/src/state.rs:20` | **must change** (§5.1) |
| Pane roster / titles / cwd known only to the webview | `src/main.ts` (`App`) | **must be published** (§5.2) |
| Networking, async server, crypto | — | **new** (§6–§8) |

Two structural changes carry the whole feature. Everything else is plumbing.

---

## 3. Architecture

```
  PHONE                                   LAPTOP (SplitView)
┌────────────────────┐        ┌──────────────────────────────────────────┐
│  tab strip         │        │  Webview — App (src/main.ts)             │
│  one TerminalView  │        │    layout tree, panes Map, focus         │
└─────────┬──────────┘        │        │                      ▲          │
          │                   │  publish_panes         remote-action     │
          │  WSS / TLS        │  provide_snapshot      remote-snapshot-  │
          │  (pinned cert)    │        │                 request         │
          │                   │        ▼                      │          │
          │  binary: PTY bytes│  ┌─────────────────────────────────────┐ │
          └──────────────────────▶  remote::server                     │ │
             text: JSON control│  │   sessions · auth · pairing        │ │
                               │  └───────┬─────────────────▲──────────┘ │
                               │          │                 │            │
                               │   write_pty            OutputSink       │
                               │          │             .subscribe()     │
                               │  ┌───────▼─────────────────┴──────────┐ │
                               │  │ AppState.panes: HashMap<PaneId, …> │ │
                               │  └───────────────────────────────────┘  │
                               └──────────────────────────────────────────┘
```

Three rules keep the boundaries the same as CLAUDE.md §2.5:

1. **Input goes straight to the PTY.** Remote keystrokes call `write_pty`
   directly; they never pass through a webview. The PTY's own echo comes back on
   the read side, so the desktop terminal and the phone converge for free.
2. **Output fans out in the backend.** The reader thread is unchanged in shape;
   it just has more than one sink.
3. **Anything that touches layout goes through the frontend.** Creating and
   closing panes are relayed as events into `App`, which uses the existing
   layout operations. Rust still does not know what a split is.

---

## 4. New files and dependencies

```
src-tauri/src/remote/
├── mod.rs          RemoteState: enabled flag, sessions, devices, pane roster
├── server.rs       TLS + WebSocket accept loop, session spawn
├── protocol.rs     frame + message encode/decode (pure, heavily unit-tested)
├── session.rs      one connected client: subscription, coalescing, backpressure
├── auth.rs         device registry, token issue/verify/revoke
├── pairing.rs      pairing code lifecycle, QR payload, rate limiting
├── identity.rs     self-signed cert generation + persistence + fingerprint
└── discovery.rs    mDNS `_splitview._tcp` advertisement

src/remote/
└── RemoteBridge.ts  publishes the pane roster, handles remote-action events
```

New crates:

| Crate | Why |
|---|---|
| `tokio` (`rt-multi-thread`, `sync`, `net`, `time`) | Tauri already vendors it; we need `broadcast` and the accept loop |
| `tokio-tungstenite` | WebSocket framing |
| `tokio-rustls`, `rustls`, `rustls-pemfile` | TLS terminator |
| `rcgen` | generate the self-signed identity certificate |
| `rand` (`getrandom`), `sha2`, `subtle`, `zeroize` | tokens, fingerprints, constant-time compare |
| `base64`, `data-encoding` | QR payload, Crockford base32 pairing codes |
| `mdns-sd` | LAN discovery |
| `bytes` | cheap chunk sharing in the fan-out |

We deliberately do **not** add a QR-rendering crate: the payload is a short
string and the Settings window draws it in TypeScript.

---

## 5. Backend changes in detail

### 5.1 Fan-out output sink

Today (`src-tauri/src/state.rs:20`):

```rust
pub type OutputSink = Arc<Mutex<Channel<Vec<u8>>>>;
```

One PTY, one webview channel. `attach_pty` swaps it when a pane moves between
windows. A remote client is a *second, concurrent* consumer, so this becomes:

```rust
pub struct OutputSink {
    /// The owning window's channel. Swapped by `attach_pty`; semantics unchanged.
    local: Mutex<Channel<Vec<u8>>>,
    /// Remote subscribers. Bounded; a slow receiver is dropped, not waited on.
    remote: tokio::sync::broadcast::Sender<Bytes>,
    /// Monotonic chunk counter, surfaced in the roster so a client can cheaply
    /// tell whether a pane it is not watching has produced output.
    seq: AtomicU64,
}

impl OutputSink {
    pub fn send(&self, chunk: &[u8]);                       // called by the reader thread
    pub fn set_local(&self, ch: Channel<Vec<u8>>);          // called by attach_pty
    pub fn subscribe(&self) -> broadcast::Receiver<Bytes>;  // called by a remote session
    pub fn seq(&self) -> u64;
}
```

Why `broadcast` specifically:

- `send` is **synchronous and non-blocking**, so the reader thread in `pty.rs`
  keeps its current shape and never blocks on the network (CLAUDE.md §11, §20).
- A receiver that falls behind gets `RecvError::Lagged(n)` instead of applying
  backpressure to the producer. That is the correct semantics here: a phone on a
  bad connection should *lose* history and resync, never stall the laptop.

`send` skips the `Bytes` allocation entirely when `receiver_count() == 0`, so
the feature costs one atomic load per chunk while it is off.

The reader-thread body in `pty.rs` becomes:

```rust
Ok(n) => sink.send(&buf[..n]),
```

with the local-channel lock moved inside `OutputSink::send`, still held only for
the duration of the send and never across the blocking read.

### 5.2 Pane roster: the frontend publishes, the backend relays

Titles, working directories, preview lines and pane ordering only exist in the
webview. Since the phone shows a **flat tab list**, we do not need to serialize
the layout tree at all — a flat array is enough, which removes a large amount of
the complexity a mirrored-layout design would have needed.

New command:

```rust
#[tauri::command]
fn publish_panes(state: State<'_, AppState>, snapshot: WindowPanes) -> Result<()>;
```

```ts
interface WindowPanes {
  windowLabel: string;       // this window's Tauri label
  focusedAt: number;         // monotonic; the backend picks the max as the target window
  focusedPaneId: string | null;
  panes: RemotePane[];
}

interface RemotePane {
  id: string;                // backend PaneId
  title: string | null;      // OSC 0/2 title
  cwd: string | null;        // OSC 7 cwd
  cols: number;
  rows: number;
  preview: string;           // Pane.previewLine()
  backgrounded: boolean;
}
```

`RemoteBridge` calls this from `App.commit()` / `App.render()`, debounced to
~150 ms, and again on a 2 s timer while any remote session is live so `preview`
and `seq` stay fresh. When remote control is disabled the call is skipped
entirely.

The backend merges the per-window snapshots into one ordered roster (window
registration order, then pane order within a window) and marks the
**target window** — the one with the highest `focusedAt` — as where a remotely
created pane will appear.

### 5.3 Snapshot on attach

When the phone subscribes to a pane it needs the current screen. We do **not**
add a ring buffer in Rust; CLAUDE.md §4 says scrollback lives in xterm.js, and
the existing detach/reattach path already solves exactly this problem.

Flow:

1. Session receives `subscribe {paneId}`.
2. Backend emits `remote-snapshot-request {paneId, requestId}` to the window
   that owns the pane.
3. `RemoteBridge` calls `pane.serialize()` and replies via a new command
   `provide_snapshot(requestId, paneId, serialized, cols, rows)`.
4. Backend sends `snapshot` to the client, **then** starts forwarding live
   chunks. The session subscribes to the pane's output *before* asking for the
   snapshot, so the stream is never interrupted; chunks that arrive while the
   request is in flight are buffered but **discarded** when the snapshot lands,
   because the snapshot is a picture of the screen those chunks had already been
   drawn into — replaying them would duplicate output.

   The honest guarantee is therefore *"no duplication, and at most the few
   milliseconds between the window serializing and its reply reaching us"*, not
   the "nothing lost or duplicated" the proposal claimed. Closing that last gap
   needs the webview to report how much of the stream it had applied, which
   means putting a sequence number on every chunk of the local hot path — too
   much cost on the thing CLAUDE.md §4 calls a dumb, fast pipe, for a sliver a
   `requestSnapshot` already recovers.
5. If no reply arrives within 3 s (window busy or gone), send `snapshot` with an
   empty buffer and let the live stream fill the screen.

Backgrounded panes work unchanged: they remain mounted and hidden, so
`serialize()` still returns their full buffer.

### 5.4 Remote actions

Creating and closing panes mutate the layout tree, so they are relayed into the
frontend rather than executed in Rust. The backend emits:

```ts
interface RemoteAction {
  windowLabel: string;                       // addressed; other windows ignore it
  action: "create_pane" | "close_pane";
  paneId?: string;
  cwd?: string;
}
```

`RemoteBridge` ignores actions addressed to another window — the same pattern
`onReattachPane` already uses in `src/main.ts` — then:

- `create_pane` → `app.split("right")`, optionally with an inherited `cwd`. The
  new pane's id comes back in the next roster publish, which the session
  correlates to the client's pending request.
- `close_pane` → `app.closePaneConfirmed(paneId)`.

**Required refactor:** `App.closePane()` (`src/main.ts:372`) calls
`window.confirm`. A remote close must not pop a modal on an unattended laptop.
Split it:

```ts
closePane(): void {                      // keymap path, unchanged behaviour
  const target = this.layout.focused;
  if (!target) return;
  if (!window.confirm("Close this pane?")) return;
  this.closePaneConfirmed(target);
}

closePaneConfirmed(paneId: string): void { this.removePane(paneId); }
```

The phone does its own confirmation. `AppActions` gains `closePaneConfirmed`;
no new *binding* is added, so CLAUDE.md §2.1 is satisfied — the remote cannot
reach anything the keyboard cannot.

If no layout window is registered, `create_pane` fails with `no_window`.

---

## 6. Wire protocol (normative)

Transport is WebSocket over TLS. WebSocket gives us framing, so we use its two
frame types to keep terminal bytes out of JSON — JSON-encoding PTY output
corrupts non-UTF-8 sequences (CLAUDE.md §10, §20).

- **Binary frames** carry PTY data.
- **Text frames** carry JSON control messages.

### 6.1 Binary frame layout

```
offset  size  meaning
0       1     opcode   0x01 = output (S→C), 0x02 = input (C→S)
1       1     pane id length, in bytes (ids are `pane-<n>`, always < 255)
2       n     pane id, ASCII
2+n     ..    raw PTY bytes, verbatim
```

No compression, no escaping, no re-encoding. A malformed frame closes the
session with `proto_error`.

### 6.2 Control messages

`proto` is an integer, currently `1`. A client whose `proto` the server does not
support is rejected with `proto_mismatch` before anything else happens.

**Client → server**

| Message | Fields | Notes |
|---|---|---|
| `hello` | `proto`, `deviceId`, `token`, `client` | reconnect with a stored token |
| `pair` | `proto`, `code`, `deviceName`, `platform` | first-time pairing |
| `subscribe` | `paneId` | server replies `snapshot`, then streams. Subscribing to a pane already subscribed re-requests its snapshot; a session may hold at most 8 subscriptions and is answered `not_permitted` past that (the client is expected to hold one — §10.3) |
| `unsubscribe` | `paneId` | |
| `requestSnapshot` | `paneId` | after a `resync`, or on app resume |
| `createPane` | `cwd?` | relayed to the target window |
| `closePane` | `paneId` | relayed to the owning window |
| `resize` | `paneId`, `cols`, `rows` | rejected with `not_permitted` unless the desktop has opted in (§9) |
| `ping` | `t` | liveness; server replies `pong` |

**Server → client**

| Message | Fields | Notes |
|---|---|---|
| `welcome` | `proto`, `serverVersion`, `hostName`, `features[]` | `features` advertises e.g. `"resize"` |
| `paired` | `deviceId`, `token` | issued once, after the desktop user approves |
| `panes` | `panes[]` | full roster; sent on connect and on every change (debounced) |
| `snapshot` | `paneId`, `serialized`, `cols`, `rows` | `SerializeAddon` dump |
| `paneClosed` | `paneId`, `reason` | `"exit"`, `"closed"`, `"window_closed"` |
| `resync` | `paneId` | the stream was cut; re-request a snapshot |
| `error` | `code`, `message` | see below |
| `pong` | `t` | |

Roster entry (`panes[]`):

```json
{
  "id": "pane-7",
  "title": "npm run dev",
  "cwd": "/home/me/project-x",
  "cols": 120,
  "rows": 34,
  "preview": "  ➜  Local:   http://localhost:5173/",
  "backgrounded": false,
  "window": "main",
  "seq": 148213
}
```

`seq` is the pane's monotonic output counter. A client that is not subscribed to
a pane can compare `seq` between rosters to show an "activity" dot without
paying for the pane's byte stream.

### 6.3 Error codes

`proto_mismatch`, `unauthorized`, `pairing_expired`, `pairing_rejected`,
`rate_limited`, `unknown_pane`, `not_permitted`, `no_window`, `proto_error`,
`server_disabled`.

All are terminal except `unknown_pane`, `not_permitted` and `no_window`, which
leave the session open.

### 6.4 Versioning

The protocol is versioned from day one and the phone updates on a different
cadence than the desktop. Breaking changes bump `proto`; additive fields must be
ignorable by an older peer. Golden encode/decode fixtures live in
`docs/remote/fixtures/` and are shared verbatim with the Flutter client's tests.

---

## 7. Security and pairing

This feature turns a terminal emulator into a remote shell server. It is treated
accordingly.

### 7.1 Transport

On first enable, the app generates a **self-signed X.509 certificate**
(`rcgen`, P-256, 10-year validity) and stores it with the private key in the
platform config directory under `remote/`, mode `0600`. No CA is involved. The
client pins the certificate's SHA-256, which it learns **out of band from the
QR code**. This is the same shape Syncthing uses, and it avoids both the
"accept any certificate" antipattern and the need for a real PKI.

### 7.2 Pairing flow

1. User enables remote control in Settings. The app binds, advertises mDNS, and
   generates a **single-use pairing code**: 8 characters of Crockford base32
   (~40 bits), TTL 120 s.
2. The QR encodes:
   `splitview://pair?h=<host>&p=<port>&f=<cert-sha256-base64url>&c=<code>&v=1`
   The code and host/port are also shown as text for manual entry.
3. The phone connects and **verifies the pin before sending a single byte of
   application data**.
4. The phone sends `pair {code, deviceName, platform}`. The server compares the
   code in constant time (`subtle`), and rate-limits to 5 attempts before
   burning the code.
5. The server emits `remote-pairing-request` and the desktop shows a
   confirmation: *"Allow **Ismail's iPhone** (192.168.1.44) to control
   SplitView?"*. Nothing is issued until the user approves. This prompt is the
   last line of defence and it costs almost nothing.
6. On approval the server issues a 256-bit random device token, stores
   `{deviceId, name, platform, tokenSha256, pairedAt, lastSeen}`, and replies
   `paired`. The phone stores the token in the Keychain/Keystore.
7. Subsequent connections send `hello {deviceId, token}` over a pinned TLS
   channel. No code, no prompt.

Because the channel is already server-authenticated by the pin and encrypted,
transmitting the pairing code inside it is sound. A PAKE (SPAKE2) would remove
the dependency on correct pinning at the cost of materially more crypto in Dart;
noted as a possible hardening, not v1.

Tokens are stored as a bare SHA-256 of the token. No KDF is needed: the token is
256 bits of CSPRNG output, not a password.

### 7.3 Controls

| Control | Default |
|---|---|
| Feature enabled | **off**, and not persisted across launches unless explicitly opted in |
| Bind address | `lan` (all interfaces) or `loopback` (for SSH/Tailscale tunnelling) |
| Desktop approval on pairing | required, not skippable |
| Concurrent sessions | max 4 |
| Idle timeout | 30 min without a frame → close |
| Pairing attempts | 5 per code, then burned |
| Connection attempts | per-IP token bucket, 10/min |
| Remote resize | allowed, and only happens once the phone opts in too (§10.1) |
| Filesystem commands (`read_dir`, `pick_file`) | **never exposed to remote** |
| Audit log | append-only `remote/audit.log`: pair, approve, reject, connect, disconnect, revoke |
| Visible indicator | shown whenever the server is enabled; distinct state while a session is live |

Revoking a device in Settings deletes the token **and** immediately terminates
any live session using it.

### 7.4 Tauri capabilities

Every new command needs a matching grant in
`src-tauri/capabilities/default.json` in the same change — a missing permission
fails at runtime and the failure is quiet (CLAUDE.md §15). New commands:

`remote_status`, `remote_enable`, `remote_disable`, `remote_pairing_start`,
`remote_pairing_cancel`, `remote_pairing_respond`, `remote_devices`,
`remote_revoke_device`, `publish_panes`, `provide_snapshot`.

New events emitted to the frontend: `remote-action`,
`remote-snapshot-request`, `remote-pairing-request`, `remote-status-changed`.

---

## 8. Reachability

The stated motivation is "do things when I'm not at my laptop", and LAN-only
does not deliver that. Being honest about the tiers:

1. **v1 — LAN.** mDNS discovery plus the QR. Works at home and at the office.
2. **Immediately, with no extra code — Tailscale / WireGuard.** The phone
   reaches the laptop's tailnet IP; mDNS will not work, so the QR or manual host
   entry covers it. For most people this genuinely solves the problem today, and
   it should be documented as the recommended path rather than treated as a
   workaround.
3. **v3, optional — a self-hosted relay.** Both ends dial *out* to a small relay
   keyed by a rendezvous id. Since the channel is end-to-end encrypted the relay
   only ever sees ciphertext. Real cost: NAT traversal, reconnection semantics,
   hosting and abuse handling. Do not front-load it.

One physical limit no amount of code fixes: a sleeping laptop is a dead server.
Keeping it awake is an OS setting, and the docs should say so plainly.

---

## 9. Settings UI

A new **Remote** section in the existing Settings window (`src/settings/`),
following the same visual rules as the rest of the app — thin lines, no chrome
(CLAUDE.md §14):

- Enable / disable toggle, with the bind mode (`LAN` / `Loopback only`).
- Current address, port, and the certificate fingerprint in short form.
- **Pair a device** → modal with the QR, the code, and a countdown.
- Paired devices list: name, platform, last seen, **Revoke**.
- Live sessions: device, IP, connected since, **Disconnect**.
- "Allow remote resize" — on by default, with a one-line explanation that it
  reflows the desktop window too.

The always-visible indicator while enabled is a 1px-tall accent strip or a small
corner dot, matching the existing backgrounded-panes indicator
(`src/background/BackgroundPanel.ts`) rather than introducing new chrome.

---

## 10. Hard problems and the decisions taken

### 10.1 Resize conflict

One PTY has one size; two viewers want different ones. This is the classic tmux
multi-attach problem and there is no free answer.

**Decision: the remote never resizes unless the phone asks to.** By default the
phone renders at the pane's existing `cols`/`rows` and scales its font (with
horizontal scroll below a minimum legible size). Its "Fit terminal to phone"
setting, off by default, resizes instead. The desktop's "Allow remote resize",
on by default, enables the `resize` message and advertises the `resize` feature
in `welcome`; with it off the server answers `not_permitted`.

Rationale: an implicit reflow of the laptop window because someone picked up a
phone is the kind of behaviour people uninstall software over, so a reflow is
always something the phone's user turned on.

### 10.2 Backpressure

A `yes` or a `cat` of a large file will saturate a phone on cellular. A naive
`for chunk in stream { ws.send(chunk).await }` will not survive contact.

`session.rs` coalesces per subscribed pane:

- accumulate into a `BytesMut` capped at **256 KB**,
- flush on a **25 ms** tick (~40 Hz, matching xterm.js's own frame cadence),
- on `broadcast::error::RecvError::Lagged`, or on exceeding the cap, **discard
  the buffer** and send `resync {paneId}`; the client re-requests a snapshot.

Since the phone subscribes to only one pane at a time (§10.3), this bounds
outbound traffic to a single pane's throughput.

### 10.3 One subscription at a time

The tabs model makes a genuinely useful simplification available: the phone
subscribes only to the **visible** tab. Inactive tabs are represented by the
roster's `preview` and `seq`, which cost nothing. Switching tabs triggers
`unsubscribe` + `subscribe`, and the fresh snapshot restores full fidelity
because the desktop's xterm.js still holds the real scrollback.

This is not a compromise — it is strictly better than streaming every pane, and
it falls out of the tab UX for free.

### 10.4 Multiple windows

The roster is flat and each entry names its `window`, which the phone may show
as a subtitle but does not act on. Remotely created panes go to the **target
window** (highest `focusedAt`). If all windows close, the roster empties and
`createPane` returns `no_window`.

### 10.5 Pane lifecycle races

- A pane closed on the desktop while the phone is subscribed → `paneClosed`
  (reason `closed` or `exit`), driven by the roster diff and the existing
  `pty-exit` event.
- A pane created on the phone whose window closes before the roster updates →
  the pending `createPane` times out after 5 s with `no_window`.
- A `subscribe` for a pane that has already gone → `unknown_pane`.

---

## 11. Milestones

Each milestone is independently reviewable and leaves the tree shippable.

All of M1–M5 have landed. Each is marked below with what carries it.

**M1 — Fan-out.** ✅ `state::OutputSink`. `OutputSink` becomes `{local, remote, seq}`. No network, no
UI, nothing user-visible. Test: a second subscriber receives byte-identical
output to the webview channel, and a deliberately lagging subscriber does not
stall the reader thread. *This de-risks everything after it and should land
first.*

**M2 — Frontend bridge.** ✅ `src/remote/RemoteBridge.ts`, `roster.ts`. `publish_panes`, `provide_snapshot`, `remote-action`,
`remote-snapshot-request`, `RemoteBridge.ts`, `closePaneConfirmed`. Still no
network; driven from a test harness that emits the events directly.

**M3 — Protocol and sessions.** ✅ `protocol.rs`, `server.rs`, `session.rs` — built directly on TLS rather than going through a plaintext step, since the transport was the cheap part and the pinning is the part worth exercising early. `protocol.rs`, `server.rs`,
`session.rs`. Full subscribe → snapshot → stream → input → create/close loop,
with coalescing and resync. Dev-only flag, `127.0.0.1` only, **no auth**. This
is where the protocol turns out to be wrong; find that out cheaply.

**M4 — TLS, pairing, security.** ✅ `identity.rs`, `auth.rs`, `pairing.rs`, `RemoteSection.ts`. `identity.rs`, `auth.rs`, `pairing.rs`, the
Settings section, desktop approval prompt, rate limiting, revocation, audit log,
capability grants. **Gate: a security review before anything binds to a LAN
interface.**

**M5 — Discovery and polish.** ✅ `discovery.rs`, the resize toggle, idle timeouts. mDNS, reconnect/resync hardening, idle timeouts,
the optional remote-resize toggle, documentation.

**M6 (optional) — Relay** for off-LAN, if Tailscale proves insufficient.

The Flutter client (see the companion document) starts against M3 and tracks M4
for pairing.

---

## 12. Testing

Following CLAUDE.md §17's bias toward pure, high-value units:

- **`protocol.rs`** — encode/decode round-trips, including non-UTF-8 payloads,
  empty payloads, maximum-length pane ids, truncated frames and unknown
  opcodes. Golden fixtures shared with the Flutter client. Highest value per
  line in the whole feature.
- **`auth.rs`** — constant-time verification, revoked tokens rejected, expired
  and burned pairing codes rejected, rate limiter behaviour.
- **Fan-out** — `spawn` a trivial command, assert both the local channel and a
  broadcast receiver observe the same bytes; assert a lagging receiver produces
  `Lagged` rather than blocking the producer.
- **`session.rs`** — coalescing flushes on tick, overflow emits `resync`,
  buffered chunks during a pending snapshot are neither lost nor duplicated.
- **Integration** (`#[tokio::test]`) — loopback TLS client: pair, subscribe,
  write `echo hi\n`, assert `hi` arrives; then `closePane` and assert
  `paneClosed`.
- **Frontend** — `RemoteBridge` roster serialization; `closePaneConfirmed` does
  not call `window.confirm`.
- **`src/layout/tree.ts` must not change.** If a remote feature wants to touch
  it, that is a design smell — the remote is a tab list, not a layout.

---

## 13. Changes required to CLAUDE.md

Done — these landed with the feature:

- **§18 (non-goals)** currently says SplitView is "not a persistent multiplexer
  server". Remote control is adjacent enough that the next contributor will
  reasonably be confused. Amend it to state that remote *viewing and control* is
  in scope while session *persistence* is not — panes still die with the window.
- **§10 (IPC contract)** — add the ten new commands and four new events.
- **§15 (capabilities)** — note the new grants.
- **New section** covering the remote architecture: the fan-out sink, the
  published roster, and the rule that layout-mutating remote actions are relayed
  to the frontend rather than executed in Rust.
- **§20 (footguns)** — add: remote input bypasses the webview and relies on PTY
  echo for convergence; and the roster must never be published when the feature
  is off.

---

## 14. Open questions — and what was decided

1. **Persist "enabled" across launches?** **Answered: opt-in.** Off by default;
   Settings → Remote has a "Turn on at startup" switch, and only the `main`
   window acts on it (enabling rebinds the socket, so a second window doing the
   same would restart a running server and drop whoever was connected). A stored
   value that is not exactly `true` is read as off, so a corrupt settings blob
   cannot open a socket.
2. **Should the phone be able to focus a pane on the desktop?** **Still
   deferred.** No `selectPane` message exists; adding one is a protocol
   addition, not a breaking change.
3. **`cwd` inheritance for `createPane`.** **Answered: the phone's view wins,
   with the desktop as the fallback.** The client may send `cwd` (the pane it
   was looking at) and that is used as-is; when it sends none, the backend fills
   in the target window's focused pane's directory, which is exactly what a
   keyboard split would have inherited. Both paths end up in the same
   `App.splitInto`.
4. **Whether to support more than one concurrent phone.** **Answered: yes, up to
   4.** The cost is bounded — each session subscribes to one pane and coalesces
   its own buffer — and Settings lists every live session with a Disconnect
   button, so the UX question has an answer the user can act on.

### Still open

- **No client exists yet**, so none of this has been exercised end to end. The
  first real bug reports will come from the Flutter app, not from here.
- **Pairing depends on correct pinning**, as §7.2 notes. A PAKE (SPAKE2) would
  remove that dependency at the cost of materially more crypto in Dart; it
  remains a possible hardening rather than a v1 requirement.
