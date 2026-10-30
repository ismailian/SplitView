//! One connected remote client: authentication, subscriptions, output
//! coalescing, and backpressure (`docs/remote-control-desktop.md` §5.3, §6,
//! §10.2, §10.3).
//!
//! The session is deliberately the only place in the backend that knows a
//! network exists. It talks to the rest of the app exactly the way the webview
//! does — `write_pty` for input, the fan-out sink for output, relayed events for
//! anything that touches layout — so a remote client cannot reach anything a
//! keyboard user cannot.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use futures_util::{SinkExt, StreamExt};
use tauri::{AppHandle, Listener, Manager};
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::pty::PTY_EXIT_EVENT;
use crate::state::{AppState, PaneId};

use super::protocol::{
    decode_client_message, encode_frame, ClientMessage, CloseReason, ErrorCode, RosterPane,
    ServerMessage, OPCODE_INPUT, OPCODE_OUTPUT, PROTO_VERSION,
};
use super::{now_secs, RemoteState, SessionInfo};

/// How long a fresh connection has to send `hello` or `pair`. An idle socket
/// that never authenticates is either broken or probing.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// Output flush cadence — ~40 Hz, matching xterm.js's own frame rate. Sending
/// every chunk as it arrives would turn a `cat` into thousands of tiny frames.
const FLUSH_INTERVAL: Duration = Duration::from_millis(25);

/// Per-pane coalescing cap. Past this the client is too far behind to catch up
/// by replaying bytes, so the buffer is dropped and it is told to resync.
const MAX_BUFFER: usize = 256 * 1024;

/// No frame in either direction for this long ends the session (§7.3).
const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Depth of the queue between the per-pane forwarder tasks and the session
/// loop. Once it fills, a forwarder stops draining its broadcast receiver, the
/// receiver lags, and the client is resynced — which is the intended path for a
/// client that cannot keep up.
const EVENT_QUEUE: usize = 256;

/// Upper bound on concurrent subscriptions. The phone subscribes to the visible
/// tab only (§10.3); this is just a ceiling on what one session can cost us.
const MAX_SUBSCRIPTIONS: usize = 8;

/// Things that reach the session loop from somewhere other than the socket.
enum Event {
    /// A chunk of a subscribed pane's output.
    Output { pane: PaneId, data: Bytes },
    /// The subscriber fell behind: history is gone, the client must resync.
    Lagged { pane: PaneId },
    /// A pane's shell exited (from the backend `pty-exit` event).
    Exited(PaneId),
    /// A snapshot request came back (or timed out, hence the `Option`).
    Snapshot {
        pane: PaneId,
        reply: Option<super::SnapshotReply>,
    },
    /// A remotely requested pane either appeared or did not.
    Created(Result<PaneId, ErrorCode>),
}

/// One subscribed pane's coalescing buffer.
struct Subscription {
    forwarder: tokio::task::JoinHandle<()>,
    buffer: BytesMut,
    /// While true, output is accumulated but not sent: the client is still
    /// waiting for the screen it will apply that output on top of.
    awaiting_snapshot: bool,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.forwarder.abort();
    }
}

/// Run one client session to completion. Never panics and never propagates an
/// error to the accept loop: a bad client ends its own session and nothing else.
pub async fn run<S>(ws: WebSocketStream<S>, peer: SocketAddr, app: AppHandle)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut ws = ws;
    let Some((device_id, device_name)) = authenticate(&mut ws, peer, &app).await else {
        let _ = ws.close(None).await;
        return;
    };

    let session_id = app.state::<RemoteState>().new_session_id();
    let info = SessionInfo {
        id: session_id,
        device_id,
        device_name,
        address: peer.ip().to_string(),
        connected_at: now_secs(),
    };
    let kill = app.state::<RemoteState>().register_session(&app, info);

    serve(&mut ws, session_id, kill, &app).await;

    app.state::<RemoteState>()
        .unregister_session(&app, session_id);
    let _ = ws.close(None).await;
}

