//! egui terminal widget: draws the grid, handles keyboard, mouse, selection and
//! clipboard. Returns bytes to write and size changes; it never does I/O.
//!
//! Keys reach it only after the app's keymap has had first refusal (prefix mode,
//! the few global chords) — anything the keymap consumed is gone from the event
//! list by the time `show` runs, so the widget itself never needs to know which
//! keys are SplitView's.

use std::time::{Duration, Instant};

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color as AColor, CursorShape, NamedColor, Rgb};
use egui::{
    Align2, Color32, CornerRadius, Event, EventFilter, FontId, Id, Key, Modifiers, PointerButton,
    Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};

use crate::input::{key_bytes, paste_bytes, sgr_mouse, x10_mouse};
use crate::{Size, Terminal};

/// How long a new size must hold before the grid and PTY follow it, so a window
/// or divider drag doesn't reflow the shell on every pixel (CLAUDE.md §13).
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(40);

/// Colors come from the app theme (the only place colors are defined).
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub fg: Color32,
    pub bg: Color32,
    pub cursor: Color32,
    pub selection: Color32,
    pub ansi: [Color32; 16],
}

#[derive(Default, Debug)]
pub struct ViewOutput {
    /// Bytes to write to the PTY.
    pub input: Vec<u8>,
    /// New grid size when the widget's area changed (resize the PTY).
    pub resized: Option<Size>,
    /// The widget holds keyboard focus.
    pub focused: bool,
    /// A primary/secondary press landed on it (the app focuses this pane).
    pub pressed: bool,
    /// Ctrl+click on a URL.
    pub open_url: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct ShowOpts {
    /// Take keyboard focus (this is the active pane and nothing else wants keys).
    pub take_focus: bool,
    /// False while a modal owns input: no focus, no keys, no mouse.
    pub interactive: bool,
    /// Dim the content (inactive pane, when the user asked for that).
    pub dim: bool,
    /// Inner padding around the grid.
    pub pad: Vec2,
}

/// Per-pane widget state.
pub struct TerminalView {
    id: Id,
    pub font: FontId,
    cell: Vec2,
    dragging: bool,
    scroll_accum: f32,
    mouse_down: Option<u8>,
    pending: Option<(Size, Instant)>,
    fresh: bool,
    asked_focus: bool,
}

impl TerminalView {
    pub fn new(id: impl std::hash::Hash + std::fmt::Debug, font: FontId) -> Self {
        Self {
            id: Id::new(("terminal", id)),
            font,
            cell: Vec2::ZERO,
            dragging: false,
            scroll_accum: 0.0,
            mouse_down: None,
            pending: None,
            fresh: true,
            asked_focus: false,
        }
    }

    pub fn id(&self) -> Id {
        self.id
    }

    /// Cell size from the last frame (zero before the first).
    pub fn cell_size(&self) -> Vec2 {
        self.cell
    }

    /// Apply the next size immediately instead of debouncing (font change).
    pub fn resize_now(&mut self) {
        self.fresh = true;
    }

    /// Lay out, handle input, and paint the terminal into `rect`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        term: &Terminal,
        pal: &Palette,
        opts: ShowOpts,
    ) -> ViewOutput {
        let mut out = ViewOutput::default();
        let resp = ui.interact(rect, self.id, Sense::click_and_drag());
        let interactive = opts.interactive;

        if (!interactive || !opts.take_focus) && resp.has_focus() {
            ui.memory_mut(|m| m.surrender_focus(self.id));
        }
        if interactive && (resp.is_pointer_button_down_on() || resp.clicked()) {
            out.pressed = true;
        }
        // egui grants keyboard focus only while the OS window has it, so ask
        // then, and wake the next frame once — never every frame, which would
        // spin while the window sits in the background.
        let window_focused = ui.input(|i| i.focused);
        if interactive && opts.take_focus && window_focused && !resp.has_focus() {
            resp.request_focus();
            if !self.asked_focus {
                self.asked_focus = true;
                ui.ctx().request_repaint();
            }
        } else {
            self.asked_focus = false;
        }
        let focused = interactive && opts.take_focus && resp.has_focus();
        out.focused = focused;
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    self.id,
                    EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }

