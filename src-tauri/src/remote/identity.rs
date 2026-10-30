//! The server's TLS identity: a self-signed certificate, generated once and
//! reused (`docs/remote-control-desktop.md` §7.1).
//!
//! There is no CA. The client pins the certificate's SHA-256, which it learns
//! **out of band** from the pairing QR code — the same shape Syncthing uses. That
//! avoids both the "accept any certificate" antipattern and the need for a real
//! PKI, at the cost of the fingerprint having to travel over a channel the
//! attacker does not control (the screen of the laptop you are standing in front
//! of).

use std::fs;
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// How long the generated certificate stays valid. Long, because rotating it
/// invalidates every paired device's pin; the private key never leaves the
/// machine and is not a CA.
const VALIDITY_YEARS: i32 = 10;

const CERT_FILE: &str = "cert.pem";
const KEY_FILE: &str = "key.pem";

/// A loaded (or freshly generated) server identity.
pub struct Identity {
    /// DER certificate chain — a single self-signed leaf.
    pub cert: CertificateDer<'static>,
    /// DER private key.
    pub key: PrivateKeyDer<'static>,
    /// SHA-256 of the DER certificate: what the client pins.
    pub fingerprint: [u8; 32],
}

impl Identity {
    /// Load the stored identity, generating and persisting one on first use.
    ///
    /// A stored pair that cannot be parsed is regenerated rather than refused —
    /// a corrupt file must not permanently break the feature — which invalidates
    /// existing pins, so the user re-pairs. That is the same outcome as a
    /// deliberate reset and is visible in Settings (the fingerprint changes).
    pub fn load_or_create(dir: &Path) -> Result<Arc<Self>> {
        if let Some(identity) = Self::load(dir)? {
            return Ok(Arc::new(identity));
        }
        let identity = Self::generate()?;
        identity.persist(dir)?;
        Ok(Arc::new(identity))
    }

    fn load(dir: &Path) -> Result<Option<Self>> {
        let (cert_pem, key_pem) = match (
            fs::read_to_string(dir.join(CERT_FILE)),
            fs::read_to_string(dir.join(KEY_FILE)),
        ) {
            (Ok(c), Ok(k)) => (c, k),
            _ => return Ok(None),
        };
        Ok(Self::from_pem(&cert_pem, &key_pem))
    }

    /// Parse a stored PEM pair. `None` (rather than an error) when the files are
    /// unusable, so the caller regenerates.
    fn from_pem(cert_pem: &str, key_pem: &str) -> Option<Self> {
        let cert = rustls_pemfile::certs(&mut cert_pem.as_bytes())
            .next()?
            .ok()?;
        let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
            .ok()
            .flatten()?;
        let fingerprint = fingerprint_of(&cert);
        Some(Self {
            cert,
            key,
            fingerprint,
        })
    }

    /// Generate a fresh P-256 self-signed certificate.
    fn generate() -> Result<Self> {
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, "SplitView Remote");
        params.subject_alt_names = subject_alt_names();
        let now = time::OffsetDateTime::now_utc();
        // Backdated a day so a client whose clock is slightly behind ours does
        // not see a not-yet-valid certificate.
        params.not_before = now - time::Duration::days(1);
        params.not_after = now + time::Duration::days(365 * VALIDITY_YEARS as i64);

        // P-256/SHA-256 is rcgen's default algorithm and what the plan specifies.
        let key_pair = KeyPair::generate().map_err(|e| Error::Remote(e.to_string()))?;
        let cert = params
            .self_signed(&key_pair)
            .map_err(|e| Error::Remote(e.to_string()))?;

        let cert_der = CertificateDer::from(cert.der().to_vec());
        let key_der = PrivateKeyDer::try_from(key_pair.serialize_der())
            .map_err(|e| Error::Remote(e.to_string()))?;
        let fingerprint = fingerprint_of(&cert_der);
        Ok(Self {
            cert: cert_der,
            key: key_der,
            fingerprint,
        })
    }

    /// Write the identity to disk, owner-readable only.
    fn persist(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        write_private(
            &dir.join(CERT_FILE),
            &pem("CERTIFICATE", self.cert.as_ref()),
        )?;
        write_private(
            &dir.join(KEY_FILE),
            &pem("PRIVATE KEY", private_key_bytes(&self.key)),
        )?;
        Ok(())
    }

    /// The fingerprint as the QR carries it: URL-safe base64, no padding.
    pub fn fingerprint_b64(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.fingerprint)
    }

    /// The fingerprint as Settings shows it: `ab:cd:ef:…`, uppercase.
    pub fn fingerprint_hex(&self) -> String {
        self.fingerprint
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":")
    }

    /// Clone the key (rustls wants an owned one per server config).
    pub fn key_clone(&self) -> PrivateKeyDer<'static> {
        self.key.clone_key()
    }
}

