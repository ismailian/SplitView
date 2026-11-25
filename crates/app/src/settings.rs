//! Settings: everything the Settings window edits, persisted as JSON next to
//! `config.toml` (it used to live in the webview's localStorage). Missing or
//! malformed fields fall back to defaults field by field, so an older or
//! hand-edited file never loses the rest.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::remote::{BindMode, DEFAULT_PORT, RemoteConfig};
use crate::shell::{Alias, PromptInfo};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Display {
    pub font_size: f32,
    /// Terminal background / foreground / cursor, `#rrggbb`.
    pub bg: String,
    pub fg: String,
    pub cursor: String,
    /// Accent: typed commands, the ANSI green slot, focus marks.
    pub accent: String,
    /// Idle divider and the divider next to the focused pane.
    pub divider: String,
    pub divider_active: String,
    /// Divider thickness in px (1–4).
    pub divider_size: f32,
    /// A thin title strip on each pane (shell, cwd, size). The focused pane's
    /// strip carries the accent.
    pub pane_headers: bool,
    /// Dim panes that don't have focus. Off by default: it can hurt readability.
    pub dim_inactive: bool,
}

impl Default for Display {
    fn default() -> Self {
        let p = &PRESETS[0];
        Self {
            font_size: 14.0,
            bg: p.bg.into(),
            fg: p.fg.into(),
            cursor: p.cursor.into(),
            accent: p.accent.into(),
            divider: p.divider.into(),
            divider_active: p.divider_active.into(),
            divider_size: 1.0,
            pane_headers: true,
            dim_inactive: false,
        }
    }
}

pub struct Preset {
    pub name: &'static str,
    pub bg: &'static str,
    pub fg: &'static str,
    pub cursor: &'static str,
    pub accent: &'static str,
    pub divider: &'static str,
    pub divider_active: &'static str,
}

/// Ready-made palettes for the Display section.
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "Wire",
        bg: "#0c0f13",
        fg: "#c9d1d9",
        cursor: "#4fc3b4",
        accent: "#5cc27a",
        divider: "#232a33",
        divider_active: "#4fc3b4",
    },
    Preset {
        name: "Parrot",
        bg: "#0c1212",
        fg: "#cfd8d4",
        cursor: "#4ce675",
        accent: "#4ce675",
        divider: "#2a4640",
        divider_active: "#4a8473",
    },
    Preset {
        name: "Midnight",
        bg: "#0b0f1a",
        fg: "#c7d0e0",
        cursor: "#7aa2f7",
        accent: "#7aa2f7",
        divider: "#222a3d",
        divider_active: "#3b4a6b",
    },
    Preset {
        name: "Carbon",
        bg: "#101010",
        fg: "#d6d6d6",
        cursor: "#e6b450",
        accent: "#e6b450",
        divider: "#2a2a2a",
        divider_active: "#4a4a4a",
    },
    Preset {
        name: "Matrix",
        bg: "#000800",
        fg: "#9bf59b",
        cursor: "#39ff14",
        accent: "#39ff14",
        divider: "#0f3d0f",
        divider_active: "#1f7a1f",
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PromptInfoS {
    pub user: bool,
    pub host: bool,
    pub dir: bool,
    pub branch: bool,
    pub time: bool,
    pub exit_status: bool,
    /// 0 = the whole path (middle elided when long), n = the last n segments.
    pub dir_segments: u8,
}

impl Default for PromptInfoS {
    fn default() -> Self {
        let p = PromptInfo::default();
        Self {
            user: p.user,
            host: p.host,
            dir: p.dir,
            branch: p.branch,
            time: p.time,
            exit_status: p.exit_status,
            dir_segments: p.dir_segments,
        }
    }
}

pub const MAX_DIR_SEGMENTS: u8 = 9;

impl From<PromptInfoS> for PromptInfo {
    fn from(p: PromptInfoS) -> Self {
        PromptInfo {
            user: p.user,
            host: p.host,
            dir: p.dir,
            branch: p.branch,
            time: p.time,
            exit_status: p.exit_status,
            dir_segments: p.dir_segments.min(MAX_DIR_SEGMENTS),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AliasS {
    pub name: String,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RemoteSettings {
    /// Re-enable the server at launch. Off by default and deliberately opt-in.
    pub enable_at_startup: bool,
    pub bind: BindMode,
    pub port: u16,
    pub allow_resize: bool,
}

impl Default for RemoteSettings {
    fn default() -> Self {
        Self {
            enable_at_startup: false,
            bind: BindMode::Lan,
            port: DEFAULT_PORT,
            allow_resize: true,
        }
    }
}

impl RemoteSettings {
    pub fn config(&self) -> RemoteConfig {
        RemoteConfig {
            bind: self.bind,
            port: clamp_port(self.port),
            allow_resize: self.allow_resize,
        }
    }
}

/// Ports below 1024 need privileges we neither have nor want; 0 = any free port.
pub fn clamp_port(p: u16) -> u16 {
    if p != 0 && p < 1024 { DEFAULT_PORT } else { p }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ShellSettings {
    /// A `ShellProfile.id`; `None` until chosen, which is what makes the first
    /// launch ask.
    pub default_profile: Option<String>,
}

/// Bits of UI state that persist (widths, toggles).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiState {
    pub sidebar_width: f32,
    pub editor_width: f32,
    pub scratch_width: f32,
    pub show_hidden: bool,
    /// Wrap long lines in the editor.
    pub editor_wrap: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            sidebar_width: 260.0,
            editor_width: 560.0,
            scratch_width: 420.0,
            show_hidden: false,
            editor_wrap: false,
        }
    }
}

/// Which side panels a new window opens with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct PaneDefaults {
    pub explorer: bool,
    pub editor: bool,
    pub scratchpad: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub display: Display,
    pub prompt_info: PromptInfoS,
    pub aliases: Vec<AliasS>,
    pub banner: String,
    pub remote: RemoteSettings,
    pub shell: ShellSettings,
    pub ui: UiState,
    pub panes: PaneDefaults,
}

impl Settings {
    pub fn aliases(&self) -> Vec<Alias> {
        self.aliases
            .iter()
            .filter(|a| !a.name.trim().is_empty())
            .map(|a| Alias {
                name: a.name.trim().to_owned(),
                command: a.command.clone(),
            })
            .collect()
    }

    /// Load, falling back to defaults (a missing or corrupt file is not fatal).
    pub fn load(path: &PathBuf) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
            .map(Self::sanitized)
            .unwrap_or_default()
    }

    fn sanitized(mut self) -> Self {
        let d = &mut self.display;
        d.font_size = d
            .font_size
            .clamp(sv_core::config::FONT_MIN, sv_core::config::FONT_MAX);
        d.divider_size = d.divider_size.clamp(1.0, 4.0);
        self.prompt_info.dir_segments = self.prompt_info.dir_segments.min(MAX_DIR_SEGMENTS);
        self.remote.port = clamp_port(self.remote.port);
        self.ui.sidebar_width = self.ui.sidebar_width.clamp(140.0, 900.0);
        self.ui.editor_width = self.ui.editor_width.clamp(260.0, 1600.0);
        self.ui.scratch_width = self.ui.scratch_width.clamp(260.0, 1600.0);
        self
    }

    /// Atomic save (write a temp file, then rename). Failures are reported, not
    /// fatal.
    pub fn save(&self, path: &PathBuf) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }
}

