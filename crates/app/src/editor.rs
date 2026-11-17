//! The code editor docked right of the panes, for changing a file without
//! leaving the terminal. Tabs, syntax colouring, find/replace, line numbers.
//!
//! The buffer only ever holds `\n`; a CRLF/BOM file remembers that it is one
//! (`sv_core::editor::{decode_text, encode_text}`), so a one-line edit stays a
//! one-line diff. The visible file is stat'ed every 2 s while the panel is open:
//! a clean buffer follows the disk, a dirty one shows a warning, and saving is
//! conditional on the stamp the buffer was read at.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::{Align2, Galley, Id, Key, Modifiers, Rect, RichText, Sense, Ui, pos2, vec2};
use egui_phosphor::regular as ph;
use sv_core::editor::{
    Eol, change_between, decode_text, detect_indent, encode_text, find_all, tab_labels,
};
use sv_core::highlight::{Lang, highlight, lang_for};

use crate::files::{self, FileStamp};
use crate::theme::{self, tok::*};
use crate::widgets as w;

const MIN_WIDTH: f32 = 280.0;
const POLL: Duration = Duration::from_secs(2);
const TAB_H: f32 = 32.0;

pub enum Action {
    Leave,
    Focus,
    Hide,
    Toast(String, bool),
    ConfirmOverwrite(usize),
    ConfirmCloseTab(usize),
    Persist,
    PickFile,
}

struct Tab {
    path: String,
    text: String,
    /// What is on disk as of `stamp` (normalized), to know if we're dirty.
    saved: String,
    eol: Eol,
    bom: bool,
    stamp: Option<FileStamp>,
    lang: Lang,
    indent: String,
    dirty: bool,
    /// The file changed on disk under a dirty buffer.
    disk_changed: bool,
    /// What was last seen on disk (may differ from `stamp` while dirty).
    disk_stamp: Option<FileStamp>,
    gone: bool,
    /// (text hash, font size, galley) of the last layout.
    galley: Option<(u64, f32, Arc<Galley>)>,
    line: usize,
    col: usize,
    /// Scroll so this line is visible next frame.
    scroll_to_line: Option<usize>,
}

#[derive(Default)]
struct Find {
    open: bool,
    query: String,
    replace: String,
    case: bool,
    focus: bool,
    current: usize,
}

pub struct Editor {
    pub open: bool,
    pub width: f32,
    pub request_focus: bool,
    tabs: Vec<Tab>,
    active: usize,
    find: Find,
    last_poll: Instant,
    /// The visible file and the stamp last seen on disk, for the watcher thread
    /// (`None` while the panel is hidden). The thread stats it every 2 s and
    /// wakes the UI only when it changed, so an idle editor costs no frames.
    watch: Arc<parking_lot::Mutex<WatchTarget>>,
}

/// The visible file and the stamp last seen on disk.
type WatchTarget = Option<(String, Option<FileStamp>)>;

/// Stat the visible file every [`POLL`] off the UI thread; repaint on change.
fn spawn_watcher(
    target: std::sync::Weak<parking_lot::Mutex<WatchTarget>>,
    hub: std::sync::Weak<crate::hub::Hub>,
) {
    let _ = std::thread::Builder::new()
        .name("sv-editor-poll".into())
        .spawn(move || {
            loop {
                std::thread::sleep(POLL);
                let Some(target) = target.upgrade() else {
                    return;
                };
                let current = target.lock().clone();
                drop(target);
                if let Some((path, known)) = current {
                    let now = files::stamp(&path).ok().flatten();
                    if now != known {
                        match hub.upgrade() {
                            Some(h) => h.repaint(),
                            None => return,
                        }
                    }
                }
            }
        });
}

fn text_id(i: &str) -> Id {
    Id::new(("editor-text", i))
}

