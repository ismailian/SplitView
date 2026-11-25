//! Where SplitView keeps its files: `config.toml` (hand-edited) and
//! `settings.json` (written by the Settings window), both in the platform
//! config dir (`~/.config/splitview`, `%APPDATA%\splitview`, …).

use std::path::PathBuf;

use sv_core::config::{self, Config};

pub struct Paths {
    pub config: PathBuf,
    pub settings: PathBuf,
    /// One JSON file per project's scratchpad.
    pub scratch: PathBuf,
}

impl Paths {
    pub fn resolve() -> Self {
        let dir = std::env::var_os("SPLITVIEW_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::config_dir().map(|d| d.join("splitview")))
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            config: dir.join("config.toml"),
            settings: dir.join("settings.json"),
            scratch: dir.join("scratchpads"),
        }
    }
}

/// Load `config.toml`. A missing file is the defaults; a malformed one is the
/// defaults plus a message saying why.
pub fn load_config(path: &PathBuf) -> (Config, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(src) => match config::parse(&src) {
            Ok(c) => (c, None),
            Err(e) => (
                Config::default(),
                Some(format!("{} not loaded: {e}", path.display())),
            ),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Config::default(), None),
        Err(e) => (
            Config::default(),
            Some(format!("{} not loaded: {e}", path.display())),
        ),
    }
}
