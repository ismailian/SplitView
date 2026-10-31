//! Remote control: an opt-in, encrypted WebSocket server that lets a paired
//! phone watch and drive this desktop's panes.
//!
//! `docs/remote-control-desktop.md` is the design document and the normative
//! wire spec; CLAUDE.md §21 is the short version. Three rules keep the
//! architecture the same as it was without this feature:
//!
//! 1. **Input goes straight to the PTY.** A remote keystroke calls the same
//!    write path `write_pty` does; it never passes through a webview. The PTY's
//!    own echo comes back on the read side, so the desktop terminal and the
//!    phone converge for free.
//! 2. **Output fans out in the backend.** The reader thread is unchanged in
//!    shape (see `state::OutputSink`); it just has more than one sink.
//! 3. **Anything that touches layout goes through the frontend.** Creating and
//!    closing panes are relayed as events into the webview, which uses the
//!    existing layout operations. Rust still does not know what a split is.
//!
//! Everything here is inert until the user enables it in Settings: no socket is
//! bound, no roster is accepted, and the fan-out costs one atomic load per PTY
//! chunk.

pub mod auth;
pub mod discovery;
pub mod identity;
pub mod pairing;
pub mod protocol;
pub mod server;
pub mod session;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::sync::{oneshot, watch, Notify};

use crate::error::{Error, Result};
use crate::state::PaneId;

use auth::{Device, DeviceRegistry};
use identity::Identity;
use pairing::{Pairing, RateLimiter};
use protocol::{ErrorCode, RosterPane};

/// Emitted to one window: do something to the layout on the remote's behalf.
pub const EVENT_ACTION: &str = "remote-action";
/// Emitted to one window: serialize a pane so a newly attached client can see
/// its current screen.
pub const EVENT_SNAPSHOT_REQUEST: &str = "remote-snapshot-request";
/// Emitted to the target window: a device is asking to pair.
pub const EVENT_PAIRING_REQUEST: &str = "remote-pairing-request";
/// Broadcast: the server was enabled/disabled, or a session came or went.
pub const EVENT_STATUS_CHANGED: &str = "remote-status-changed";

/// How long a window gets to answer a snapshot request before the client is
/// sent an empty screen and left to fill it from the live stream.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a remotely requested pane creation waits to appear in the roster.
const CREATE_TIMEOUT: Duration = Duration::from_secs(5);

/// Which interfaces the server binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BindMode {
    /// Every interface: reachable from the LAN.
    Lan,
    /// Loopback only — for reaching the app through an SSH or Tailscale tunnel.
    Loopback,
}

impl BindMode {
    fn address(self, port: u16) -> SocketAddr {
        match self {
            Self::Lan => SocketAddr::from(([0, 0, 0, 0], port)),
            Self::Loopback => SocketAddr::from(([127, 0, 0, 1], port)),
        }
    }
}

/// The server's user-facing configuration, handed in by Settings on enable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteConfig {
    pub bind: BindMode,
    /// `0` asks the OS for an ephemeral port.
    pub port: u16,
    /// Whether a client may resize a pane, which reflows the desktop window too
    /// (`docs/remote-control-desktop.md` §10.1). On by default: the phone has
    /// its own opt-in, and nothing resizes until that is on as well.
    pub allow_resize: bool,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            bind: BindMode::Lan,
            port: DEFAULT_PORT,
            allow_resize: true,
        }
    }
}

/// Default listening port. Nothing well-known, easy to remember, easy to change.
pub const DEFAULT_PORT: u16 = 8765;

/// Max concurrent client sessions (§7.3).
pub const MAX_SESSIONS: usize = 4;

/// A pane exactly as one window published it. Titles, working directories,
/// preview lines and ordering only exist in the webview, so the webview is what
/// tells us about them.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedPane {
    pub id: PaneId,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub preview: String,
    pub backgrounded: bool,
}

/// One window's view of itself, pushed by `publish_panes`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowPanes {
    pub window_label: String,
    /// Monotonic, set by the window whenever it takes focus. The window with the
    /// highest value is where a remotely created pane appears.
    pub focused_at: f64,
    pub focused_pane_id: Option<PaneId>,
    pub panes: Vec<PublishedPane>,
}

