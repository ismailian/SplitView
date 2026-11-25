//! The scratchpad: a canvas docked right of the panes for notes, sketches and
//! shapes. One document per project (`sv_core::scratch`): the focused pane's
//! git work tree, else its directory. Autosaved ~1 s after the last change
//! (and on exit) to `<config dir>/scratchpads/<hash>.json`.
//!
//! Keyboard: `Enter` writes a new note, `v t p l a r o x` pick a tool, `Ctrl+Z`
//! undoes, `Esc` leaves a note, then the pad. Scroll or middle-drag pans. With
//! the select tool: drag moves, arrows nudge (Shift: 10), `Delete` removes,
//! double-click edits a note.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{
    Align, Align2, Frame, Key, Layout, Margin, Modifiers, PointerButton, Pos2, Rect, Sense, Stroke,
    StrokeKind, Ui, UiBuilder, Vec2, pos2, vec2,
};
use egui_phosphor::regular as ph;
use sv_core::scratch::{Doc, Item, Shape, bounds, file_name, hit_ink, project_root, translate};

use crate::theme::{self, INK, tok::*};
use crate::widgets as w;

const MIN_WIDTH: f32 = 260.0;
const SAVE_AFTER: Duration = Duration::from_millis(800);
const RETRY_AFTER: Duration = Duration::from_secs(10);
const UNDO_DEPTH: usize = 64;
const TEXT_SIZE: f32 = 14.0;
const INK_W: f32 = 2.0;
const GRID: f32 = 24.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Select,
    Text,
    Ink(Shape),
    Eraser,
}

const TOOLS: &[(Tool, &str, &str, Key)] = &[
    (
        Tool::Select,
        ph::CURSOR,
        "Select (v) — drag moves, arrows nudge, Delete removes",
        Key::V,
    ),
    (
        Tool::Text,
        ph::TEXT_T,
        "Note (t) — Enter writes a new one",
        Key::T,
    ),
    (Tool::Ink(Shape::Pen), ph::SCRIBBLE, "Pen (p)", Key::P),
    (Tool::Ink(Shape::Line), ph::LINE_SEGMENT, "Line (l)", Key::L),
    (
        Tool::Ink(Shape::Arrow),
        ph::ARROW_UP_RIGHT,
        "Arrow (a)",
        Key::A,
    ),
    (Tool::Ink(Shape::Rect), ph::SQUARE, "Rectangle (r)", Key::R),
    (Tool::Ink(Shape::Ellipse), ph::CIRCLE, "Ellipse (o)", Key::O),
    (Tool::Eraser, ph::ERASER, "Eraser (x)", Key::X),
];

pub enum Action {
    Leave,
    Focus,
    Hide,
    Toast(String, bool),
    Persist,
}

pub struct Scratchpad {
    dir: PathBuf,
    pub width: f32,
    /// Focus the pad next frame (opened from the keyboard or the top bar).
    pub request_focus: bool,
    docs: HashMap<String, Doc>,
    dirty: HashSet<String>,
    save_at: Option<Instant>,
    /// cwd → project root, so `.git` is looked up once per directory.
    roots: HashMap<String, String>,
    undo: HashMap<String, Vec<Vec<Item>>>,
    pan: HashMap<String, Vec2>,
    tool: Tool,
    color: u8,
    /// The stroke or shape being drawn.
    drawing: Option<Item>,
    /// (project, item index) of the note being edited.
    editing: Option<(String, usize)>,
    focus_note: bool,
    /// (project, item index) picked with the select tool.
    selected: Option<(String, usize)>,
}

fn is_empty_note(i: &Item) -> bool {
    matches!(i, Item::Text { text, .. } if text.trim().is_empty())
}

impl Scratchpad {
    pub fn new(dir: PathBuf, width: f32) -> Self {
        Self {
            dir,
            width: width.max(MIN_WIDTH),
            request_focus: false,
            docs: HashMap::new(),
            dirty: HashSet::new(),
            save_at: None,
            roots: HashMap::new(),
            undo: HashMap::new(),
            pan: HashMap::new(),
            tool: Tool::Text,
            color: 0,
            drawing: None,
            editing: None,
            focus_note: false,
            selected: None,
        }
    }