/// Read the first control message and either admit the client or refuse it.
/// Returns `(device_id, device_name)` on success.
async fn authenticate<S>(
    ws: &mut WebSocketStream<S>,
    peer: SocketAddr,
    app: &AppHandle,
) -> Option<(String, String)>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    let first = tokio::time::timeout(HANDSHAKE_TIMEOUT, ws.next())
        .await
        .ok()??
        .ok()?;
    let text = match first {
        Message::Text(text) => text,
        // Anything else before authenticating is a protocol error: no bytes may
        // flow until we know who this is.
        _ => {
            refuse(ws, ErrorCode::ProtoError, "expected hello or pair").await;
            return None;
        }
    };
    let Ok(message) = decode_client_message(&text) else {
        refuse(ws, ErrorCode::ProtoError, "unparseable message").await;
        return None;
    };

    // `disable` stops the accept loop, but a handshake already in flight can
    // still land here; it must not turn into a session.
    if !app.state::<RemoteState>().is_enabled() {
        refuse(ws, ErrorCode::ServerDisabled, "remote control is off").await;
        return None;
    }

    match message {
        ClientMessage::Hello {
            proto,
            device_id,
            token,
            ..
        } => {
            if proto != PROTO_VERSION {
                refuse(ws, ErrorCode::ProtoMismatch, "unsupported protocol").await;
                return None;
            }
            let device = app
                .state::<RemoteState>()
                .verify_device(&device_id, &token)?;
            if !send(ws, welcome(app)).await {
                return None;
            }
            Some((device.id, device.name))
        }
        ClientMessage::Pair {
            proto,
            code,
            device_name,
            platform,
        } => {
            if proto != PROTO_VERSION {
                refuse(ws, ErrorCode::ProtoMismatch, "unsupported protocol").await;
                return None;
            }
            pair(ws, peer, app, &code, &device_name, &platform).await
        }
        _ => {
            refuse(ws, ErrorCode::Unauthorized, "authenticate first").await;
            None
        }
    }
}

/// The pairing handshake: check the code, ask the desktop user, issue a token.
async fn pair<S>(
    ws: &mut WebSocketStream<S>,
    peer: SocketAddr,
    app: &AppHandle,
    code: &str,
    device_name: &str,
    platform: &str,
) -> Option<(String, String)>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    use super::pairing::CodeCheck;

    match app.state::<RemoteState>().check_code(code) {
        CodeCheck::Accepted => {}
        CodeCheck::Rejected => {
            refuse(ws, ErrorCode::PairingRejected, "wrong pairing code").await;
            return None;
        }
        CodeCheck::Burned => {
            refuse(ws, ErrorCode::RateLimited, "too many attempts").await;
            return None;
        }
        CodeCheck::Expired => {
            refuse(ws, ErrorCode::PairingExpired, "no pairing code is active").await;
            return None;
        }
    }

    // The code was right, but that is not enough: nothing is issued until the
    // person at the laptop says yes (§7.2).
    let approved = app
        .state::<RemoteState>()
        .request_approval(app, device_name, platform, &peer.ip().to_string())
        .await;
    if !approved {
        refuse(ws, ErrorCode::PairingRejected, "the desktop refused").await;
        return None;
    }

    let (device, token) = app
        .state::<RemoteState>()
        .issue_device(device_name, platform)
        .ok()?;
    if !send(
        ws,
        ServerMessage::Paired {
            device_id: device.id.clone(),
            token,
        },
    )
    .await
    {
        return None;
    }
    if !send(ws, welcome(app)).await {
        return None;
    }
    app.state::<RemoteState>().emit_current_status(app);
    Some((device.id, device.name))
}

fn welcome(app: &AppHandle) -> ServerMessage {
    let mut features = Vec::new();
    if app.state::<RemoteState>().config().allow_resize {
        features.push("resize".to_string());
    }
    ServerMessage::Welcome {
        proto: PROTO_VERSION,
        server_version: env!("CARGO_PKG_VERSION").to_string(),
        host_name: super::identity::host_name(),
        features,
    }
}

