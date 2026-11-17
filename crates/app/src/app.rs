//! The eframe app: owns the panes, the windows (workspaces), settings and the
//! keymap; drains backend events; dispatches actions. Rendering lives in
//! `panes.rs` (split area), `chrome.rs` (top/status bars), `dialogs.rs`
//! (modals) and `settings_ui.rs`; the sidebar and editor are self-contained.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use egui::ViewportId;
use parking_lot::Mutex;
use sv_core::config::{Config, FONT_MAX, FONT_MIN, PromptStyle};
use sv_core::keymap::{Keymap, KeymapConfig};
use sv_core::layout::{Direction, Layout, PaneId};
use sv_core::quote::{ShellFamily, cd_command_for, quote_path_for};
use sv_term::{Size, Terminal, TerminalView};

use crate::dialogs::{Modal, PickerMode};
use crate::editor::Editor;
use crate::hub::{Hub, UiEvent};
use crate::profiles::{self, ShellProfile};
use crate::remote::{PairingRequest, RemoteStatus, RemoteUi};
use crate::settings::Settings;
use crate::shell::{self, ShellConfig};
use crate::sidebar::Sidebar;
use crate::theme::{self, Skin};

/// One terminal pane: an emulator + its widget state + what the shell told us.
pub struct Pane {
    pub id: PaneId,
    pub term: Terminal,
    pub view: TerminalView,
    pub title: Arc<Mutex<Option<String>>>,
    pub cwd: Option<String>,
    pub family: ShellFamily,
    /// What runs in it, for headers (`bash`, `PowerShell`).
    pub shell: String,
    /// Set for an msys shell (Git Bash): maps /tmp, /usr ... to real paths.
    pub msys_root: Option<String>,
}

impl Pane {
    /// What the program in the pane calls itself (OSC 0/2), or the shell's
    /// name. ConPTY titles a console with its executable path
    /// (`C:\Program Files\Git\bin\bash.exe`); that says less than "Git Bash".
    pub fn title(&self) -> String {
        match self.title.lock().as_deref() {
            Some(t) if !is_program_path(t) => t.to_owned(),
            _ => self.shell.clone(),
        }
    }
}

fn is_program_path(t: &str) -> bool {
    t.to_ascii_lowercase().ends_with(".exe")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Terminal,
    Sidebar,
    Editor,
}

/// One window: a layout of panes with its own sidebar and editor.
pub struct Workspace {
    pub key: u64,
    /// Remote roster label (`main`, `sv-win-2`, `detached-3`).
    pub label: String,
    pub viewport: ViewportId,
    /// Set for a popped-out single pane: the window it goes back to.
    pub detached_from: Option<u64>,
    pub layout: Layout,
    /// Panes running outside the layout (`<prefix> b`), oldest first.
    pub background: Vec<(PaneId, Instant)>,
    pub focus: Focus,
    pub sidebar: Sidebar,
    pub editor: Editor,
    /// Monotonic, bumped when the window gains focus (remote target window).
    pub focused_at: f64,
    /// Close this (secondary) window at the end of the frame.
    pub closing: bool,
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

pub struct App {
    pub hub: Arc<Hub>,
    egui_ctx: egui::Context,
    events: Receiver<UiEvent>,
    pub panes: HashMap<PaneId, Pane>,
    pub windows: Vec<Workspace>,
    next_window: u64,
    pub settings: Settings,
    settings_path: PathBuf,
    pub config: Config,
    config_path: PathBuf,
    pub keymap: Keymap,
    pub skin: Skin,
    /// Current terminal font size (zoom), starts at the configured size.
    pub font_size: f32,
    pub profiles: Vec<ShellProfile>,
    /// The open modal and the window it belongs to.
    pub modal: Option<(u64, Modal)>,
    /// Pairing requests waiting for an answer (outrank every other modal).
    pub pairing: Vec<PairingRequest>,
    pub remote_status: RemoteStatus,
    pub toast: Option<Toast>,
    /// No shell could be started at all: say so instead of an empty window.
    pub startup_error: Option<String>,
    roster_dirty: bool,
    last_publish: Instant,
    focus_clock: f64,
    /// `--shot`: a scripted scene, then a screenshot and exit.
    pub shot: Option<crate::shot::Shot>,
    /// The root window asked to close and is waiting on a confirmation.
    quit_confirmed: bool,
    /// `SPLITVIEW_FRAME_STATS=<file>`: frames per 5 s, with what asked for them.
    frame_stats: Option<(PathBuf, Instant, u32)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, shot: Option<crate::shot::Shot>) -> Self {
        let paths = crate::store::Paths::resolve();
        let (config, config_error) = crate::store::load_config(&paths.config);
        let settings = Settings::load(&paths.settings);
        let font_warning = theme::install(&cc.egui_ctx, config.font_file.as_deref());
        let (tx, events) = mpsc::channel();
        let hub = Hub::new(cc.egui_ctx.clone(), tx);
        let skin = theme::skin(&settings.display);
        let font_size = if (config.font_size - 14.0).abs() > f32::EPSILON {
            config.font_size
        } else {
            settings.display.font_size
        };
        let mut app = Self {
            remote_status: hub.remote.status(),
            egui_ctx: cc.egui_ctx.clone(),
            hub,
            events,
            panes: HashMap::new(),
            windows: Vec::new(),
            next_window: 1,
            keymap: Keymap::new(keymap_config(&config)),
            skin,
            font_size,
            profiles: profiles::detect(),
            modal: None,
            pairing: Vec::new(),
            toast: None,
            startup_error: None,
            roster_dirty: true,
            last_publish: Instant::now(),
            focus_clock: 1.0,
            shot,
            quit_confirmed: false,
            frame_stats: std::env::var_os("SPLITVIEW_FRAME_STATS")
                .map(|p| (p.into(), Instant::now(), 0)),
            settings,
            settings_path: paths.settings,
            config,
            config_path: paths.config,
        };
        let main = app.new_workspace("main".into(), ViewportId::ROOT, None);
        app.windows.push(main);
        if let Some(e) = config_error.or(font_warning) {
            app.toast(e, true);
        }
        app.first_pane(None);
        if app.settings.remote.enable_at_startup {
            let cfg = app.settings.remote.config();
            if let Err(e) = app.hub.remote.enable(&app.hub, cfg) {
                app.toast(format!("Remote control did not start: {e}"), true);
            }
        }
        app
    }

