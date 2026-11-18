//! The split area: maps the layout tree to pixels, draws the thin dividers
//! (draggable), the per-pane title strips, and each pane's terminal.

use egui::emath::GuiRounding;
use egui::{Align2, CursorIcon, Id, Rect, Sense, Ui, pos2, vec2};
use egui_phosphor::regular as ph;
use sv_core::layout::{Orientation, PaneId};
use sv_core::paths::compact_path;
use sv_term::view::ShowOpts;

use crate::app::{App, Focus};
use crate::theme::{self, tok::*};

const EPS: f32 = 0.5;

impl App {
    pub fn show_center(&mut self, ui: &mut Ui, wi: usize) {
        let bg = self.skin.term.bg;
        egui::CentralPanel::no_frame()
            .frame(egui::Frame::NONE.fill(bg))
            .show(ui, |ui| {
                let area = ui.max_rect();
                if let Some(err) = &self.startup_error {
                    empty_state(ui, area, "No shell could be started", err);
                    return;
                }
                if self.windows[wi].layout.is_empty() {
                    let hint = if self.modal.is_some() {
                        "Pick a shell to start"
                    } else {
                        "Prefix p opens a pane with any shell"
                    };
                    empty_state(ui, area, "No pane", hint);
                    return;
                }
                self.show_panes(ui, wi, area);
            });
    }

