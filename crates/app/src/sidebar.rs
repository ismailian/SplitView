//! The file sidebar (EXPLORER): a `tree(1)`-styled column left of the panes.
//!
//! It follows the focused terminal — rooted at the directory its shell reports
//! over OSC 7, re-rooting when focus moves or the shell `cd`s. Each pane keeps
//! its own view (open folders, cursor) for as long as the app runs.
//!
//! It is live and cheap: directories are read off the UI thread; while open,
//! exactly the directories on screen are watched (`watch.rs`), and only the ones
//! that changed are re-read; a re-read that changed nothing changes nothing.
//! Fully keyboard-driven (j/k, l/h, Enter, c, e, u, ., r, <, >, =, Esc).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::{Align2, Id, Key, Modifiers, Rect, Sense, Ui, pos2, vec2};
use egui_phosphor::regular as ph;
use sv_core::layout::PaneId;
use sv_core::paths::{compact_path, to_native};
use sv_core::tree::{
    DirEntry, Listing, Row, RowStatus, TreeShape, build_tree, connectors, same_entries,
    selection_index, sort_entries,
};

use crate::hub::{Hub, UiEvent};
use crate::theme::{self, tok::*};
use crate::watch::DirWatch;
use crate::widgets as w;

const ROW_H: f32 = 22.0;
const MIN_WIDTH: f32 = 160.0;
const WIDTH_STEP: f32 = 24.0;
const DEFAULT_WIDTH: f32 = 260.0;
/// Upper bound on entries per directory: a `node_modules` must not become 100k rows.
const MAX_ENTRIES: usize = 4096;

/// What a directory read produced.
#[derive(Debug, Clone)]
pub struct DirRead {
    pub parent: Option<String>,
    pub entries: Vec<DirEntry>,
    pub truncated: bool,
}

/// List a directory (runs on a worker thread).
pub fn read_dir(path: &str) -> Result<DirRead, String> {
    let dir = std::path::PathBuf::from(to_native(path));
    let rd = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    for entry in rd {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        let ft = entry.file_type().ok();
        let is_symlink = ft.is_some_and(|t| t.is_symlink());
        let is_dir = match ft {
            Some(t) if t.is_symlink() => std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir()),
            Some(t) => t.is_dir(),
            None => false,
        };
        let meta = entry.metadata().ok();
        entries.push(DirEntry {
            hidden: is_hidden(&name, meta.as_ref()),
            path: entry.path().to_string_lossy().into_owned(),
            name,
            is_dir,
            is_symlink,
        });
    }
    sort_entries(&mut entries);
    let truncated = entries.len() > MAX_ENTRIES;
    entries.truncate(MAX_ENTRIES);
    Ok(DirRead {
        parent: dir.parent().map(|p| p.to_string_lossy().into_owned()),
        entries,
        truncated,
    })
}

/// Dot-file, or hidden/system on Windows.
fn is_hidden(name: &str, meta: Option<&std::fs::Metadata>) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if let Some(m) = meta {
            return m.file_attributes() & 0x6 != 0;
        }
    }
    let _ = meta;
    false
}

pub enum Action {
    InsertPath(String),
    ChangeDir(String),
    OpenFile(String),
    Leave,
    Focus,
    Close,
    Persist,
}

/// Keys the sidebar uses while it has focus (everything else falls through to
/// the keymap, so the prefix works from inside the tree).
pub fn wants_key(k: Key, m: Modifiers) -> bool {
    if m.ctrl || m.alt || m.mac_cmd {
        return false;
    }
    matches!(
        k,
        Key::J
            | Key::K
            | Key::H
            | Key::L
            | Key::C
            | Key::E
            | Key::U
            | Key::R
            | Key::G
            | Key::Y
            | Key::Period
            | Key::Equals
            | Key::Enter
            | Key::Space
            | Key::Escape
            | Key::ArrowUp
            | Key::ArrowDown
            | Key::ArrowLeft
            | Key::ArrowRight
            | Key::PageUp
            | Key::PageDown
            | Key::Home
            | Key::End
    )
}

#[derive(Default)]
struct View {
    /// The cwd this view was last rooted at, so the same directory reported at
    /// the next prompt doesn't undo moving the root with `u`.
    cwd: Option<String>,
    root: String,
    open: HashSet<String>,
    selected: Option<String>,
    cursor: usize,
}