impl Editor {
    pub fn new(width: f32, hub: &Arc<crate::hub::Hub>) -> Self {
        let watch = Arc::new(parking_lot::Mutex::new(None));
        spawn_watcher(Arc::downgrade(&watch), Arc::downgrade(hub));
        Self {
            watch,
            open: false,
            width: width.max(MIN_WIDTH),
            request_focus: false,
            tabs: Vec::new(),
            active: 0,
            find: Find::default(),
            last_poll: Instant::now(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn dirty_count(&self) -> usize {
        self.tabs.iter().filter(|t| t.dirty).count()
    }

    pub fn tab_name(&self, i: usize) -> String {
        self.tabs
            .get(i)
            .map(|t| sv_core::paths::base_name(&t.path).to_owned())
            .unwrap_or_default()
    }

    /// Status-bar line for the active tab.
    pub fn status(&self) -> Option<String> {
        let t = self.tabs.get(self.active)?;
        Some(format!(
            "Ln {}, Col {} · {} · UTF-8{} · {}",
            t.line + 1,
            t.col + 1,
            t.eol.label(),
            if t.bom { " BOM" } else { "" },
            t.lang.name
        ))
    }

    /// Open (or switch to) a file.
    pub fn open_file(&mut self, path: &str) -> Result<(), String> {
        let f = files::read_text(path).map_err(|e| format!("{path}: {e}"))?;
        if let Some(i) = self.tabs.iter().position(|t| t.path == f.path) {
            self.active = i;
            return Ok(());
        }
        let d = decode_text(&f.content);
        let lang = lang_for(&f.path);
        self.tabs.push(Tab {
            indent: detect_indent(&d.text),
            path: f.path,
            saved: d.text.clone(),
            text: d.text,
            eol: d.eol,
            bom: d.bom,
            stamp: Some(f.stamp),
            disk_stamp: Some(f.stamp),
            lang,
            dirty: false,
            disk_changed: false,
            gone: false,
            galley: None,
            line: 0,
            col: 0,
            scroll_to_line: None,
        });
        self.active = self.tabs.len() - 1;
        Ok(())
    }

    /// Save tab `i`. `force` overwrites a file that changed on disk.
    pub fn save(&mut self, i: usize, force: bool) -> Option<Action> {
        let t = self.tabs.get_mut(i)?;
        let content = encode_text(&t.text, t.eol, t.bom);
        let expected = if force { None } else { t.stamp };
        match files::write_text(&t.path, &content, expected) {
            Ok(Some(stamp)) => {
                t.stamp = Some(stamp);
                t.disk_stamp = Some(stamp);
                t.saved = t.text.clone();
                t.dirty = false;
                t.disk_changed = false;
                t.gone = false;
                Some(Action::Toast(
                    format!("Saved {}", sv_core::paths::base_name(&t.path)),
                    false,
                ))
            }
            Ok(None) => Some(Action::ConfirmOverwrite(i)),
            Err(e) => Some(Action::Toast(format!("Save failed: {e}"), true)),
        }
    }

    pub fn close_tab(&mut self, i: usize, force: bool) -> Option<Action> {
        if i >= self.tabs.len() {
            return None;
        }
        if self.tabs[i].dirty && !force {
            return Some(Action::ConfirmCloseTab(i));
        }
        self.tabs.remove(i);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        if self.tabs.is_empty() {
            return Some(Action::Hide);
        }
        None
    }

    /// Reload a tab from disk (dropping local changes).
    pub fn reload(&mut self, i: usize) {
        let Some(t) = self.tabs.get_mut(i) else {
            return;
        };
        if let Ok(f) = files::read_text(&t.path) {
            let d = decode_text(&f.content);
            t.text = d.text.clone();
            t.saved = d.text;
            t.eol = d.eol;
            t.bom = d.bom;
            t.stamp = Some(f.stamp);
            t.disk_stamp = Some(f.stamp);
            t.dirty = false;
            t.disk_changed = false;
            t.galley = None;
        }
    }

    /// Follow the disk for the visible tab.
    fn poll(&mut self) {
        if self.last_poll.elapsed() < POLL {
            return;
        }
        self.last_poll = Instant::now();
        let Some(t) = self.tabs.get_mut(self.active) else {
            return;
        };
        let now = files::stamp(&t.path).ok().flatten();
        t.disk_stamp = now;
        match now {
            None => t.gone = true,
            Some(s) if Some(s) != t.stamp => {
                t.gone = false;
                if t.dirty {
                    t.disk_changed = true;
                } else if let Ok(f) = files::read_text(&t.path) {
                    let d = decode_text(&f.content);
                    // Apply as a minimal change so the cursor stays put unless
                    // the change reaches it.
                    if let Some((from, to, insert)) = change_between(&t.text, &d.text) {
                        t.text.replace_range(from..to, insert);
                    }
                    t.saved = d.text;
                    t.eol = d.eol;
                    t.bom = d.bom;
                    t.stamp = Some(f.stamp);
                    t.galley = None;
                }
            }
            _ => t.gone = false,
        }
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        focused: bool,
        interactive: bool,
        font_size: f32,
    ) -> Vec<Action> {
        let mut out = Vec::new();
        let panel = ui.max_rect();
        let max_w = (ui.ctx().content_rect().width() * 0.7).max(MIN_WIDTH);
        self.width = self.width.clamp(MIN_WIDTH, max_w);
        if self.open {
            self.poll();
        }
        *self.watch.lock() = if self.open {
            self.tabs
                .get(self.active)
                .map(|t| (t.path.clone(), t.disk_stamp))
        } else {
            None
        };
        if focused {
            self.shortcuts(ui, &mut out);
        }

        self.tab_strip(ui, &mut out);
        if self.tabs.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(w::dim("No file open"));
                ui.label(w::faint(
                    "Press e on a file in the sidebar, or run  splitview edit <file>",
                ));
                if ui
                    .button(format!("{} Pick a file", ph::TREE_STRUCTURE))
                    .clicked()
                {
                    out.push(Action::PickFile);
                }
            });
            return out;
        }
        let i = self.active;

        // Disk warnings.
        let (changed, gone) = (self.tabs[i].disk_changed, self.tabs[i].gone);
        if changed || gone {
            egui::Frame::NONE
                .fill(WARN_DIM)
                .inner_margin(egui::Margin::symmetric(12, 5))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let text = if gone {
                            "Deleted on disk — saving recreates it"
                        } else {
                            "Changed on disk since you started editing"
                        };
                        ui.label(
                            RichText::new(format!("{} {text}", ph::WARNING))
                                .color(WARN)
                                .size(11.5),
                        );
                        if changed {
                            if ui.small_button("Reload").clicked() {
                                self.reload(i);
                            }
                            if ui.small_button("Keep mine").clicked() {
                                self.tabs[i].disk_changed = false;
                            }
                        }
                    });
                });
        }

        if self.find.open {
            self.find_bar(ui, focused);
        }

        self.body(ui, i, focused, interactive, font_size, &mut out);

        let (dx, done) = crate::chrome::edge_drag(ui, panel.left(), panel, ui.id().with("ed-edge"));
        if dx != 0.0 {
            self.width = (self.width - dx).max(MIN_WIDTH);
        }
        if done {
            out.push(Action::Persist);
        }
        out
    }

    fn shortcuts(&mut self, ui: &mut Ui, out: &mut Vec<Action>) {
        let (ctrl, ctrl_alt) = (Modifiers::CTRL, Modifiers::CTRL | Modifiers::ALT);
        let take = |m: Modifiers, k: Key| ui.input_mut(|i| i.consume_key(m, k));
        if take(ctrl, Key::S)
            && let Some(a) = self.save(self.active, false)
        {
            out.push(a);
        }
        if take(ctrl, Key::W)
            && let Some(a) = self.close_tab(self.active, false)
        {
            out.push(a);
        }
        if take(ctrl, Key::PageDown) && !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
        if take(ctrl, Key::PageUp) && !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        }
        if take(ctrl_alt, Key::ArrowLeft) {
            self.width += 40.0;
            out.push(Action::Persist);
        }
        if take(ctrl_alt, Key::ArrowRight) {
            self.width = (self.width - 40.0).max(MIN_WIDTH);
            out.push(Action::Persist);
        }
        if take(ctrl, Key::F) || take(ctrl, Key::H) {
            self.find.open = true;
            self.find.focus = true;
            // Seed with the selection.
            if let Some(t) = self.tabs.get(self.active)
                && let Some(st) = egui::TextEdit::load_state(ui.ctx(), text_id(&t.path))
                && let Some(r) = st.cursor.char_range()
            {
                let (a, b) = (
                    r.primary.index.0.min(r.secondary.index.0),
                    r.primary.index.0.max(r.secondary.index.0),
                );
                if b > a && b - a < 200 {
                    let s: String = t.text.chars().skip(a).take(b - a).collect();
                    if !s.contains('\n') {
                        self.find.query = s;
                    }
                }
            }
        }
        if take(Modifiers::NONE, Key::Escape) {
            if self.find.open {
                self.find.open = false;
                self.request_focus = true;
            } else {
                out.push(Action::Leave);
            }
        }
    }

    fn tab_strip(&mut self, ui: &mut Ui, out: &mut Vec<Action>) {
        let (strip, _) = ui.allocate_exact_size(vec2(ui.available_width(), TAB_H), Sense::hover());
        let p = ui.painter();
        p.rect_filled(strip, 0, BG1);
        p.rect_filled(
            Rect::from_min_max(pos2(strip.left(), strip.bottom() - 1.0), strip.max),
            0,
            LINE,
        );
        let paths: Vec<&str> = self.tabs.iter().map(|t| t.path.as_str()).collect();
        let labels = tab_labels(&paths);
        let tools_w = 84.0;
        let mut x = strip.left();
        let mut select = None;
        let mut close = None;
        for (i, label) in labels.iter().enumerate() {
            let on = i == self.active;
            let color = if on { TX0 } else { TX2 };
            let (icon, ic) = (ph::FILE_CODE, if on { INFO } else { TX3 });
            let g = ui
                .painter()
                .layout_no_wrap(label.clone(), theme::prop(12.0), color);
            let wdt = (g.size().x + 54.0).min(220.0);
            if x + wdt > strip.right() - tools_w {
                break;
            }
            let rect = Rect::from_min_size(pos2(x, strip.top()), vec2(wdt, TAB_H));
            let resp = ui.interact(
                rect,
                Id::new(("ed-tab", &self.tabs[i].path)),
                Sense::click(),
            );
            let p = ui.painter();
            if on {
                p.rect_filled(rect, 0, BG0);
                p.rect_filled(
                    Rect::from_min_size(rect.min, vec2(rect.width(), 2.0)),
                    0,
                    ACC,
                );
            } else if resp.hovered() {
                p.rect_filled(rect, 0, BG2);
            }
            p.rect_filled(
                Rect::from_min_max(pos2(rect.right() - 1.0, rect.top()), rect.max),
                0,
                LINE,
            );
            p.text(
                pos2(rect.left() + 10.0, rect.center().y),
                Align2::LEFT_CENTER,
                icon,
                theme::prop(12.5),
                ic,
            );
            let clip = Rect::from_min_max(
                pos2(rect.left() + 28.0, rect.top()),
                pos2(rect.right() - 24.0, rect.bottom()),
            );
            ui.painter_at(clip).galley(
                pos2(clip.left(), rect.center().y - g.size().y / 2.0),
                g,
                color,
            );
            let cr = Rect::from_center_size(
                pos2(rect.right() - 14.0, rect.center().y),
                vec2(18.0, 18.0),
            );
            let c = ui.interact(
                cr,
                Id::new(("ed-close", &self.tabs[i].path)),
                Sense::click(),
            );
            if c.hovered() {
                ui.painter().rect_filled(cr, 3, BG3);
            }
            let dirty = self.tabs[i].dirty;
            let mark = if dirty && !c.hovered() { "●" } else { ph::X };
            ui.painter().text(
                cr.center(),
                Align2::CENTER_CENTER,
                mark,
                theme::prop(if dirty && !c.hovered() { 10.0 } else { 11.0 }),
                if dirty {
                    WARN
                } else if c.hovered() {
                    TX0
                } else {
                    TX3
                },
            );
            if c.clicked() || resp.middle_clicked() {
                close = Some(i);
            } else if resp.clicked() {
                select = Some(i);
            }
            resp.on_hover_text(&self.tabs[i].path);
            x = rect.right();
        }
        if let Some(i) = select {
            self.active = i;
            self.request_focus = true;
            out.push(Action::Focus);
        }
        if let Some(i) = close
            && let Some(a) = self.close_tab(i, false)
        {
            out.push(a);
        }
        let tools = Rect::from_min_max(pos2(strip.right() - tools_w, strip.top()), strip.max);
        let mut c = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(tools)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        if w::icon_button(
            &mut c,
            ph::CARET_DOUBLE_RIGHT,
            "Hide editor (prefix Shift+E)",
            false,
        )
        .clicked()
        {
            out.push(Action::Hide);
        }
        if w::icon_button(&mut c, ph::FLOPPY_DISK, "Save (Ctrl+S)", false).clicked()
            && let Some(a) = self.save(self.active, false)
        {
            out.push(a);
        }
        if w::icon_button(
            &mut c,
            ph::MAGNIFYING_GLASS,
            "Find / replace (Ctrl+F)",
            self.find.open,
        )
        .clicked()
        {
            self.find.open = !self.find.open;
            self.find.focus = self.find.open;
        }
    }

    fn find_bar(&mut self, ui: &mut Ui, focused: bool) {
        let i = self.active;
        let mut go: Option<isize> = None;
        let mut replace_one = false;
        let mut replace_all = false;
        egui::Frame::NONE
            .fill(BG2)
            .inner_margin(egui::Margin::symmetric(8, 5))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let q = ui.add(
                        egui::TextEdit::singleline(&mut self.find.query)
                            .hint_text("Find")
                            .desired_width((ui.available_width() - 120.0).max(80.0))
                            .font(theme::mono(12.0)),
                    );
                    if self.find.focus && focused {
                        q.request_focus();
                        self.find.focus = false;
                    }
                    if q.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        go = Some(if ui.input(|i| i.modifiers.shift) {
                            -1
                        } else {
                            1
                        });
                        q.request_focus();
                    }
                    if q.changed() {
                        self.find.current = 0;
                        go = Some(0);
                    }
                    if w::icon_button(ui, ph::ARROW_UP, "Previous (Shift+Enter)", false).clicked() {
                        go = Some(-1);
                    }
                    if w::icon_button(ui, ph::ARROW_DOWN, "Next (Enter)", false).clicked() {
                        go = Some(1);
                    }
                    if w::icon_button(ui, ph::TEXT_AA, "Match case", self.find.case).clicked() {
                        self.find.case = !self.find.case;
                        go = Some(0);
                    }
                });
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.find.replace)
                            .hint_text("Replace")
                            .desired_width((ui.available_width() - 150.0).max(80.0))
                            .font(theme::mono(12.0)),
                    );
                    if ui.small_button("Replace").clicked() {
                        replace_one = true;
                    }
                    if ui.small_button("All").clicked() {
                        replace_all = true;
                    }
                    let n = self.tabs.get(i).map_or(0, |t| {
                        find_all(&t.text, &self.find.query, self.find.case).len()
                    });
                    ui.label(w::faint(if n == 0 {
                        "no results".to_owned()
                    } else {
                        format!("{}/{n}", self.find.current.min(n - 1) + 1)
                    }));
                });
            });
        let Some(t) = self.tabs.get_mut(i) else {
            return;
        };
        let matches = find_all(&t.text, &self.find.query, self.find.case);
        if replace_all && !matches.is_empty() {
            for &(a, b) in matches.iter().rev() {
                t.text.replace_range(a..b, &self.find.replace);
            }
            t.dirty = t.text != t.saved;
            t.galley = None;
            return;
        }
        if replace_one
            && let Some(&(a, b)) =
                matches.get(self.find.current.min(matches.len().saturating_sub(1)))
        {
            t.text.replace_range(a..b, &self.find.replace);
            t.dirty = t.text != t.saved;
            t.galley = None;
            go = Some(0);
        }
        let matches = find_all(&t.text, &self.find.query, self.find.case);
        if let Some(step) = go
            && !matches.is_empty()
        {
            let n = matches.len() as isize;
            self.find.current = ((self.find.current as isize + step).rem_euclid(n)) as usize;
            let (a, b) = matches[self.find.current];
            let ca = t.text[..a].chars().count();
            let cb = ca + t.text[a..b].chars().count();
            let id = text_id(&t.path);
            let mut st = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
            st.cursor
                .set_char_range(Some(CCursorRange::two(CCursor::new(ca), CCursor::new(cb))));
            st.store(ui.ctx(), id);
            t.scroll_to_line = Some(t.text[..a].matches('\n').count());
        }
    }

    fn body(
        &mut self,
        ui: &mut Ui,
        i: usize,
        focused: bool,
        interactive: bool,
        font_size: f32,
        out: &mut Vec<Action>,
    ) {
        let t = &mut self.tabs[i];
        let font = theme::mono((font_size - 1.0).max(8.0));
        let row_h = ui.fonts_mut(|f| f.row_height(&font));
        let lines = t.text.matches('\n').count() + 1;
        let digits = (lines.ilog10() + 1).max(3) as f32;
        let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
        let gutter = digits * char_w + 22.0;
        let id = text_id(&t.path);

        // Tab inserts the file's indent unit; Enter keeps the line's indent.
        if focused && ui.memory(|m| m.has_focus(id)) && indent_keys(ui, id, &mut t.text, &t.indent)
        {
            t.dirty = t.text != t.saved;
        }

        let bg = BG0;
        let area_rect = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area_rect, 0, bg);
        ui.painter().rect_filled(
            Rect::from_min_size(area_rect.min, vec2(gutter - 6.0, area_rect.height())),
            0,
            BG1,
        );
        let mut scroll = egui::ScrollArea::both()
            .id_salt(("editor-scroll", &t.path))
            .auto_shrink([false, false]);
        if let Some(line) = t.scroll_to_line.take() {
            let y = (line as f32 * row_h - area_rect.height() / 3.0).max(0.0);
            scroll = scroll.vertical_scroll_offset(y);
        }
        let lang = t.lang;
        let cache = &mut t.galley;
        let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, _wrap: f32| -> Arc<Galley> {
            let text = buf.as_str();
            let h = egui::util::hash(text);
            if let Some((ch, cf, g)) = cache.as_ref()
                && *ch == h
                && *cf == font.size
            {
                return g.clone();
            }
            let mut job = LayoutJob::default();
            for (s, e, k) in highlight(text, &lang) {
                job.append(
                    &text[s..e],
                    0.0,
                    TextFormat {
                        font_id: font.clone(),
                        color: theme::syntax(k),
                        ..Default::default()
                    },
                );
            }
            job.wrap.max_width = f32::INFINITY;
            let g = ui.fonts_mut(|f| f.layout_job(job));
            *cache = Some((h, font.size, g.clone()));
            g
        };
        let output = scroll
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.add_space(gutter);
                    egui::TextEdit::multiline(&mut t.text)
                        .id(id)
                        .code_editor()
                        .frame(egui::Frame::NONE)
                        .margin(egui::Margin {
                            left: 0,
                            right: 8,
                            top: 4,
                            bottom: 40,
                        })
                        .desired_width(f32::INFINITY)
                        .desired_rows(lines.max(4))
                        .lock_focus(true)
                        .interactive(interactive)
                        .layouter(&mut layouter)
                        .show(ui)
                })
                .inner
            })
            .inner;

        // Line numbers in the gutter, only for the rows on screen.
        let clip = area_rect;
        let p = ui.painter_at(clip);
        // The gutter stays put while text scrolls sideways under it.
        p.rect_filled(
            Rect::from_min_size(clip.min, vec2(gutter - 6.0, clip.height())),
            0,
            BG1,
        );
        let cur_line = output.cursor_range.map(|r| {
            t.text
                .chars()
                .take(r.primary.index.0)
                .filter(|&c| c == '\n')
                .count()
        });
        let mut line_no = 0usize;
        let mut new_para = true;
        for row in &output.galley.rows {
            let y = output.galley_pos.y + row.min_y();
            if new_para && y + row_h >= clip.top() && y <= clip.bottom() {
                let on = Some(line_no) == cur_line;
                p.text(
                    pos2(clip.left() + gutter - 14.0, y),
                    Align2::RIGHT_TOP,
                    (line_no + 1).to_string(),
                    font.clone(),
                    if on { TX1 } else { TX3 },
                );
            }
            new_para = row.ends_with_newline;
            if row.ends_with_newline {
                line_no += 1;
            }
            if y > clip.bottom() {
                break;
            }
        }

        if output.response.changed() {
            t.dirty = t.text != t.saved;
        }
        if let Some(r) = output.cursor_range {
            let upto: String = t.text.chars().take(r.primary.index.0).collect();
            t.line = upto.matches('\n').count();
            t.col = upto.rsplit('\n').next().map_or(0, |l| l.chars().count());
        }
        let resp = &output.response.response;
        if resp.clicked() || resp.gained_focus() {
            out.push(Action::Focus);
        }
        if (self.request_focus || (focused && !resp.has_focus() && !self.find.open)) && interactive
        {
            resp.request_focus();
            self.request_focus = false;
        }
    }
}

/// Tab → the file's indent unit at the cursor; Enter → newline plus the current
/// line's leading whitespace. Done by editing the buffer and moving the cursor
/// before the TextEdit sees the key.
fn indent_keys(ui: &Ui, id: Id, text: &mut String, indent: &str) -> bool {
    let tab = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab));
    let enter = !tab && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
    if !tab && !enter {
        return false;
    }
    let mut st = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
    let (a, b) = st
        .cursor
        .char_range()
        .map(|r| {
            let (x, y) = (r.primary.index.0, r.secondary.index.0);
            (x.min(y), x.max(y))
        })
        .unwrap_or((0, 0));
    let byte = |c: usize| text.char_indices().nth(c).map_or(text.len(), |(i, _)| i);
    let (ba, bb) = (byte(a), byte(b));
    let insert = if tab {
        indent.to_owned()
    } else {
        let line_start = text[..ba].rfind('\n').map_or(0, |p| p + 1);
        let lead: String = text[line_start..]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        format!("\n{lead}")
    };
    text.replace_range(ba..bb, &insert);
    let at = a + insert.chars().count();
    st.cursor
        .set_char_range(Some(CCursorRange::one(CCursor::new(at))));
    st.store(ui.ctx(), id);
    true
}