/// The main loop, entered once the client is authenticated.
async fn serve<S>(
    ws: &mut WebSocketStream<S>,
    session_id: u64,
    kill: Arc<tokio::sync::Notify>,
    app: &AppHandle,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (events_tx, mut events_rx) = mpsc::channel::<Event>(EVENT_QUEUE);
    let mut subs: HashMap<PaneId, Subscription> = HashMap::new();
    let mut roster_rx = app.state::<RemoteState>().watch_roster();
    let mut known = roster_index(&app.state::<RemoteState>().roster());
    let mut exited: HashSet<PaneId> = HashSet::new();
    let mut last_activity = Instant::now();

    // A shell that exits is how most panes disappear; without this the client
    // would be told "closed" for something that ended on its own.
    let exit_listener = {
        let tx = events_tx.clone();
        app.listen_any(PTY_EXIT_EVENT, move |event| {
            if let Ok(pane) = serde_json::from_str::<PaneId>(event.payload()) {
                let _ = tx.try_send(Event::Exited(pane));
            }
        })
    };

    if !send(ws, ServerMessage::Panes { panes: roster(app) }).await {
        app.unlisten(exit_listener);
        return;
    }

    let mut flush = tokio::time::interval(FLUSH_INTERVAL);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let killed = kill.notified();
    tokio::pin!(killed);

    loop {
        tokio::select! {
            // Revoked, disconnected from Settings, or the server was disabled.
            _ = &mut killed => break,

            incoming = ws.next() => {
                let Some(Ok(message)) = incoming else { break };
                last_activity = Instant::now();
                if !handle_message(ws, message, &mut subs, &events_tx, app).await {
                    break;
                }
            }

            event = events_rx.recv() => {
                let Some(event) = event else { break };
                if !handle_event(ws, event, &mut subs, &mut exited, app).await {
                    break;
                }
            }

            changed = roster_rx.changed() => {
                if changed.is_err() { break }
                let panes = Arc::clone(&roster_rx.borrow_and_update());
                if !announce_roster(ws, &panes, &mut known, &mut exited, &mut subs).await {
                    break;
                }
            }

            _ = flush.tick() => {
                if last_activity.elapsed() > IDLE_TIMEOUT { break }
                if !flush_buffers(ws, &mut subs).await { break }
            }
        }
    }

    app.unlisten(exit_listener);
    let _ = session_id;
}

/// Handle one message from the client. `false` ends the session.
async fn handle_message<S>(
    ws: &mut WebSocketStream<S>,
    message: Message,
    subs: &mut HashMap<PaneId, Subscription>,
    events_tx: &mpsc::Sender<Event>,
    app: &AppHandle,
) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    match message {
        // Keystrokes: straight to the PTY, no webview in the path. The PTY's own
        // echo is what makes the desktop terminal and the phone converge.
        Message::Binary(bytes) => {
            let Ok(frame) = super::protocol::decode_frame(&bytes) else {
                return fail(ws, ErrorCode::ProtoError, "malformed binary frame").await;
            };
            if frame.opcode != OPCODE_INPUT {
                return fail(ws, ErrorCode::ProtoError, "unexpected opcode").await;
            }
            if let Err(code) = write_to_pane(app, &frame.pane_id, &frame.data) {
                return report(ws, code, "cannot write to that pane").await;
            }
            true
        }
        Message::Text(text) => {
            let Ok(message) = decode_client_message(&text) else {
                return fail(ws, ErrorCode::ProtoError, "unparseable message").await;
            };
            handle_control(ws, message, subs, events_tx, app).await
        }
        Message::Close(_) => false,
        // Ping/Pong are answered by the WebSocket layer itself.
        _ => true,
    }
}

async fn handle_control<S>(
    ws: &mut WebSocketStream<S>,
    message: ClientMessage,
    subs: &mut HashMap<PaneId, Subscription>,
    events_tx: &mpsc::Sender<Event>,
    app: &AppHandle,
) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    match message {
        ClientMessage::Ping { t } => send(ws, ServerMessage::Pong { t }).await,

        ClientMessage::Subscribe { pane_id } => {
            if subs.contains_key(&pane_id) {
                request_snapshot(&pane_id, events_tx, app);
                mark_awaiting(subs, &pane_id);
                return true;
            }
            if subs.len() >= MAX_SUBSCRIPTIONS {
                return report(ws, ErrorCode::NotPermitted, "too many subscriptions").await;
            }
            let Some(receiver) = subscribe_pane(app, &pane_id) else {
                return report(ws, ErrorCode::UnknownPane, "no such pane").await;
            };
            // Subscribe *before* asking for the snapshot so nothing produced
            // during the round trip is missed (§5.3).
            let forwarder = spawn_forwarder(pane_id.clone(), receiver, events_tx.clone());
            subs.insert(
                pane_id.clone(),
                Subscription {
                    forwarder,
                    buffer: BytesMut::new(),
                    awaiting_snapshot: true,
                },
            );
            request_snapshot(&pane_id, events_tx, app);
            true
        }

        ClientMessage::Unsubscribe { pane_id } => {
            subs.remove(&pane_id);
            true
        }

        ClientMessage::RequestSnapshot { pane_id } => {
            if !subs.contains_key(&pane_id) {
                return report(ws, ErrorCode::UnknownPane, "not subscribed").await;
            }
            mark_awaiting(subs, &pane_id);
            request_snapshot(&pane_id, events_tx, app);
            true
        }

        ClientMessage::CreatePane { cwd } => {
            let app = app.clone();
            let tx = events_tx.clone();
            // Awaiting the new pane here would stall every other message for up
            // to five seconds, so the wait runs on its own task.
            tokio::spawn(async move {
                let result = app.state::<RemoteState>().create_pane(&app, cwd).await;
                let _ = tx.send(Event::Created(result)).await;
            });
            true
        }

        ClientMessage::ClosePane { pane_id } => {
            match app.state::<RemoteState>().close_pane(app, &pane_id) {
                Ok(()) => true,
                Err(code) => report(ws, code, "cannot close that pane").await,
            }
        }

        ClientMessage::Resize {
            pane_id,
            cols,
            rows,
        } => {
            // One PTY has one size, and resizing it reflows the laptop's window
            // too — so this is off unless the desktop user opted in (§10.1).
            if !app.state::<RemoteState>().config().allow_resize {
                return report(ws, ErrorCode::NotPermitted, "remote resize is disabled").await;
            }
            match resize_pane(app, &pane_id, cols, rows) {
                Ok(()) => true,
                Err(code) => report(ws, code, "cannot resize that pane").await,
            }
        }

        // Authenticating twice on one socket is a protocol error, not a way to
        // change identity mid-session.
        ClientMessage::Hello { .. } | ClientMessage::Pair { .. } => {
            fail(ws, ErrorCode::ProtoError, "already authenticated").await
        }
    }
}