pub struct Sidebar {
    key: u64,
    pub open: bool,
    pub width: f32,
    pub show_hidden: bool,
    listings: HashMap<String, Listing>,
    loading: HashSet<String>,
    again: HashSet<String>,
    views: HashMap<PaneId, View>,
    /// The pane whose view is shown (None: home directory view).
    current: Option<PaneId>,
    fallback: View,
    watch: Option<DirWatch>,
    watched: Vec<String>,
    shape: TreeShape,
    dirty: bool,
    scroll_to_cursor: bool,
}

impl Sidebar {
    pub fn new(key: u64, width: f32, show_hidden: bool) -> Self {
        Self {
            key,
            open: false,
            width: if width > 0.0 { width } else { DEFAULT_WIDTH },
            show_hidden,
            listings: HashMap::new(),
            loading: HashSet::new(),
            again: HashSet::new(),
            views: HashMap::new(),
            current: None,
            fallback: View::default(),
            watch: None,
            watched: Vec::new(),
            shape: TreeShape::default(),
            dirty: true,
            scroll_to_cursor: true,
        }
    }

    fn view(&self) -> &View {
        self.current
            .and_then(|c| self.views.get(&c))
            .unwrap_or(&self.fallback)
    }

    fn view_mut(&mut self) -> &mut View {
        match self.current {
            Some(c) => self.views.entry(c).or_default(),
            None => &mut self.fallback,
        }
    }

    pub fn set_open(&mut self, open: bool, hub: &Arc<Hub>) {
        if open == self.open {
            return;
        }
        self.open = open;
        if open {
            let h = hub.clone();
            let key = self.key;
            self.watch = DirWatch::new(move |dirs| h.emit(UiEvent::FsChanged(key, dirs))).ok();
            self.watched.clear();
            // Listings may be stale after a while closed: re-read what's shown.
            let dirs: Vec<String> = self.listings.keys().cloned().collect();
            for d in dirs {
                self.read(&d, hub);
            }
            self.dirty = true;
        } else {
            self.watch = None; // releases every OS watch
            self.watched.clear();
        }
    }

    pub fn forget_pane(&mut self, id: PaneId) {
        self.views.remove(&id);
        if self.current == Some(id) {
            self.current = None;
            self.dirty = true;
        }
    }

