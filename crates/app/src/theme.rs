//! Design tokens mapped onto egui — the only file that constructs chrome colors
//! (the terminal's colors come from the user's Display settings, see
//! [`terminal_palette`]). The palette is SecureWire's: dark surfaces in five
//! steps, one accent, four semantic colors.

use egui::style::{Selection, WidgetVisuals};
use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};

use crate::settings::{Display, parse_hex};

pub mod tok {
    use egui::Color32 as C;
    pub const BG0: C = C::from_rgb(0x0c, 0x0f, 0x13);
    pub const BG1: C = C::from_rgb(0x12, 0x16, 0x1b);
    pub const BG2: C = C::from_rgb(0x18, 0x1d, 0x23);
    pub const BG3: C = C::from_rgb(0x20, 0x27, 0x31);
    pub const BG4: C = C::from_rgb(0x2a, 0x32, 0x3d);
    pub const LINE: C = C::from_rgb(0x23, 0x2a, 0x33);
    pub const LINE2: C = C::from_rgb(0x31, 0x3a, 0x46);
    pub const TX0: C = C::from_rgb(0xe8, 0xed, 0xf2);
    pub const TX1: C = C::from_rgb(0xb3, 0xbd, 0xc8);
    pub const TX2: C = C::from_rgb(0x7b, 0x86, 0x96);
    pub const TX3: C = C::from_rgb(0x54, 0x5e, 0x6b);
    pub const ACC: C = C::from_rgb(0x4f, 0xc3, 0xb4);
    pub const ACC_DIM: C = C::from_rgb(0x17, 0x37, 0x34);
    pub const OK: C = C::from_rgb(0x5c, 0xc2, 0x7a);
    pub const WARN: C = C::from_rgb(0xe3, 0xa9, 0x4b);
    pub const WARN_DIM: C = C::from_rgb(0x3a, 0x2c, 0x14);
    pub const ERR: C = C::from_rgb(0xec, 0x64, 0x50);
    pub const ERR_DIM: C = C::from_rgb(0x3d, 0x1a, 0x16);
    pub const INFO: C = C::from_rgb(0x6e, 0xa8, 0xea);
    pub const INFO_DIM: C = C::from_rgb(0x16, 0x27, 0x3b);
    pub const PURPLE: C = C::from_rgb(0xc7, 0x92, 0xea);
    /// Modal backdrop.
    pub const SCRIM: C = C::from_rgba_premultiplied(4, 6, 8, 150);
}

use tok::*;

pub const R: u8 = 3;

/// Height of the top bar and the status bar.
pub const TOP_H: f32 = 36.0;
pub const STATUS_H: f32 = 24.0;
/// Panel header row (EXPLORER, EDITOR).
pub const PANEL_HEADER_H: f32 = 30.0;
/// Per-pane title strip.
pub const PANE_HEADER_H: f32 = 24.0;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Scratchpad ink, indexed by `sv_core::scratch::Item`'s `color`.
pub const INK: [egui::Color32; 6] = [TX0, ACC, INFO, WARN, ERR, PURPLE];

/// Usage meter colour: accent, then warn from 70 %, err from 90 %.
pub fn level(frac: f32) -> egui::Color32 {
    if frac >= 0.9 {
        ERR
    } else if frac >= 0.7 {
        WARN
    } else {
        ACC
    }
}

pub fn visuals() -> Visuals {
    let mut v = Visuals::dark();
    v.panel_fill = BG1;
    v.window_fill = BG1;
    v.extreme_bg_color = BG0;
    v.faint_bg_color = BG2;
    v.code_bg_color = BG2;
    v.window_stroke = Stroke::new(1.0, LINE2);
    v.window_corner_radius = CornerRadius::same(6);
    v.menu_corner_radius = CornerRadius::same(4);
    v.selection = Selection {
        bg_fill: Color32::from_rgb(0x1f, 0x4a, 0x46),
        stroke: Stroke::new(1.0, ACC),
    };
    v.hyperlink_color = ACC;
    v.warn_fg_color = WARN;
    v.error_fg_color = ERR;
    v.text_cursor.stroke = Stroke::new(2.0, ACC);
    // A blinking cursor repaints the whole window several times a second;
    // like the terminal cursor, it stays solid.
    v.text_cursor.blink = false;
    let w = |fill: Color32, stroke: Color32, fg: Color32| WidgetVisuals {
        bg_fill: fill,
        weak_bg_fill: fill,
        bg_stroke: Stroke::new(1.0, stroke),
        fg_stroke: Stroke::new(1.0, fg),
        corner_radius: CornerRadius::same(R),
        expansion: 0.0,
    };
    v.widgets.noninteractive = w(BG1, LINE, TX1);
    v.widgets.inactive = w(BG2, LINE2, TX1);
    v.widgets.hovered = w(BG3, TX3, TX0);
    v.widgets.active = w(BG4, ACC, TX0);
    v.widgets.open = w(BG3, LINE2, TX0);
    v
}

/// The terminal font family: JetBrains Mono, or the user's `fontFile`.
pub fn term_family() -> FontFamily {
    FontFamily::Name("term".into())
}

pub fn term_font(size: f32) -> FontId {
    FontId::new(size, term_family())
}