    /// The project a pane's cwd belongs to (`""`: no cwd yet, the global pad).
    fn project(&mut self, cwd: Option<&str>) -> String {
        let Some(cwd) = cwd else {
            return String::new();
        };
        self.roots
            .entry(cwd.to_owned())
            .or_insert_with(|| project_root(cwd, |p| Path::new(p).join(".git").exists()))
            .clone()
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(file_name(key))
    }

    /// Load a project's pad once. A file that doesn't parse is kept aside as
    /// `.bak` instead of being overwritten by the next save.
    fn load(&mut self, key: &str) {
        if self.docs.contains_key(key) {
            return;
        }
        let path = self.path(key);
        let mut doc = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str::<Doc>(&s).unwrap_or_else(|_| {
                let _ = std::fs::copy(&path, path.with_extension("json.bak"));
                Doc::default()
            }),
            Err(_) => Doc::default(),
        };
        doc.dir = key.to_owned();
        doc.items.retain(|i| !is_empty_note(i));
        self.docs.insert(key.to_owned(), doc);
    }

    fn items(&mut self, key: &str) -> &mut Vec<Item> {
        &mut self.docs.entry(key.to_owned()).or_default().items
    }

    fn touch(&mut self, key: &str) {
        self.dirty.insert(key.to_owned());
        self.save_at = Some(Instant::now() + SAVE_AFTER);
    }

    /// Remember the items before a change, for `Ctrl+Z`.
    fn snapshot(&mut self, key: &str) {
        let items = self.items(key).clone();
        let stack = self.undo.entry(key.to_owned()).or_default();
        stack.push(items);
        if stack.len() > UNDO_DEPTH {
            stack.remove(0);
        }
    }

    fn undo(&mut self, key: &str) {
        if let Some(prev) = self.undo.get_mut(key).and_then(|s| s.pop()) {
            self.editing = None;
            self.selected = None;
            *self.items(key) = prev;
            self.touch(key);
        }
    }

    /// Write every changed pad now. Returns the first error.
    pub fn flush(&mut self) -> Option<String> {
        let mut err = None;
        for key in std::mem::take(&mut self.dirty) {
            let Some(doc) = self.docs.get(&key) else {
                continue;
            };
            let doc = Doc {
                dir: doc.dir.clone(),
                items: doc
                    .items
                    .iter()
                    .filter(|i| !is_empty_note(i))
                    .cloned()
                    .collect(),
            };
            if let Err(e) = save(&self.path(&key), &doc) {
                err.get_or_insert(format!("Scratchpad not saved: {e}"));
                self.dirty.insert(key);
            }
        }
        self.save_at = (!self.dirty.is_empty()).then(|| Instant::now() + RETRY_AFTER);
        err
    }

    /// Save once the pad has been quiet for a moment; call every frame.
    pub fn autosave(&mut self, ctx: &egui::Context) -> Option<String> {
        let at = self.save_at?;
        let now = Instant::now();
        if now >= at {
            self.flush()
        } else {
            ctx.request_repaint_after(at - now);
            None
        }
    }

    fn new_note(&mut self, key: &str, at: [f32; 2]) {
        self.snapshot(key);
        let color = self.color;
        let items = self.items(key);
        items.push(Item::Text {
            at,
            text: String::new(),
            color,
        });
        self.editing = Some((key.to_owned(), items.len() - 1));
        self.focus_note = true;
    }

    /// Where a note typed from the keyboard goes: under everything else.
    fn next_note_at(&mut self, key: &str, text_bottoms: f32) -> [f32; 2] {
        let ink = self
            .items(key)
            .iter()
            .filter_map(|i| match i {
                Item::Ink { points, .. } => points.iter().map(|p| p[1]).reduce(f32::max),
                Item::Text { .. } => None,
            })
            .fold(text_bottoms, f32::max);
        [16.0, if ink > 0.0 { ink + 16.0 } else { 16.0 }]
    }

    /// `--shot scratch`: a few notes and shapes, not saved.
    pub fn demo(&mut self, cwd: Option<&str>) {
        use Item::{Ink, Text};
        let key = self.project(cwd);
        let ink = |shape, points: &[[f32; 2]], color| Ink {
            shape,
            points: points.to_vec(),
            color,
        };
        let text = |at, t: &str, color| Text {
            at,
            text: t.into(),
            color,
        };
        let pen: Vec<[f32; 2]> = (0..40)
            .map(|i| {
                [
                    200.0 + i as f32 * 4.0,
                    250.0 + (i as f32 / 3.0).sin() * 10.0,
                ]
            })
            .collect();
        let items = vec![
            text([16.0, 16.0], "Ideas", 1),
            text(
                [16.0, 44.0],
                "- cache the galley per wrap width
- pad per git root",
                0,
            ),
            ink(Shape::Rect, &[[20.0, 140.0], [150.0, 200.0]], 2),
            text([34.0, 160.0], "parser", 2),
            ink(Shape::Arrow, &[[150.0, 170.0], [230.0, 170.0]], 0),
            ink(Shape::Ellipse, &[[230.0, 140.0], [360.0, 200.0]], 3),
            text([262.0, 160.0], "emulator", 3),
            ink(Shape::Pen, &pen, 5),
        ];
        self.tool = Tool::Select;
        self.selected = Some((key.clone(), 5));
        self.docs.insert(key.clone(), Doc { dir: key, items });
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        cwd: Option<&str>,
        focused: bool,
        interactive: bool,
    ) -> Vec<Action> {
        let mut out = Vec::new();
        let panel = ui.max_rect();
        let max_w = (ui.ctx().content_rect().width() * 0.7).max(MIN_WIDTH);
        self.width = self.width.clamp(MIN_WIDTH, max_w);
        let key = self.project(cwd);
        self.load(&key);
        if self.editing.as_ref().is_some_and(|(k, _)| *k != key) {
            self.editing = None;
        }
        if self.tool != Tool::Select || self.selected.as_ref().is_some_and(|(k, _)| *k != key) {
            self.selected = None;
        }

        self.header(ui, &key, &mut out);
        let canvas = ui.available_rect_before_wrap();
        let id = ui.id().with("scratch-canvas");
        let resp = ui.interact(canvas, id, Sense::click_and_drag());

        // Pan: scroll wheel / touchpad, or drag with the middle button.
        let pan = self.pan.entry(key.clone()).or_default();
        if resp.hovered() {
            let d = ui.input(|i| i.smooth_scroll_delta);
            *pan += d;
        }
        if resp.dragged_by(PointerButton::Middle) {
            *pan += resp.drag_delta();
        }
        pan.x = pan.x.min(0.0);
        pan.y = pan.y.min(0.0);
        let origin = canvas.min + *pan;
        let to_screen = |p: [f32; 2]| origin + vec2(p[0], p[1]);
        let to_world = |p: Pos2| [p.x - origin.x, p.y - origin.y];

        let painter = ui.painter_at(canvas);
        painter.rect_filled(canvas, 0, BG0);
        let off = vec2(pan.x.rem_euclid(GRID), pan.y.rem_euclid(GRID));
        let mut y = canvas.top() + off.y;
        while y < canvas.bottom() {
            let mut x = canvas.left() + off.x;
            while x < canvas.right() {
                painter.circle_filled(pos2(x, y), 0.8, LINE2);
                x += GRID;
            }
            y += GRID;
        }

        // Items, collecting note rects for hit tests.
        let font = theme::prop(TEXT_SIZE);
        let mut note_rects: Vec<(usize, Rect)> = Vec::new();
        let editing = self.editing.as_ref().map(|(_, i)| *i);
        let mut stop_editing = false;
        let focus_note = std::mem::take(&mut self.focus_note);
        let mut changed = false;
        let doc = self.docs.entry(key.clone()).or_default();
        for (i, item) in doc.items.iter_mut().enumerate() {
            match item {
                Item::Ink {
                    shape,
                    points,
                    color,
                } => paint_ink(&painter, *shape, points, ink(*color), &to_screen),
                Item::Text { at, text, color } => {
                    let pos = to_screen(*at);
                    let wrap = (canvas.right() - pos.x - 12.0).max(120.0);
                    if Some(i) == editing {
                        let r = Rect::from_min_size(pos, vec2(wrap, canvas.bottom() - pos.y));
                        let mut child = ui.new_child(UiBuilder::new().max_rect(r));
                        child.set_clip_rect(canvas);
                        let o = egui::TextEdit::multiline(text)
                            .id(id.with(("note", i)))
                            .frame(Frame::NONE)
                            .margin(Margin::ZERO)
                            .font(font.clone())
                            .text_color(ink(*color))
                            .hint_text("Write…")
                            .desired_width(wrap)
                            .desired_rows(1)
                            .lock_focus(true)
                            .interactive(interactive)
                            .show(&mut child);
                        let tr = &o.response.response;
                        if focus_note && interactive {
                            tr.request_focus();
                        }
                        changed |= o.response.changed();
                        if tr.lost_focus() || (!tr.has_focus() && !focus_note && !focused) {
                            stop_editing = true;
                        }
                        if tr.clicked() || tr.gained_focus() {
                            out.push(Action::Focus);
                        }
                        let rr = tr.rect;
                        painter.line_segment(
                            [
                                rr.left_top() - vec2(6.0, 0.0),
                                rr.left_bottom() - vec2(6.0, 0.0),
                            ],
                            Stroke::new(2.0, ACC_DIM),
                        );
                        note_rects.push((i, rr));
                    } else if !text.trim().is_empty() {
                        let g = painter.layout(text.clone(), font.clone(), ink(*color), wrap);
                        note_rects.push((i, Rect::from_min_size(pos, g.size())));
                        painter.galley(pos, g, ink(*color));
                    }
                }
            }
        }
        if changed {
            self.touch(&key);
        }
        if stop_editing {
            self.editing = None;
        }
        // The selection's outline.
        let sel = self.selected.as_ref().map(|(_, i)| *i);
        let sel_rect = sel.and_then(|i| match self.items(&key).get(i)? {
            Item::Text { .. } => note_rects.iter().find(|(j, _)| *j == i).map(|(_, r)| *r),
            Item::Ink { points, .. } => {
                bounds(points).map(|(a, b)| Rect::from_two_pos(to_screen(a), to_screen(b)))
            }
        });
        if let Some(r) = sel_rect {
            painter.rect_stroke(r.expand(6.0), 3, Stroke::new(1.0, ACC), StrokeKind::Middle);
        }

        // Keys, when the pad (not a note) has the keyboard.
        if focused && interactive && self.editing.is_none() {
            if !resp.has_focus() && ui.input(|i| i.viewport().focused.unwrap_or(false)) {
                resp.request_focus();
            }
            let take = |m: Modifiers, k: Key| ui.input_mut(|i| i.consume_key(m, k));
            for &(tool, _, _, k) in TOOLS {
                if take(Modifiers::NONE, k) {
                    self.tool = tool;
                }
            }
            if take(Modifiers::CTRL, Key::Z) {
                self.undo(&key);
            }
            if let Some(i) = self.selected.as_ref().map(|(_, i)| *i) {
                if take(Modifiers::NONE, Key::Delete) || take(Modifiers::NONE, Key::Backspace) {
                    self.snapshot(&key);
                    self.items(&key).remove(i);
                    self.selected = None;
                    self.touch(&key);
                }
                let mut nudge = [0.0f32; 2];
                for (m, step) in [(Modifiers::NONE, 1.0), (Modifiers::SHIFT, 10.0)] {
                    for (k, d) in [
                        (Key::ArrowLeft, [-step, 0.0]),
                        (Key::ArrowRight, [step, 0.0]),
                        (Key::ArrowUp, [0.0, -step]),
                        (Key::ArrowDown, [0.0, step]),
                    ] {
                        if take(m, k) {
                            nudge = [nudge[0] + d[0], nudge[1] + d[1]];
                        }
                    }
                }
                if nudge != [0.0, 0.0] && i < self.items(&key).len() {
                    self.snapshot(&key);
                    translate(&mut self.items(&key)[i], nudge);
                    self.touch(&key);
                }
                if take(Modifiers::NONE, Key::Escape) {
                    self.selected = None;
                }
            }
            if take(Modifiers::NONE, Key::Enter) {
                let bottom = note_rects
                    .iter()
                    .map(|(_, r)| r.bottom() - origin.y)
                    .fold(0.0, f32::max);
                let at = self.next_note_at(&key, bottom);
                // Bring it into view.
                let pan = self.pan.entry(key.clone()).or_default();
                if at[1] + pan.y > canvas.height() - 60.0 {
                    pan.y = -(at[1] - canvas.height() / 3.0);
                }
                self.new_note(&key, at);
            }
            if take(Modifiers::NONE, Key::Escape) {
                out.push(Action::Leave);
            }
        }
        // Opened from the keyboard: start writing (the last note, or a new one).
        if std::mem::take(&mut self.request_focus) && interactive && self.editing.is_none() {
            let last = self
                .items(&key)
                .iter()
                .rposition(|i| matches!(i, Item::Text { .. }));
            match last {
                Some(i) => {
                    self.editing = Some((key.clone(), i));
                    self.focus_note = true;
                }
                None if self.items(&key).is_empty() => self.new_note(&key, [16.0, 16.0]),
                None => {}
            }
        }

        // Pointer.
        if interactive {
            let pos = resp.interact_pointer_pos();
            if resp.clicked() || resp.drag_started() {
                out.push(Action::Focus);
            }
            match self.tool {
                Tool::Select => {
                    // Where the button went down: a drag is only recognised
                    // after the pointer has already moved off a thin outline.
                    let at = if resp.drag_started_by(PointerButton::Primary) {
                        ui.input(|i| i.pointer.press_origin()).or(pos)
                    } else {
                        pos
                    };
                    let hit = at.and_then(|p| {
                        // Grabbing inside the selection's box keeps it.
                        if sel_rect.is_some_and(|r| r.expand(8.0).contains(p)) {
                            return sel;
                        }
                        pick_at(self.items(&key), &note_rects, to_world(p), p)
                    });
                    if resp.double_clicked()
                        && let Some(i) = hit
                        && matches!(self.items(&key)[i], Item::Text { .. })
                    {
                        self.selected = None;
                        self.editing = Some((key.clone(), i));
                        self.focus_note = true;
                    } else if resp.clicked() || resp.drag_started_by(PointerButton::Primary) {
                        self.selected = hit.map(|i| (key.clone(), i));
                        if resp.drag_started_by(PointerButton::Primary) && hit.is_some() {
                            self.snapshot(&key); // one undo step per move
                        }
                    }
                    if resp.dragged_by(PointerButton::Primary)
                        && let Some(i) = self.selected.as_ref().map(|(_, i)| *i)
                        && resp.drag_delta() != Vec2::ZERO
                        && let Some(item) = self.items(&key).get_mut(i)
                    {
                        let d = resp.drag_delta();
                        translate(item, [d.x, d.y]);
                        self.touch(&key);
                    }
                }
                Tool::Text => {
                    if resp.clicked()
                        && let Some(p) = pos
                    {
                        match note_rects.iter().find(|(_, r)| r.expand(4.0).contains(p)) {
                            Some(&(i, _)) => {
                                self.editing = Some((key.clone(), i));
                                self.focus_note = true;
                            }
                            None => self.new_note(&key, to_world(p)),
                        }
                    }
                }
                Tool::Eraser => {
                    if (resp.clicked() || resp.dragged_by(PointerButton::Primary))
                        && let Some(p) = pos
                        && let Some(i) = hit_at(self.items(&key), &note_rects, to_world(p), p)
                    {
                        self.snapshot(&key);
                        self.editing = None;
                        self.items(&key).remove(i);
                        self.touch(&key);
                    }
                }
                Tool::Ink(shape) => {
                    if resp.drag_started_by(PointerButton::Primary)
                        && let Some(p) = pos
                    {
                        self.drawing = Some(Item::Ink {
                            shape,
                            points: vec![to_world(p)],
                            color: self.color,
                        });
                    }
                    if resp.dragged_by(PointerButton::Primary)
                        && let Some(p) = pos
                        && let Some(Item::Ink { points, .. }) = self.drawing.as_mut()
                    {
                        let wp = to_world(p);
                        if shape == Shape::Pen {
                            let last = points[points.len() - 1];
                            if (wp[0] - last[0]).hypot(wp[1] - last[1]) > 1.5 {
                                points.push(wp);
                            }
                        } else {
                            points.truncate(1);
                            points.push(wp);
                        }
                    }
                    if resp.drag_stopped_by(PointerButton::Primary)
                        && let Some(item) = self.drawing.take()
                    {
                        let keep = match &item {
                            Item::Ink { points, .. } => {
                                points.len() >= 2
                                    && (points[0][0] - points[points.len() - 1][0])
                                        .hypot(points[0][1] - points[points.len() - 1][1])
                                        > 2.0
                                    || points.len() > 3
                            }
                            Item::Text { .. } => false,
                        };
                        if keep {
                            self.snapshot(&key);
                            self.items(&key).push(item);
                            self.touch(&key);
                        }
                    }
                }
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(match self.tool {
                    Tool::Select if resp.dragged() => egui::CursorIcon::Grabbing,
                    Tool::Select => egui::CursorIcon::Default,
                    Tool::Text => egui::CursorIcon::Text,
                    Tool::Eraser => egui::CursorIcon::Cell,
                    Tool::Ink(_) => egui::CursorIcon::Crosshair,
                });
            }
        }
        if let Some(Item::Ink {
            shape,
            points,
            color,
        }) = &self.drawing
        {
            paint_ink(&painter, *shape, points, ink(*color), &to_screen);
        }
        if self.items(&key).is_empty() && self.editing.is_none() {
            painter.text(
                canvas.center(),
                Align2::CENTER_CENTER,
                "Click to write · Enter for a note · p to draw",
                theme::prop(12.0),
                TX3,
            );
        }

        let (dx, done) = crate::chrome::edge_drag(ui, panel.left(), panel, ui.id().with("sp-edge"));
        if dx != 0.0 {
            self.width = (self.width - dx).max(MIN_WIDTH);
        }
        if done {
            out.push(Action::Persist);
        }
        out
    }

    fn header(&mut self, ui: &mut Ui, key: &str, out: &mut Vec<Action>) {
        let name = if key.is_empty() {
            "no folder".to_owned()
        } else {
            sv_core::paths::base_name(key).to_owned()
        };
        w::panel_header(ui, "SCRATCHPAD", |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if w::icon_button(ui, ph::CARET_DOUBLE_RIGHT, "Hide (prefix w)", false).clicked() {
                out.push(Action::Hide);
            }
            if w::icon_button(ui, ph::TRASH, "Clear the pad (Ctrl+Z undoes)", false).clicked()
                && !self.items(key).is_empty()
            {
                self.snapshot(key);
                self.editing = None;
                self.selected = None;
                self.items(key).clear();
                self.touch(key);
                out.push(Action::Toast(
                    "Scratchpad cleared — Ctrl+Z on the pad restores it".into(),
                    false,
                ));
            }
            if w::icon_button(ui, ph::ARROW_U_UP_LEFT, "Undo (Ctrl+Z)", false).clicked() {
                self.undo(key);
            }
            ui.add_space(6.0);
            ui.label(w::faint(name)).on_hover_text(if key.is_empty() {
                "Shown until the shell reports its directory"
            } else {
                key
            });
        });
        // Tool row: tools left, ink colours right.
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
        ui.painter().rect_filled(row, 0, BG1);
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(row.shrink2(vec2(8.0, 0.0)))
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for &(tool, icon, tip, _) in TOOLS {
                    if w::icon_button(ui, icon, tip, self.tool == tool).clicked() {
                        self.tool = tool;
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for c in (0..INK.len() as u8).rev() {
                        let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
                        let p = ui.painter();
                        if self.color == c {
                            p.circle_stroke(r.center(), 7.5, Stroke::new(1.5, TX1));
                        } else if resp.hovered() {
                            p.circle_stroke(r.center(), 7.5, Stroke::new(1.0, TX3));
                        }
                        p.circle_filled(r.center(), 5.0, ink(c));
                        if resp.clicked() {
                            self.color = c;
                            // Recolour the note being written, too.
                            if let Some((k, i)) = self.editing.clone()
                                && let Some(Item::Text { color, .. }) = self.items(&k).get_mut(i)
                            {
                                *color = c;
                                self.touch(&k);
                            }
                        }
                    }
                });
            },
        );
        w::hline(ui);
    }
}