    /// Show `pane`'s view, rooted at `cwd` (re-rooted if the shell moved).
    pub fn follow(&mut self, pane: Option<PaneId>, cwd: Option<&str>, hub: &Arc<Hub>) {
        if pane != self.current {
            self.current = pane;
            self.dirty = true;
            self.scroll_to_cursor = true;
        }
        let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());
        let cwd = cwd.map(str::to_owned).or(home);
        let v = self.view_mut();
        if v.cwd.as_deref() == cwd.as_deref() && !v.root.is_empty() {
            return;
        }
        v.cwd = cwd.clone();
        if let Some(c) = cwd {
            let root = to_native(&c);
            v.root = root.clone();
            v.selected = None;
            v.cursor = 0;
            self.dirty = true;
            self.scroll_to_cursor = true;
            if !self.listings.contains_key(&root) {
                self.read(&root, hub);
            }
        }
    }

    /// Start a read of `dir` on a worker thread (coalesced with one in flight).
    fn read(&mut self, dir: &str, hub: &Arc<Hub>) {
        if self.loading.contains(dir) {
            self.again.insert(dir.to_owned());
            return;
        }
        self.loading.insert(dir.to_owned());
        let (d, h) = (dir.to_owned(), hub.clone());
        let _ = std::thread::Builder::new()
            .name("sv-readdir".into())
            .spawn(move || {
                let r = read_dir(&d);
                h.emit(UiEvent::Listing(d, r));
            });
    }

    /// A read finished. Only re-renders when something actually changed.
    pub fn listed(&mut self, dir: &str, r: &Result<DirRead, String>, hub: &Arc<Hub>) {
        if !self.loading.remove(dir) && !self.listings.contains_key(dir) {
            return; // not ours
        }
        let entry = self.listings.entry(dir.to_owned()).or_default();
        match r {
            Ok(read) => {
                if !same_entries(entry.entries.as_deref(), &read.entries) || entry.error.is_some() {
                    entry.entries = Some(read.entries.clone());
                    entry.error = None;
                    entry.parent = read.parent.clone();
                    entry.truncated = read.truncated;
                    self.dirty = true;
                }
            }
            Err(e) => {
                if entry.error.as_deref() != Some(e) {
                    entry.error = Some(e.clone());
                    entry.entries = None;
                    self.dirty = true;
                }
            }
        }
        if self.again.remove(dir) {
            self.read(dir, hub);
        }
    }

    /// The watch saw these directories change.
    pub fn changed(&mut self, dirs: &[String], hub: &Arc<Hub>) {
        for d in dirs {
            if self.listings.contains_key(d) {
                self.read(d, hub);
            }
        }
    }

    fn rebuild(&mut self, hub: &Arc<Hub>) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let v = self.view();
        let shape = build_tree(&v.root, &self.listings, &v.open, self.show_hidden);
        // Open folders not read yet: read them.
        let missing: Vec<String> = shape
            .rows
            .iter()
            .filter(|r| r.status == Some(RowStatus::Loading))
            .filter_map(|r| r.entry(&self.listings).map(|e| e.path.clone()))
            .collect();
        let (selected, prev) = (v.selected.clone(), v.cursor);
        let cursor = selection_index(&shape.rows, &self.listings, selected.as_deref(), prev);
        self.shape = shape;
        let sel_path = self
            .shape
            .rows
            .get(cursor)
            .and_then(|r| r.entry(&self.listings))
            .map(|e| e.path.clone());
        let v = self.view_mut();
        v.cursor = cursor;
        v.selected = sel_path;
        for m in missing {
            self.read(&m, hub);
        }
        if self.shape.shown != self.watched {
            self.watched = self.shape.shown.clone();
            if let Some(w) = self.watch.as_mut() {
                let _ = w.set_dirs(self.watched.clone());
            }
        }
    }

    fn row_entry(&self, i: usize) -> Option<&DirEntry> {
        self.shape.rows.get(i)?.entry(&self.listings)
    }

    fn select(&mut self, i: usize) {
        let n = self.shape.rows.len();
        if n == 0 {
            return;
        }
        let i = i.min(n - 1);
        let path = self.row_entry(i).map(|e| e.path.clone());
        let v = self.view_mut();
        v.cursor = i;
        v.selected = path;
        self.scroll_to_cursor = true;
    }

    fn toggle_dir(&mut self, path: &str, open: Option<bool>, hub: &Arc<Hub>) {
        let v = self.view_mut();
        let now = open.unwrap_or(!v.open.contains(path));
        if now {
            v.open.insert(path.to_owned());
        } else {
            v.open.remove(path);
        }
        if now && !self.listings.contains_key(path) {
            self.read(path, hub);
        }
        self.dirty = true;
    }

    fn set_root(&mut self, root: String, hub: &Arc<Hub>) {
        let v = self.view_mut();
        v.root = root.clone();
        v.cursor = 0;
        v.selected = None;
        self.dirty = true;
        if !self.listings.contains_key(&root) {
            self.read(&root, hub);
        }
    }

    /// Keys routed to the sidebar this frame (it has focus).
    pub fn keys(&mut self, keys: &[(Key, Modifiers)], text: &str, hub: &Arc<Hub>) -> Vec<Action> {
        let mut out = Vec::new();
        self.rebuild(hub);
        for &(k, m) in keys {
            let cur = self.view().cursor;
            let entry = self.row_entry(cur).cloned();
            match k {
                Key::J | Key::ArrowDown => self.select(cur + 1),
                Key::K | Key::ArrowUp => self.select(cur.saturating_sub(1)),
                Key::PageDown => self.select(cur + 15),
                Key::PageUp => self.select(cur.saturating_sub(15)),
                Key::Home => self.select(0),
                Key::End => self.select(usize::MAX),
                Key::G if m.shift => self.select(usize::MAX),
                Key::G => self.select(0),
                Key::L | Key::ArrowRight => {
                    if let Some(e) = entry {
                        if e.is_dir {
                            if self.view().open.contains(&e.path) {
                                self.select(cur + 1);
                            } else {
                                self.toggle_dir(&e.path, Some(true), hub);
                            }
                        } else {
                            out.push(Action::OpenFile(e.path));
                        }
                    }
                }
                Key::H | Key::ArrowLeft => {
                    if let Some(e) = entry {
                        if e.is_dir && self.view().open.contains(&e.path) {
                            self.toggle_dir(&e.path, Some(false), hub);
                        } else if let Some(row) = self.shape.rows.get(cur)
                            && row.depth > 0
                        {
                            // Go to the parent folder's row.
                            let parent_dir = row.dir.clone();
                            if let Some(i) = self.shape.rows.iter().position(|r| {
                                r.entry(&self.listings)
                                    .is_some_and(|e| e.path == parent_dir)
                            }) {
                                self.select(i);
                            }
                        }
                    }
                }
                Key::Enter | Key::Space => {
                    if let Some(e) = entry {
                        if e.is_dir {
                            self.toggle_dir(&e.path, None, hub);
                        } else if k == Key::Enter {
                            out.push(Action::InsertPath(e.path));
                        }
                    }
                }
                Key::C => {
                    let dir = match entry {
                        Some(e) if e.is_dir => Some(e.path),
                        Some(_) => self.shape.rows.get(cur).map(|r| r.dir.clone()),
                        None => Some(self.view().root.clone()),
                    };
                    if let Some(d) = dir {
                        out.push(Action::ChangeDir(d));
                    }
                }
                Key::Y => {
                    if let Some(e) = entry {
                        out.push(Action::InsertPath(e.path));
                    }
                }
                Key::E => {
                    if let Some(e) = entry
                        && !e.is_dir
                    {
                        out.push(Action::OpenFile(e.path));
                    }
                }
                Key::U => {
                    let root = self.view().root.clone();
                    if let Some(parent) = self
                        .listings
                        .get(&root)
                        .and_then(|l| l.parent.clone())
                        .or_else(|| {
                            std::path::Path::new(&root)
                                .parent()
                                .map(|p| p.to_string_lossy().into_owned())
                        })
                    {
                        self.set_root(parent, hub);
                        // The old root opens inside the new one.
                        self.view_mut().open.insert(root.clone());
                        self.view_mut().selected = Some(root);
                    }
                }
                Key::Period => {
                    self.show_hidden = !self.show_hidden;
                    self.dirty = true;
                    out.push(Action::Persist);
                }
                Key::R => {
                    let dirs = self.shape.shown.clone();
                    for d in dirs {
                        self.read(&d, hub);
                    }
                }
                Key::Equals => {
                    self.width = DEFAULT_WIDTH;
                    out.push(Action::Persist);
                }
                Key::Escape => out.push(Action::Leave),
                _ => {}
            }
            self.rebuild(hub);
        }
        for c in text.chars() {
            match c {
                '>' => {
                    self.width += WIDTH_STEP;
                    out.push(Action::Persist);
                }
                '<' => {
                    self.width = (self.width - WIDTH_STEP).max(MIN_WIDTH);
                    out.push(Action::Persist);
                }
                _ => {}
            }
        }
        out
    }

    pub fn show(&mut self, ui: &mut Ui, focused: bool, hub: &Arc<Hub>) -> Vec<Action> {
        let mut out = Vec::new();
        self.rebuild(hub);
        let panel = ui.max_rect();
        let max_w = (ui.ctx().content_rect().width() * 0.6).max(MIN_WIDTH);
        self.width = self.width.clamp(MIN_WIDTH, max_w);

        w::panel_header(ui, "EXPLORER", |ui| {
            if w::icon_button(ui, ph::X, "Close (prefix e, then Esc)", false).clicked() {
                out.push(Action::Close);
            }
            if w::icon_button(ui, ph::ARROW_CLOCKWISE, "Reload (r)", false).clicked() {
                let dirs = self.shape.shown.clone();
                for d in dirs {
                    self.read(&d, hub);
                }
            }
            let (icon, tip) = if self.show_hidden {
                (ph::EYE, "Hide hidden files (.)")
            } else {
                (ph::EYE_SLASH, "Show hidden files (.)")
            };
            if w::icon_button(ui, icon, tip, self.show_hidden).clicked() {
                self.show_hidden = !self.show_hidden;
                self.dirty = true;
                out.push(Action::Persist);
            }
            if w::icon_button(ui, ph::ARROW_UP, "Up one level (u)", false).clicked() {
                let root = self.view().root.clone();
                if let Some(p) = std::path::Path::new(&root).parent() {
                    self.set_root(p.to_string_lossy().into_owned(), hub);
                }
            }
        });
        self.rebuild(hub);

        // Root row.
        let root = self.view().root.clone();
        let (rr, rresp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
        {
            let p = ui.painter();
            p.text(
                pos2(rr.left() + 12.0, rr.center().y),
                Align2::LEFT_CENTER,
                ph::FOLDER_OPEN,
                theme::prop(13.0),
                WARN,
            );
            let name = sv_core::paths::base_name(&root).to_owned();
            p.text(
                pos2(rr.left() + 32.0, rr.center().y),
                Align2::LEFT_CENTER,
                if name.is_empty() {
                    "…".to_owned()
                } else {
                    name
                },
                theme::prop(12.5),
                TX0,
            );
        }
        rresp.on_hover_text(&root).context_menu(|ui| {
            if ui.button("cd here").clicked() {
                out.push(Action::ChangeDir(root.clone()));
                ui.close();
            }
            if ui.button("Copy path").clicked() {
                ui.ctx().copy_text(root.clone());
                ui.close();
            }
        });

        // Rows.
        let footer_h = 44.0;
        let list_h = (ui.available_height() - footer_h).max(ROW_H);
        let n = self.shape.rows.len();
        let cursor = self.view().cursor;
        let error = self.listings.get(&root).and_then(|l| l.error.clone());
        let mut clicked: Option<(usize, bool, bool)> = None; // row, double, secondary
        let mut ctx_action: Option<Action> = None;
        let scroll_to = std::mem::take(&mut self.scroll_to_cursor).then_some(cursor);
        ui.allocate_ui(vec2(ui.available_width(), list_h), |ui| {
            if let Some(e) = &error {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new(format!("{} {e}", ph::WARNING))
                            .color(ERR)
                            .size(11.5),
                    );
                });
            }
            let mut area = egui::ScrollArea::vertical()
                .id_salt(("sidebar-scroll", self.key))
                .auto_shrink([false, false]);
            if let Some(c) = scroll_to {
                // Keep the cursor in view: aim for it to sit a third of the way down.
                let off = ui.ctx().memory(|m| {
                    m.data
                        .get_temp::<f32>(Id::new(("sb-off", self.key)))
                        .unwrap_or(0.0)
                });
                let y = c as f32 * ROW_H;
                if y < off || y + ROW_H > off + list_h {
                    area = area.vertical_scroll_offset((y - list_h / 3.0).max(0.0));
                }
            }
            let shown = area.show_rows(ui, ROW_H, n, |ui, range| {
                for i in range {
                    let Some(row) = self.shape.rows.get(i) else {
                        continue;
                    };
                    let Some(e) = row.entry(&self.listings) else {
                        continue;
                    };
                    let (r, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                    paint_row(ui, r, row, e, i == cursor, focused, resp.hovered());
                    if resp.clicked() {
                        clicked = Some((i, false, false));
                    }
                    if resp.double_clicked() {
                        clicked = Some((i, true, false));
                    }
                    let path = e.path.clone();
                    let is_dir = e.is_dir;
                    let dir = row.dir.clone();
                    resp.context_menu(|ui| {
                        if !is_dir
                            && ui
                                .button(format!("{} Open in editor  (e)", ph::CODE))
                                .clicked()
                        {
                            ctx_action = Some(Action::OpenFile(path.clone()));
                            ui.close();
                        }
                        if ui
                            .button(format!("{} Insert path  (Enter)", ph::TEXT_T))
                            .clicked()
                        {
                            ctx_action = Some(Action::InsertPath(path.clone()));
                            ui.close();
                        }
                        if ui
                            .button(format!("{} cd here  (c)", ph::TERMINAL))
                            .clicked()
                        {
                            ctx_action = Some(Action::ChangeDir(if is_dir {
                                path.clone()
                            } else {
                                dir.clone()
                            }));
                            ui.close();
                        }
                        if ui.button(format!("{} Copy path", ph::COPY)).clicked() {
                            ui.ctx().copy_text(path.clone());
                            ui.close();
                        }
                    });
                }
            });
            ui.ctx().memory_mut(|m| {
                m.data
                    .insert_temp(Id::new(("sb-off", self.key)), shown.state.offset.y)
            });
        });
        if let Some(a) = ctx_action {
            out.push(a);
        }
        if let Some((i, double, _)) = clicked {
            self.select(i);
            self.scroll_to_cursor = false;
            out.push(Action::Focus);
            if let Some(e) = self.row_entry(i).cloned() {
                if e.is_dir {
                    self.toggle_dir(&e.path, None, hub);
                } else if double {
                    out.push(Action::OpenFile(e.path));
                }
            }
        }
        if root.is_empty() || (n == 0 && error.is_none()) {
            let loading = self.loading.contains(&root);
            ui.label(w::faint(if loading { "  reading…" } else { "  (empty)" }));
        }

        // Footer: where we are + key hints.
        let (fr, _) = ui.allocate_exact_size(vec2(ui.available_width(), footer_h), Sense::hover());
        let p = ui.painter();
        p.rect_filled(Rect::from_min_size(fr.min, vec2(fr.width(), 1.0)), 0, LINE);
        let truncated = self.listings.get(&root).is_some_and(|l| l.truncated);
        p.text(
            pos2(fr.left() + 12.0, fr.top() + 13.0),
            Align2::LEFT_CENTER,
            compact_path(&root, ((fr.width() - 24.0) / 6.6) as usize),
            theme::mono(10.5),
            if truncated { WARN } else { TX3 },
        );
        p.text(
            pos2(fr.left() + 12.0, fr.top() + 31.0),
            Align2::LEFT_CENTER,
            if focused {
                "j/k move · l open · c cd · e edit · Esc back"
            } else {
                "prefix e to focus · double-click to open"
            },
            theme::prop(10.5),
            TX3,
        );

        // Resize edge.
        let (dx, done) =
            crate::chrome::edge_drag(ui, panel.right(), panel, Id::new(("sb-edge", self.key)));
        if dx != 0.0 {
            self.width = (self.width + dx).max(MIN_WIDTH);
        }
        if done {
            out.push(Action::Persist);
        }
        out
    }
}