/// A window's snapshot reply, routed back to the session that asked.
#[derive(Debug, Clone)]
pub struct SnapshotReply {
    pub serialized: String,
    pub cols: u16,
    pub rows: u16,
}

/// A paired device as *Settings* sees it. Deliberately not `auth::Device`: the
/// token digest is of no use to the UI, and a secret-adjacent value that never
/// leaves the backend cannot leak from it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub paired_at: u64,
    pub last_seen: u64,
}

impl From<&Device> for DeviceInfo {
    fn from(device: &Device) -> Self {
        Self {
            id: device.id.clone(),
            name: device.name.clone(),
            platform: device.platform.clone(),
            paired_at: device.paired_at,
            last_seen: device.last_seen,
        }
    }
}

/// A live client session, as Settings lists it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: u64,
    pub device_id: String,
    pub device_name: String,
    pub address: String,
    /// Unix seconds.
    pub connected_at: u64,
}

struct SessionHandle {
    info: SessionInfo,
    /// Notified to end the session (revocation, disconnect, server shutdown).
    kill: Arc<Notify>,
}

/// Everything Settings needs to render the Remote section.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStatus {
    pub enabled: bool,
    pub bind: BindMode,
    pub allow_resize: bool,
    /// The bound port while running, else the configured one.
    pub port: u16,
    /// The address to type into the phone: the LAN address, or loopback.
    pub host: Option<String>,
    pub host_name: String,
    /// `AB:CD:…`, present once an identity exists.
    pub fingerprint: Option<String>,
    pub sessions: Vec<SessionInfo>,
    pub devices: Vec<DeviceInfo>,
    /// The live pairing code and how many seconds are left on it.
    pub pairing: Option<PairingStatus>,
}

/// The in-flight pairing code, for the QR modal and its countdown.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingStatus {
    pub code: String,
    pub seconds_left: u64,
    /// The full `splitview://pair?…` payload the QR encodes.
    pub payload: String,
}

/// A device waiting for the desktop user to say yes.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingRequest {
    pub request_id: u64,
    pub device_name: String,
    pub platform: String,
    pub address: String,
}

/// Asks a window to change its layout on the remote's behalf. Addressed: every
/// other window ignores it, the same way `reattach-pane` is addressed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAction {
    pub window_label: String,
    /// `"create_pane"` or `"close_pane"`.
    pub action: &'static str,
    pub pane_id: Option<PaneId>,
    pub cwd: Option<String>,
}

/// Asks a window for a pane's current screen.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRequest {
    pub window_label: String,
    pub request_id: u64,
    pub pane_id: PaneId,
}

/// The per-window roster, in window registration order.
#[derive(Debug, Default)]
struct WindowRoster {
    order: Vec<String>,
    windows: HashMap<String, WindowPanes>,
}

impl WindowRoster {
    fn publish(&mut self, snapshot: WindowPanes) {
        // `order` is the registration order the roster is built in, so a window
        // that re-publishes must not move — and must not be listed twice, which
        // would put every one of its panes in the roster twice.
        if !self.order.iter().any(|l| l == &snapshot.window_label) {
            self.order.push(snapshot.window_label.clone());
        }
        self.windows.insert(snapshot.window_label.clone(), snapshot);
    }

    fn remove(&mut self, label: &str) {
        self.windows.remove(label);
        self.order.retain(|l| l != label);
    }

    /// Forget every window (the feature was turned off). Both halves, or a
    /// stale label left in `order` would list its panes twice on the next
    /// enable.
    fn clear(&mut self) {
        self.windows.clear();
        self.order.clear();
    }

    /// The window a remotely created pane goes to: whichever was focused last.
    fn target_window(&self) -> Option<String> {
        self.windows
            .values()
            .max_by(|a, b| a.focused_at.total_cmp(&b.focused_at))
            .map(|w| w.window_label.clone())
    }

    /// The working directory of the target window's focused pane — what a
    /// keyboard split would inherit, and the fallback when the client did not
    /// say where to open (`docs/remote-control-desktop.md` §14.3).
    fn focused_cwd(&self, label: &str) -> Option<String> {
        let window = self.windows.get(label)?;
        let focused = window.focused_pane_id.as_deref()?;
        window
            .panes
            .iter()
            .find(|p| p.id == focused)
            .and_then(|p| p.cwd.clone())
    }