/// `#rrggbb` → color (None if malformed).
pub fn parse_hex(s: &str) -> Option<egui::Color32> {
    let h = s.trim().strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(egui::Color32::from_rgb(
        (v >> 16) as u8,
        (v >> 8) as u8,
        v as u8,
    ))
}

pub fn to_hex(c: egui::Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_and_corrupt_files_fall_back_per_field() {
        let s: Settings =
            serde_json::from_str(r##"{"display":{"bg":"#000000"},"banner":"hi"}"##).unwrap();
        assert_eq!(s.display.bg, "#000000");
        assert_eq!(s.display.fg, Display::default().fg);
        assert_eq!(s.banner, "hi");
        assert!(!s.remote.enable_at_startup);
        let dir = std::env::temp_dir().join(format!("sv-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        let mut s = Settings::default();
        s.aliases.push(AliasS {
            name: "ll".into(),
            command: "ls -l".into(),
        });
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn clamps_on_load() {
        let s: Settings = serde_json::from_str(
            r#"{"display":{"fontSize":500,"dividerSize":0},"remote":{"port":80},"promptInfo":{"dirSegments":50}}"#,
        )
        .unwrap();
        let s = s.sanitized();
        assert_eq!(s.display.font_size, sv_core::config::FONT_MAX);
        assert_eq!(s.display.divider_size, 1.0);
        assert_eq!(s.remote.port, DEFAULT_PORT);
        assert_eq!(s.prompt_info.dir_segments, MAX_DIR_SEGMENTS);
    }

    #[test]
    fn hex_colors() {
        assert_eq!(
            parse_hex("#4fc3b4"),
            Some(egui::Color32::from_rgb(0x4f, 0xc3, 0xb4))
        );
        assert_eq!(parse_hex("4fc3b4"), None);
        assert_eq!(parse_hex("#zzzzzz"), None);
        assert_eq!(to_hex(egui::Color32::from_rgb(1, 2, 255)), "#0102ff");
    }
}