    fn new_workspace(
        &mut self,
        label: String,
        viewport: ViewportId,
        from: Option<u64>,
    ) -> Workspace {
        let key = self.next_window;
        self.next_window += 1;
        Workspace {
            key,
            label,
            viewport,
            detached_from: from,
            layout: Layout::default(),
            background: Vec::new(),
            focus: Focus::Terminal,
            sidebar: Sidebar::new(
                key,
                self.settings.ui.sidebar_width,
                self.settings.ui.show_hidden,
            ),
            editor: Editor::new(self.settings.ui.editor_width, &self.hub),
            focused_at: 0.0,
            closing: false,
        }
    }

    pub fn win(&self, key: u64) -> Option<usize> {
        self.windows.iter().position(|w| w.key == key)
    }

    pub fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some(Toast {
            text: text.into(),
            error,
            at: Instant::now(),
        });
    }

    // ---- shells & panes ------------------------------------------------

    /// The program new panes run: the config's `shell`, else the saved default
    /// profile, else `None` (the backend picks the best installed shell).
    pub fn default_program(&self) -> Option<String> {
        self.config
            .shell
            .clone()
            .or_else(|| self.default_profile().map(|p| p.program.clone()))
    }

    pub fn default_profile(&self) -> Option<&ShellProfile> {
        let id = self.settings.shell.default_profile.as_deref()?;
        self.profiles.iter().find(|p| p.id == id)
    }

    fn shell_label(&self, program: &str) -> String {
        self.profiles
            .iter()
            .find(|p| p.program == program)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| profiles::base_name(program))
    }

    /// Start a window's first shell, asking which one when that is not settled.
    /// On a fresh install no default exists, so the picker opens before anything
    /// is spawned; it opens again — with the reason — if the shell fails.
    pub fn first_pane(&mut self, message: Option<String>) {
        let Some(key) = self.windows.first().map(|w| w.key) else {
            return;
        };
        let ask = message.is_some();
        if self.config.shell.is_none() || ask {
            let resolved = if ask {
                None
            } else {
                self.default_profile().cloned()
            };
            if resolved.is_none() {
                if self.profiles.len() == 1 && !ask {
                    let p = self.profiles[0].clone();
                    self.set_default_profile(&p.id);
                } else if !self.profiles.is_empty() {
                    self.modal = Some((
                        key,
                        Modal::picker(
                            PickerMode::FirstRun,
                            message,
                            self.settings.shell.default_profile.clone(),
                        ),
                    ));
                    return;
                }
            }
        }
        if let Err(e) = self.split_into(0, Direction::Right, None, None, true) {
            let name = self.default_program().unwrap_or_else(|| "the shell".into());
            let msg = format!("Couldn't start {name}: {e}");
            if self.profiles.len() >= 2 {
                self.first_pane(Some(msg));
            } else {
                self.startup_error = Some(msg);
            }
        }
    }

    pub fn set_default_profile(&mut self, id: &str) {
        self.settings.shell.default_profile = Some(id.to_owned());
        self.save_settings();
    }

    pub fn shell_config(&self, cwd: Option<String>, program: Option<String>) -> ShellConfig {
        ShellConfig {
            program: program.or_else(|| self.default_program()),
            cwd,
            prompt: self.config.prompt.clone(),
            style: match self.config.prompt_style {
                PromptStyle::Plain => shell::PromptStyle::Plain,
                PromptStyle::Parrot => shell::PromptStyle::Parrot,
            },
            aliases: self.settings.aliases(),
            prompt_info: self.settings.prompt_info.into(),
            banner: self.settings.banner.clone(),
        }
    }

    /// Spawn a pane (not yet placed in any layout).
    pub fn spawn_pane(
        &mut self,
        cwd: Option<String>,
        program: Option<String>,
        size: Size,
    ) -> Result<PaneId, String> {
        let cfg = self.shell_config(cwd.clone(), program);
        let id = self.hub.alloc_id();
        let s = self.hub.spawn(id, size, &cfg).map_err(|e| e.to_string())?;
        let shell = self.shell_label(&s.program);
        self.panes.insert(
            id,
            Pane {
                id,
                term: s.term,
                view: TerminalView::new(id, theme::term_font(self.font_size)),
                title: s.title,
                cwd: cwd.map(|c| sv_core::paths::to_native(&c)),
                family: s.family,
                msys_root: s.msys_root,
                shell,
            },
        );
        self.roster_dirty = true;
        Ok(id)
    }

    /// Split the focused pane of window `wi`, inheriting its cwd unless told
    /// otherwise. `focus` = focus follows to the new pane.
    pub fn split_into(
        &mut self,
        wi: usize,
        dir: Direction,
        cwd: Option<String>,
        program: Option<String>,
        focus: bool,
    ) -> Result<PaneId, String> {
        let w = &self.windows[wi];
        let from = w.layout.focused.and_then(|f| self.panes.get(&f));
        let cwd = cwd.or_else(|| from.and_then(|p| p.cwd.clone()));
        let size = from.map_or(
            Size {
                cols: 100,
                rows: 30,
            },
            |p| {
                let s = p.term.size();
                match dir {
                    Direction::Right | Direction::Left => Size {
                        cols: (s.cols / 2).max(10),
                        rows: s.rows,
                    },
                    _ => Size {
                        cols: s.cols,
                        rows: (s.rows / 2).max(4),
                    },
                }
            },
        );
        let id = self.spawn_pane(cwd, program, size)?;
        let w = &mut self.windows[wi];
        let before = w.layout.focused;
        w.layout = w.layout.split_focused(dir, id);
        if !focus && let Some(b) = before {
            w.layout = w.layout.focus(b);
        }
        if focus {
            w.focus = Focus::Terminal;
        }
        w.layout.zoomed = None;
        Ok(id)
    }

    /// Which window holds a pane (in its layout or backgrounded).
    pub fn window_of(&self, id: PaneId) -> Option<usize> {
        self.windows
            .iter()
            .position(|w| w.layout.has_pane(id) || w.background.iter().any(|b| b.0 == id))
    }

    /// Close a pane for good: kill its shell and drop it from its window.
    pub fn close_pane(&mut self, id: PaneId) {
        self.hub.close(id);
        self.panes.remove(&id);
        self.roster_dirty = true;
        let Some(wi) = self.window_of(id) else {
            return;
        };
        let w = &mut self.windows[wi];
        w.sidebar.forget_pane(id);
        if let Some(i) = w.background.iter().position(|b| b.0 == id) {
            w.background.remove(i);
            return;
        }
        let (layout, _) = w.layout.close_pane(id);
        w.layout = layout;
        if w.layout.is_empty() {
            // Closing the last visible pane restores the newest backgrounded one
            // rather than leaving nothing.
            if let Some((bg, _)) = w.background.pop() {
                w.layout = Layout::single(bg);
                return;
            }
            if w.detached_from.is_some() || wi > 0 {
                w.closing = true;
            } else {
                // The main window's last shell exited: the app closes, as a
                // terminal window does.
                self.request_quit();
            }
        }
    }

    pub fn request_quit(&mut self) {
        self.quit_confirmed = true;
        self.egui_ctx
            .send_viewport_cmd_to(ViewportId::ROOT, egui::ViewportCommand::Close);
    }

    /// Write text to the focused pane of window `wi` as if typed.
    pub fn type_into(&mut self, wi: usize, text: &str) {
        if let Some(f) = self.windows[wi].layout.focused {
            let _ = self.hub.write(f, text.as_bytes());
            self.windows[wi].focus = Focus::Terminal;
        }
    }

    pub fn focused_pane(&self, wi: usize) -> Option<&Pane> {
        self.windows[wi]
            .layout
            .focused
            .and_then(|f| self.panes.get(&f))
    }

    fn focused_family(&self, wi: usize) -> ShellFamily {
        self.focused_pane(wi)
            .map_or(ShellFamily::Posix, |p| p.family)
    }

    pub fn insert_path(&mut self, wi: usize, native: &str) {
        let q = quote_path_for(self.focused_family(wi), native);
        self.type_into(wi, &q);
    }

    pub fn change_dir(&mut self, wi: usize, native: &str) {
        let c = cd_command_for(self.focused_family(wi), native);
        self.type_into(wi, &c);
    }

    // ---- settings ------------------------------------------------------

    pub fn save_settings(&mut self) {
        if let Err(e) = self.settings.save(&self.settings_path) {
            self.toast(format!("Settings not saved: {e}"), true);
        }
    }

    /// Settings changed in the Settings window: apply live and persist.
    pub fn apply_settings(&mut self) {
        let skin = theme::skin(&self.settings.display);
        if skin != self.skin {
            self.skin = skin;
        }
        let size = self.settings.display.font_size;
        if (size - self.font_size).abs() > f32::EPSILON {
            self.set_font_size(size);
        }
        self.save_settings();
    }

    pub fn set_font_size(&mut self, px: f32) {
        self.font_size = px.clamp(FONT_MIN, FONT_MAX);
        for p in self.panes.values_mut() {
            p.view.font = theme::term_font(self.font_size);
            p.view.resize_now();
        }
    }

    pub fn reload_config(&mut self) {
        let (config, err) = crate::store::load_config(&self.config_path);
        self.keymap.set_config(keymap_config(&config));
        self.config = config;
        match err {
            Some(e) => self.toast(e, true),
            None => self.toast(
                format!("Config reloaded from {}", self.config_path.display()),
                false,
            ),
        }
    }

    pub fn persist_widths(&mut self) {
        if let Some(w) = self.windows.first() {
            let (sw, ew, hidden) = (w.sidebar.width, w.editor.width, w.sidebar.show_hidden);
            if (sw - self.settings.ui.sidebar_width).abs() > 0.5
                || (ew - self.settings.ui.editor_width).abs() > 0.5
                || hidden != self.settings.ui.show_hidden
            {
                self.settings.ui.sidebar_width = sw;
                self.settings.ui.editor_width = ew;
                self.settings.ui.show_hidden = hidden;
                self.save_settings();
            }
        }
    }

    // ---- actions ---------------------------------------------------------

    /// Run a keymap action in window `wi`.
    pub fn run_action(&mut self, wi: usize, name: &str) {
        let step = self.config.resize_step;
        let w = &mut self.windows[wi];
        let dir = |s: &str| match s {
            "left" => Some(Direction::Left),
            "right" => Some(Direction::Right),
            "up" => Some(Direction::Up),
            "down" => Some(Direction::Down),
            _ => None,
        };
        if let Some(d) = name.strip_prefix("focus_").and_then(dir) {
            w.layout = w.layout.focus_direction(d);
            w.focus = Focus::Terminal;
            return;
        }
        if let Some(d) = name.strip_prefix("move_").and_then(dir) {
            w.layout = w.layout.move_pane(d);
            return;
        }
        if let Some(d) = name.strip_prefix("resize_").and_then(dir) {
            w.layout = w.layout.resize_focused(d, step);
            return;
        }
        match name {
            "split_down" | "split_right" => {
                let d = if name == "split_down" {
                    Direction::Down
                } else {
                    Direction::Right
                };
                if self.windows[wi].detached_from.is_some() {
                    self.toast("A detached pane can't be split — reattach it first", false);
                    return;
                }
                if let Err(e) = self.split_into(wi, d, None, None, true) {
                    let msg = format!("Couldn't start the shell: {e}");
                    let key = self.windows[wi].key;
                    self.modal = Some((
                        key,
                        Modal::picker(
                            PickerMode::Pick,
                            Some(msg),
                            self.settings.shell.default_profile.clone(),
                        ),
                    ));
                }
            }
            "focus_next" => {
                w.layout = w.layout.focus_next();
                w.focus = Focus::Terminal;
            }
            "toggle_zoom" => w.layout = w.layout.toggle_zoom(),
            "close_pane" => {
                if let Some(f) = w.layout.focused {
                    let key = w.key;
                    self.modal = Some((key, Modal::ConfirmClose(f)));
                }
            }
            "background_pane" => self.background_focused(wi),
            "show_background" => {
                let key = w.key;
                if w.background.is_empty() {
                    self.toast("No backgrounded panes", false);
                } else {
                    self.modal = Some((key, Modal::Background { sel: 0 }));
                }
            }
            "detach_pane" => self.detach(wi),
            "reattach_pane" => self.reattach(wi),
            "new_window" => self.new_window(wi),
            "toggle_sidebar" => {
                let open = !w.sidebar.open || w.focus != Focus::Sidebar;
                if w.sidebar.open && w.focus != Focus::Sidebar {
                    // Already open: just go there.
                    w.focus = Focus::Sidebar;
                } else {
                    w.sidebar.set_open(open, &self.hub);
                    w.focus = if open {
                        Focus::Sidebar
                    } else {
                        Focus::Terminal
                    };
                }
                self.sync_sidebar(wi);
            }
            "toggle_editor" => {
                if w.editor.is_empty() && !w.editor.open {
                    // Nothing to edit yet: the sidebar is where files are picked.
                    w.sidebar.set_open(true, &self.hub);
                    w.focus = Focus::Sidebar;
                    self.sync_sidebar(wi);
                    self.toast("Pick a file: e on a file opens it in the editor", false);
                } else if w.editor.open && w.focus == Focus::Editor {
                    w.editor.open = false;
                    w.focus = Focus::Terminal;
                } else {
                    w.editor.open = true;
                    w.focus = Focus::Editor;
                    w.editor.request_focus = true;
                }
            }
            "open_settings" => {
                let key = w.key;
                self.modal = Some((key, Modal::settings()));
            }
            "command_palette" => {
                let key = w.key;
                self.modal = Some((key, Modal::palette()));
            }
            "pick_shell" => {
                self.profiles = profiles::detect();
                let key = self.windows[wi].key;
                self.modal = Some((
                    key,
                    Modal::picker(
                        PickerMode::Pick,
                        None,
                        self.settings.shell.default_profile.clone(),
                    ),
                ));
            }
            "reload_config" => self.reload_config(),
            "zoom_in" => self.set_font_size(self.font_size + 1.0),
            "zoom_out" => self.set_font_size(self.font_size - 1.0),
            "zoom_reset" => self.set_font_size(self.settings.display.font_size),
            "open_file_dialog" => {
                let hub = self.hub.clone();
                std::thread::spawn(move || {
                    let picked = rfd::FileDialog::new()
                        .set_title("Insert file path")
                        .pick_file()
                        .map(|p| p.to_string_lossy().into_owned());
                    hub.emit(UiEvent::FilePicked(picked));
                });
            }
            _ => {}
        }
    }

    /// Push the focused pane out of the layout, leaving its shell running.
    fn background_focused(&mut self, wi: usize) {
        let w = &mut self.windows[wi];
        let Some(f) = w.layout.focused else { return };
        if w.layout.leaves().len() == 1 && w.detached_from.is_some() {
            self.toast(
                "The only pane of a detached window can't be backgrounded",
                false,
            );
            return;
        }
        let (layout, _) = w.layout.close_pane(f);
        w.layout = layout;
        w.background.push((f, Instant::now()));
        if w.layout.is_empty() {
            // Nothing visible left: keep a shell in the window.
            if let Err(e) = self.split_into(wi, Direction::Right, None, None, true) {
                self.toast(e, true);
            }
        }
        self.roster_dirty = true;
        let n = self.windows[wi].background.len();
        self.toast(
            format!("Pane backgrounded — {n} running. Prefix Shift+B lists them"),
            false,
        );
    }

    pub fn restore_background(&mut self, wi: usize, i: usize) {
        let w = &mut self.windows[wi];
        if i < w.background.len() {
            let (id, _) = w.background.remove(i);
            w.layout = w.layout.insert_beside(id);
            w.focus = Focus::Terminal;
            self.roster_dirty = true;
        }
    }

    fn detach(&mut self, wi: usize) {
        if self.windows[wi].detached_from.is_some() {
            return;
        }
        let w = &mut self.windows[wi];
        let Some(f) = w.layout.focused else { return };
        if w.layout.leaves().len() < 2 {
            self.toast("Detaching needs another pane left behind", false);
            return;
        }
        let (layout, _) = w.layout.close_pane(f);
        w.layout = layout;
        let owner = w.key;
        let n = self.next_window;
        let mut d = self.new_workspace(
            format!("detached-{n}"),
            ViewportId::from_hash_of(("sv-detached", n)),
            Some(owner),
        );
        d.layout = Layout::single(f);
        self.windows.push(d);
        self.roster_dirty = true;
    }

    /// Send a detached pane back to the window it came from.
    fn reattach(&mut self, wi: usize) {
        let Some(owner) = self.windows[wi].detached_from else {
            return;
        };
        let panes = self.windows[wi].layout.leaves();
        let target = self.win(owner).unwrap_or(0);
        for id in panes {
            let t = &mut self.windows[target];
            t.layout = t.layout.insert_beside(id);
        }
        self.windows[wi].layout = Layout::default();
        self.windows[wi].closing = true;
        self.roster_dirty = true;
    }

    fn new_window(&mut self, wi: usize) {
        let cwd = self.focused_pane(wi).and_then(|p| p.cwd.clone());
        let n = self.next_window;
        let w = self.new_workspace(
            format!("sv-win-{n}"),
            ViewportId::from_hash_of(("sv-win", n)),
            None,
        );
        self.windows.push(w);
        let wi = self.windows.len() - 1;
        if let Err(e) = self.split_into(wi, Direction::Right, cwd, None, true) {
            self.windows.pop();
            self.toast(format!("Couldn't open a window: {e}"), true);
        }
    }

    /// Point the sidebar at the focused pane (it follows focus and `cd`).
    pub fn sync_sidebar(&mut self, wi: usize) {
        let w = &mut self.windows[wi];
        if !w.sidebar.open {
            return;
        }
        let focused = w.layout.focused;
        let cwd = focused
            .and_then(|f| self.panes.get(&f))
            .and_then(|p| p.cwd.clone());
        w.sidebar.follow(focused, cwd.as_deref(), &self.hub);
    }

    // ---- events ----------------------------------------------------------

    pub fn drain_events(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                UiEvent::Exit(id) => {
                    if self.panes.contains_key(&id) {
                        self.close_pane(id);
                    }
                }
                UiEvent::Cwd(id, path) => {
                    let root = self.panes.get(&id).and_then(|p| p.msys_root.clone());
                    let temp = std::env::temp_dir().to_string_lossy().into_owned();
                    let native = sv_core::paths::msys_to_native(&path, root.as_deref(), &temp);
                    if let Some(p) = self.panes.get_mut(&id)
                        && p.cwd.as_deref() != Some(native.as_str())
                    {
                        p.cwd = Some(native);
                        self.roster_dirty = true;
                        if let Some(wi) = self.window_of(id)
                            && self.windows[wi].layout.focused == Some(id)
                        {
                            self.sync_sidebar(wi);
                        }
                    }
                }
                UiEvent::AppCommand(id, cmd) => self.app_command(id, &cmd),
                UiEvent::FsChanged(key, dirs) => {
                    if let Some(wi) = self.win(key) {
                        self.windows[wi].sidebar.changed(&dirs, &self.hub);
                    }
                }
                UiEvent::Listing(dir, result) => {
                    for w in &mut self.windows {
                        w.sidebar.listed(&dir, &result, &self.hub);
                    }
                }
                UiEvent::FilePicked(Some(path)) => {
                    let wi = self.active_window();
                    self.insert_path(wi, &path);
                }
                UiEvent::FilePicked(None) => {}
                UiEvent::Remote(r) => self.remote_event(r),
            }
        }
    }

    /// The injected `splitview …` command (OSC 5170).
    fn app_command(&mut self, id: PaneId, cmd: &str) {
        let Some(wi) = self.window_of(id) else { return };
        let (verb, rest) = cmd.split_once(' ').unwrap_or((cmd, ""));
        match verb {
            "settings" => {
                let key = self.windows[wi].key;
                self.modal = Some((key, Modal::settings()));
            }
            "edit" if !rest.trim().is_empty() => {
                let cwd = self.panes.get(&id).and_then(|p| p.cwd.clone());
                let path = sv_core::paths::resolve_path(cwd.as_deref(), rest.trim());
                self.open_in_editor(wi, &path);
            }
            "edit" => self.toast("usage: splitview edit <file>", false),
            _ => self.toast(
                format!("splitview: unknown command '{verb}' (try: edit <file>, settings)"),
                false,
            ),
        }
    }

    pub fn open_in_editor(&mut self, wi: usize, path: &str) {
        let w = &mut self.windows[wi];
        match w.editor.open_file(path) {
            Ok(()) => {
                w.editor.open = true;
                w.focus = Focus::Editor;
                w.editor.request_focus = true;
            }
            Err(e) => self.toast(e, true),
        }
    }

    fn remote_event(&mut self, r: RemoteUi) {
        match r {
            RemoteUi::Status(s) => {
                self.remote_status = s;
                self.roster_dirty = true;
            }
            RemoteUi::Pairing(p) => self.pairing.push(p),
            RemoteUi::Action(a) => {
                let Some(wi) = self.windows.iter().position(|w| w.label == a.window_label) else {
                    return;
                };
                match a.action {
                    "create_pane" => {
                        let wi = if self.windows[wi].detached_from.is_some() {
                            0
                        } else {
                            wi
                        };
                        if let Err(e) =
                            self.split_into(wi, Direction::Right, a.cwd.clone(), None, false)
                        {
                            self.toast(format!("Remote: couldn't open a pane: {e}"), true);
                        }
                    }
                    "close_pane" => {
                        if let Some(id) = a.pane_id.as_deref().and_then(crate::remote::parse_pane)
                            && self.panes.contains_key(&id)
                        {
                            self.close_pane(id);
                        }
                    }
                    _ => {}
                }
                self.publish_roster(true);
            }
        }
    }

    pub fn active_window(&self) -> usize {
        self.windows
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.focused_at.total_cmp(&b.1.focused_at))
            .map_or(0, |(i, _)| i)
    }

    /// Publish every window's panes to remote clients (debounced; a no-op while
    /// remote control is off — the roster must never exist unless asked for).
    pub fn publish_roster(&mut self, force: bool) {
        if !self.remote_status.enabled {
            return;
        }
        let live = !self.remote_status.sessions.is_empty();
        let due = (self.roster_dirty && self.last_publish.elapsed() > Duration::from_millis(150))
            || (live && self.last_publish.elapsed() > Duration::from_secs(2));
        if !force && !due {
            return;
        }
        self.roster_dirty = false;
        self.last_publish = Instant::now();
        let seqs = crate::remote::wire_seqs(&self.hub);
        for w in &self.windows {
            let mut panes = Vec::new();
            let mut push = |id: PaneId, backgrounded: bool| {
                if let Some(p) = self.panes.get(&id) {
                    let size = p.term.size();
                    panes.push(crate::remote::PublishedPane {
                        id: crate::remote::pane_key(id),
                        title: p.title.lock().clone(),
                        cwd: p.cwd.clone(),
                        cols: size.cols,
                        rows: size.rows,
                        preview: p.term.preview_line(),
                        backgrounded,
                    });
                }
            };
            for id in w.layout.leaves() {
                push(id, false);
            }
            for (id, _) in &w.background {
                push(*id, true);
            }
            let snap = crate::remote::WindowPanes {
                window_label: w.label.clone(),
                focused_at: w.focused_at,
                focused_pane_id: w.layout.focused.map(crate::remote::pane_key),
                panes,
            };
            let _ = self.hub.remote.publish_window(snap, &seqs);
        }
    }

    // ---- frame -----------------------------------------------------------

    /// Handle a window's close button. Secondary windows close (detached ones
    /// reattach their pane); the main window asks first if there is unsaved work.
    fn handle_close_request(&mut self, ctx: &egui::Context, wi: usize) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        if wi == 0 {
            if self.quit_confirmed {
                return;
            }
            let dirty: usize = self.windows.iter().map(|w| w.editor.dirty_count()).sum();
            if dirty > 0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                let key = self.windows[0].key;
                self.modal = Some((key, Modal::ConfirmQuit(dirty)));
            }
            return;
        }
        if self.windows[wi].editor.dirty_count() > 0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            let key = self.windows[wi].key;
            self.modal = Some((key, Modal::ConfirmCloseWindow));
            return;
        }
        self.close_window(wi);
    }

    pub fn close_window(&mut self, wi: usize) {
        if self.windows[wi].detached_from.is_some() {
            self.reattach(wi);
        } else {
            let ids: Vec<PaneId> = self.windows[wi]
                .layout
                .leaves()
                .into_iter()
                .chain(self.windows[wi].background.iter().map(|b| b.0))
                .collect();
            for id in ids {
                self.hub.close(id);
                self.panes.remove(&id);
            }
            self.windows[wi].layout = Layout::default();
            self.windows[wi].closing = true;
        }
    }

    /// Everything one window draws and handles in one frame.
    fn show_window(&mut self, ui: &mut egui::Ui, wi: usize) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().focused.unwrap_or(false)) && self.active_window() != wi {
            self.focus_clock += 1.0;
            self.windows[wi].focused_at = self.focus_clock;
            self.roster_dirty = true;
        }
        self.handle_close_request(&ctx, wi);
        self.route_input(&ctx, wi);
        if wi >= self.windows.len() || self.windows[wi].closing {
            return;
        }
        self.show_chrome(ui, wi);
        self.show_side_panels(ui, wi);
        self.show_center(ui, wi);
        self.show_overlays(&ctx, wi);
    }

    /// Keys: pairing prompt > modal > keymap (+ sidebar's own keys) > widgets.
    fn route_input(&mut self, ctx: &egui::Context, wi: usize) {
        let key = self.windows[wi].key;
        if !self.pairing.is_empty() && wi == self.active_window() {
            return; // the prompt reads its own keys
        }
        if self.modal.as_ref().is_some_and(|(k, _)| *k == key) {
            return; // the modal reads its own keys
        }
        let focus = self.windows[wi].focus;
        let mut sidebar_keys = Vec::new();
        let routed = crate::keys::route(ctx, &mut self.keymap, |k, m, _| {
            if focus == Focus::Sidebar && crate::sidebar::wants_key(k, m) {
                sidebar_keys.push((k, m));
                true
            } else {
                false
            }
        });
        if focus == Focus::Sidebar {
            let text = ctx.input_mut(|i| {
                let mut t = String::new();
                i.events.retain(|e| match e {
                    egui::Event::Text(s) => {
                        t.push_str(s);
                        false
                    }
                    _ => true,
                });
                t
            });
            let actions = self.windows[wi]
                .sidebar
                .keys(&sidebar_keys, &text, &self.hub);
            self.sidebar_actions(wi, actions);
        }
        if routed.send_prefix {
            let bytes = self.keymap.prefix_bytes();
            let w = &self.windows[wi];
            match w.focus {
                Focus::Terminal => {
                    if let Some(f) = w.layout.focused {
                        let _ = self.hub.write(f, &bytes);
                    }
                }
                // The literal prefix goes to whatever has focus, never to a
                // shell behind the editor: Ctrl+A twice selects all there.
                Focus::Editor => ctx.input_mut(|i| {
                    i.events.push(egui::Event::Key {
                        key: egui::Key::A,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::CTRL,
                    })
                }),
                Focus::Sidebar => {}
            }
        }
        if let Some(deadline) = self.keymap.prefix_deadline() {
            ctx.request_repaint_after(deadline.saturating_duration_since(Instant::now()));
        }
        for a in routed.actions {
            self.run_action(wi, &a);
            if wi >= self.windows.len() {
                return;
            }
        }
    }

    pub fn sidebar_actions(&mut self, wi: usize, actions: Vec<crate::sidebar::Action>) {
        use crate::sidebar::Action;
        for a in actions {
            match a {
                Action::InsertPath(p) => self.insert_path(wi, &p),
                Action::ChangeDir(p) => self.change_dir(wi, &p),
                Action::OpenFile(p) => self.open_in_editor(wi, &p),
                Action::Leave => self.windows[wi].focus = Focus::Terminal,
                Action::Focus => self.windows[wi].focus = Focus::Sidebar,
                Action::Close => {
                    self.windows[wi].sidebar.set_open(false, &self.hub);
                    self.windows[wi].focus = Focus::Terminal;
                }
                Action::Persist => self.persist_widths(),
            }
        }
    }
}