fn fingerprint_of(cert: &CertificateDer<'_>) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(cert.as_ref());
    hasher.finalize().into()
}

fn private_key_bytes<'a>(key: &'a PrivateKeyDer<'a>) -> &'a [u8] {
    match key {
        PrivateKeyDer::Pkcs1(k) => k.secret_pkcs1_der(),
        PrivateKeyDer::Sec1(k) => k.secret_sec1_der(),
        PrivateKeyDer::Pkcs8(k) => k.secret_pkcs8_der(),
        _ => &[],
    }
}

/// Minimal PEM encoder — the only thing standing between DER and a file we can
/// read back with `rustls-pemfile`.
fn pem(label: &str, der: &[u8]) -> String {
    let body = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for line in body.as_bytes().chunks(64) {
        out.push_str(&String::from_utf8_lossy(line));
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

/// Write a file only its owner can read. On Windows the file inherits the
/// user profile's ACL, which is already per-user.
fn write_private(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Names the certificate claims. The client pins the fingerprint and never
/// checks these, but a correct SAN list keeps well-behaved TLS stacks (and
/// anything we might point at the server for debugging) from complaining.
fn subject_alt_names() -> Vec<SanType> {
    let mut names = vec![
        SanType::DnsName("localhost".try_into().unwrap_or_else(|_| {
            // `localhost` is always a valid Ia5String; this arm is unreachable.
            "localhost".try_into().expect("literal dns name")
        })),
        SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
    ];
    if let Ok(name) = host_name().try_into() {
        names.push(SanType::DnsName(name));
    }
    if let Some(ip) = primary_lan_ip() {
        names.push(SanType::IpAddress(IpAddr::V4(ip)));
    }
    names
}

/// This machine's name, for the certificate, the mDNS instance and the
/// `welcome.hostName` the phone shows. Env first (set on every platform we
/// target), then `/etc/hostname`, then a constant — never a failure.
pub fn host_name() -> String {
    for var in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(name) = std::env::var(var) {
            let name = name.trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }
    if let Ok(name) = fs::read_to_string("/etc/hostname") {
        let name = name.trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }
    "splitview".to_string()
}

/// The address a LAN peer would reach us on, discovered by asking the routing
/// table which interface would be used for an off-link destination. No packet
/// is sent — a connected UDP socket only fixes the local endpoint.
pub fn primary_lan_ip() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?; // TEST-NET-1: routable, never answers
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() && !ip.is_loopback() => Some(ip),
        _ => None,
    }
}

/// Where the identity, device registry and audit log live.
///
/// Pure: the directory is only created when the feature is actually enabled, so
/// an app that never turns remote control on leaves nothing behind.
pub fn remote_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("splitview")
        .join("remote")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "splitview-identity-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn generates_persists_and_reloads_the_same_certificate() {
        let dir = temp_dir("roundtrip");
        let first = Identity::load_or_create(&dir).expect("generate");
        let second = Identity::load_or_create(&dir).expect("reload");

        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(first.cert, second.cert);
        assert!(dir.join(CERT_FILE).exists());
        assert!(dir.join(KEY_FILE).exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_stored_identity_is_regenerated_rather_than_fatal() {
        let dir = temp_dir("corrupt");
        let first = Identity::load_or_create(&dir).expect("generate");
        fs::write(dir.join(CERT_FILE), "not a certificate").expect("corrupt it");

        let second = Identity::load_or_create(&dir).expect("regenerate");
        assert_ne!(first.fingerprint, second.fingerprint);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fingerprint_renderings_match_the_digest() {
        let dir = temp_dir("fingerprint");
        let identity = Identity::load_or_create(&dir).expect("generate");

        let hex = identity.fingerprint_hex();
        assert_eq!(hex.len(), 32 * 3 - 1);
        assert!(hex.starts_with(&format!("{:02X}", identity.fingerprint[0])));

        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(identity.fingerprint_b64())
            .expect("base64url");
        assert_eq!(decoded, identity.fingerprint.to_vec());
        // The QR carries it unpadded and URL-safe, so it must survive a query
        // string untouched.
        assert!(!identity.fingerprint_b64().contains(['=', '+', '/']));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn host_name_is_never_empty() {
        assert!(!host_name().is_empty());
    }
}