    fn owner_of(&self, pane_id: &str) -> Option<String> {
        self.order.iter().find_map(|label| {
            let window = self.windows.get(label)?;
            window
                .panes
                .iter()
                .any(|p| p.id == pane_id)
                .then(|| label.clone())
        })
    }

    /// The flat roster the phone sees: window registration order, then pane
    /// order within a window.
    ///
    /// Only panes the backend still has a live PTY for are listed — the roster
    /// names panes you can actually talk to, so a stale webview snapshot cannot
    /// advertise a pane that is already gone.
    fn merge(&self, seqs: &HashMap<PaneId, u64>) -> Vec<RosterPane> {
        let mut out = Vec::new();
        for label in &self.order {
            let Some(window) = self.windows.get(label) else {
                continue;
            };
            for pane in &window.panes {
                let Some(seq) = seqs.get(&pane.id) else {
                    continue;
                };
                out.push(RosterPane {
                    id: pane.id.clone(),
                    title: pane.title.clone(),
                    cwd: pane.cwd.clone(),
                    cols: pane.cols,
                    rows: pane.rows,
                    preview: pane.preview.clone(),
                    backgrounded: pane.backgrounded,
                    window: label.clone(),
                    seq: *seq,
                });
            }
        }
        out
    }
}

/// A bound, running server.
struct RunningServer {
    addr: SocketAddr,
    /// Flipped to stop the accept loop.
    shutdown: watch::Sender<bool>,
    /// mDNS advertisement; dropped (and unregistered) with the server.
    _advert: Option<discovery::Advert>,
}

/// All remote-control state. Managed by Tauri alongside `AppState`.
pub struct RemoteState {
    dir: PathBuf,
    config: Mutex<RemoteConfig>,
    running: Mutex<Option<RunningServer>>,
    identity: Mutex<Option<Arc<Identity>>>,
    windows: Mutex<WindowRoster>,
    /// The merged roster, watched by every session.
    roster: watch::Sender<Arc<Vec<RosterPane>>>,
    pending_snapshots: Mutex<HashMap<u64, oneshot::Sender<SnapshotReply>>>,
    pending_pairings: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
    devices: Mutex<DeviceRegistry>,
    pairing: Mutex<Pairing>,
    limiter: Mutex<RateLimiter>,
    sessions: Mutex<Vec<SessionHandle>>,
    next_id: AtomicU64,
}

