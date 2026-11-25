//! Top bar, status bar and the side panels' hosting (after SecureWire's IDE
//! frame: a 36 px top bar, a 24 px status bar, hairlines between regions).

use std::time::Instant;

use egui::{Align, Align2, Frame, Id, Layout, Margin, Rect, RichText, Sense, Ui, pos2, vec2};
use egui_phosphor::regular as ph;
use sv_core::keymap::pretty;
use sv_core::paths::compact_path;

use crate::app::{App, Focus};
use crate::theme::{self, tok::*};
use crate::widgets as w;

impl App {
    pub fn show_chrome(&mut self, ui: &mut Ui, wi: usize) {
        let key = self.windows[wi].key;
        egui::Panel::top(Id::new(("top", key)))
            .exact_size(theme::TOP_H)
            .frame(Frame::NONE.fill(BG2).inner_margin(Margin::symmetric(10, 0)))
            .show(ui, |ui| self.top_bar(ui, wi));
        egui::Panel::bottom(Id::new(("status", key)))
            .exact_size(theme::STATUS_H)
            .frame(Frame::NONE.fill(BG2))
            .show(ui, |ui| self.status_bar(ui, wi));
    }

    fn top_bar(&mut self, ui: &mut Ui, wi: usize) {
        let mut actions: Vec<&'static str> = Vec::new();
        let bar = ui.max_rect();
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            theme::logo(ui, 18.0).on_hover_text(format!("SplitView {}", theme::VERSION));
            ui.add_space(14.0);

            // The focused pane, like the active host tab in SecureWire.
            let w = &self.windows[wi];
            if w.detached_from.is_some() {
                w::badge(ui, "detached", INFO, INFO_DIM);
                ui.add_space(8.0);
            }
            if let Some(p) = self.focused_pane(wi) {
                let title: String = p.title().chars().take(48).collect();
                let g = ui.painter().layout_no_wrap(title, theme::prop(12.5), TX0);
                let (r, _) =
                    ui.allocate_exact_size(vec2(g.size().x + 38.0, theme::TOP_H), Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(r, 0, BG1);
                painter.rect_filled(
                    Rect::from_min_max(pos2(r.left(), r.bottom() - 2.0), r.max),
                    0,
                    self.skin.accent,
                );
                painter.circle_filled(pos2(r.left() + 13.0, r.center().y), 3.5, OK);
                painter.galley(
                    pos2(r.left() + 24.0, r.center().y - g.size().y / 2.0),
                    g,
                    TX0,
                );
                let n = w.layout.leaves().len();
                if n > 1 {
                    ui.add_space(10.0);
                    ui.label(w::faint(format!("{n} panes")));
                }
            }
            let left_end = ui.min_rect().right();

            let right_start = ui
                .with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if w::icon_button(ui, ph::GEAR_SIX, "Settings (prefix ,)", false).clicked() {
                        actions.push("open_settings");
                    }
                    // Remote indicator: only while the server is on.
                    let rs = &self.remote_status;
                    if rs.enabled {
                        let live = !rs.sessions.is_empty();
                        let tip = if live {
                            format!("Remote control: {} live session(s)", rs.sessions.len())
                        } else {
                            "Remote control is on (no one connected)".to_owned()
                        };
                        let r = w::icon_button(ui, ph::BROADCAST, &tip, true);
                        if live {
                            ui.painter().circle_filled(
                                r.rect.right_top() + vec2(-5.0, 5.0),
                                3.0,
                                WARN,
                            );
                        }
                        if r.clicked() {
                            actions.push("open_settings_remote");
                        }
                    }
                    let bg = self.windows[wi].background.len();
                    if bg > 0 {
                        let r = w::icon_button(
                            ui,
                            ph::STACK,
                            &format!("{bg} backgrounded pane(s) — prefix Shift+B"),
                            true,
                        );
                        ui.painter().text(
                            r.rect.right_top() + vec2(-4.0, 5.0),
                            Align2::CENTER_CENTER,
                            bg.to_string(),
                            theme::prop(9.0),
                            WARN,
                        );
                        if r.clicked() {
                            actions.push("show_background");
                        }
                    }
                    ui.add_space(6.0);
                    let w = &self.windows[wi];
                    if w::icon_button(ui, ph::NOTE_PENCIL, "Scratchpad (prefix w)", w.scratch_open)
                        .clicked()
                    {
                        actions.push("toggle_scratchpad");
                    }
                    if w::icon_button(ui, ph::CODE, "Editor (prefix Shift+E)", w.editor.open)
                        .clicked()
                    {
                        actions.push("toggle_editor");
                    }
                    if w::icon_button(
                        ui,
                        ph::ARROWS_OUT_SIMPLE,
                        "Zoom pane (prefix z)",
                        w.layout.zoomed.is_some(),
                    )
                    .clicked()
                    {
                        actions.push("toggle_zoom");
                    }
                    if w::icon_button(
                        ui,
                        ph::SQUARE_SPLIT_VERTICAL,
                        "Split down (prefix -)",
                        false,
                    )
                    .clicked()
                    {
                        actions.push("split_down");
                    }
                    if w::icon_button(
                        ui,
                        ph::SQUARE_SPLIT_HORIZONTAL,
                        "Split right (prefix \\)",
                        false,
                    )
                    .clicked()
                    {
                        actions.push("split_right");
                    }
                    if w::icon_button(ui, ph::TREE_STRUCTURE, "Files (prefix e)", w.sidebar.open)
                        .clicked()
                    {
                        actions.push("toggle_sidebar_click");
                    }
                    ui.min_rect().left()
                })
                .inner;