        // ---- metrics & size ----
        self.cell = ui
            .ctx()
            .fonts_mut(|f| vec2(f.glyph_width(&self.font, 'M'), f.row_height(&self.font)));
        let inner = rect.shrink2(opts.pad);
        let cols = ((inner.width() / self.cell.x).floor() as u16).max(2);
        let rows = ((inner.height() / self.cell.y).floor() as u16).max(1);
        let want = Size { cols, rows };
        if term.size() != want {
            let now = Instant::now();
            let apply = self.fresh
                || matches!(self.pending, Some((s, t)) if s == want && now.duration_since(t) >= RESIZE_DEBOUNCE);
            if apply {
                term.resize(want);
                out.resized = Some(want);
                self.pending = None;
            } else {
                let since = match self.pending {
                    Some((s, t)) if s == want => t,
                    _ => {
                        self.pending = Some((want, now));
                        now
                    }
                };
                ui.ctx()
                    .request_repaint_after(RESIZE_DEBOUNCE.saturating_sub(now - since));
            }
        } else {
            self.pending = None;
        }
        self.fresh = false;

        let mut g = term.lock();
        let t = &mut g.term;
        let mode = *t.mode();
        let (cols, rows) = (t.columns(), t.screen_lines());

        // ---- keyboard ----
        if focused {
            let mods = ui.input(|i| i.modifiers);
            ui.input(|i| {
                for ev in &i.events {
                    match ev {
                        Event::Text(s) if !(mods.alt && !mods.ctrl) => {
                            out.input.extend_from_slice(s.as_bytes())
                        }
                        Event::Ime(egui::ImeEvent::Commit(s)) => {
                            out.input.extend_from_slice(s.as_bytes())
                        }
                        Event::Key {
                            key,
                            pressed: true,
                            modifiers,
                            ..
                        } => {
                            let m = *modifiers;
                            if m.shift && !m.ctrl {
                                let scroll = match key {
                                    Key::PageUp => Some(Scroll::PageUp),
                                    Key::PageDown => Some(Scroll::PageDown),
                                    Key::Home if !mode.contains(TermMode::ALT_SCREEN) => {
                                        Some(Scroll::Top)
                                    }
                                    Key::End if !mode.contains(TermMode::ALT_SCREEN) => {
                                        Some(Scroll::Bottom)
                                    }
                                    _ => None,
                                };
                                if let Some(s) = scroll {
                                    t.scroll_display(s);
                                    continue;
                                }
                            }
                            // Ctrl+Shift+Enter: ESC CR, which multi-line REPLs read
                            // as "insert a newline" rather than "submit".
                            if *key == Key::Enter && m.ctrl && m.shift && !m.alt {
                                out.input.extend_from_slice(b"\x1b\r");
                                continue;
                            }
                            if let Some(b) = key_bytes(*key, m, mode.contains(TermMode::APP_CURSOR))
                            {
                                out.input.extend_from_slice(&b);
                            }
                        }
                        Event::Copy => {
                            // Ctrl+Shift+C, or Ctrl+C with a selection, copies; plain
                            // Ctrl+C is SIGINT.
                            match t.selection_to_string().filter(|s| !s.is_empty()) {
                                Some(s) => {
                                    ui.ctx().copy_text(s);
                                    t.selection = None;
                                }
                                None if !mods.shift => out.input.push(0x03),
                                None => {}
                            }
                        }
                        Event::Cut => out.input.push(0x18),
                        Event::Paste(s) => out
                            .input
                            .extend(paste_bytes(s, mode.contains(TermMode::BRACKETED_PASTE))),
                        _ => {}
                    }
                }
            });
            if !out.input.is_empty() {
                t.scroll_display(Scroll::Bottom);
                t.selection = None;
            }
        }

        // ---- mouse ----
        let cell = self.cell;
        let to_cell = |p: Pos2| -> (usize, usize, Side) {
            let x = ((p.x - inner.left()) / cell.x).max(0.0);
            let y = ((p.y - inner.top()) / cell.y).max(0.0);
            let col = (x as usize).min(cols - 1);
            let side = if x.fract() < 0.5 {
                Side::Left
            } else {
                Side::Right
            };
            (col, (y as usize).min(rows - 1), side)
        };
        let offset = t.grid().display_offset() as i32;
        let grid_point =
            |col: usize, row: usize| Point::new(Line(row as i32 - offset), Column(col));
        let mods = ui.input(|i| i.modifiers);
        let report_mouse = mode.intersects(TermMode::MOUSE_MODE) && !mods.shift;