/// Install fonts, visuals and text styles. `font_file` is the config's optional
/// terminal font (a .ttf/.otf path); a file that can't be read is reported and
/// the bundled font is used.
pub fn install(ctx: &egui::Context, font_file: Option<&str>) -> Option<String> {
    let mut fonts = egui::FontDefinitions::default();
    // JetBrains Mono (NL: no ligatures, which egui would not shape anyway): box
    // drawing, arrows, tabular digits. egui's Hack stays behind it as a fallback.
    fonts.font_data.insert(
        "jetbrains-mono".into(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/JetBrainsMonoNL-Regular.ttf"
        ))),
    );
    let mono = fonts.families.entry(FontFamily::Monospace).or_default();
    mono.insert(0, "jetbrains-mono".into());
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    // Icons must also resolve inside monospace text.
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .push("phosphor".into());

    let mut warning = None;
    let mut term: Vec<String> = Vec::new();
    if let Some(path) = font_file {
        match std::fs::read(path) {
            Ok(bytes) => {
                fonts.font_data.insert(
                    "user-term".into(),
                    std::sync::Arc::new(egui::FontData::from_owned(bytes)),
                );
                term.push("user-term".into());
            }
            Err(e) => warning = Some(format!("fontFile {path}: {e}")),
        }
    }
    term.extend(fonts.families[&FontFamily::Monospace].iter().cloned());
    fonts.families.insert(term_family(), term);
    ctx.set_fonts(fonts);

    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, visuals());
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(11.0)),
            (TextStyle::Body, FontId::proportional(12.5)),
            (TextStyle::Button, FontId::proportional(12.5)),
            (TextStyle::Monospace, FontId::monospace(12.0)),
            (TextStyle::Heading, FontId::proportional(14.0)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 4.0);
        s.spacing.button_padding = egui::vec2(8.0, 3.0);
        s.spacing.interact_size.y = 22.0;
        s.spacing.scroll.bar_width = 6.0;
        s.spacing.scroll.floating = true;
        s.interaction.selectable_labels = false;
        s.animation_time = 0.08;
    });
    load_logo(ctx);
    warning
}

/// Window / taskbar icon.
pub const ICON_PNG: &[u8] = include_bytes!("../../../packaging/icons/splitview.png");
const LOGO_PNG: &[u8] = include_bytes!("../../../packaging/icons/splitview-128.png");

fn logo_id() -> egui::Id {
    egui::Id::new("splitview-logo")
}

fn load_logo(ctx: &egui::Context) {
    let Ok(icon) = eframe::icon_data::from_png_bytes(LOGO_PNG) else {
        return;
    };
    let size = [icon.width as usize, icon.height as usize];
    let img = egui::ColorImage::from_rgba_unmultiplied(size, &icon.rgba);
    let opts = egui::TextureOptions {
        mipmap_mode: Some(egui::TextureFilter::Linear),
        ..egui::TextureOptions::LINEAR
    };
    let tex = ctx.load_texture("logo", img, opts);
    ctx.data_mut(|d| d.insert_temp(logo_id(), tex));
}

/// The SplitView logo at `size` points.
pub fn logo(ui: &mut egui::Ui, size: f32) -> egui::Response {
    let tex: Option<egui::TextureHandle> = ui.ctx().data(|d| d.get_temp(logo_id()));
    let size = egui::vec2(size, size);
    match tex {
        Some(t) => ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(
            t.id(),
            size,
        ))),
        None => ui.allocate_exact_size(size, egui::Sense::hover()).1,
    }
}

pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

pub fn prop(size: f32) -> FontId {
    FontId::proportional(size)
}

/// Colors derived from the user's Display settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Skin {
    pub term: sv_term::Palette,
    pub accent: Color32,
    pub divider: Color32,
    pub divider_active: Color32,
    pub divider_size: f32,
}

fn hex_or(s: &str, fallback: Color32) -> Color32 {
    parse_hex(s).unwrap_or(fallback)
}

pub fn skin(d: &Display) -> Skin {
    let bg = hex_or(&d.bg, BG0);
    let fg = hex_or(&d.fg, Color32::from_rgb(0xc9, 0xd1, 0xd9));
    let cursor = hex_or(&d.cursor, ACC);
    let accent = hex_or(&d.accent, OK);
    use egui::Color32 as C;
    let selection = C::from_rgb(
        lerp(bg.r(), cursor.r(), 0.3),
        lerp(bg.g(), cursor.g(), 0.3),
        lerp(bg.b(), cursor.b(), 0.3),
    );
    Skin {
        term: sv_term::Palette {
            fg,
            bg,
            cursor,
            selection,
            ansi: [
                C::from_rgb(0x1a, 0x1f, 0x26),
                ERR,
                accent,
                WARN,
                INFO,
                PURPLE,
                ACC,
                TX1,
                C::from_rgb(0x54, 0x5e, 0x6b),
                C::from_rgb(0xf3, 0x8a, 0x7a),
                C::from_rgb(
                    lerp(accent.r(), 255, 0.25),
                    lerp(accent.g(), 255, 0.25),
                    lerp(accent.b(), 255, 0.25),
                ),
                C::from_rgb(0xef, 0xc1, 0x72),
                C::from_rgb(0x93, 0xc0, 0xf2),
                C::from_rgb(0xd9, 0xb0, 0xf2),
                C::from_rgb(0x7a, 0xd6, 0xca),
                TX0,
            ],
        },
        accent: cursor,
        divider: hex_or(&d.divider, LINE),
        divider_active: hex_or(&d.divider_active, ACC),
        divider_size: d.divider_size.clamp(1.0, 4.0),
    }
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}

/// Syntax colors for the editor.
pub fn syntax(kind: sv_core::highlight::Kind) -> Color32 {
    use sv_core::highlight::Kind;
    match kind {
        Kind::Plain => Color32::from_rgb(0xc9, 0xd1, 0xd9),
        Kind::Comment => TX3,
        Kind::String => OK,
        Kind::Number => WARN,
        Kind::Keyword => PURPLE,
        Kind::Type => ACC,
        Kind::Function => INFO,
        Kind::Tag => Color32::from_rgb(0xf0, 0x7f, 0x9c),
    }
}