async fn handle_event<S>(
    ws: &mut WebSocketStream<S>,
    event: Event,
    subs: &mut HashMap<PaneId, Subscription>,
    exited: &mut HashSet<PaneId>,
    app: &AppHandle,
) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    match event {
        Event::Output { pane, data } => {
            let Some(sub) = subs.get_mut(&pane) else {
                return true;
            };
            if sub.buffer.len() + data.len() > MAX_BUFFER {
                // Too far behind to replay: drop what we have and let the
                // client rebuild from a fresh snapshot (§10.2).
                sub.buffer.clear();
                sub.awaiting_snapshot = false;
                return send(ws, ServerMessage::Resync { pane_id: pane }).await;
            }
            sub.buffer.extend_from_slice(&data);
            true
        }

        Event::Lagged { pane } => {
            if let Some(sub) = subs.get_mut(&pane) {
                sub.buffer.clear();
                sub.awaiting_snapshot = false;
            }
            send(ws, ServerMessage::Resync { pane_id: pane }).await
        }

        Event::Exited(pane) => {
            // Remembered, not acted on: the roster diff is what actually
            // removes the tab, and this only decides how it is explained.
            exited.insert(pane);
            true
        }

        Event::Snapshot { pane, reply } => {
            let (serialized, cols, rows) = match reply {
                Some(reply) => (reply.serialized, reply.cols, reply.rows),
                // The window never answered. An empty screen plus the live
                // stream beats leaving the client staring at nothing (§5.3).
                None => (String::new(), 0, 0),
            };
            if !send(
                ws,
                ServerMessage::Snapshot {
                    pane_id: pane.clone(),
                    serialized,
                    cols,
                    rows,
                },
            )
            .await
            {
                return false;
            }
            if let Some(sub) = subs.get_mut(&pane) {
                // The snapshot is the screen *after* those buffered chunks were
                // already drawn into it, so replaying them would duplicate
                // output. Drop them and stream from here.
                sub.buffer.clear();
                sub.awaiting_snapshot = false;
            }
            true
        }

        Event::Created(Ok(_)) => {
            // The roster publish that carried the new pane has already been
            // forwarded; the client correlates it by diffing.
            true
        }
        Event::Created(Err(code)) => {
            let _ = app;
            report(ws, code, "could not create a pane").await
        }
    }
}

/// Send the roster, and tell the client about panes that vanished since the last
/// one. `false` ends the session.
async fn announce_roster<S>(
    ws: &mut WebSocketStream<S>,
    panes: &[RosterPane],
    known: &mut HashMap<PaneId, String>,
    exited: &mut HashSet<PaneId>,
    subs: &mut HashMap<PaneId, Subscription>,
) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let next = roster_index_of(panes);
    let live_windows: HashSet<&String> = next.values().collect();

    for (pane_id, window) in known.iter() {
        if next.contains_key(pane_id) {
            continue;
        }
        let reason = if exited.remove(pane_id) {
            CloseReason::Exit
        } else if !live_windows.contains(window) {
            CloseReason::WindowClosed
        } else {
            CloseReason::Closed
        };
        subs.remove(pane_id);
        if !send(
            ws,
            ServerMessage::PaneClosed {
                pane_id: pane_id.clone(),
                reason,
            },
        )
        .await
        {
            return false;
        }
    }

    *known = next;
    send(
        ws,
        ServerMessage::Panes {
            panes: panes.to_vec(),
        },
    )
    .await
}

