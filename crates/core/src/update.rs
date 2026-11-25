//! The update manifest the site publishes at `releases/latest.json`, and the
//! checks the app applies before offering (or downloading) a release.

use serde::Deserialize;

/// Installers may only come from this repository's GitHub releases, so a
/// tampered manifest can't point the app at an arbitrary binary.
pub const DOWNLOAD_PREFIX: &str = "https://github.com/ismailian/SplitView/releases/download/";
/// Refuse anything absurd before downloading it.
pub const MAX_SIZE: u64 = 200 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Release {
    pub version: String,
    pub url: String,
    /// Bytes.
    pub size: u64,
    /// Lowercase hex SHA-256 of the installer.
    pub sha256: String,
    #[serde(default)]
    pub changelog: String,
}

impl Release {
    /// A release worth offering over `current`: newer, from our GitHub
    /// releases, with a plausible size and checksum.
    pub fn offer_over(&self, current: &str) -> bool {
        is_newer(&self.version, current)
            && self.url.starts_with(DOWNLOAD_PREFIX)
            && !self.url.contains("..")
            && self.size > 0
            && self.size <= MAX_SIZE
            && self.sha256.len() == 64
            && self.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    }
}

/// `x.y.z` (any number of parts, optional `v`), digits only — so a version is
/// also safe to put in a file name.
fn parse(v: &str) -> Option<Vec<u64>> {
    let v = v.strip_prefix('v').unwrap_or(v);
    v.split('.')
        .map(|p| {
            (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                .then(|| p.parse().ok())
                .flatten()
        })
        .collect()
}

/// True when `candidate` is a valid version strictly above `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse(candidate), parse(current)) {
        (Some(mut a), Some(mut b)) => {
            let n = a.len().max(b.len());
            a.resize(n, 0);
            b.resize(n, 0);
            a > b
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_order() {
        assert!(is_newer("0.4.1", "0.4.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("v1.0", "0.99.99"));
        assert!(!is_newer("0.4.0", "0.4.0"));
        assert!(!is_newer("0.4", "0.4.0"));
        assert!(!is_newer("0.3.9", "0.4.0"));
        assert!(!is_newer("0.5.0-beta", "0.4.0"));
        assert!(!is_newer("0.5.0/../x", "0.4.0"));
        assert!(!is_newer("", "0.4.0"));
        assert!(!is_newer("1..0", "0.4.0"));
    }

    fn rel(url: &str) -> Release {
        Release {
            version: "0.5.0".into(),
            url: url.into(),
            size: 6_000_000,
            sha256: "c4070184bf556e9e0d24c11b6c7aa1360b012e253b8e8c2be7b57670d0959142".into(),
            changelog: String::new(),
        }
    }

    #[test]
    fn offer_checks() {
        let ok = "https://github.com/ismailian/SplitView/releases/download/v0.5.0/SplitView_0.5.0_x64-setup.exe";
        assert!(rel(ok).offer_over("0.4.0"));
        assert!(!rel(ok).offer_over("0.5.0"));
        assert!(!rel("https://evil.example/setup.exe").offer_over("0.4.0"));
        assert!(
            !rel("http://github.com/ismailian/SplitView/releases/download/x.exe")
                .offer_over("0.4.0")
        );
        let mut r = rel(ok);
        r.sha256 = "nothex".into();
        assert!(!r.offer_over("0.4.0"));
        let mut r = rel(ok);
        r.size = MAX_SIZE + 1;
        assert!(!r.offer_over("0.4.0"));
    }

    #[test]
    fn parses_manifest() {
        let r: Release = serde_json::from_str(
            r#"{"version":"0.5.0","url":"u","size":1,"sha256":"ab","changelog":"- x"}"#,
        )
        .unwrap();
        assert_eq!(r.changelog, "- x");
    }
}