        if interactive && resp.hovered() {
            let rows_f = rows as f32;
            let scroll: f32 = ui.input(|i| {
                i.events
                    .iter()
                    .map(|e| match e {
                        Event::MouseWheel { unit, delta, .. } => match unit {
                            egui::MouseWheelUnit::Point => delta.y / cell.y,
                            egui::MouseWheelUnit::Line => delta.y,
                            egui::MouseWheelUnit::Page => delta.y * rows_f,
                        },
                        _ => 0.0,
                    })
                    .sum()
            });
            if scroll != 0.0 {
                self.scroll_accum += scroll;
                let lines = self.scroll_accum.trunc() as i32;
                self.scroll_accum -= lines as f32;
                if lines != 0 {
                    if report_mouse {
                        if let Some(p) = resp.hover_pos() {
                            let (c, r, _) = to_cell(p);
                            let btn = if lines > 0 { 64 } else { 65 };
                            for _ in 0..lines.unsigned_abs().min(5) {
                                out.input.extend(mouse_report(mode, btn, c, r, true, mods));
                            }
                        }
                    } else if mode.contains(TermMode::ALT_SCREEN)
                        && mode.contains(TermMode::ALTERNATE_SCROLL)
                    {
                        let k = if lines > 0 {
                            Key::ArrowUp
                        } else {
                            Key::ArrowDown
                        };
                        for _ in 0..lines.unsigned_abs() {
                            out.input.extend(
                                key_bytes(k, Modifiers::NONE, mode.contains(TermMode::APP_CURSOR))
                                    .unwrap_or_default(),
                            );
                        }
                    } else {
                        t.scroll_display(Scroll::Delta(lines * 3));
                    }
                }
            }
        }