pub fn keymap_config(cfg: &Config) -> KeymapConfig {
    let mut k = KeymapConfig {
        prefix: cfg.prefix.clone(),
        ..KeymapConfig::default()
    };
    for (chord, action) in &cfg.keybindings {
        k.prefix_bindings.insert(chord.clone(), action.clone());
    }
    k
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some((path, since, n)) = self.frame_stats.as_mut() {
            *n += 1;
            if since.elapsed() >= Duration::from_secs(5) {
                use std::io::Write;
                let cause = ctx.repaint_causes();
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&*path)
                {
                    let _ = writeln!(f, "{n} frames/5s; last causes: {cause:?}");
                }
                *n = 0;
                *since = Instant::now();
            }
        }
        self.drain_events();
        if let Some(shot) = self.shot.as_mut() {
            shot.tick(&ctx);
        }
        crate::shot::drive(self, &ctx);

        self.show_window(ui, 0);

        let mut i = 1;
        while i < self.windows.len() {
            if self.windows[i].closing {
                i += 1;
                continue;
            }
            let vp = self.windows[i].viewport;
            let title = self.window_title(i);
            let builder = egui::ViewportBuilder::default()
                .with_title(title)
                .with_icon(eframe::icon_data::from_png_bytes(theme::ICON_PNG).unwrap_or_default())
                .with_inner_size(if self.windows[i].detached_from.is_some() {
                    [820.0, 520.0]
                } else {
                    [1100.0, 700.0]
                })
                .with_min_inner_size([480.0, 320.0]);
            let wi = i;
            ctx.show_viewport_immediate(vp, builder, |ui, _class| {
                egui::CentralPanel::no_frame().show(ui, |ui| self.show_window(ui, wi));
            });
            i += 1;
        }
        // Drop closed windows (never the main one).
        let before = self.windows.len();
        let mut k = 0;
        self.windows.retain(|w| {
            k += 1;
            k == 1 || !w.closing
        });
        if self.windows.len() != before {
            self.roster_dirty = true;
            let labels: Vec<String> = self.windows.iter().map(|w| w.label.clone()).collect();
            let seqs = crate::remote::wire_seqs(&self.hub);
            let _ = labels;
            // Windows that are gone must not keep their panes in the roster.
            for label in self
                .hub
                .remote
                .roster()
                .iter()
                .map(|p| p.window.clone())
                .collect::<std::collections::HashSet<_>>()
            {
                if !self.windows.iter().any(|w| w.label == label) {
                    self.hub.remote.forget_window(&label, &seqs);
                }
            }
        }
        self.publish_roster(false);
        if self.remote_status.enabled && !self.remote_status.sessions.is_empty() {
            ctx.request_repaint_after(Duration::from_secs(2));
        }
        if let Some(t) = &self.toast {
            let left = Duration::from_secs(4).saturating_sub(t.at.elapsed());
            if left.is_zero() {
                self.toast = None;
            } else {
                ctx.request_repaint_after(left);
            }
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.skin.term.bg.to_normalized_gamma_f32()
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persist_widths();
        let _ = self.hub.remote.disable(&self.hub);
        self.hub.close_all();
    }
}

impl App {
    pub fn window_title(&self, wi: usize) -> String {
        let w = &self.windows[wi];
        let focused = w.layout.focused.and_then(|f| self.panes.get(&f));
        match focused {
            Some(p) => {
                let t = p.title();
                if w.detached_from.is_some() {
                    format!("{t} — SplitView (detached)")
                } else {
                    format!("{t} — SplitView")
                }
            }
            None => "SplitView".into(),
        }
    }
}