impl Default for RemoteState {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteState {
    pub fn new() -> Self {
        let (roster, _) = watch::channel(Arc::new(Vec::new()));
        Self {
            dir: identity::remote_dir(),
            config: Mutex::new(RemoteConfig::default()),
            running: Mutex::new(None),
            identity: Mutex::new(None),
            windows: Mutex::new(WindowRoster::default()),
            roster,
            pending_snapshots: Mutex::new(HashMap::new()),
            pending_pairings: Mutex::new(HashMap::new()),
            devices: Mutex::new(DeviceRegistry::default()),
            pairing: Mutex::new(Pairing::default()),
            limiter: Mutex::new(RateLimiter::default()),
            sessions: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.running.lock().map(|r| r.is_some()).unwrap_or(false)
    }

    pub fn config(&self) -> RemoteConfig {
        self.config
            .lock()
            .map(|c| *c)
            .unwrap_or_else(|_| RemoteConfig::default())
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    // --- lifecycle -------------------------------------------------------

    /// Bind, advertise and start accepting. Returns the running status.
    ///
    /// Enabling is the moment this app becomes a remote shell server, so it is
    /// also the moment the identity is generated, the device registry is read,
    /// and the first audit line is written.
    pub fn enable(&self, app: &AppHandle, config: RemoteConfig) -> Result<RemoteStatus> {
        self.disable(app)?;

        std::fs::create_dir_all(&self.dir)?;
        let identity = Identity::load_or_create(&self.dir)?;
        *self.identity.lock().map_err(|_| Error::Poisoned)? = Some(Arc::clone(&identity));
        *self.devices.lock().map_err(|_| Error::Poisoned)? = DeviceRegistry::load(&self.dir);
        *self.config.lock().map_err(|_| Error::Poisoned)? = config;

        let (shutdown, shutdown_rx) = watch::channel(false);
        let addr = server::start(app.clone(), config, identity, shutdown_rx)?;

        let advert = match config.bind {
            // Loopback is for tunnelled access; advertising it on the LAN would
            // be advertising an address no one on the LAN can reach.
            BindMode::Loopback => None,
            BindMode::Lan => discovery::advertise(addr.port()),
        };

        *self.running.lock().map_err(|_| Error::Poisoned)? = Some(RunningServer {
            addr,
            shutdown,
            _advert: advert,
        });

        self.audit("enable", &format!("{addr} bind={:?}", config.bind));
        let status = self.status();
        self.emit_status(app, &status);
        Ok(status)
    }

    /// Stop the server, end every session and drop the pairing code.
    pub fn disable(&self, app: &AppHandle) -> Result<RemoteStatus> {
        let running = self.running.lock().map_err(|_| Error::Poisoned)?.take();
        if let Some(running) = running {
            let _ = running.shutdown.send(true);
            self.audit("disable", &running.addr.to_string());
        }
        self.pairing.lock().map_err(|_| Error::Poisoned)?.cancel();
        for handle in self.sessions.lock().map_err(|_| Error::Poisoned)?.drain(..) {
            handle.kill.notify_one();
        }
        // A roster published while the feature was on must not outlive it.
        self.windows.lock().map_err(|_| Error::Poisoned)?.clear();
        let _ = self.roster.send_replace(Arc::new(Vec::new()));

        let status = self.status();
        self.emit_status(app, &status);
        Ok(status)
    }

    /// Everything Settings renders.
    pub fn status(&self) -> RemoteStatus {
        let config = self.config();
        let running = self
            .running
            .lock()
            .ok()
            .and_then(|r| r.as_ref().map(|s| s.addr));
        let host = running.map(|addr| match config.bind {
            BindMode::Loopback => "127.0.0.1".to_string(),
            BindMode::Lan => identity::primary_lan_ip()
                .map(|ip| ip.to_string())
                .unwrap_or_else(|| addr.ip().to_string()),
        });
        let fingerprint = self
            .identity
            .lock()
            .ok()
            .and_then(|i| i.as_ref().map(|id| id.fingerprint_hex()));
        let pairing = self.pairing_status(host.as_deref(), running.map(|a| a.port()));

        RemoteStatus {
            enabled: running.is_some(),
            bind: config.bind,
            allow_resize: config.allow_resize,
            port: running.map(|a| a.port()).unwrap_or(config.port),
            host,
            host_name: identity::host_name(),
            fingerprint,
            sessions: self
                .sessions
                .lock()
                .map(|s| s.iter().map(|h| h.info.clone()).collect())
                .unwrap_or_default(),
            devices: self
                .devices
                .lock()
                .map(|d| d.list().iter().map(DeviceInfo::from).collect())
                .unwrap_or_default(),
            pairing,
        }
    }

    fn pairing_status(&self, host: Option<&str>, port: Option<u16>) -> Option<PairingStatus> {
        let (code, left) = self.pairing.lock().ok()?.current()?;
        let fingerprint = self
            .identity
            .lock()
            .ok()?
            .as_ref()
            .map(|i| i.fingerprint_b64())?;
        Some(PairingStatus {
            payload: pairing::qr_payload(host?, port?, &fingerprint, &code),
            code,
            seconds_left: left.as_secs(),
        })
    }

    /// Push the current status to every window (the indicator and any open
    /// Settings section read it).
    pub fn emit_status(&self, app: &AppHandle, status: &RemoteStatus) {
        let _ = app.emit(EVENT_STATUS_CHANGED, status);
    }

    pub fn emit_current_status(&self, app: &AppHandle) {
        let status = self.status();
        self.emit_status(app, &status);
    }

    // --- pairing ---------------------------------------------------------

    /// Issue a pairing code (the user pressed "Pair a device").
    pub fn start_pairing(&self, app: &AppHandle) -> Result<RemoteStatus> {
        if !self.is_enabled() {
            return Err(Error::Remote("remote control is not enabled".into()));
        }
        let code = self.pairing.lock().map_err(|_| Error::Poisoned)?.start();
        self.audit("pairing_start", &format!("code={code}"));
        let status = self.status();
        self.emit_status(app, &status);
        Ok(status)
    }

    pub fn cancel_pairing(&self, app: &AppHandle) -> Result<RemoteStatus> {
        self.pairing.lock().map_err(|_| Error::Poisoned)?.cancel();
        self.audit("pairing_cancel", "");
        let status = self.status();
        self.emit_status(app, &status);
        Ok(status)
    }

    /// Check a presented pairing code. Consumes the code on success.
    pub fn check_code(&self, code: &str) -> pairing::CodeCheck {
        self.pairing
            .lock()
            .map(|mut p| p.check(code))
            .unwrap_or(pairing::CodeCheck::Expired)
    }

    /// Ask the desktop user to approve a device, and wait for the answer.
    ///
    /// This prompt is the last line of defence (§7.2) and it is not skippable:
    /// with no window to ask, the answer is no.
    pub async fn request_approval(
        &self,
        app: &AppHandle,
        device_name: &str,
        platform: &str,
        address: &str,
    ) -> bool {
        let Some(window) = self.target_window() else {
            return false;
        };
        let request_id = self.next_id();
        let (tx, rx) = oneshot::channel();
        {
            let Ok(mut pending) = self.pending_pairings.lock() else {
                return false;
            };
            pending.insert(request_id, tx);
        }
        let payload = PairingRequest {
            request_id,
            device_name: device_name.to_string(),
            platform: platform.to_string(),
            address: address.to_string(),
        };
        if app
            .emit_to(&window, EVENT_PAIRING_REQUEST, payload)
            .is_err()
        {
            self.take_pairing(request_id);
            return false;
        }
        // No timeout: the prompt stays until the user answers or the code's own
        // TTL makes the point moot. A client that gives up just disconnects.
        let approved = rx.await.unwrap_or(false);
        self.take_pairing(request_id);
        approved
    }

    fn take_pairing(&self, request_id: u64) -> Option<oneshot::Sender<bool>> {
        self.pending_pairings.lock().ok()?.remove(&request_id)
    }

    /// The desktop user answered a pairing prompt.
    pub fn respond_to_pairing(&self, request_id: u64, approve: bool) -> Result<()> {
        if let Some(tx) = self.take_pairing(request_id) {
            let _ = tx.send(approve);
        }
        self.audit(
            if approve {
                "pairing_approve"
            } else {
                "pairing_reject"
            },
            &format!("request={request_id}"),
        );
        Ok(())
    }

    /// Mint a token for an approved device.
    pub fn issue_device(&self, name: &str, platform: &str) -> Result<(Device, String)> {
        let issued = self
            .devices
            .lock()
            .map_err(|_| Error::Poisoned)?
            .issue(name, platform)?;
        self.audit("pair", &format!("{} ({})", issued.0.name, issued.0.id));
        Ok(issued)
    }

    /// Verify a returning device's token.
    pub fn verify_device(&self, device_id: &str, token: &str) -> Option<Device> {
        let device = self.devices.lock().ok()?.verify(device_id, token)?;
        if let Ok(mut devices) = self.devices.lock() {
            devices.touch(device_id);
        }
        Some(device)
    }

    /// Forget a device **and** cut any session using it — a revoked token that
    /// stays connected is not revoked (§7.3).
    pub fn revoke_device(&self, app: &AppHandle, device_id: &str) -> Result<RemoteStatus> {
        let removed = self
            .devices
            .lock()
            .map_err(|_| Error::Poisoned)?
            .revoke(device_id)?;
        if removed {
            self.kill_sessions(|info| info.device_id == device_id)?;
            self.audit("revoke", device_id);
        }
        let status = self.status();
        self.emit_status(app, &status);
        Ok(status)
    }

    /// End one live session (Settings' "Disconnect").
    pub fn disconnect_session(&self, app: &AppHandle, session_id: u64) -> Result<RemoteStatus> {
        self.kill_sessions(|info| info.id == session_id)?;
        self.audit("disconnect", &format!("session={session_id}"));
        let status = self.status();
        self.emit_status(app, &status);
        Ok(status)
    }

    fn kill_sessions(&self, mut matches: impl FnMut(&SessionInfo) -> bool) -> Result<()> {
        let mut sessions = self.sessions.lock().map_err(|_| Error::Poisoned)?;
        let mut kept = Vec::with_capacity(sessions.len());
        for handle in sessions.drain(..) {
            if matches(&handle.info) {
                handle.kill.notify_one();
            } else {
                kept.push(handle);
            }
        }
        *sessions = kept;
        Ok(())
    }

    /// Check (and count) a connection attempt against the per-IP throttle.
    pub fn allow_connection(&self, ip: std::net::IpAddr) -> bool {
        self.limiter
            .lock()
            .map(|mut l| l.allow(ip))
            .unwrap_or(false)
    }

    pub fn session_count(&self) -> usize {
        self.sessions.lock().map(|s| s.len()).unwrap_or(0)
    }

    /// Register an authenticated session; the returned `Notify` is how the
    /// session learns it has been revoked or disconnected.
    pub fn register_session(&self, app: &AppHandle, info: SessionInfo) -> Arc<Notify> {
        let kill = Arc::new(Notify::new());
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.push(SessionHandle {
                info: info.clone(),
                kill: Arc::clone(&kill),
            });
        }
        self.audit(
            "connect",
            &format!(
                "{} ({}) from {}",
                info.device_name, info.device_id, info.address
            ),
        );
        self.emit_current_status(app);
        kill
    }

    pub fn unregister_session(&self, app: &AppHandle, session_id: u64) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.retain(|h| h.info.id != session_id);
        }
        self.audit("disconnect", &format!("session={session_id}"));
        self.emit_current_status(app);
    }