/// The topmost item at a point (`wp` in canvas points, `p` on screen).
fn hit_at(items: &[Item], note_rects: &[(usize, Rect)], wp: [f32; 2], p: Pos2) -> Option<usize> {
    items.iter().enumerate().rev().find_map(|(i, it)| {
        let on = match it {
            Item::Ink { shape, points, .. } => hit_ink(*shape, points, wp, 6.0),
            Item::Text { .. } => note_rects
                .iter()
                .any(|(j, r)| *j == i && r.expand(2.0).contains(p)),
        };
        on.then_some(i)
    })
}

/// What the select tool takes at a point: an outline under it, else the
/// topmost shape or stroke whose box contains it (so shapes grab from inside).
fn pick_at(items: &[Item], note_rects: &[(usize, Rect)], wp: [f32; 2], p: Pos2) -> Option<usize> {
    hit_at(items, note_rects, wp, p).or_else(|| {
        items.iter().enumerate().rev().find_map(|(i, it)| match it {
            Item::Ink { points, .. } => bounds(points)
                .filter(|(a, b)| {
                    (a[0] - 6.0..=b[0] + 6.0).contains(&wp[0])
                        && (a[1] - 6.0..=b[1] + 6.0).contains(&wp[1])
                })
                .map(|_| i),
            Item::Text { .. } => None,
        })
    })
}

