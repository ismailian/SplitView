//! The remote-control wire protocol — pure encode/decode, no I/O.
//!
//! `docs/remote-control-desktop.md` §6 is the normative spec and this module is
//! its implementation; the Flutter client re-implements the same shapes from the
//! same document. The two are kept honest by the golden fixtures in
//! `docs/remote/fixtures/`, which the tests at the bottom of this file assert
//! against and the mobile repo copies verbatim.
//!
//! Transport is WebSocket, and we use both of its frame types deliberately:
//!
//! - **binary frames** carry raw PTY bytes, verbatim,
//! - **text frames** carry JSON control messages.
//!
//! Terminal output is arbitrary bytes, not text. Round-tripping it through a
//! JSON string corrupts every non-UTF-8 sequence, which is why the byte path
//! never touches `serde_json` (CLAUDE.md §10, §20).

use bytes::Bytes;
use serde::{Deserialize, Serialize};

/// Protocol version. A client whose `proto` we do not support is rejected with
/// [`ErrorCode::ProtoMismatch`] before anything else happens.
pub const PROTO_VERSION: u32 = 1;

/// Binary frame opcode: PTY output, server → client.
pub const OPCODE_OUTPUT: u8 = 0x01;
/// Binary frame opcode: keystrokes, client → server.
pub const OPCODE_INPUT: u8 = 0x02;

/// A pane id is length-prefixed with a single byte, so it cannot exceed this.
/// Ids are `pane-<n>`, so the real limit is never approached.
pub const MAX_PANE_ID_LEN: usize = u8::MAX as usize;

/// Why a frame could not be decoded. Every variant closes the session with
/// [`ErrorCode::ProtoError`]; they are distinguished for tests and the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("frame is shorter than its header")]
    TooShort,
    #[error("frame is truncated: pane id runs past the end")]
    Truncated,
    #[error("unknown opcode")]
    UnknownOpcode,
    #[error("pane id is not ASCII")]
    NonAsciiPaneId,
    #[error("pane id is too long to length-prefix")]
    PaneIdTooLong,
}

/// A decoded binary frame: one chunk of PTY bytes, addressed to a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub opcode: u8,
    pub pane_id: String,
    pub data: Bytes,
}

/// Encode a binary frame.
///
/// ```text
/// offset  size  meaning
/// 0       1     opcode   0x01 = output (S->C), 0x02 = input (C->S)
/// 1       1     pane id length, in bytes
/// 2       n     pane id, ASCII
/// 2+n     ..    raw PTY bytes, verbatim
/// ```
///
/// No compression, no escaping, no re-encoding.
pub fn encode_frame(opcode: u8, pane_id: &str, data: &[u8]) -> Result<Vec<u8>, FrameError> {
    if !pane_id.is_ascii() {
        return Err(FrameError::NonAsciiPaneId);
    }
    if pane_id.len() > MAX_PANE_ID_LEN {
        return Err(FrameError::PaneIdTooLong);
    }
    let mut out = Vec::with_capacity(2 + pane_id.len() + data.len());
    out.push(opcode);
    out.push(pane_id.len() as u8);
    out.extend_from_slice(pane_id.as_bytes());
    out.extend_from_slice(data);
    Ok(out)
}

/// Decode a binary frame. A malformed frame closes the session — there is no
/// partial recovery, because a desynchronised byte stream is worse than a
/// reconnect.
pub fn decode_frame(bytes: &[u8]) -> Result<Frame, FrameError> {
    if bytes.len() < 2 {
        return Err(FrameError::TooShort);
    }
    let opcode = bytes[0];
    if opcode != OPCODE_OUTPUT && opcode != OPCODE_INPUT {
        return Err(FrameError::UnknownOpcode);
    }
    let id_len = bytes[1] as usize;
    let id_end = 2 + id_len;
    if bytes.len() < id_end {
        return Err(FrameError::Truncated);
    }
    let pane_id = std::str::from_utf8(&bytes[2..id_end])
        .map_err(|_| FrameError::NonAsciiPaneId)?
        .to_string();
    if !pane_id.is_ascii() {
        return Err(FrameError::NonAsciiPaneId);
    }
    Ok(Frame {
        opcode,
        pane_id,
        data: Bytes::copy_from_slice(&bytes[id_end..]),
    })
}

/// One pane as the phone sees it: a flat tab, never a node in a layout tree.
///
/// Assembled by the backend from what each window publishes (`publish_panes`)
/// plus the pane's own output counter, which only the backend knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterPane {
    pub id: String,
    /// Terminal title (OSC 0/2), if the running program set one.
    pub title: Option<String>,
    /// Working directory the shell reported over OSC 7.
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
    /// The pane's last non-empty output line, so an unwatched tab is still
    /// recognisable.
    pub preview: String,
    pub backgrounded: bool,
    /// Tauri label of the window that owns the pane. The phone may show it as a
    /// subtitle; it never acts on it.
    pub window: String,
    /// The pane's monotonic output-chunk counter. Comparing it between rosters
    /// is how a client shows an activity dot for a pane it is not streaming.
    pub seq: u64,
}