        if !interactive {
            // A dialog owns input.
        } else if report_mouse {
            if let Some(p) = resp.interact_pointer_pos() {
                let (c, r, _) = to_cell(p);
                for (btn, pb) in [
                    (0u8, PointerButton::Primary),
                    (1, PointerButton::Middle),
                    (2, PointerButton::Secondary),
                ] {
                    if ui.input(|i| i.pointer.button_pressed(pb)) {
                        self.mouse_down = Some(btn);
                        out.input.extend(mouse_report(mode, btn, c, r, true, mods));
                    }
                    if ui.input(|i| i.pointer.button_released(pb)) && self.mouse_down == Some(btn) {
                        self.mouse_down = None;
                        out.input.extend(mouse_report(mode, btn, c, r, false, mods));
                    }
                }
            }
        } else {
            if resp.drag_started_by(PointerButton::Primary)
                && let Some(p) = resp.interact_pointer_pos()
            {
                let (c, r, side) = to_cell(p);
                t.selection = Some(Selection::new(
                    SelectionType::Simple,
                    grid_point(c, r),
                    side,
                ));
                self.dragging = true;
            }
            if self.dragging {
                if let (Some(sel), Some(p)) = (t.selection.as_mut(), resp.interact_pointer_pos()) {
                    let (c, r, side) = to_cell(p);
                    sel.update(grid_point(c, r), side);
                }
                if resp.drag_stopped() {
                    self.dragging = false;
                }
            }
            if resp.triple_clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let (c, r, side) = to_cell(p);
                    t.selection =
                        Some(Selection::new(SelectionType::Lines, grid_point(c, r), side));
                }
            } else if resp.double_clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let (c, r, side) = to_cell(p);
                    t.selection = Some(Selection::new(
                        SelectionType::Semantic,
                        grid_point(c, r),
                        side,
                    ));
                }
            } else if resp.clicked() {
                t.selection = None;
                if mods.command
                    && let Some(p) = resp.interact_pointer_pos()
                {
                    let (c, r, _) = to_cell(p);
                    let line = grid_point(c, r).line;
                    if line >= t.grid().topmost_line() && line <= t.grid().bottommost_line() {
                        let row = &t.grid()[line];
                        let text: String = (0..cols).map(|x| row[Column(x)].c).collect();
                        out.open_url = crate::url_at(&text, c);
                    }
                }
            }
        }

        // ---- paint ----
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::ZERO, pal.bg);
        let content = t.renderable_content();
        let sel = content.selection;
        let colors = content.colors;
        let cursor = content.cursor;
        let display_offset = content.display_offset as i32;
        let resolve = |c: AColor, bold: bool| -> Color32 {
            match c {
                AColor::Spec(Rgb { r, g, b }) => Color32::from_rgb(r, g, b),
                AColor::Indexed(i) => indexed(i, pal, bold),
                AColor::Named(n) => named(n, pal, colors, bold),
            }
        };

        let mut runs: Vec<Run> = Vec::with_capacity(rows * 2);
        let mut run = Run::default();
        let mut row_bg: Vec<(usize, usize, usize, Color32)> = Vec::new(); // row, col, len, color
        for cell in content.display_iter {
            let row = (cell.point.line.0 + display_offset) as usize;
            let col = cell.point.column.0;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let bold = cell.flags.contains(Flags::BOLD);
            let mut fg = resolve(cell.fg, bold);
            let mut bg = resolve(cell.bg, false);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::DIM) {
                fg = fg.gamma_multiply(0.66);
            }
            if sel.is_some_and(|s| s.contains_cell(&cell, cursor.point, cursor.shape)) {
                bg = pal.selection;
            }
            let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            if bg != pal.bg {
                match row_bg.last_mut() {
                    Some((r, c0, len, color)) if *r == row && *c0 + *len == col && *color == bg => {
                        *len += width
                    }
                    _ => row_bg.push((row, col, width, bg)),
                }
            }
            let ch = if cell.flags.contains(Flags::HIDDEN) || cell.c == '\0' {
                ' '
            } else {
                cell.c
            };
            let style = Style {
                fg,
                underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                strike: cell.flags.contains(Flags::STRIKEOUT),
            };
            // ASCII runs are guaranteed monospace; anything else is placed alone
            // at its own cell, so a fallback glyph can never shift the columns.
            let ascii = ch.is_ascii() && width == 1;
            if !ascii || run.row != row || run.style != style || run.col + run.text.len() != col {
                runs.push(std::mem::take(&mut run));
                run = Run {
                    row,
                    col,
                    style,
                    text: String::new(),
                };
            }
            if ascii {
                run.text.push(ch);
            } else {
                runs.push(Run {
                    row,
                    col,
                    style,
                    text: ch.to_string(),
                });
                run.col = col + width;
            }
        }
        runs.push(run);

        // 1. backgrounds (incl. selection)
        for &(row, col, len, color) in &row_bg {
            let r = Rect::from_min_size(
                inner.min + vec2(col as f32 * cell.x, row as f32 * cell.y),
                vec2(len as f32 * cell.x, cell.y),
            );
            painter.rect_filled(r, CornerRadius::ZERO, color);
        }

        // 2. cursor (under the glyph so the character stays readable)
        let crow = cursor.point.line.0 + display_offset;
        if cursor.shape != CursorShape::Hidden && crow >= 0 && (crow as usize) < rows {
            let p = inner.min + vec2(cursor.point.column.0 as f32 * cell.x, crow as f32 * cell.y);
            let r = Rect::from_min_size(p, cell);
            match (focused, cursor.shape) {
                (true, CursorShape::Beam) => painter.rect_filled(
                    Rect::from_min_size(p, vec2(2.0, cell.y)),
                    CornerRadius::ZERO,
                    pal.cursor,
                ),
                (true, CursorShape::Underline) => painter.rect_filled(
                    Rect::from_min_size(p + vec2(0.0, cell.y - 2.0), vec2(cell.x, 2.0)),
                    CornerRadius::ZERO,
                    pal.cursor,
                ),
                (true, _) => {
                    painter.rect_filled(r, CornerRadius::ZERO, pal.cursor.gamma_multiply(0.75))
                }
                (false, _) => painter.rect_stroke(
                    r.shrink(0.5),
                    CornerRadius::ZERO,
                    Stroke::new(1.0, pal.cursor.gamma_multiply(0.6)),
                    StrokeKind::Inside,
                ),
            };
        }

        // 3. text
        for mut r in runs {
            r.flush(&painter, inner.min, cell, &self.font);
        }

        // Scrolled back: a thin marker so it's obvious new output isn't shown.
        if display_offset > 0 {
            let h = rect.height() * (rows as f32 / (rows as f32 + t.grid().history_size() as f32));
            let pos = 1.0 - display_offset as f32 / t.grid().history_size().max(1) as f32;
            let y = rect.top() + (rect.height() - h) * pos;
            painter.rect_filled(
                Rect::from_min_size(pos2(rect.right() - 3.0, y), vec2(3.0, h.max(8.0))),
                CornerRadius::same(1),
                pal.cursor.gamma_multiply(0.5),
            );
        }

        if opts.dim {
            painter.rect_filled(rect, CornerRadius::ZERO, pal.bg.gamma_multiply(0.35));
        }
        drop(g);

        if interactive {
            context_menu(&resp, term);
        }
        out
    }
}

