//! Small, cheap widgets drawn with plain `Painter` calls (after SecureWire's).

use egui::{
    Align, Align2, Color32, Layout, Rect, Response, RichText, Sense, Stroke, Ui, UiBuilder, pos2,
    vec2,
};

use crate::theme::{self, R, tok::*};

/// Full-width 1 px hairline at the cursor.
pub fn hline(ui: &mut Ui) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r, 0, LINE);
}

pub fn dim(text: impl Into<String>) -> RichText {
    RichText::new(text).color(TX2)
}

pub fn faint(text: impl Into<String>) -> RichText {
    RichText::new(text).color(TX3).size(11.0)
}

pub fn strong(text: impl Into<String>) -> RichText {
    RichText::new(text).color(TX0)
}

/// Small uppercase pill: `PREFIX`, `2 BG`, `LIVE`.
pub fn badge(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_uppercase(), theme::prop(10.0), fg);
    let size = galley.size() + vec2(10.0, 3.0);
    let (r, resp) = ui.allocate_exact_size(vec2(size.x, 16.0), Sense::click());
    ui.painter().rect_filled(r, R, bg);
    ui.painter()
        .galley(r.center() - galley.size() / 2.0, galley, fg);
    resp
}

/// Frameless icon button (24×22). `active` paints it in the accent.
pub fn icon_button(ui: &mut Ui, icon: &str, tooltip: &str, active: bool) -> Response {
    let (r, resp) = ui.allocate_exact_size(vec2(24.0, 22.0), Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.painter().rect_filled(r, R, BG3);
    }
    let c = if active {
        ACC
    } else if hovered {
        TX0
    } else {
        TX2
    };
    ui.painter().text(
        r.center(),
        Align2::CENTER_CENTER,
        icon,
        theme::prop(14.0),
        c,
    );
    resp.on_hover_text(tooltip)
}

/// Toggle switch with label.
pub fn switch(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let color = if *on { TX0 } else { TX1 };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), theme::prop(12.5), color);
    let (r, mut resp) = ui.allocate_exact_size(vec2(34.0 + galley.size().x, 20.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let track = Rect::from_min_size(pos2(r.left(), r.center().y - 7.0), vec2(26.0, 14.0));
    let p = ui.painter();
    p.rect_filled(track, 7, if *on { ACC_DIM } else { BG4 });
    let knob_x = if *on {
        track.right() - 7.0
    } else {
        track.left() + 7.0
    };
    p.circle_filled(
        pos2(knob_x, track.center().y),
        5.0,
        if *on { ACC } else { TX2 },
    );
    p.galley(
        pos2(track.right() + 8.0, r.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    resp
}

/// A key cap: `Ctrl A`, `s`.
pub fn kbd(ui: &mut Ui, text: &str) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), theme::mono(11.0), TX1);
    let (r, resp) = ui.allocate_exact_size(vec2(galley.size().x + 10.0, 18.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(r, R, BG3);
    p.rect_stroke(r, R, Stroke::new(1.0, LINE2), egui::StrokeKind::Inside);
    p.galley(r.center() - galley.size() / 2.0, galley, TX1);
    resp
}

/// Small mono label on a `bg-3` pill.
pub fn chip(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), theme::mono(11.0), color);
    let (r, resp) = ui.allocate_exact_size(vec2(galley.size().x + 12.0, 18.0), Sense::hover());
    ui.painter().rect_filled(r, R, BG3);
    ui.painter()
        .galley(r.center() - galley.size() / 2.0, galley, color);
    resp
}

/// Uppercase panel title row (30 px) with right-aligned tools.
pub fn panel_header(ui: &mut Ui, title: &str, tools: impl FnOnce(&mut Ui)) {
    let (rect, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme::PANEL_HEADER_H),
        Sense::hover(),
    );
    ui.painter().text(
        pos2(rect.left() + 12.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        theme::prop(11.0),
        TX2,
    );
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(rect.shrink2(vec2(6.0, 0.0)))
            .layout(Layout::right_to_left(Align::Center)),
        tools,
    );
    hline(ui);
}

/// A primary (accent) button.
pub fn primary(ui: &mut Ui, text: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(BG0).strong())
            .fill(ACC)
            .stroke(Stroke::NONE),
    )
}

/// Status-bar usage meter: `ctx ▬▬▭ 57%`, coloured by level (`None`: no data).
/// One allocation, so it lays out the same in either direction.
pub fn meter(ui: &mut Ui, label: &str, frac: Option<f32>) -> Rect {
    const BAR: f32 = 32.0;
    let p = ui.painter();
    let lg = p.layout_no_wrap(label.to_owned(), theme::prop(10.5), TX3);
    let text = frac.map_or("–".to_owned(), |f| format!("{:.0}%", f * 100.0));
    let color = frac.map_or(TX3, theme::level);
    let vg = p.layout_no_wrap(text, theme::mono(10.5), TX1);
    let wdt = lg.size().x + 5.0 + BAR + 5.0 + vg.size().x.max(24.0);
    let (r, _) = ui.allocate_exact_size(vec2(wdt, theme::STATUS_H), Sense::hover());
    let p = ui.painter();
    let cy = r.center().y;
    p.galley(pos2(r.left(), cy - lg.size().y / 2.0), lg.clone(), TX3);
    let x = r.left() + lg.size().x + 5.0;
    let track = Rect::from_min_size(pos2(x, cy - 2.0), vec2(BAR, 4.0));
    p.rect_filled(track, 2, BG4);
    if let Some(f) = frac {
        let fill = Rect::from_min_size(track.min, vec2(BAR * f.clamp(0.0, 1.0), 4.0));
        p.rect_filled(fill, 2, color);
    }
    p.galley(pos2(track.right() + 5.0, cy - vg.size().y / 2.0), vg, TX1);
    r
}