fn ink(c: u8) -> egui::Color32 {
    INK[c as usize % INK.len()]
}

fn paint_ink(
    p: &egui::Painter,
    shape: Shape,
    points: &[[f32; 2]],
    color: egui::Color32,
    to_screen: &impl Fn([f32; 2]) -> Pos2,
) {
    let stroke = Stroke::new(INK_W, color);
    let (Some(&first), Some(&last)) = (points.first(), points.last()) else {
        return;
    };
    let (a, b) = (to_screen(first), to_screen(last));
    match shape {
        Shape::Pen if points.len() == 1 => {
            p.circle_filled(a, INK_W / 2.0, color);
        }
        Shape::Pen => {
            p.add(egui::Shape::line(
                points.iter().map(|&q| to_screen(q)).collect(),
                stroke,
            ));
        }
        Shape::Line => {
            p.line_segment([a, b], stroke);
        }
        Shape::Arrow => {
            p.line_segment([a, b], stroke);
            let d = b - a;
            if d.length() > 1.0 {
                let u = d.normalized();
                let n = vec2(-u.y, u.x);
                let head = 10.0f32.min(d.length() / 2.0);
                for side in [n, -n] {
                    p.line_segment([b, b - u * head + side * head * 0.55], stroke);
                }
            }
        }
        Shape::Rect => {
            p.rect_stroke(Rect::from_two_pos(a, b), 2, stroke, StrokeKind::Middle);
        }
        Shape::Ellipse => {
            let r = Rect::from_two_pos(a, b);
            p.add(egui::Shape::ellipse_stroke(
                r.center(),
                r.size() / 2.0,
                stroke,
            ));
        }
    }
}