fn context_menu(resp: &Response, term: &Terminal) {
    resp.context_menu(|ui| {
        let sel = term
            .lock()
            .term
            .selection_to_string()
            .filter(|s| !s.is_empty());
        if ui
            .add_enabled(sel.is_some(), egui::Button::new("Copy"))
            .clicked()
        {
            if let Some(s) = sel {
                ui.ctx().copy_text(s);
            }
            ui.close();
        }
        if ui.button("Paste").clicked() {
            // egui has no synchronous clipboard read; ask the platform for a Paste event.
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            ui.close();
        }
        if ui.button("Clear scrollback").clicked() {
            term.lock().term.grid_mut().clear_history();
            ui.close();
        }
    });
}

fn mouse_report(
    mode: TermMode,
    btn: u8,
    col: usize,
    row: usize,
    pressed: bool,
    m: Modifiers,
) -> Vec<u8> {
    if mode.contains(TermMode::SGR_MOUSE) {
        sgr_mouse(btn, col, row, pressed, m)
    } else {
        x10_mouse(btn, col, row, pressed).unwrap_or_default()
    }
}

#[derive(Default, Clone, Copy, PartialEq)]
struct Style {
    fg: Color32,
    underline: bool,
    strike: bool,
}

#[derive(Default)]
struct Run {
    row: usize,
    col: usize,
    style: Style,
    text: String,
}

impl Run {
    fn flush(&mut self, p: &egui::Painter, origin: Pos2, cell: Vec2, font: &FontId) {
        if self.text.trim().is_empty() {
            self.text.clear();
            return;
        }
        let pos = origin + vec2(self.col as f32 * cell.x, self.row as f32 * cell.y);
        p.text(
            pos,
            Align2::LEFT_TOP,
            &self.text,
            font.clone(),
            self.style.fg,
        );
        let w = self.text.chars().count() as f32 * cell.x;
        if self.style.underline {
            let y = pos.y + cell.y - 1.5;
            p.line_segment(
                [pos2(pos.x, y), pos2(pos.x + w, y)],
                Stroke::new(1.0, self.style.fg),
            );
        }
        if self.style.strike {
            let y = pos.y + cell.y * 0.55;
            p.line_segment(
                [pos2(pos.x, y), pos2(pos.x + w, y)],
                Stroke::new(1.0, self.style.fg),
            );
        }
        self.text.clear();
    }
}

fn indexed(i: u8, pal: &Palette, bold: bool) -> Color32 {
    match i {
        0..=7 if bold => pal.ansi[i as usize + 8],
        0..=15 => pal.ansi[i as usize],
        16..=231 => {
            let i = i - 16;
            let v = |x: u8| if x == 0 { 0 } else { 55 + x * 40 };
            Color32::from_rgb(v(i / 36), v((i / 6) % 6), v(i % 6))
        }
        _ => {
            let g = 8 + (i - 232) * 10;
            Color32::from_rgb(g, g, g)
        }
    }
}

fn named(
    n: NamedColor,
    pal: &Palette,
    colors: &alacritty_terminal::term::color::Colors,
    bold: bool,
) -> Color32 {
    // Colors changed by the program via OSC 4/10/11 take precedence.
    if let Some(Rgb { r, g, b }) = colors[n as usize] {
        return Color32::from_rgb(r, g, b);
    }
    let idx = n as usize;
    match n {
        NamedColor::Foreground | NamedColor::BrightForeground => pal.fg,
        NamedColor::Background => pal.bg,
        NamedColor::Cursor => pal.cursor,
        NamedColor::DimForeground => pal.fg.gamma_multiply(0.66),
        _ if idx < 8 && bold => pal.ansi[idx + 8],
        _ if idx < 16 => pal.ansi[idx],
        // Dim variants follow Foreground/Background/Cursor: map back to base 0–7.
        _ => {
            let base = idx.saturating_sub(NamedColor::DimBlack as usize).min(7);
            pal.ansi[base].gamma_multiply(0.66)
        }
    }
}