    fn show_panes(&mut self, ui: &mut Ui, wi: usize, area: Rect) {
        let d = self.skin.divider_size;
        let key = self.windows[wi].key;
        let modal_here =
            self.modal.as_ref().is_some_and(|(k, _)| *k == key) || !self.pairing.is_empty();
        let layout = self.windows[wi].layout.clone();
        let focused = layout.focused;
        let w_focus = self.windows[wi].focus;
        let headers = self.settings.display.pane_headers;
        let px = |x: f32, y: f32| {
            pos2(
                area.left() + x * area.width(),
                area.top() + y * area.height(),
            )
        };

        let rects: Vec<(PaneId, Rect)> = match layout.zoomed {
            Some(z) => vec![(z, area)],
            None => layout
                .rects()
                .into_iter()
                .map(|(id, n)| {
                    let mut r = Rect::from_min_max(px(n.x, n.y), px(n.x + n.w, n.y + n.h));
                    // Interior edges give up half the divider each.
                    if r.left() > area.left() + EPS {
                        r.min.x += d / 2.0;
                    }
                    if r.right() < area.right() - EPS {
                        r.max.x -= d / 2.0;
                    }
                    if r.top() > area.top() + EPS {
                        r.min.y += d / 2.0;
                    }
                    if r.bottom() < area.bottom() - EPS {
                        r.max.y -= d / 2.0;
                    }
                    (id, r.round_to_pixels(ui.pixels_per_point()))
                })
                .collect(),
        };
        let many = rects.len() > 1;

        // ---- panes ----
        let mut focus_to: Option<PaneId> = None;
        let mut header_action: Option<(PaneId, &'static str)> = None;
        let mut open_url = None;
        for &(id, r) in &rects {
            let is_focused = Some(id) == focused;
            let Some(pane) = self.panes.get_mut(&id) else {
                continue;
            };
            let mut body = r;
            if headers {
                let h = Rect::from_min_size(r.min, vec2(r.width(), theme::PANE_HEADER_H));
                body.min.y = h.max.y;
                if let Some(a) = pane_header(ui, h, pane, is_focused && many, &self.skin, key) {
                    header_action = Some((id, a));
                }
            }
            let take_focus = is_focused && w_focus == Focus::Terminal && !modal_here;
            let out = pane.view.show(
                ui,
                body,
                &pane.term,
                &self.skin.term,
                ShowOpts {
                    take_focus,
                    interactive: !modal_here,
                    dim: self.settings.display.dim_inactive && many && !is_focused,
                    pad: vec2(8.0, 4.0),
                },
            );
            if !out.input.is_empty() {
                let _ = self.hub.write(id, &out.input);
            }
            if let Some(s) = out.resized {
                let _ = self.hub.resize(id, s);
            }
            if out.pressed && (!is_focused || w_focus != Focus::Terminal) {
                focus_to = Some(id);
            }
            if out.open_url.is_some() {
                open_url = out.open_url;
            }
        }

        // ---- dividers ----
        // After the panes, on purpose: egui gives a drag to the topmost widget
        // under the pointer, and each terminal senses drags (text selection)
        // over its whole body. Registered first, a divider's grab band lost to
        // the terminals on both sides and only its 1 px line could be grabbed.
        if layout.zoomed.is_none() {
            for s in layout.splits() {
                let top_left = px(s.rect.x, s.rect.y);
                let bottom_right = px(s.rect.x + s.rect.w, s.rect.y + s.rect.h);
                let split_px = Rect::from_min_max(top_left, bottom_right);
                let line = match s.orientation {
                    Orientation::Row => {
                        let x = split_px.left() + split_px.width() * s.ratio;
                        Rect::from_min_max(
                            pos2(x - d / 2.0, split_px.top()),
                            pos2(x + d / 2.0, split_px.bottom()),
                        )
                    }
                    Orientation::Column => {
                        let y = split_px.top() + split_px.height() * s.ratio;
                        Rect::from_min_max(
                            pos2(split_px.left(), y - d / 2.0),
                            pos2(split_px.right(), y + d / 2.0),
                        )
                    }
                };
                let grab = line.expand(3.0);
                let resp = ui.interact(grab, Id::new(("divider", key, &s.path)), Sense::drag());
                let cursor = match s.orientation {
                    Orientation::Row => CursorIcon::ResizeHorizontal,
                    Orientation::Column => CursorIcon::ResizeVertical,
                };
                let active = resp.hovered() || resp.dragged();
                if active {
                    ui.ctx().set_cursor_icon(cursor);
                }
                if resp.dragged()
                    && let Some(p) = resp.interact_pointer_pos()
                {
                    let ratio = match s.orientation {
                        Orientation::Row => (p.x - split_px.left()) / split_px.width(),
                        Orientation::Column => (p.y - split_px.top()) / split_px.height(),
                    };
                    let w = &mut self.windows[wi];
                    w.layout = w.layout.set_ratio(&s.path, ratio);
                }
                ui.painter().rect_filled(
                    line,
                    0,
                    if active {
                        self.skin.divider_active
                    } else {
                        self.skin.divider
                    },
                );
            }
        }

        // The focused pane's interior edges carry the active divider color.
        if many
            && layout.zoomed.is_none()
            && let Some((_, r)) = rects.iter().find(|(id, _)| Some(*id) == focused)
        {
            let c = self.skin.divider_active;
            let p = ui.painter();
            let half = d / 2.0;
            if r.left() > area.left() + EPS {
                p.rect_filled(
                    Rect::from_min_max(
                        pos2(r.left() - d, r.top()),
                        pos2(r.left() - d + d.max(1.0), r.bottom()),
                    ),
                    0,
                    c,
                );
            }
            if r.right() < area.right() - EPS {
                p.rect_filled(
                    Rect::from_min_max(
                        pos2(r.right(), r.top()),
                        pos2(r.right() + d.max(1.0), r.bottom()),
                    ),
                    0,
                    c,
                );
            }
            if r.top() > area.top() + EPS {
                p.rect_filled(
                    Rect::from_min_max(
                        pos2(r.left(), r.top() - d),
                        pos2(r.right(), r.top() - d + d.max(1.0)),
                    ),
                    0,
                    c,
                );
            }
            if r.bottom() < area.bottom() - EPS {
                p.rect_filled(
                    Rect::from_min_max(
                        pos2(r.left(), r.bottom()),
                        pos2(r.right(), r.bottom() + d.max(1.0)),
                    ),
                    0,
                    c,
                );
            }
            let _ = half;
        }
        if layout.zoomed.is_some() && layout.leaves().len() > 1 {
            // Zoomed: say so, and how many panes are hidden.
            let n = layout.leaves().len() - 1;
            let text = format!("{}  ZOOM · {n} hidden", ph::ARROWS_OUT_SIMPLE);
            let g = ui.painter().layout_no_wrap(text, theme::prop(10.5), ACC);
            let at = pos2(
                area.right() - g.size().x - 14.0,
                area.bottom() - g.size().y - 10.0,
            );
            let bgr = Rect::from_min_size(at, g.size()).expand2(vec2(7.0, 3.0));
            ui.painter().rect_filled(bgr, 3, ACC_DIM);
            ui.painter().galley(at, g, ACC);
        }

        if let Some(id) = focus_to {
            let w = &mut self.windows[wi];
            w.layout = w.layout.focus(id);
            w.focus = Focus::Terminal;
            self.sync_sidebar(wi);
        }
        if let Some((id, a)) = header_action {
            let w = &mut self.windows[wi];
            w.layout = w.layout.focus(id);
            w.focus = Focus::Terminal;
            match a {
                "split_right" | "split_down" | "toggle_zoom" | "close_pane" => {
                    self.run_action(wi, a)
                }
                _ => {}
            }
        }
        if let Some(url) = open_url {
            open_external(&url);
        }
    }
}

/// The thin strip above a pane: what runs there and where; tools on hover.
fn pane_header(
    ui: &mut Ui,
    r: Rect,
    pane: &crate::app::Pane,
    focused: bool,
    skin: &theme::Skin,
    key: u64,
) -> Option<&'static str> {
    let resp = ui.interact(r, Id::new(("pane-header", key, pane.id)), Sense::click());
    let p = ui.painter_at(r);
    p.rect_filled(r, 0, if focused { skin.term.bg } else { BG1 });
    p.rect_filled(
        Rect::from_min_max(pos2(r.left(), r.bottom() - 1.0), r.max),
        0,
        LINE,
    );
    if focused {
        p.rect_filled(
            Rect::from_min_size(r.min, vec2(r.width(), 2.0)),
            0,
            skin.accent,
        );
    }
    let y = r.center().y + 0.5;
    let fg = if focused { TX0 } else { TX2 };
    p.text(
        pos2(r.left() + 10.0, y),
        Align2::LEFT_CENTER,
        ph::TERMINAL_WINDOW,
        theme::prop(13.0),
        if focused { skin.accent } else { TX3 },
    );
    let title: String = pane.title().chars().take(60).collect();
    let tg = ui.painter().layout_no_wrap(title, theme::prop(12.0), fg);
    let tw = tg.size().x;
    p.galley(pos2(r.left() + 28.0, y - tg.size().y / 2.0), tg, fg);
    let x = r.left() + 28.0 + tw + 10.0;

