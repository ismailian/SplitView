//! TLS + WebSocket accept loop (`docs/remote-control-desktop.md` §7.1, §7.3).
//!
//! Everything a connection has to get past before it becomes a session lives
//! here: the per-IP throttle, the concurrent-session cap, the TLS handshake, and
//! the WebSocket upgrade on the one path we serve. Nothing in this file knows
//! what a pane is.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;

use crate::error::{Error, Result};

use super::identity::Identity;
use super::{AppHandle, MAX_SESSIONS, RemoteConfig, session};

/// The only path the server answers on. Anything else is refused at the
/// handshake, so a stray browser tab gets a clean 404 rather than a socket.
pub const WS_PATH: &str = "/remote";

/// A connection that does not complete TLS and the WebSocket upgrade in this
/// long is dropped, so a half-open socket cannot hold a session slot.
const UPGRADE_TIMEOUT: Duration = Duration::from_secs(15);

/// Bind, start accepting, and return the address we actually bound.
///
/// Binding happens synchronously so Settings can show the real port (which
/// matters when the configured one is taken and the OS picks another). The
/// accept loop then runs on Tauri's async runtime until `shutdown` flips.
pub fn start(
    app: AppHandle,
    config: RemoteConfig,
    identity: Arc<Identity>,
    shutdown: watch::Receiver<bool>,
) -> Result<SocketAddr> {
    let acceptor = tls_acceptor(&identity)?;
    let requested = super::BindMode::address(config.bind, config.port);

    let rt = app.runtime();
    let listener = rt.block_on(async move {
        match TcpListener::bind(requested).await {
            Ok(listener) => Ok(listener),
            // The configured port is taken (another SplitView, or something
            // else): fall back to an ephemeral one rather than refusing to
            // start. Settings shows the port that was actually bound.
            Err(_) if requested.port() != 0 => {
                let fallback = super::BindMode::address(config.bind, 0);
                TcpListener::bind(fallback).await
            }
            Err(e) => Err(e),
        }
    })?;
    let addr = listener.local_addr()?;

    rt.spawn(accept_loop(listener, acceptor, app.clone(), shutdown));
    Ok(addr)
}

/// Build a rustls config for our self-signed identity.
///
/// The crypto provider is passed explicitly because the crate is built without
/// default features — `ring` is what we compile, so `ring` is what we install,
/// rather than depending on a process-wide default being set somewhere.
fn tls_acceptor(identity: &Identity) -> Result<TlsAcceptor> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Remote(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(vec![identity.cert.clone()], identity.key_clone())
        .map_err(|e| Error::Remote(e.to_string()))?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

async fn accept_loop(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    app: AppHandle,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                // Either the sender said stop, or it was dropped with the
                // server; both mean the same thing.
                if changed.is_err() || *shutdown.borrow() { break }
            }
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else { continue };
                if !admit(&app, peer) {
                    continue;
                }
                tokio::spawn(handshake(stream, peer, acceptor.clone(), app.clone()));
            }
        }
    }
}

/// The two cheap checks that happen before a byte of TLS: is this IP hammering
/// us, and do we already have as many sessions as we allow?
fn admit(app: &AppHandle, peer: SocketAddr) -> bool {
    let state = &app.remote;
    if !state.allow_connection(peer.ip()) {
        state.audit("rate_limited", &peer.ip().to_string());
        return false;
    }
    if state.session_count() >= MAX_SESSIONS {
        state.audit("refused_busy", &peer.ip().to_string());
        return false;
    }
    true
}

/// TLS, then the WebSocket upgrade, then hand off to the session.
async fn handshake(stream: TcpStream, peer: SocketAddr, acceptor: TlsAcceptor, app: AppHandle) {
    // Nagle off: a terminal is latency-bound, not throughput-bound.
    let _ = stream.set_nodelay(true);

    let upgraded = tokio::time::timeout(UPGRADE_TIMEOUT, async {
        let tls = acceptor.accept(stream).await.ok()?;
        tokio_tungstenite::accept_hdr_async(tls, check_path)
            .await
            .ok()
    })
    .await;

    let Ok(Some(ws)) = upgraded else { return };
    session::run(ws, peer, app).await;
}