    pub fn new_session_id(&self) -> u64 {
        self.next_id()
    }

    // --- roster ----------------------------------------------------------

    /// Accept a window's snapshot and republish the merged roster.
    ///
    /// Refused while the feature is off: a roster that names every terminal you
    /// have open is exactly the thing that must not exist when nobody asked for
    /// remote control (CLAUDE.md §20).
    pub fn publish_window(&self, snapshot: WindowPanes, seqs: &HashMap<PaneId, u64>) -> Result<()> {
        if !self.is_enabled() {
            return Ok(());
        }
        let mut windows = self.windows.lock().map_err(|_| Error::Poisoned)?;
        windows.publish(snapshot);
        let merged = windows.merge(seqs);
        drop(windows);
        let _ = self.roster.send_replace(Arc::new(merged));
        Ok(())
    }

    /// A window went away: drop its panes from the roster.
    pub fn forget_window(&self, label: &str, seqs: &HashMap<PaneId, u64>) {
        let Ok(mut windows) = self.windows.lock() else {
            return;
        };
        windows.remove(label);
        let merged = windows.merge(seqs);
        drop(windows);
        let _ = self.roster.send_replace(Arc::new(merged));
    }

    pub fn roster(&self) -> Arc<Vec<RosterPane>> {
        Arc::clone(&self.roster.borrow())
    }