    // Tools (right): visible when hovered or focused.
    let hovered = resp.hovered() || ui.rect_contains_pointer(r);
    let mut action = None;
    let mut right = r.right() - 4.0;
    if hovered || focused {
        for (icon, tip, act) in [
            (ph::X, "Close pane (prefix x)", "close_pane"),
            (ph::ARROWS_OUT_SIMPLE, "Zoom (prefix z)", "toggle_zoom"),
            (
                ph::SQUARE_SPLIT_VERTICAL,
                "Split down (prefix -)",
                "split_down",
            ),
            (
                ph::SQUARE_SPLIT_HORIZONTAL,
                "Split right (prefix \\)",
                "split_right",
            ),
        ] {
            let br = Rect::from_center_size(pos2(right - 10.0, y), vec2(20.0, 18.0));
            right -= 22.0;
            if br.left() < x + 40.0 {
                break;
            }
            let b = ui.interact(
                br,
                Id::new(("pane-tool", key, pane.id, act)),
                Sense::click(),
            );
            if b.hovered() {
                ui.painter().rect_filled(br, 3, BG3);
            }
            ui.painter().text(
                br.center(),
                Align2::CENTER_CENTER,
                icon,
                theme::prop(12.5),
                if b.hovered() { TX0 } else { TX3 },
            );
            if b.on_hover_text(tip).clicked() {
                action = Some(act);
            }
        }
    }
    let size = pane.term.size();
    let size_text = format!("{}×{}", size.cols, size.rows);
    let sg = ui
        .painter()
        .layout_no_wrap(size_text, theme::mono(10.5), TX3);
    let sx = right - sg.size().x - 6.0;
    if sx > x + 40.0 {
        p.galley(pos2(sx, y - sg.size().y / 2.0), sg, TX3);
        right = sx - 10.0;
    }
    if let Some(cwd) = &pane.cwd {
        // Shrink the path until it fits between the title and the size.
        let char_w = ui.fonts_mut(|f| f.glyph_width(&theme::mono(11.0), '0'));
        let room = ((right - x) / char_w).max(0.0) as usize;
        if room > 8 {
            let shown = compact_path(cwd, room);
            let cg = ui.painter().layout_no_wrap(shown, theme::mono(11.0), TX3);
            if x + cg.size().x <= right {
                p.galley(pos2(x, y - cg.size().y / 2.0), cg, TX3);
            }
        }
    }
    if action.is_none() && (resp.clicked() || resp.double_clicked()) {
        if resp.double_clicked() {
            return Some("toggle_zoom");
        }
        return Some("focus");
    }
    action
}

fn empty_state(ui: &mut Ui, area: Rect, title: &str, detail: &str) {
    let c = area.center();
    let p = ui.painter();
    p.text(
        c - vec2(0.0, 14.0),
        Align2::CENTER_CENTER,
        title,
        theme::prop(15.0),
        TX1,
    );
    p.text(
        c + vec2(0.0, 12.0),
        Align2::CENTER_CENTER,
        detail,
        theme::prop(12.0),
        TX3,
    );
}

/// Open an http(s) URL in the system browser. The URL came from terminal text,
/// so it is passed as a single argument and never through a shell.
pub fn open_external(url: &str) {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return;
    }
    #[cfg(windows)]
    let r = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    let _ = r;
}