fn file_icon(name: &str) -> (&'static str, egui::Color32) {
    let ext = name
        .rsplit_once('.')
        .map_or("", |(_, e)| e)
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "c" | "h" | "cpp" | "java" | "kt"
        | "cs" | "swift" | "rb" | "php" | "lua" | "zig" | "dart" => (ph::FILE_CODE, INFO),
        "sh" | "bash" | "zsh" | "ps1" | "bat" | "cmd" | "fish" => (ph::TERMINAL, OK),
        "md" | "txt" | "rst" | "markdown" => (ph::FILE_TEXT, TX2),
        "toml" | "yaml" | "yml" | "json" | "ini" | "cfg" | "conf" | "env" | "lock" => {
            (ph::GEAR_SIX, TX2)
        }
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" => (ph::IMAGE, PURPLE),
        "html" | "htm" | "css" | "scss" | "xml" | "vue" => (ph::BROWSER, WARN),
        "zip" | "gz" | "tar" | "xz" | "7z" => (ph::FILE_ZIP, TX2),
        _ => (ph::FILE, TX2),
    }
}

fn paint_row(
    ui: &Ui,
    r: Rect,
    row: &Row,
    e: &DirEntry,
    selected: bool,
    focused: bool,
    hovered: bool,
) {
    let p = ui.painter();
    if selected {
        p.rect_filled(r, 0, if focused { BG3 } else { BG2 });
        if focused {
            p.rect_filled(Rect::from_min_size(r.min, vec2(2.0, r.height())), 0, ACC);
        }
    } else if hovered {
        p.rect_filled(r, 0, BG2);
    }
    let y = r.center().y;
    let prefix = connectors(row);
    let g = ui
        .painter()
        .layout_no_wrap(prefix, theme::mono(12.0), LINE2);
    let x0 = r.left() + 10.0;
    let cw = g.size().x;
    p.galley(pos2(x0, y - g.size().y / 2.0), g, LINE2);
    let x = x0 + cw + 2.0;
    let (icon, color) = if e.is_dir {
        (
            if row.open {
                ph::FOLDER_OPEN
            } else {
                ph::FOLDER
            },
            WARN,
        )
    } else {
        file_icon(&e.name)
    };
    p.text(
        pos2(x, y),
        Align2::LEFT_CENTER,
        icon,
        theme::prop(13.0),
        color,
    );
    let name_color = if e.hidden {
        TX3
    } else if e.is_dir {
        TX0
    } else {
        TX1
    };
    let mut name = e.name.clone();
    if e.is_symlink {
        name.push_str(" ↗");
    }
    match row.status {
        Some(RowStatus::Loading) => name.push_str("  …"),
        Some(RowStatus::Error) => name.push_str("  ⚠"),
        None => {}
    }
    p.text(
        pos2(x + 19.0, y),
        Align2::LEFT_CENTER,
        name,
        theme::prop(12.5),
        name_color,
    );
}