    pub fn watch_roster(&self) -> watch::Receiver<Arc<Vec<RosterPane>>> {
        self.roster.subscribe()
    }

    fn target_window(&self) -> Option<String> {
        self.windows.lock().ok()?.target_window()
    }

    fn owner_window(&self, pane_id: &str) -> Option<String> {
        self.windows.lock().ok()?.owner_of(pane_id)
    }

    // --- snapshots and actions -------------------------------------------

    /// Ask the window that owns `pane_id` for its current screen.
    ///
    /// `None` means the window did not answer in time (busy, closing, gone); the
    /// caller sends an empty snapshot and lets the live stream fill the screen
    /// rather than leaving the client waiting.
    pub async fn request_snapshot(&self, app: &AppHandle, pane_id: &str) -> Option<SnapshotReply> {
        let window = self.owner_window(pane_id)?;
        let request_id = self.next_id();
        let (tx, rx) = oneshot::channel();
        self.pending_snapshots.lock().ok()?.insert(request_id, tx);

        let payload = SnapshotRequest {
            window_label: window.clone(),
            request_id,
            pane_id: pane_id.to_string(),
        };
        if app
            .emit_to(&window, EVENT_SNAPSHOT_REQUEST, payload)
            .is_err()
        {
            self.pending_snapshots.lock().ok()?.remove(&request_id);
            return None;
        }

        let reply = tokio::time::timeout(SNAPSHOT_TIMEOUT, rx).await;
        self.pending_snapshots.lock().ok()?.remove(&request_id);
        reply.ok()?.ok()
    }