/// Write every pane's accumulated output as one binary frame each.
async fn flush_buffers<S>(
    ws: &mut WebSocketStream<S>,
    subs: &mut HashMap<PaneId, Subscription>,
) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    for (pane_id, sub) in subs.iter_mut() {
        if sub.awaiting_snapshot || sub.buffer.is_empty() {
            continue;
        }
        let chunk = sub.buffer.split().freeze();
        let Ok(frame) = encode_frame(OPCODE_OUTPUT, pane_id, &chunk) else {
            continue; // a pane id we could not frame is one we cannot stream
        };
        if ws.send(Message::Binary(frame)).await.is_err() {
            return false;
        }
    }
    true
}

// --- helpers -------------------------------------------------------------

fn mark_awaiting(subs: &mut HashMap<PaneId, Subscription>, pane_id: &str) {
    if let Some(sub) = subs.get_mut(pane_id) {
        sub.awaiting_snapshot = true;
        sub.buffer.clear();
    }
}

/// Ask the owning window for a pane's screen, off the session loop.
fn request_snapshot(pane_id: &str, events_tx: &mpsc::Sender<Event>, app: &AppHandle) {
    let app = app.clone();
    let tx = events_tx.clone();
    let pane = pane_id.to_string();
    tokio::spawn(async move {
        let reply = app
            .state::<RemoteState>()
            .request_snapshot(&app, &pane)
            .await;
        let _ = tx.send(Event::Snapshot { pane, reply }).await;
    });
}

/// Pump one pane's broadcast receiver into the session's event queue.
fn spawn_forwarder(
    pane: PaneId,
    mut receiver: broadcast::Receiver<Bytes>,
    events_tx: mpsc::Sender<Event>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(data) => {
                    if events_tx
                        .send(Event::Output {
                            pane: pane.clone(),
                            data,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                // The client could not keep up. Tell it, then keep going from
                // wherever the stream is now — never stall the producer.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if events_tx
                        .send(Event::Lagged { pane: pane.clone() })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                // The PTY is gone; the roster diff closes the tab.
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    })
}

fn roster(app: &AppHandle) -> Vec<RosterPane> {
    app.state::<RemoteState>().roster().as_ref().clone()
}

fn roster_index(panes: &[RosterPane]) -> HashMap<PaneId, String> {
    roster_index_of(panes)
}

fn roster_index_of(panes: &[RosterPane]) -> HashMap<PaneId, String> {
    panes
        .iter()
        .map(|p| (p.id.clone(), p.window.clone()))
        .collect()
}

/// Input path. Identical to what `write_pty` does for the webview, including
/// holding the panes lock only for the write itself.
fn write_to_pane(app: &AppHandle, pane_id: &str, data: &[u8]) -> Result<(), ErrorCode> {
    use std::io::Write;
    let state = app.state::<AppState>();
    let mut panes = state.panes.lock().map_err(|_| ErrorCode::UnknownPane)?;
    let handle = panes.get_mut(pane_id).ok_or(ErrorCode::UnknownPane)?;
    handle
        .writer
        .write_all(data)
        .map_err(|_| ErrorCode::UnknownPane)?;
    handle.writer.flush().map_err(|_| ErrorCode::UnknownPane)?;
    Ok(())
}

fn resize_pane(app: &AppHandle, pane_id: &str, cols: u16, rows: u16) -> Result<(), ErrorCode> {
    let state = app.state::<AppState>();
    let panes = state.panes.lock().map_err(|_| ErrorCode::UnknownPane)?;
    let handle = panes.get(pane_id).ok_or(ErrorCode::UnknownPane)?;
    handle
        .master
        .resize(portable_pty::PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|_| ErrorCode::UnknownPane)
}

fn subscribe_pane(app: &AppHandle, pane_id: &str) -> Option<broadcast::Receiver<Bytes>> {
    app.state::<AppState>()
        .subscribe_pane(pane_id)
        .ok()
        .flatten()
        .map(|(receiver, _seq)| receiver)
}

// --- socket writes -------------------------------------------------------

/// Send a control message. `false` means the socket is gone.
async fn send<S>(ws: &mut WebSocketStream<S>, message: ServerMessage) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    ws.send(Message::Text(message.to_text())).await.is_ok()
}