            // The command palette, as a search box centred on the window; when
            // the sides crowd it, centred in the gap between them.
            let (cx, gap) = (bar.center().x, 12.0);
            let mut half = 190.0f32
                .min(cx - left_end - gap)
                .min(right_start - cx - gap);
            let mut center = cx;
            if half < 100.0 {
                center = (left_end + right_start) / 2.0;
                half = 190.0f32.min((right_start - left_end) / 2.0 - gap);
            }
            if half >= 60.0 {
                let r =
                    Rect::from_center_size(pos2(center, bar.center().y), vec2(half * 2.0, 24.0));
                let width = r.width();
                let resp = ui.interact(r, Id::new(("palette-box", wi)), Sense::click());
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                let p = ui.painter();
                p.rect_filled(r, 3, BG1);
                p.rect_stroke(
                    r,
                    3,
                    egui::Stroke::new(1.0, if resp.hovered() { TX3 } else { LINE2 }),
                    egui::StrokeKind::Inside,
                );
                p.text(
                    pos2(r.left() + 8.0, r.center().y),
                    Align2::LEFT_CENTER,
                    format!("{}  Run an action, pick a shell…", ph::MAGNIFYING_GLASS),
                    theme::prop(12.0),
                    TX3,
                );
                if width > 250.0 {
                    p.text(
                        pos2(r.right() - 8.0, r.center().y),
                        Align2::RIGHT_CENTER,
                        "Ctrl Shift P",
                        theme::mono(10.5),
                        TX2,
                    );
                }
                if resp.clicked() {
                    actions.push("command_palette");
                }
            }
        });
        for a in actions {
            match a {
                "open_settings_remote" => {
                    let key = self.windows[wi].key;
                    self.modal = Some((
                        key,
                        crate::dialogs::Modal::settings_at(crate::settings_ui::Section::Remote),
                    ));
                }
                "toggle_sidebar_click" => {
                    let w = &mut self.windows[wi];
                    let open = !w.sidebar.open;
                    w.sidebar.set_open(open, &self.hub);
                    w.focus = if open {
                        Focus::Sidebar
                    } else {
                        Focus::Terminal
                    };
                    self.sync_sidebar(wi);
                }
                a => self.run_action(wi, a),
            }
        }
    }

    fn status_bar(&mut self, ui: &mut Ui, wi: usize) {
        let in_prefix = self.keymap.in_prefix(Instant::now());
        let prefix = pretty(&self.keymap.config().prefix);
        let w = &self.windows[wi];
        let pane = self.focused_pane(wi);
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let small = |t: String, c| RichText::new(t).size(11.5).color(c);
            let item = |ui: &mut Ui, t: RichText| {
                ui.add_space(10.0);
                ui.label(t);
                ui.add_space(10.0);
            };

            // Left block: the shell in the focused pane.
            let (label, fg, bg) = match pane {
                Some(p) => (format!("{} {}", ph::TERMINAL_WINDOW, p.shell), ACC, ACC_DIM),
                None => ("no shell".to_owned(), TX2, BG3),
            };
            Frame::NONE
                .fill(bg)
                .inner_margin(Margin::symmetric(10, 4))
                .show(ui, |ui| {
                    ui.label(small(label, fg));
                });
            if let Some(p) = pane {
                if let Some(cwd) = &p.cwd {
                    item(
                        ui,
                        RichText::new(compact_path(cwd, 48))
                            .monospace()
                            .size(11.5)
                            .color(TX2),
                    );
                }
                let s = p.term.size();
                item(ui, small(format!("{}×{}", s.cols, s.rows), TX3));
            }
            let n = w.layout.leaves().len();
            item(
                ui,
                small(
                    format!(
                        "{} {n} pane{}",
                        ph::SQUARES_FOUR,
                        if n == 1 { "" } else { "s" }
                    ),
                    TX2,
                ),
            );
            if !w.background.is_empty() {
                item(
                    ui,
                    small(
                        format!("{} {} in background", ph::STACK, w.background.len()),
                        WARN,
                    ),
                );
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(10.0);
                ui.label(small(format!("v{}", theme::VERSION), TX3));
                ui.add_space(10.0);
                self.claude_meters(ui, wi);
                if self.remote_status.enabled {
                    let n = self.remote_status.sessions.len();
                    let (t, c) = if n > 0 {
                        (format!("{} remote · {n} live", ph::BROADCAST), WARN)
                    } else {
                        (format!("{} remote on", ph::BROADCAST), ACC)
                    };
                    item(ui, small(t, c));
                }
                if w.focus == Focus::Editor
                    && let Some(info) = w.editor.status()
                {
                    item(ui, small(info, TX2));
                }
                if in_prefix {
                    ui.add_space(6.0);
                    w::badge(ui, "prefix", BG0, ACC);
                    ui.add_space(6.0);
                } else {
                    item(ui, small(format!("{prefix} prefix"), TX3));
                }
                let focus = match w.focus {
                    Focus::Terminal => "terminal",
                    Focus::Sidebar => "files",
                    Focus::Editor => "editor",
                    Focus::Scratch => "scratchpad",
                };
                item(ui, small(format!("focus: {focus}"), TX3));
            });
        });
    }

    pub fn show_side_panels(&mut self, ui: &mut Ui, wi: usize) {
        let key = self.windows[wi].key;
        let modal = self.modal.as_ref().is_some_and(|(k, _)| *k == key) || !self.pairing.is_empty();
        if self.windows[wi].sidebar.open {
            self.sync_sidebar(wi);
            let width = self.windows[wi].sidebar.width;
            let focused = self.windows[wi].focus == Focus::Sidebar && !modal;
            let mut actions = Vec::new();
            egui::Panel::left(Id::new(("sidebar", key)))
                .exact_size(width)
                .resizable(false)
                .frame(Frame::NONE.fill(BG1))
                .show(ui, |ui| {
                    actions = self.windows[wi].sidebar.show(ui, focused, &self.hub);
                });
            self.sidebar_actions(wi, actions);
        }
        if self.windows[wi].scratch_open {
            let width = self.scratch.width;
            let focused = self.windows[wi].focus == Focus::Scratch && !modal;
            let cwd = self.focused_pane(wi).and_then(|p| p.cwd.clone());
            let mut actions = Vec::new();
            egui::Panel::right(Id::new(("scratch", key)))
                .exact_size(width)
                .resizable(false)
                .frame(Frame::NONE.fill(BG1))
                .show(ui, |ui| {
                    actions = self.scratch.show(ui, cwd.as_deref(), focused, !modal);
                });
            self.scratch_actions(wi, actions);
        }
        if self.windows[wi].editor.open {
            let width = self.windows[wi].editor.width;
            let focused = self.windows[wi].focus == Focus::Editor && !modal;
            let font = self.font_size;
            let mut actions = Vec::new();
            egui::Panel::right(Id::new(("editor", key)))
                .exact_size(width)
                .resizable(false)
                .frame(Frame::NONE.fill(BG1))
                .show(ui, |ui| {
                    actions = self.windows[wi].editor.show(ui, focused, !modal, font);
                });
            self.editor_actions(wi, actions);
        }
    }

    pub fn scratch_actions(&mut self, wi: usize, actions: Vec<crate::scratchpad::Action>) {
        use crate::scratchpad::Action;
        for a in actions {
            match a {
                Action::Leave => self.windows[wi].focus = Focus::Terminal,
                Action::Focus => self.windows[wi].focus = Focus::Scratch,
                Action::Hide => {
                    self.windows[wi].scratch_open = false;
                    self.windows[wi].focus = Focus::Terminal;
                }
                Action::Toast(t, err) => self.toast(t, err),
                Action::Persist => self.persist_widths(),
            }
        }
    }

    /// Claude Code's context / 5-hour / weekly use, only while it runs. Shows
    /// the session in the focused pane's directory, else the latest active.
    fn claude_meters(&self, ui: &mut Ui, wi: usize) {
        let u = &self.claude;
        let cwd = self.focused_pane(wi).and_then(|p| p.cwd.as_deref());
        let norm = |a: &str| a.replace('\\', "/").trim_end_matches('/').to_lowercase();
        let Some(s) = cwd
            .and_then(|c| u.sessions.iter().find(|s| norm(&s.cwd) == norm(c)))
            .or(u.sessions.first())
        else {
            return;
        };
        let pct = |l: Option<crate::claude::Limit>| l.map(|l| l.percent / 100.0);
        let ctx = s.context.map(|(n, w)| n as f32 / w as f32);
        // Right to left: weekly, 5-hour, context, then the mark.
        let mut area = w::meter(ui, "wk", pct(u.seven_day));
        ui.add_space(10.0);
        area = area.union(w::meter(ui, "5h", pct(u.five_hour)));
        ui.add_space(10.0);
        area = area.union(w::meter(ui, "ctx", ctx));
        let busy = s.status == "busy";
        let (r, _) = ui.allocate_exact_size(vec2(22.0, theme::STATUS_H), Sense::hover());
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            ph::SPARKLE,
            theme::prop(12.0),
            if busy { WARN } else { ACC },
        );
        area = area.union(r);
        ui.add_space(8.0);
        let now = crate::claude::unix_now();
        ui.interact(area, Id::new(("claude-usage", wi)), Sense::hover())
            .on_hover_ui(|ui| {
                ui.set_min_width(250.0);
                let row = |ui: &mut Ui, k: &str, v: String| {
                    ui.horizontal(|ui| {
                        ui.label(w::dim(k));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(w::strong(v));
                        });
                    });
                };
                let title = if s.name.is_empty() {
                    "Claude Code"
                } else {
                    &s.name
                };
                ui.label(RichText::new(title).color(TX0).strong());
                ui.label(w::faint(format!(
                    "{} · {}",
                    if busy { "working" } else { "idle" },
                    compact_path(&s.cwd, 40)
                )));
                ui.add_space(6.0);
                match s.context {
                    Some((n, wnd)) => row(
                        ui,
                        "Context",
                        format!(
                            "{}k / {}k · {:.0}%",
                            n / 1000,
                            wnd / 1000,
                            n as f32 * 100.0 / wnd as f32
                        ),
                    ),
                    None => row(ui, "Context", "no reply yet".into()),
                }
                let limit = |ui: &mut Ui, k: &str, l: Option<crate::claude::Limit>| {
                    let v = match l {
                        Some(crate::claude::Limit {
                            percent,
                            resets_at: Some(t),
                        }) => format!(
                            "{percent:.0}% · resets in {}",
                            crate::claude::until(t - now)
                        ),
                        Some(l) => format!("{:.0}%", l.percent),
                        None => "–".into(),
                    };
                    row(ui, k, v);
                };
                limit(ui, "5-hour limit", u.five_hour);
                limit(ui, "Weekly limit", u.seven_day);
                if !s.model.is_empty() {
                    row(ui, "Model", s.model.clone());
                }
                if u.sessions.len() > 1 {
                    ui.label(w::faint(format!(
                        "{} Claude sessions running",
                        u.sessions.len()
                    )));
                }
                if let Some(e) = &u.limits_error {
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("Limits: {e}")).color(WARN).size(11.0));
                }
            });
    }

    pub fn editor_actions(&mut self, wi: usize, actions: Vec<crate::editor::Action>) {
        use crate::editor::Action;
        for a in actions {
            match a {
                Action::Leave => self.windows[wi].focus = Focus::Terminal,
                Action::Focus => self.windows[wi].focus = Focus::Editor,
                Action::Hide => {
                    self.windows[wi].editor.open = false;
                    self.windows[wi].focus = Focus::Terminal;
                }
                Action::Toast(t, err) => self.toast(t, err),
                Action::ConfirmOverwrite(i) => {
                    let key = self.windows[wi].key;
                    self.modal = Some((key, crate::dialogs::Modal::ConfirmOverwrite(i)));
                }
                Action::ConfirmCloseTab(i) => {
                    let key = self.windows[wi].key;
                    self.modal = Some((key, crate::dialogs::Modal::ConfirmCloseTab(i)));
                }
                Action::Persist => self.persist_widths(),
                Action::PickFile => self.run_action(wi, "toggle_editor"),
            }
        }
    }
}

/// A vertical drag handle on a panel edge; returns the drag delta in x.
pub fn edge_drag(ui: &mut Ui, x: f32, rect: Rect, id: Id) -> (f32, bool) {
    let r = Rect::from_min_max(pos2(x - 3.0, rect.top()), pos2(x + 3.0, rect.bottom()));
    let resp = ui.interact(r, id, Sense::drag());
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        ui.painter().rect_filled(
            Rect::from_min_max(pos2(x - 1.0, rect.top()), pos2(x + 1.0, rect.bottom())),
            0,
            ACC,
        );
    }
    (resp.drag_delta().x, resp.drag_stopped())
}
