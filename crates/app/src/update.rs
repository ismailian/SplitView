//! Update checker (Windows: the site only ships a Windows installer).
//!
//! At every launch a `sv-update` thread fetches the site's
//! `releases/latest.json` and tells the UI when a newer release is out. On "Download" the
//! installer is fetched to `%TEMP%\splitview-update`, checked against the
//! manifest's size and SHA-256, run, and deleted once it exits. The installer
//! may close SplitView before that; the next launch sweeps the folder instead.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use sv_core::update::Release;

use crate::hub::{Hub, UiEvent};
use crate::theme::VERSION;

const MANIFEST: &str = "https://splitview.pages.dev/releases/latest.json";

fn temp_dir() -> PathBuf {
    std::env::temp_dir().join("splitview-update")
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .user_agent(format!("SplitView/{VERSION}"))
        .build()
        .into()
}

/// Check once, at launch, on a worker thread; tell the UI if a newer release is out.
pub fn check(hub: Arc<Hub>) {
    let _ = std::thread::Builder::new()
        .name("sv-update".into())
        .spawn(move || {
            let _ = std::fs::remove_dir_all(temp_dir()); // leftovers of an earlier update
            // Offline or the site is down: say nothing, the next launch checks again.
            if let Ok(rel) = fetch()
                && rel.offer_over(VERSION)
            {
                hub.emit(UiEvent::UpdateAvailable(Box::new(rel)));
            }
        });
}

fn fetch() -> Result<Release, String> {
    let body = agent()
        .get(MANIFEST)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

/// Download, verify and run the installer, then delete it — on a worker thread.
pub fn install(hub: Arc<Hub>, rel: Release) {
    let _ = std::thread::Builder::new()
        .name("sv-update-dl".into())
        .spawn(move || {
            let path = temp_dir().join(format!("SplitView_{}_x64-setup.exe", rel.version));
            let result = download(&rel, &path).and_then(|()| {
                std::process::Command::new(&path)
                    .spawn()
                    .map_err(|e| format!("couldn't start the installer: {e}"))
            });
            match result {
                Ok(mut child) => {
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&path);
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&path);
                    hub.emit(UiEvent::UpdateFailed(e));
                }
            }
        });
}

fn download(rel: &Release, path: &Path) -> Result<(), String> {
    // offer_over vetted the URL, size and hash; re-check here, it's the trust boundary.
    if !rel.offer_over(VERSION) {
        return Err("the release manifest is invalid".into());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut resp = agent().get(&rel.url).call().map_err(|e| e.to_string())?;
    let mut reader = resp.body_mut().as_reader().take(rel.size + 1);
    let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut len = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        len += n as u64;
    }
    file.sync_all().map_err(|e| e.to_string())?;
    let digest: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if len != rel.size || !digest.eq_ignore_ascii_case(&rel.sha256) {
        return Err("the download didn't match its checksum".into());
    }
    Ok(())
}