/// Report a non-fatal error and keep the session open.
async fn report<S>(ws: &mut WebSocketStream<S>, code: ErrorCode, message: &str) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    let sent = send(ws, ServerMessage::error(code, message)).await;
    sent && !code.is_fatal()
}

/// Report a fatal error; the caller ends the session.
async fn fail<S>(ws: &mut WebSocketStream<S>, code: ErrorCode, message: &str) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    send(ws, ServerMessage::error(code, message)).await;
    false
}

/// Refuse a connection before it is authenticated.
async fn refuse<S>(ws: &mut WebSocketStream<S>, code: ErrorCode, message: &str)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    send(ws, ServerMessage::error(code, message)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster_pane(id: &str, window: &str) -> RosterPane {
        RosterPane {
            id: id.to_string(),
            title: None,
            cwd: None,
            cols: 80,
            rows: 24,
            preview: String::new(),
            backgrounded: false,
            window: window.to_string(),
            seq: 0,
        }
    }

    #[test]
    fn a_buffer_under_the_cap_accumulates_rather_than_sending_per_chunk() {
        let mut buffer = BytesMut::new();
        for _ in 0..100 {
            buffer.extend_from_slice(b"hello ");
        }
        assert_eq!(buffer.len(), 600);
        assert!(buffer.len() < MAX_BUFFER);
        let flushed = buffer.split().freeze();
        assert_eq!(flushed.len(), 600);
        assert!(buffer.is_empty());
    }

    #[test]
    fn a_full_buffer_is_dropped_rather_than_grown() {
        // The overflow rule the session applies: past the cap the buffer is
        // cleared and the client resyncs instead of being sent stale bytes.
        let mut buffer = BytesMut::new();
        buffer.extend_from_slice(&vec![b'x'; MAX_BUFFER - 10]);
        let incoming = [b'y'; 64];
        let overflows = buffer.len() + incoming.len() > MAX_BUFFER;
        assert!(overflows);
        buffer.clear();
        assert!(buffer.is_empty());
    }

    #[test]
    fn roster_index_maps_every_pane_to_its_window() {
        let panes = vec![
            roster_pane("pane-1", "main"),
            roster_pane("pane-2", "sv-win-a"),
        ];
        let index = roster_index(&panes);
        assert_eq!(index.get("pane-1").map(String::as_str), Some("main"));
        assert_eq!(index.get("pane-2").map(String::as_str), Some("sv-win-a"));
        assert_eq!(index.len(), 2);
    }

    /// The reason a pane vanished, as `announce_roster` decides it. Kept as a
    /// pure helper here so the decision itself is testable without a socket.
    fn reason_for(
        pane_id: &str,
        window: &str,
        next: &HashMap<PaneId, String>,
        exited: &HashSet<PaneId>,
    ) -> CloseReason {
        let live_windows: HashSet<&String> = next.values().collect();
        if exited.contains(pane_id) {
            CloseReason::Exit
        } else if !live_windows.contains(&window.to_string()) {
            CloseReason::WindowClosed
        } else {
            CloseReason::Closed
        }
    }

    #[test]
    fn a_vanished_pane_is_explained_by_how_it_went() {
        let next = roster_index(&[roster_pane("pane-9", "main")]);

        let mut exited = HashSet::new();
        exited.insert("pane-1".to_string());
        assert_eq!(
            reason_for("pane-1", "main", &next, &exited),
            CloseReason::Exit
        );

        let empty = HashSet::new();
        assert_eq!(
            reason_for("pane-2", "main", &next, &empty),
            CloseReason::Closed
        );
        // Its window is not in the new roster at all.
        assert_eq!(
            reason_for("pane-3", "sv-win-gone", &next, &empty),
            CloseReason::WindowClosed
        );
    }

    #[test]
    fn non_fatal_errors_keep_the_session_open() {
        // `report` returns `sent && !code.is_fatal()`; this pins the second half.
        assert!(!ErrorCode::UnknownPane.is_fatal());
        assert!(!ErrorCode::NotPermitted.is_fatal());
        assert!(ErrorCode::ProtoError.is_fatal());
        assert!(ErrorCode::Unauthorized.is_fatal());
    }
}
