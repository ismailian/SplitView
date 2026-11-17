//! Device registry: issuing, verifying and revoking device tokens
//! (`docs/remote-control-desktop.md` §7.2).
//!
//! A token is 256 bits of CSPRNG output, so it is stored as a bare SHA-256 with
//! no KDF — there is nothing to brute-force and no password to stretch. The
//! comparison is constant-time, because a timing oracle on a token check is a
//! token check that does not work.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::Zeroize;

use crate::error::{Error, Result};

const DEVICES_FILE: &str = "devices.json";

/// A paired device, as stored on disk and shown in Settings. The token itself is
/// never stored — only its digest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    /// What the phone called itself when pairing, e.g. "Ismail's iPhone".
    pub name: String,
    pub platform: String,
    /// Hex SHA-256 of the issued token.
    pub token_sha256: String,
    /// Unix seconds.
    pub paired_at: u64,
    pub last_seen: u64,
}

/// The set of paired devices, persisted next to the TLS identity.
#[derive(Debug, Default)]
pub struct DeviceRegistry {
    devices: HashMap<String, Device>,
    path: Option<PathBuf>,
}

impl DeviceRegistry {
    /// Load the registry from `dir`, starting empty if there is nothing (or
    /// nothing readable) there. A corrupt file must not lock the user out of
    /// their own Settings, so it is replaced on the next write.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(DEVICES_FILE);
        let devices = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Vec<Device>>(&text).ok())
            .map(|list| list.into_iter().map(|d| (d.id.clone(), d)).collect())
            .unwrap_or_default();
        Self {
            devices,
            path: Some(path),
        }
    }

    /// Mint a token for a newly approved device and return `(device, token)`.
    /// The plaintext token is returned exactly once — it goes straight into the
    /// `paired` message and is never stored, logged, or recoverable.
    pub fn issue(&mut self, name: &str, platform: &str) -> Result<(Device, String)> {
        let mut raw = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut raw);
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        raw.zeroize();

        let mut id_bytes = [0u8; 8];
        rand::thread_rng().fill_bytes(&mut id_bytes);
        let device = Device {
            id: format!("dev-{}", hex(&id_bytes)),
            name: sanitize_name(name),
            platform: sanitize_name(platform),
            token_sha256: hex(&digest(&token)),
            paired_at: now_secs(),
            last_seen: now_secs(),
        };
        self.devices.insert(device.id.clone(), device.clone());
        self.persist()?;
        Ok((device, token))
    }

    /// Constant-time token check. Returns the device on success so the caller
    /// can name it in the audit log.
    ///
    /// An unknown device id still performs the comparison (against a zero
    /// digest) so the answer takes the same shape either way.
    pub fn verify(&self, device_id: &str, token: &str) -> Option<Device> {
        let presented = digest(token);
        let stored = self
            .devices
            .get(device_id)
            .and_then(|d| unhex32(&d.token_sha256))
            .unwrap_or([0u8; 32]);
        let matches: bool = presented.ct_eq(&stored).into();
        // A device with no parseable digest can never authenticate, even if the
        // presented token happened to hash to zero.
        if matches && self.devices.contains_key(device_id) {
            self.devices.get(device_id).cloned()
        } else {
            None
        }
    }

    /// Record that a device just connected (shown as "last seen" in Settings).
    pub fn touch(&mut self, device_id: &str) {
        if let Some(device) = self.devices.get_mut(device_id) {
            device.last_seen = now_secs();
            let _ = self.persist();
        }
    }

    /// Forget a device. The caller is responsible for also terminating any live
    /// session using it — a revoked token that stays connected is not revoked.
    pub fn revoke(&mut self, device_id: &str) -> Result<bool> {
        let removed = self.devices.remove(device_id).is_some();
        if removed {
            self.persist()?;
        }
        Ok(removed)
    }

    /// Devices for the Settings list, newest pairing first.
    pub fn list(&self) -> Vec<Device> {
        let mut list: Vec<Device> = self.devices.values().cloned().collect();
        list.sort_by_key(|d| std::cmp::Reverse(d.paired_at));
        list
    }

    fn persist(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(()); // in-memory registry (tests)
        };
        let list = self.list();
        let json = serde_json::to_string_pretty(&list).map_err(|e| Error::Remote(e.to_string()))?;
        std::fs::write(path, json)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