/// Refuse anything that is not our endpoint, with a real status code so a human
/// poking at the port gets an answer rather than a hang.
///
/// The wide `Err` variant is tungstenite's callback signature, not a choice.
#[allow(clippy::result_large_err)]
fn check_path(
    request: &Request,
    response: Response,
) -> std::result::Result<Response, ErrorResponse> {
    if request.uri().path() == WS_PATH {
        return Ok(response);
    }
    let mut refusal = ErrorResponse::new(Some("not found".to_string()));
    *refusal.status_mut() = StatusCode::NOT_FOUND;
    Err(refusal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_exactly_one_path() {
        let ok = Request::builder()
            .uri("wss://host:8765/remote")
            .body(())
            .expect("request");
        assert!(check_path(&ok, Response::new(())).is_ok());

        for path in ["/", "/remote/", "/Remote", "/../remote"] {
            let bad = Request::builder()
                .uri(format!("wss://host:8765{path}"))
                .body(())
                .expect("request");
            let refused = check_path(&bad, Response::new(()));
            assert!(refused.is_err(), "{path} should not be served");
            assert_eq!(
                refused.err().map(|r| r.status()),
                Some(StatusCode::NOT_FOUND)
            );
        }
    }

    #[test]
    fn builds_a_tls_acceptor_from_a_generated_identity() {
        let identity = test_identity("acceptor");
        assert!(tls_acceptor(&identity).is_ok());
    }

    // --- transport integration ------------------------------------------
    //
    // These run a real TLS listener over loopback and connect to it with a
    // client that pins the certificate exactly the way the Flutter app does
    // (`docs/remote-control-mobile.md` §6.1). The pin is the whole security
    // model — there is no CA — so "the right fingerprint is accepted and any
    // other one is refused" is the single most important thing to keep true.

    use std::sync::Arc as StdArc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn test_identity(tag: &str) -> StdArc<Identity> {
        let dir = std::env::temp_dir().join(format!(
            "splitview-server-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let identity = Identity::load_or_create(&dir).expect("identity");
        let _ = std::fs::remove_dir_all(&dir);
        identity
    }

    /// A client that trusts exactly one certificate, by SHA-256 — no CA, no
    /// hostname check, and no "accept anything" fallback.
    #[derive(Debug)]
    struct PinnedVerifier {
        expected: [u8; 32],
        provider: StdArc<rustls::crypto::CryptoProvider>,
    }

    impl rustls::client::danger::ServerCertVerifier for PinnedVerifier {
        fn verify_server_cert(
            &self,
            end_entity: &rustls::pki_types::CertificateDer<'_>,
            _intermediates: &[rustls::pki_types::CertificateDer<'_>],
            _server_name: &rustls::pki_types::ServerName<'_>,
            _ocsp: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error>
        {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(end_entity.as_ref());
            let actual: [u8; 32] = hasher.finalize().into();
            if actual == self.expected {
                Ok(rustls::client::danger::ServerCertVerified::assertion())
            } else {
                Err(rustls::Error::General("fingerprint mismatch".into()))
            }
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &rustls::pki_types::CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
        {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &self.provider.signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &rustls::pki_types::CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
        {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &self.provider.signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.provider
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    fn pinned_client(expected: [u8; 32]) -> tokio_rustls::TlsConnector {
        let provider = StdArc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(StdArc::clone(&provider))
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .dangerous()
            .with_custom_certificate_verifier(StdArc::new(PinnedVerifier { expected, provider }))
            .with_no_client_auth();
        tokio_rustls::TlsConnector::from(StdArc::new(config))
    }

    /// Bind a loopback listener that serves exactly one TLS+WebSocket
    /// connection, echoing whatever it is sent back. Returns its address.
    async fn one_shot_server(identity: StdArc<Identity>) -> SocketAddr {
        let acceptor = tls_acceptor(&identity).expect("acceptor");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let Ok(tls) = acceptor.accept(stream).await else {
                return;
            };
            let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tls, check_path).await else {
                return;
            };
            use futures_util::{SinkExt, StreamExt};
            while let Some(Ok(message)) = ws.next().await {
                if ws.send(message).await.is_err() {
                    return;
                }
            }
        });
        addr
    }

    fn server_name() -> rustls::pki_types::ServerName<'static> {
        rustls::pki_types::ServerName::try_from("localhost").expect("server name")
    }

    #[tokio::test]
    async fn a_client_pinning_our_fingerprint_connects_and_exchanges_frames() {
        use futures_util::{SinkExt, StreamExt};

        let identity = test_identity("pin-ok");
        let addr = one_shot_server(StdArc::clone(&identity)).await;

        let tcp = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let tls = pinned_client(identity.fingerprint)
            .connect(server_name(), tcp)
            .await
            .expect("pinned TLS handshake");
        let (mut ws, _) = tokio_tungstenite::client_async(
            format!("wss://localhost:{}{WS_PATH}", addr.port()),
            tls,
        )
        .await
        .expect("websocket upgrade");

        // A text frame (control messages) and a binary one (PTY bytes,
        // deliberately not valid UTF-8) both survive the round trip.
        ws.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"ping","t":1}"#.to_string(),
        ))
        .await
        .expect("send text");
        let echoed = ws.next().await.expect("reply").expect("reply");
        assert_eq!(
            echoed.into_text().expect("text"),
            r#"{"type":"ping","t":1}"#
        );

        let frame = super::super::protocol::encode_frame(
            super::super::protocol::OPCODE_INPUT,
            "pane-1",
            &[0xff, 0x00, 0x1b],
        )
        .expect("encode");
        ws.send(tokio_tungstenite::tungstenite::Message::Binary(
            frame.clone(),
        ))
        .await
        .expect("send binary");
        let echoed = ws.next().await.expect("reply").expect("reply");
        assert_eq!(echoed.into_data(), frame);
    }

    #[tokio::test]
    async fn a_client_pinning_the_wrong_fingerprint_is_refused() {
        let identity = test_identity("pin-bad");
        let addr = one_shot_server(StdArc::clone(&identity)).await;

        // One bit different: a substituted certificate, or the wrong laptop.
        let mut wrong = identity.fingerprint;
        wrong[0] ^= 0x01;

        let tcp = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let result = pinned_client(wrong).connect(server_name(), tcp).await;
        assert!(result.is_err(), "a mismatched pin must not connect");
    }

    #[tokio::test]
    async fn the_wrong_path_is_refused_over_a_real_connection() {
        let identity = test_identity("pin-path");
        let addr = one_shot_server(StdArc::clone(&identity)).await;

        let tcp = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let tls = pinned_client(identity.fingerprint)
            .connect(server_name(), tcp)
            .await
            .expect("pinned TLS handshake");
        let upgrade =
            tokio_tungstenite::client_async(format!("wss://localhost:{}/", addr.port()), tls).await;
        assert!(upgrade.is_err(), "only {WS_PATH} is served");
    }

    #[tokio::test]
    async fn plain_http_never_reaches_the_websocket_layer() {
        let identity = test_identity("plain");
        let addr = one_shot_server(identity).await;

        // No TLS at all: the server must not answer, it must fail the
        // handshake and close.
        let mut tcp = tokio::net::TcpStream::connect(addr).await.expect("connect");
        tcp.write_all(b"GET /remote HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .expect("write");
        let mut buf = [0u8; 64];
        let read = tokio::time::timeout(Duration::from_secs(5), tcp.read(&mut buf)).await;
        match read {
            // Closed without answering, or a TLS alert — never an HTTP reply.
            Ok(Ok(0)) | Err(_) => {}
            Ok(Ok(n)) => assert_ne!(&buf[..n.min(4)], b"HTTP", "answered plain HTTP"),
            Ok(Err(_)) => {}
        }
    }
}