    /// A window answered a snapshot request.
    pub fn provide_snapshot(&self, request_id: u64, reply: SnapshotReply) -> Result<()> {
        if let Some(tx) = self
            .pending_snapshots
            .lock()
            .map_err(|_| Error::Poisoned)?
            .remove(&request_id)
        {
            let _ = tx.send(reply);
        }
        Ok(())
    }

    /// Relay "create a pane" to the target window and wait for it to appear.
    ///
    /// The frontend owns layout, so Rust asks rather than acts; the new pane's
    /// id comes back in the next roster publish, which is what this correlates.
    pub async fn create_pane(
        &self,
        app: &AppHandle,
        cwd: Option<String>,
    ) -> std::result::Result<PaneId, ErrorCode> {
        let window = self.target_window().ok_or(ErrorCode::NoWindow)?;
        // The client names the directory it wants (the pane it was looking at);
        // with none, fall back to what a keyboard split would have inherited.
        let cwd = cwd.or_else(|| {
            self.windows
                .lock()
                .ok()
                .and_then(|w| w.focused_cwd(&window))
        });
        let before: Vec<PaneId> = self.roster().iter().map(|p| p.id.clone()).collect();
        let mut rx = self.watch_roster();

        app.emit_to(
            &window,
            EVENT_ACTION,
            RemoteAction {
                window_label: window.clone(),
                action: "create_pane",
                pane_id: None,
                cwd,
            },
        )
        .map_err(|_| ErrorCode::NoWindow)?;

        let wait = async {
            loop {
                if rx.changed().await.is_err() {
                    return Err(ErrorCode::NoWindow);
                }
                let roster = Arc::clone(&rx.borrow_and_update());
                if let Some(pane) = roster.iter().find(|p| !before.contains(&p.id)) {
                    return Ok(pane.id.clone());
                }
            }
        };
        tokio::time::timeout(CREATE_TIMEOUT, wait)
            .await
            .unwrap_or(Err(ErrorCode::NoWindow))
    }

    /// Relay "close this pane" to the window that owns it.
    pub fn close_pane(&self, app: &AppHandle, pane_id: &str) -> std::result::Result<(), ErrorCode> {
        let window = self.owner_window(pane_id).ok_or(ErrorCode::UnknownPane)?;
        app.emit_to(
            &window,
            EVENT_ACTION,
            RemoteAction {
                window_label: window.clone(),
                action: "close_pane",
                pane_id: Some(pane_id.to_string()),
                cwd: None,
            },
        )
        .map_err(|_| ErrorCode::NoWindow)
    }

    // --- audit -----------------------------------------------------------