/// Why a pane disappeared, for the client's tab strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    /// The shell exited on its own.
    Exit,
    /// Someone closed the pane (on the desktop or from the phone).
    Closed,
    /// The window holding it went away.
    WindowClosed,
}

/// Terminal and non-terminal failures. All are terminal for the session except
/// [`Self::UnknownPane`], [`Self::NotPermitted`] and [`Self::NoWindow`], which
/// answer one request and leave the session open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    ProtoMismatch,
    Unauthorized,
    PairingExpired,
    PairingRejected,
    RateLimited,
    UnknownPane,
    NotPermitted,
    NoWindow,
    ProtoError,
    ServerDisabled,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProtoMismatch => "proto_mismatch",
            Self::Unauthorized => "unauthorized",
            Self::PairingExpired => "pairing_expired",
            Self::PairingRejected => "pairing_rejected",
            Self::RateLimited => "rate_limited",
            Self::UnknownPane => "unknown_pane",
            Self::NotPermitted => "not_permitted",
            Self::NoWindow => "no_window",
            Self::ProtoError => "proto_error",
            Self::ServerDisabled => "server_disabled",
        }
    }

    /// True when the session must be closed after sending this error.
    pub fn is_fatal(self) -> bool {
        !matches!(
            self,
            Self::UnknownPane | Self::NotPermitted | Self::NoWindow
        )
    }
}

/// Control messages the client sends. Unknown message types and unknown fields
/// are a protocol error rather than a silent no-op — a client that thinks it
/// asked for something must not be told nothing happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClientMessage {
    /// Reconnect with a stored device token.
    #[serde(rename_all = "camelCase")]
    Hello {
        proto: u32,
        device_id: String,
        token: String,
        /// Free-form client version string, for the log.
        #[serde(default)]
        client: Option<String>,
    },
    /// First-time pairing with the code from the QR.
    #[serde(rename_all = "camelCase")]
    Pair {
        proto: u32,
        code: String,
        device_name: String,
        platform: String,
    },
    /// Start streaming a pane: the server replies `snapshot`, then live frames.
    #[serde(rename_all = "camelCase")]
    Subscribe { pane_id: String },
    #[serde(rename_all = "camelCase")]
    Unsubscribe { pane_id: String },
    /// Re-request the screen after a `resync`, or on app resume.
    #[serde(rename_all = "camelCase")]
    RequestSnapshot { pane_id: String },
    /// Create a pane in the target window, optionally inheriting a directory.
    #[serde(rename_all = "camelCase")]
    CreatePane {
        #[serde(default)]
        cwd: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    ClosePane { pane_id: String },
    /// Rejected with `not_permitted` unless the desktop opted in (§10.1).
    #[serde(rename_all = "camelCase")]
    Resize {
        pane_id: String,
        cols: u16,
        rows: u16,
    },
    /// Liveness; the server replies `pong` with the same `t`.
    #[serde(rename_all = "camelCase")]
    Ping { t: i64 },
}

/// Control messages the server sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerMessage {
    #[serde(rename_all = "camelCase")]
    Welcome {
        proto: u32,
        server_version: String,
        host_name: String,
        /// Optional capabilities, e.g. `"resize"` when remote resize is allowed.
        features: Vec<String>,
    },
    /// Issued once, after the desktop user approves the pairing.
    #[serde(rename_all = "camelCase")]
    Paired { device_id: String, token: String },
    /// The full roster; sent on connect and on every change (debounced).
    #[serde(rename_all = "camelCase")]
    Panes { panes: Vec<RosterPane> },
    /// An `@xterm/addon-serialize` dump of the pane's current screen.
    #[serde(rename_all = "camelCase")]
    Snapshot {
        pane_id: String,
        serialized: String,
        cols: u16,
        rows: u16,
    },
    #[serde(rename_all = "camelCase")]
    PaneClosed {
        pane_id: String,
        reason: CloseReason,
    },
    /// The stream was cut (the client fell too far behind); re-request a
    /// snapshot rather than rendering a hole.
    #[serde(rename_all = "camelCase")]
    Resync { pane_id: String },
    #[serde(rename_all = "camelCase")]
    Error { code: String, message: String },
    #[serde(rename_all = "camelCase")]
    Pong { t: i64 },
}

impl ServerMessage {
    /// Build an `error` message from a code and a human-readable reason.
    pub fn error(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Error {
            code: code.as_str().to_string(),
            message: message.into(),
        }
    }