/// Atomic write: temp file, then rename.
fn save(path: &Path, doc: &Doc) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec(doc).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pads_persist_per_project_and_skip_empty_notes() {
        let dir = std::env::temp_dir().join(format!("sv-scratch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = Scratchpad::new(dir.clone(), 400.0);
        s.load("/p/a");
        s.items("/p/a").push(Item::Text {
            at: [1.0, 1.0],
            text: "idea".into(),
            color: 1,
        });
        s.new_note("/p/a", [0.0, 40.0]); // left empty: not saved
        s.load("/p/b");
        s.items("/p/b").push(Item::Ink {
            shape: Shape::Rect,
            points: vec![[0.0, 0.0], [9.0, 9.0]],
            color: 0,
        });
        s.touch("/p/a");
        s.touch("/p/b");
        assert_eq!(s.flush(), None);

        let mut t = Scratchpad::new(dir.clone(), 400.0);
        t.load("/p/a");
        t.load("/p/b");
        assert_eq!(t.items("/p/a").len(), 1);
        assert!(matches!(&t.items("/p/a")[0], Item::Text { text, .. } if text == "idea"));
        assert_eq!(t.items("/p/b").len(), 1);

        // Undo restores what a clear removed.
        t.snapshot("/p/b");
        t.items("/p/b").clear();
        t.undo("/p/b");
        assert_eq!(t.items("/p/b").len(), 1);

        // A corrupt file is kept aside, not overwritten.
        std::fs::write(dir.join(file_name("/p/c")), "{nope").unwrap();
        t.load("/p/c");
        assert!(
            dir.join(file_name("/p/c"))
                .with_extension("json.bak")
                .exists()
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