    /// Append one line to `remote/audit.log`. Best-effort: a failure to write
    /// the log must never break the thing being logged.
    pub fn audit(&self, event: &str, detail: &str) {
        use std::io::Write;
        let _ = std::fs::create_dir_all(&self.dir);
        let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("audit.log"))
        else {
            return;
        };
        let _ = writeln!(file, "{}\t{event}\t{detail}", timestamp());
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ`, hand-formatted so the `time` crate's formatting
/// feature (and its dependencies) stay out of the build.
fn timestamp() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

/// Unix seconds, for `SessionInfo::connected_at`.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str) -> PublishedPane {
        PublishedPane {
            id: id.to_string(),
            title: None,
            cwd: None,
            cols: 80,
            rows: 24,
            preview: String::new(),
            backgrounded: false,
        }
    }

    fn window(label: &str, focused_at: f64, ids: &[&str]) -> WindowPanes {
        WindowPanes {
            window_label: label.to_string(),
            focused_at,
            focused_pane_id: ids.first().map(|s| s.to_string()),
            panes: ids.iter().map(|id| pane(id)).collect(),
        }
    }

    fn seqs(pairs: &[(&str, u64)]) -> HashMap<PaneId, u64> {
        pairs
            .iter()
            .map(|(id, seq)| (id.to_string(), *seq))
            .collect()
    }

    #[test]
    fn merges_windows_in_registration_order_then_pane_order() {
        let mut roster = WindowRoster::default();
        roster.publish(window("main", 1.0, &["pane-1", "pane-2"]));
        roster.publish(window("sv-win-a", 2.0, &["pane-3"]));
        // A re-publish must not re-order the windows.
        roster.publish(window("main", 3.0, &["pane-1", "pane-2"]));

        let merged = roster.merge(&seqs(&[("pane-1", 10), ("pane-2", 20), ("pane-3", 30)]));
        let ids: Vec<&str> = merged.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["pane-1", "pane-2", "pane-3"]);
        assert_eq!(merged[0].window, "main");
        assert_eq!(merged[2].window, "sv-win-a");
        assert_eq!(merged[1].seq, 20);
    }

    #[test]
    fn a_pane_with_no_live_pty_is_not_advertised() {
        let mut roster = WindowRoster::default();
        roster.publish(window("main", 1.0, &["pane-1", "pane-gone"]));
        let merged = roster.merge(&seqs(&[("pane-1", 1)]));
        let ids: Vec<&str> = merged.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["pane-1"]);
    }

    #[test]
    fn the_target_window_is_the_one_focused_last() {
        let mut roster = WindowRoster::default();
        roster.publish(window("main", 5.0, &["pane-1"]));
        roster.publish(window("sv-win-a", 9.0, &["pane-2"]));
        assert_eq!(roster.target_window().as_deref(), Some("sv-win-a"));

        roster.publish(window("main", 12.0, &["pane-1"]));
        assert_eq!(roster.target_window().as_deref(), Some("main"));
    }

    #[test]
    fn panes_are_owned_by_the_window_that_published_them() {
        let mut roster = WindowRoster::default();
        roster.publish(window("main", 1.0, &["pane-1"]));
        roster.publish(window("sv-win-a", 2.0, &["pane-2"]));
        assert_eq!(roster.owner_of("pane-2").as_deref(), Some("sv-win-a"));
        assert_eq!(roster.owner_of("pane-nope"), None);
    }

    #[test]
    fn forgetting_a_window_drops_its_panes_and_its_place_in_the_order() {
        let mut roster = WindowRoster::default();
        roster.publish(window("main", 1.0, &["pane-1"]));
        roster.publish(window("sv-win-a", 2.0, &["pane-2"]));
        roster.remove("main");

        let merged = roster.merge(&seqs(&[("pane-1", 1), ("pane-2", 2)]));
        let ids: Vec<&str> = merged.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["pane-2"]);
        assert_eq!(roster.target_window().as_deref(), Some("sv-win-a"));
        assert_eq!(roster.owner_of("pane-1"), None);
    }

    #[test]
    fn re_publishing_a_window_never_duplicates_its_panes() {
        let mut roster = WindowRoster::default();
        roster.publish(window("main", 1.0, &["pane-1"]));
        roster.publish(window("main", 2.0, &["pane-1"]));

        assert_eq!(roster.merge(&seqs(&[("pane-1", 1)])).len(), 1);
    }

    #[test]
    fn clearing_forgets_the_window_order_too() {
        // Disabling clears the roster; re-enabling must not find a stale label
        // still in `order`, which would list the window's panes twice.
        let mut roster = WindowRoster::default();
        roster.publish(window("main", 1.0, &["pane-1"]));
        roster.clear();
        roster.publish(window("main", 1.0, &["pane-1"]));

        let merged = roster.merge(&seqs(&[("pane-1", 1)]));
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id, "pane-1");
    }

    #[test]
    fn an_empty_roster_has_no_target_window() {
        let roster = WindowRoster::default();
        assert_eq!(roster.target_window(), None);
        assert!(roster.merge(&seqs(&[("pane-1", 1)])).is_empty());
    }

    #[test]
    fn the_roster_stays_empty_while_the_feature_is_off() {
        let state = RemoteState::new();
        assert!(!state.is_enabled());
        state
            .publish_window(window("main", 1.0, &["pane-1"]), &seqs(&[("pane-1", 1)]))
            .expect("publish");
        assert!(state.roster().is_empty());
    }

    #[test]
    fn bind_modes_pick_the_right_interface() {
        assert_eq!(
            BindMode::Loopback.address(8765).ip().to_string(),
            "127.0.0.1"
        );
        assert_eq!(BindMode::Lan.address(8765).ip().to_string(), "0.0.0.0");
    }

    #[test]
    fn timestamps_are_rfc3339_shaped() {
        let ts = timestamp();
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
    }
}