fn digest(token: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hasher.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex32(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// A device name is displayed in a desktop prompt and written to the audit log,
/// so it is trimmed, length-capped, and stripped of control characters — a
/// client-supplied string never gets to draw on the screen.
fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect::<String>()
        .trim()
        .to_string();
    if cleaned.is_empty() {
        "Unnamed device".to_string()
    } else {
        cleaned
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issues_a_token_that_verifies_once_and_only_for_its_device() {
        let mut registry = DeviceRegistry::default();
        let (device, token) = registry.issue("Pixel 8", "android").expect("issue");

        assert!(registry.verify(&device.id, &token).is_some());
        assert!(registry.verify(&device.id, "wrong").is_none());
        assert!(registry.verify("dev-nobody", &token).is_none());
    }

    #[test]
    fn tokens_are_stored_only_as_digests() {
        let mut registry = DeviceRegistry::default();
        let (device, token) = registry.issue("iPhone", "ios").expect("issue");
        let stored = registry.list();
        assert_eq!(stored.len(), 1);
        assert_ne!(stored[0].token_sha256, token);
        assert_eq!(stored[0].token_sha256, hex(&digest(&token)));
        assert_eq!(stored[0].id, device.id);
    }

    #[test]
    fn two_devices_get_distinct_ids_and_tokens() {
        let mut registry = DeviceRegistry::default();
        let (a, token_a) = registry.issue("A", "ios").expect("issue");
        let (b, token_b) = registry.issue("B", "android").expect("issue");
        assert_ne!(a.id, b.id);
        assert_ne!(token_a, token_b);
        // A token must not be accepted for the other device.
        assert!(registry.verify(&a.id, &token_b).is_none());
        assert!(registry.verify(&b.id, &token_a).is_none());
    }

    #[test]
    fn a_revoked_device_stops_verifying() {
        let mut registry = DeviceRegistry::default();
        let (device, token) = registry.issue("Pixel", "android").expect("issue");
        assert!(registry.revoke(&device.id).expect("revoke"));
        assert!(registry.verify(&device.id, &token).is_none());
        assert!(!registry.revoke(&device.id).expect("revoke again"));
        assert!(registry.list().is_empty());
    }

    #[test]
    fn a_device_whose_digest_is_unparseable_can_never_authenticate() {
        let mut registry = DeviceRegistry::default();
        let (device, _) = registry.issue("Pixel", "android").expect("issue");
        // Simulate a hand-edited / corrupted registry file.
        if let Some(entry) = registry.devices.get_mut(&device.id) {
            entry.token_sha256 = "not-hex".to_string();
        }
        assert!(registry.verify(&device.id, "").is_none());
        assert!(registry.verify(&device.id, "anything").is_none());
    }

    #[test]
    fn device_names_are_sanitized_before_they_reach_a_prompt() {
        let mut registry = DeviceRegistry::default();
        let (device, _) = registry
            .issue("  Ismail's\u{7} iPhone\n", "ios")
            .expect("issue");
        assert_eq!(device.name, "Ismail's iPhone");
        let (blank, _) = registry.issue("   ", "").expect("issue");
        assert_eq!(blank.name, "Unnamed device");
        assert_eq!(blank.platform, "Unnamed device");
    }

    #[test]
    fn persists_and_reloads_across_restarts() {
        let dir = std::env::temp_dir().join(format!("splitview-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");

        let (device, token) = {
            let mut registry = DeviceRegistry::load(&dir);
            registry.issue("Pixel 8", "android").expect("issue")
        };

        let reloaded = DeviceRegistry::load(&dir);
        assert!(reloaded.verify(&device.id, &token).is_some());
        assert_eq!(reloaded.list()[0].name, "Pixel 8");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