    /// Serialize for a WebSocket **text** frame. Serialization of these plain
    /// structs cannot fail in practice; a failure degrades to a protocol error
    /// rather than a panic (CLAUDE.md §11).
    pub fn to_text(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"type":"error","code":"proto_error","message":"encode failed"}"#.to_string()
        })
    }
}

/// Parse a WebSocket text frame into a client message.
pub fn decode_client_message(text: &str) -> Result<ClientMessage, serde_json::Error> {
    serde_json::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- binary frames ---------------------------------------------------

    #[test]
    fn round_trips_a_frame_with_non_utf8_payload() {
        // The whole reason terminal bytes never go through JSON.
        let data = [0xff, 0x00, 0x1b, 0x5b, 0x41, 0xfe, 0x80];
        let encoded = encode_frame(OPCODE_OUTPUT, "pane-7", &data).expect("encode");
        assert_eq!(&encoded[..2], &[OPCODE_OUTPUT, 6]);
        assert_eq!(&encoded[2..8], b"pane-7");

        let frame = decode_frame(&encoded).expect("decode");
        assert_eq!(frame.opcode, OPCODE_OUTPUT);
        assert_eq!(frame.pane_id, "pane-7");
        assert_eq!(frame.data.as_ref(), data.as_slice());
    }

    #[test]
    fn round_trips_an_empty_payload() {
        let encoded = encode_frame(OPCODE_INPUT, "pane-1", &[]).expect("encode");
        let frame = decode_frame(&encoded).expect("decode");
        assert_eq!(frame.opcode, OPCODE_INPUT);
        assert_eq!(frame.pane_id, "pane-1");
        assert!(frame.data.is_empty());
    }

    #[test]
    fn round_trips_a_maximum_length_pane_id() {
        let id = "p".repeat(MAX_PANE_ID_LEN);
        let encoded = encode_frame(OPCODE_OUTPUT, &id, b"x").expect("encode");
        assert_eq!(encoded[1], 255);
        let frame = decode_frame(&encoded).expect("decode");
        assert_eq!(frame.pane_id, id);
        assert_eq!(frame.data.as_ref(), b"x");
    }

    #[test]
    fn rejects_a_pane_id_too_long_to_length_prefix() {
        let id = "p".repeat(MAX_PANE_ID_LEN + 1);
        assert_eq!(
            encode_frame(OPCODE_OUTPUT, &id, b"").unwrap_err(),
            FrameError::PaneIdTooLong
        );
    }

    #[test]
    fn rejects_a_non_ascii_pane_id() {
        assert_eq!(
            encode_frame(OPCODE_OUTPUT, "pane-é", b"").unwrap_err(),
            FrameError::NonAsciiPaneId
        );
    }

    #[test]
    fn rejects_truncated_and_malformed_frames() {
        assert_eq!(decode_frame(&[]).unwrap_err(), FrameError::TooShort);
        assert_eq!(
            decode_frame(&[OPCODE_OUTPUT]).unwrap_err(),
            FrameError::TooShort
        );
        // Says the id is 9 bytes but only 4 follow.
        assert_eq!(
            decode_frame(&[OPCODE_OUTPUT, 9, b'p', b'a', b'n', b'e']).unwrap_err(),
            FrameError::Truncated
        );
        assert_eq!(
            decode_frame(&[0x7f, 0, b'x']).unwrap_err(),
            FrameError::UnknownOpcode
        );
    }

    #[test]
    fn a_zero_length_pane_id_decodes_as_empty_rather_than_panicking() {
        let frame = decode_frame(&[OPCODE_INPUT, 0, b'h', b'i']).expect("decode");
        assert_eq!(frame.pane_id, "");
        assert_eq!(frame.data.as_ref(), b"hi");
    }

    // --- control messages ------------------------------------------------

    #[test]
    fn client_messages_round_trip_through_json() {
        let cases = vec![
            ClientMessage::Hello {
                proto: PROTO_VERSION,
                device_id: "dev-1".into(),
                token: "t".into(),
                client: Some("splitview-remote/0.1".into()),
            },
            ClientMessage::Pair {
                proto: PROTO_VERSION,
                code: "A1B2C3D4".into(),
                device_name: "Pixel 8".into(),
                platform: "android".into(),
            },
            ClientMessage::Subscribe {
                pane_id: "pane-2".into(),
            },
            ClientMessage::CreatePane {
                cwd: Some("/home/me".into()),
            },
            ClientMessage::Resize {
                pane_id: "pane-2".into(),
                cols: 80,
                rows: 24,
            },
            ClientMessage::Ping { t: 1_726_000_000 },
        ];
        for case in cases {
            let json = serde_json::to_string(&case).expect("encode");
            assert_eq!(decode_client_message(&json).expect("decode"), case);
        }
    }

    #[test]
    fn client_message_field_names_are_camel_case_on_the_wire() {
        let json = serde_json::to_value(ClientMessage::Subscribe {
            pane_id: "pane-3".into(),
        })
        .expect("encode");
        assert_eq!(json["type"], "subscribe");
        assert_eq!(json["paneId"], "pane-3");
    }

    #[test]
    fn server_message_field_names_are_camel_case_on_the_wire() {
        let json = serde_json::to_value(ServerMessage::Snapshot {
            pane_id: "pane-3".into(),
            serialized: "hi".into(),
            cols: 100,
            rows: 30,
        })
        .expect("encode");
        assert_eq!(json["type"], "snapshot");
        assert_eq!(json["paneId"], "pane-3");
        assert_eq!(json["serialized"], "hi");
    }

    #[test]
    fn unknown_message_types_are_rejected() {
        assert!(decode_client_message(r#"{"type":"selectPane","paneId":"pane-1"}"#).is_err());
        assert!(decode_client_message("not json").is_err());
    }

    #[test]
    fn optional_fields_may_be_omitted_by_an_older_client() {
        let hello =
            decode_client_message(r#"{"type":"hello","proto":1,"deviceId":"d","token":"t"}"#)
                .expect("decode");
        assert_eq!(
            hello,
            ClientMessage::Hello {
                proto: 1,
                device_id: "d".into(),
                token: "t".into(),
                client: None,
            }
        );
        assert_eq!(
            decode_client_message(r#"{"type":"createPane"}"#).expect("decode"),
            ClientMessage::CreatePane { cwd: None }
        );
    }

    #[test]
    fn error_codes_know_whether_they_end_the_session() {
        assert!(ErrorCode::Unauthorized.is_fatal());
        assert!(ErrorCode::ProtoError.is_fatal());
        assert!(!ErrorCode::UnknownPane.is_fatal());
        assert!(!ErrorCode::NotPermitted.is_fatal());
        assert!(!ErrorCode::NoWindow.is_fatal());
    }

    // --- golden fixtures, shared verbatim with the Flutter client ---------

    fn fixtures(name: &str) -> serde_json::Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/remote/fixtures")
            .join(name);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
    }

    fn from_hex(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex digit"))
            .collect()
    }

    /// The binary fixtures pin the exact byte layout. If this test fails the
    /// phone client will see garbage, so the fixture is the thing to trust.
    #[test]
    fn binary_frame_fixtures_encode_and_decode_byte_for_byte() {
        let cases = fixtures("binary-frames.json");
        let cases = cases["frames"].as_array().expect("frames array");
        assert!(!cases.is_empty());
        for case in cases {
            let name = case["name"].as_str().expect("name");
            let opcode = case["opcode"].as_u64().expect("opcode") as u8;
            let pane_id = case["paneId"].as_str().expect("paneId");
            let data = from_hex(case["dataHex"].as_str().expect("dataHex"));
            let expected = from_hex(case["frameHex"].as_str().expect("frameHex"));

            let encoded = encode_frame(opcode, pane_id, &data)
                .unwrap_or_else(|e| panic!("{name}: encode failed: {e}"));
            assert_eq!(encoded, expected, "{name}: encoding drifted");

            let decoded =
                decode_frame(&expected).unwrap_or_else(|e| panic!("{name}: decode failed: {e}"));
            assert_eq!(decoded.opcode, opcode, "{name}: opcode");
            assert_eq!(decoded.pane_id, pane_id, "{name}: pane id");
            assert_eq!(decoded.data.as_ref(), data.as_slice(), "{name}: payload");
        }
    }

    #[test]
    fn client_message_fixtures_decode_and_re_encode_identically() {
        let cases = fixtures("client-messages.json");
        let cases = cases["messages"].as_array().expect("messages array");
        assert!(!cases.is_empty());
        for case in cases {
            let name = case["name"].as_str().expect("name");
            let json = &case["json"];
            let decoded: ClientMessage = serde_json::from_value(json.clone())
                .unwrap_or_else(|e| panic!("{name}: decode failed: {e}"));
            let re_encoded = serde_json::to_value(&decoded).expect("encode");
            assert_eq!(&re_encoded, json, "{name}: re-encoding drifted");
        }
    }

    #[test]
    fn server_message_fixtures_decode_and_re_encode_identically() {
        let cases = fixtures("server-messages.json");
        let cases = cases["messages"].as_array().expect("messages array");
        assert!(!cases.is_empty());
        for case in cases {
            let name = case["name"].as_str().expect("name");
            let json = &case["json"];
            let decoded: ServerMessage = serde_json::from_value(json.clone())
                .unwrap_or_else(|e| panic!("{name}: decode failed: {e}"));
            let re_encoded = serde_json::to_value(&decoded).expect("encode");
            assert_eq!(&re_encoded, json, "{name}: re-encoding drifted");
        }
    }
}
