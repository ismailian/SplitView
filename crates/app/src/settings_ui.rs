//! The Settings window: a modal with a section list on the left. Changes apply
//! live and are saved as they are made (prompt-related ones reach new panes).

use std::time::Instant;

use egui::{Align, Color32, Frame, Key, Layout, Margin, RichText, Sense, Ui, vec2};
use egui_phosphor::regular as ph;
use sv_core::keymap::{ACTIONS, chords_for, pretty};
use sv_core::paths::prompt_dir;

use crate::app::App;
use crate::remote::{BindMode, RemoteStatus};
use crate::settings::{AliasS, MAX_DIR_SEGMENTS, PRESETS, parse_hex, to_hex};
use crate::theme::{self, tok::*};
use crate::widgets as w;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Section {
    #[default]
    Display,
    Panes,
    Keys,
    Aliases,
    Prompt,
    Banner,
    Shell,
    Remote,
    About,
}

const SECTIONS: &[(Section, &str, &str)] = &[
    (Section::Display, ph::PALETTE, "Display"),
    (Section::Panes, ph::SIDEBAR_SIMPLE, "Panes"),
    (Section::Keys, ph::KEYBOARD, "Keybindings"),
    (Section::Aliases, ph::ARROW_BEND_DOWN_RIGHT, "Aliases"),
    (Section::Prompt, ph::INFO, "Terminal info"),
    (Section::Banner, ph::ROWS, "Banner"),
    (Section::Shell, ph::TERMINAL_WINDOW, "Shell"),
    (Section::Remote, ph::BROADCAST, "Remote"),
    (Section::About, ph::QUESTION, "About"),
];

#[derive(Default)]
pub struct SettingsState {
    pub section: Section,
    /// Remote status polled while the Remote section is open (countdown).
    pub(crate) status: Option<(RemoteStatus, Option<Instant>)>,
    pub(crate) error: Option<String>,
}

fn heading(ui: &mut Ui, text: &str, sub: &str) {
    ui.label(RichText::new(text).size(15.0).color(TX0).strong());
    if !sub.is_empty() {
        ui.label(w::dim(sub));
    }
    ui.add_space(10.0);
}

fn field(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(150.0, 22.0), Sense::hover());
        ui.painter().text(
            r.left_center(),
            egui::Align2::LEFT_CENTER,
            label,
            theme::prop(12.5),
            TX1,
        );
        add(ui);
    });
    ui.add_space(4.0);
}

fn color_field(ui: &mut Ui, label: &str, hex: &mut String) {
    field(ui, label, |ui| {
        let mut c = parse_hex(hex).unwrap_or(Color32::BLACK);
        if egui::color_picker::color_edit_button_srgba(
            ui,
            &mut c,
            egui::color_picker::Alpha::Opaque,
        )
        .changed()
        {
            *hex = to_hex(c);
        }
        ui.add(
            egui::TextEdit::singleline(hex)
                .desired_width(80.0)
                .font(theme::mono(11.5)),
        );
    });
}

impl App {
    pub fn settings_window(
        &mut self,
        ctx: &egui::Context,
        wi: usize,
        st: &mut SettingsState,
    ) -> bool {
        let mut open = true;
        // Escape closes (unless a text field has the keyboard).
        if !ctx.memory(|m| m.focused().is_some())
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
        {
            open = false;
        }
        let before = self.settings.clone();
        let screen = ctx.content_rect();
        let size = vec2(
            820.0f32.min(screen.width() - 40.0),
            560.0f32.min(screen.height() - 60.0),
        );
        crate::dialogs::dialog(ctx, "settings", size.x, |ui| {
            // Title bar.
            Frame::NONE
                .inner_margin(Margin {
                    left: 16,
                    right: 8,
                    top: 10,
                    bottom: 8,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        theme::logo(ui, 16.0);
                        ui.label(RichText::new("Settings").size(14.5).color(TX0).strong());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if w::icon_button(ui, ph::X, "Close (Esc)", false).clicked() {
                                open = false;
                            }
                        });
                    });
                });
            w::hline(ui);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                // Nav.
                Frame::NONE
                    .fill(BG2)
                    .inner_margin(Margin::symmetric(6, 8))
                    .show(ui, |ui| {
                        ui.set_width(170.0);
                        ui.set_height(size.y - 50.0);
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.vertical(|ui| {
                            for (s, icon, label) in SECTIONS {
                                let on = st.section == *s;
                                let (r, resp) =
                                    ui.allocate_exact_size(vec2(158.0, 28.0), Sense::click());
                                if on {
                                    ui.painter().rect_filled(r, 3, BG3);
                                    ui.painter().rect_filled(
                                        egui::Rect::from_min_size(r.min, vec2(2.0, r.height())),
                                        0,
                                        ACC,
                                    );
                                } else if resp.hovered() {
                                    ui.painter().rect_filled(r, 3, BG3);
                                }
                                ui.painter().text(
                                    r.left_center() + vec2(12.0, 0.0),
                                    egui::Align2::LEFT_CENTER,
                                    *icon,
                                    theme::prop(14.0),
                                    if on { ACC } else { TX2 },
                                );
                                ui.painter().text(
                                    r.left_center() + vec2(34.0, 0.0),
                                    egui::Align2::LEFT_CENTER,
                                    *label,
                                    theme::prop(12.5),
                                    if on { TX0 } else { TX1 },
                                );
                                if resp.clicked() {
                                    st.section = *s;
                                }
                            }
                        });
                    });
                let rest = vec2(size.x - 170.0, size.y - 50.0);
                ui.allocate_ui_with_layout(rest, Layout::top_down(Align::Min), |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("settings-body")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            Frame::NONE
                                .inner_margin(Margin::symmetric(22, 16))
                                .show(ui, |ui| {
                                    ui.spacing_mut().item_spacing = vec2(8.0, 4.0);
                                    ui.set_width(rest.x - 44.0);
                                    match st.section {
                                        Section::Display => self.section_display(ui),
                                        Section::Panes => self.section_panes(ui),
                                        Section::Keys => self.section_keys(ui),
                                        Section::Aliases => self.section_aliases(ui),
                                        Section::Prompt => self.section_prompt(ui),
                                        Section::Banner => self.section_banner(ui),
                                        Section::Shell => self.section_shell(ui),
                                        Section::Remote => self.section_remote(ui, st),
                                        Section::About => self.section_about(ui),
                                    }
                                });
                        });
                });
            });
        });
        if self.settings != before {
            self.apply_settings();
        }
        if !open {
            self.focus_terminal(wi);
        }
        open
    }

    fn section_display(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Display",
            "Colors, font size and dividers. Changes apply immediately.",
        );
        ui.horizontal_wrapped(|ui| {
            for p in PRESETS {
                let bg = parse_hex(p.bg).unwrap_or(BG0);
                let acc = parse_hex(p.cursor).unwrap_or(ACC);
                let (r, resp) = ui.allocate_exact_size(vec2(96.0, 46.0), Sense::click());
                let d = &self.settings.display;
                let on = d.bg == p.bg && d.cursor == p.cursor;
                ui.painter().rect_filled(r, 4, bg);
                ui.painter().rect_stroke(
                    r,
                    4,
                    egui::Stroke::new(if on { 2.0 } else { 1.0 }, if on { acc } else { LINE2 }),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    r.left_top() + vec2(10.0, 9.0),
                    egui::Align2::LEFT_TOP,
                    p.name,
                    theme::prop(12.0),
                    parse_hex(p.fg).unwrap_or(TX0),
                );
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(r.left_bottom() + vec2(10.0, -14.0), vec2(30.0, 4.0)),
                    2,
                    acc,
                );
                if resp.clicked() {
                    let d = &mut self.settings.display;
                    d.bg = p.bg.into();
                    d.fg = p.fg.into();
                    d.cursor = p.cursor.into();
                    d.accent = p.accent.into();
                    d.divider = p.divider.into();
                    d.divider_active = p.divider_active.into();
                }
            }
        });
        ui.add_space(12.0);
        let d = &mut self.settings.display;
        color_field(ui, "Background", &mut d.bg);
        color_field(ui, "Text", &mut d.fg);
        color_field(ui, "Cursor", &mut d.cursor);
        color_field(ui, "Accent (green)", &mut d.accent);
        color_field(ui, "Divider", &mut d.divider);
        color_field(ui, "Active divider", &mut d.divider_active);
        ui.add_space(8.0);
        field(ui, "Font size", |ui| {
            ui.add(
                egui::Slider::new(&mut d.font_size, sv_core::config::FONT_MIN..=28.0)
                    .step_by(1.0)
                    .suffix(" px"),
            );
        });
        field(ui, "Divider thickness", |ui| {
            ui.add(
                egui::Slider::new(&mut d.divider_size, 1.0..=4.0)
                    .step_by(1.0)
                    .suffix(" px"),
            );
        });
        ui.add_space(6.0);
        w::switch(
            ui,
            &mut d.pane_headers,
            "Pane title bars (shell, directory, size)",
        );
        ui.add_space(4.0);
        w::switch(ui, &mut d.dim_inactive, "Dim panes that don't have focus");
        ui.add_space(10.0);
        ui.label(w::faint(format!(
            "Terminal font: JetBrains Mono (bundled). Set fontFile in {} to use another.",
            "config.toml"
        )));
    }

    fn section_panes(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Panes",
            "Which side panels a new window opens with. Each still toggles from the top bar or its key.",
        );
        let p = &mut self.settings.panes;
        let row = |ui: &mut Ui, on: &mut bool, label: &str, key: &str| {
            ui.horizontal(|ui| {
                w::switch(ui, on, label);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    w::kbd(ui, key);
                    ui.label(w::faint("prefix"));
                });
            });
            ui.add_space(6.0);
        };
        row(ui, &mut p.explorer, "Explorer (files, left)", "e");
        row(ui, &mut p.editor, "Editor (right)", "Shift E");
        row(ui, &mut p.scratchpad, "Scratchpad (right)", "w");
        ui.add_space(10.0);
        heading(ui, "Editor", "");
        let mut wrap = self.settings.ui.editor_wrap;
        if w::switch(ui, &mut wrap, "Wrap long lines (Alt+Z in the editor)").changed() {
            self.settings.ui.editor_wrap = wrap;
            for win in &mut self.windows {
                win.editor.wrap = wrap;
            }
        }
        ui.add_space(10.0);
        heading(ui, "Scratchpad", "");
        ui.label(w::dim(
            "One pad per project: the git work tree of the focused pane, else its folder.              Saved automatically next to settings.json, in scratchpads/.",
        ));
    }

    fn section_keys(&mut self, ui: &mut Ui) {
        let km = self.keymap.config().clone();
        heading(
            ui,
            "Keybindings",
            &format!(
                "Press the prefix ({}), then a key. Override them under [keybindings] in config.toml.",
                pretty(&km.prefix)
            ),
        );
        let mut cat = "";
        for (name, label, c) in ACTIONS {
            if *c != cat {
                cat = c;
                ui.add_space(8.0);
                ui.label(RichText::new(c.to_uppercase()).size(10.5).color(TX2));
                ui.add_space(2.0);
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new(*label).color(TX1));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    for (pre, chord) in chords_for(&km, name).into_iter().rev() {
                        let text = if pre {
                            pretty(chord)
                        } else {
                            format!("{} (global)", pretty(chord))
                        };
                        w::kbd(ui, &text);
                    }
                });
            });
        }
    }

    fn section_aliases(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Aliases",
            "Defined in every new pane's shell (bash, zsh, PowerShell, cmd).",
        );
        let mut remove = None;
        for (i, a) in self.settings.aliases.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut a.name)
                        .hint_text("name")
                        .desired_width(110.0)
                        .font(theme::mono(12.0)),
                );
                ui.label(w::dim("→"));
                ui.add(
                    egui::TextEdit::singleline(&mut a.command)
                        .hint_text("command")
                        .desired_width(ui.available_width() - 34.0)
                        .font(theme::mono(12.0)),
                );
                if w::icon_button(ui, ph::TRASH, "Remove", false).clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            self.settings.aliases.remove(i);
        }
        ui.add_space(6.0);
        if ui.button(format!("{} Add alias", ph::PLUS)).clicked() {
            self.settings.aliases.push(AliasS::default());
        }
        ui.add_space(6.0);
        ui.label(w::faint(
            "Names may use letters, digits, - and _. New panes pick changes up.",
        ));
    }

    fn section_prompt(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Terminal info",
            "What the prompt's first line shows (new panes).",
        );
        let p = &mut self.settings.prompt_info;
        for (on, label) in [
            (&mut p.user, "User"),
            (&mut p.host, "Computer name"),
            (&mut p.dir, "Working directory"),
            (&mut p.branch, "Git branch"),
            (&mut p.time, "Clock"),
            (&mut p.exit_status, "Exit status of failed commands"),
        ] {
            w::switch(ui, on, label);
            ui.add_space(3.0);
        }
        ui.add_space(8.0);
        field(ui, "Directory detail", |ui| {
            let mut full = p.dir_segments == 0;
            if ui.radio(full, "Full path").clicked() {
                full = true;
                p.dir_segments = 0;
            }
            if ui.radio(!full, "Last").clicked() && full {
                p.dir_segments = 2;
            }
            let mut n = p.dir_segments.max(1);
            if ui
                .add_enabled(
                    !full,
                    egui::DragValue::new(&mut n).range(1..=MAX_DIR_SEGMENTS),
                )
                .changed()
            {
                p.dir_segments = n;
            }
            ui.label(w::dim("segments"));
        });
        let sample = "~/projects/project-x/mobile-app";
        let user = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "you".into());
        let host = crate::remote::identity::host_name();
        let mut line = String::from("┌─");
        if p.user || p.host {
            let who = match (p.user, p.host) {
                (true, true) => format!("{user}@{host}"),
                (true, false) => user.clone(),
                _ => host.clone(),
            };
            line.push_str(&format!("[{who}]─"));
        }
        if p.dir {
            line.push_str(&format!(
                "[{}]",
                prompt_dir(sample, p.dir_segments as usize)
            ));
        }
        if p.branch {
            line.push_str("─(main)");
        }
        if p.time {
            line.push_str("─[12:34]");
        }
        ui.add_space(8.0);
        Frame::NONE
            .fill(self.skin.term.bg)
            .corner_radius(4)
            .inner_margin(Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(line).font(theme::term_font(13.0)).color(ERR));
                ui.label(
                    RichText::new("└──╼ $ ")
                        .font(theme::term_font(13.0))
                        .color(ERR),
                );
            });
    }

    fn section_banner(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Banner",
            "Printed once in each new pane, before the first prompt. \\e[…m colors work.",
        );
        ui.add(
            egui::TextEdit::multiline(&mut self.settings.banner)
                .font(theme::mono(12.0))
                .desired_rows(10)
                .desired_width(f32::INFINITY),
        );
    }

    fn section_shell(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Shell",
            "What new panes run. Prefix p opens a pane with any of these.",
        );
        if let Some(s) = &self.config.shell {
            ui.label(
                RichText::new(format!(
                    "config.toml sets shell = \"{s}\", which outranks this choice."
                ))
                .color(WARN),
            );
            ui.add_space(6.0);
        }
        let current = self.settings.shell.default_profile.clone();
        let mut pick = None;
        for p in &self.profiles {
            ui.horizontal(|ui| {
                if ui
                    .radio(
                        current.as_deref() == Some(p.id.as_str()),
                        RichText::new(&p.name).color(TX0),
                    )
                    .clicked()
                {
                    pick = Some(p.id.clone());
                }
                ui.label(RichText::new(&p.program).monospace().size(11.0).color(TX3));
            });
        }
        if let Some(id) = pick {
            self.settings.shell.default_profile = Some(id);
        }
        ui.add_space(8.0);
        if ui
            .button(format!("{} Detect again", ph::ARROW_CLOCKWISE))
            .clicked()
        {
            self.profiles = crate::profiles::detect();
        }
    }

    fn section_remote(&mut self, ui: &mut Ui, st: &mut SettingsState) {
        heading(
            ui,
            "Remote control",
            "Let a paired phone watch your panes, type into them, and open or close them. Off unless you turn it on.",
        );
        // Keep the status fresh (countdown, sessions) while this is on screen.
        let fresh = st
            .status
            .as_ref()
            .is_none_or(|(_, t)| t.is_none_or(|t| t.elapsed().as_millis() > 900));
        if fresh {
            st.status = Some((self.hub.remote.status(), Some(Instant::now())));
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
        }
        let status = st
            .status
            .as_ref()
            .map(|s| s.0.clone())
            .unwrap_or_else(|| self.remote_status.clone());
        let mut enabled = status.enabled;
        let hub = self.hub.clone();
        let label = if enabled {
            "Remote control is on"
        } else {
            "Remote control is off"
        };
        if w::switch(ui, &mut enabled, label).changed() {
            let r = if enabled {
                hub.remote.enable(&hub, self.settings.remote.config())
            } else {
                hub.remote.disable(&hub)
            };
            match r {
                Ok(s) => {
                    self.remote_status = s.clone();
                    st.status = Some((s, Some(Instant::now())));
                    st.error = None;
                }
                Err(e) => st.error = Some(e.to_string()),
            }
        }
        if let Some(e) = &st.error {
            ui.label(RichText::new(e).color(ERR));
        }
        ui.add_space(8.0);
        let rs = &mut self.settings.remote;
        field(ui, "Listen on", |ui| {
            ui.add_enabled_ui(!status.enabled, |ui| {
                ui.radio_value(&mut rs.bind, BindMode::Lan, "Local network");
                ui.radio_value(
                    &mut rs.bind,
                    BindMode::Loopback,
                    "This computer only (tunnel)",
                );
            });
        });
        field(ui, "Port", |ui| {
            ui.add_enabled(
                !status.enabled,
                egui::DragValue::new(&mut rs.port).range(0..=65535),
            );
            ui.label(w::faint("0 = any free port"));
        });
        w::switch(ui, &mut rs.allow_resize, "Allow a phone to resize a pane");
        ui.add_space(3.0);
        w::switch(ui, &mut rs.enable_at_startup, "Turn on at startup");
        ui.add_space(10.0);

        if !status.enabled {
            ui.label(w::faint(
                "Turn it on to pair a device. Over the internet, use Tailscale or another overlay.",
            ));
            return;
        }
        w::hline(ui);
        ui.add_space(8.0);
        egui::Grid::new("remote-info")
            .num_columns(2)
            .spacing([14.0, 5.0])
            .show(ui, |ui| {
                ui.label(w::dim("Address"));
                ui.label(
                    RichText::new(format!(
                        "{}:{}",
                        status.host.clone().unwrap_or_default(),
                        status.port
                    ))
                    .monospace()
                    .color(TX0),
                );
                ui.end_row();
                ui.label(w::dim("Name"));
                ui.label(w::strong(&status.host_name));
                ui.end_row();
                if let Some(fp) = &status.fingerprint {
                    ui.label(w::dim("Fingerprint"));
                    ui.label(RichText::new(fp).monospace().size(10.5).color(TX2));
                    ui.end_row();
                }
            });
        ui.add_space(10.0);
        match &status.pairing {
            Some(p) => {
                ui.horizontal_top(|ui| {
                    qr(ui, &p.payload, 180.0);
                    ui.add_space(12.0);
                    ui.vertical(|ui| {
                        ui.label(w::dim("Scan with the SplitView app, or type the code:"));
                        ui.label(RichText::new(&p.code).monospace().size(24.0).color(ACC));
                        ui.label(w::faint(format!(
                            "expires in {} s · single use",
                            p.seconds_left
                        )));
                        ui.add_space(8.0);
                        if ui.button("Cancel pairing").clicked() {
                            let _ = hub.remote.cancel_pairing(&hub);
                            st.status = None;
                        }
                    });
                });
            }
            None => {
                if w::primary(ui, &format!("{} Pair a device", ph::DEVICE_MOBILE)).clicked() {
                    match hub.remote.start_pairing(&hub) {
                        Ok(s) => st.status = Some((s, Some(Instant::now()))),
                        Err(e) => st.error = Some(e.to_string()),
                    }
                }
            }
        }
        ui.add_space(12.0);
        ui.label(RichText::new("LIVE SESSIONS").size(10.5).color(TX2));
        if status.sessions.is_empty() {
            ui.label(w::faint("No one is connected."));
        }
        for s in &status.sessions {
            ui.horizontal(|ui| {
                ui.label(RichText::new(ph::CIRCLE).color(WARN).size(9.0));
                ui.label(w::strong(&s.device_name));
                ui.label(RichText::new(&s.address).monospace().size(11.0).color(TX3));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("Disconnect").clicked() {
                        let _ = hub.remote.disconnect_session(&hub, s.id);
                        st.status = None;
                    }
                });
            });
        }
        ui.add_space(10.0);
        ui.label(RichText::new("PAIRED DEVICES").size(10.5).color(TX2));
        if status.devices.is_empty() {
            ui.label(w::faint("None yet."));
        }
        for d in &status.devices {
            ui.horizontal(|ui| {
                ui.label(RichText::new(ph::DEVICE_MOBILE).color(TX2));
                ui.label(w::strong(&d.name));
                ui.label(w::faint(&d.platform));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("Revoke").clicked() {
                        let _ = hub.remote.revoke_device(&hub, &d.id);
                        st.status = None;
                    }
                });
            });
        }
    }

    fn section_about(&mut self, ui: &mut Ui) {
        heading(ui, "About", "");
        ui.horizontal(|ui| {
            theme::logo(ui, 40.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("SplitView").size(16.0).color(TX0).strong());
                ui.label(w::dim(format!(
                    "version {} · egui + alacritty_terminal",
                    theme::VERSION
                )));
            });
        });
        ui.add_space(10.0);
        let paths = crate::store::Paths::resolve();
        egui::Grid::new("about")
            .num_columns(2)
            .spacing([14.0, 5.0])
            .show(ui, |ui| {
                ui.label(w::dim("Config"));
                ui.label(
                    RichText::new(paths.config.display().to_string())
                        .monospace()
                        .size(11.0),
                );
                ui.end_row();
                ui.label(w::dim("Settings"));
                ui.label(
                    RichText::new(paths.settings.display().to_string())
                        .monospace()
                        .size(11.0),
                );
                ui.end_row();
                ui.label(w::dim("Live panes"));
                ui.label(self.hub.pane_count().to_string());
                ui.end_row();
            });
        ui.add_space(8.0);
        ui.label(w::faint(
            "From a terminal: splitview edit <file> · splitview settings",
        ));
    }
}

/// Draw a QR code for `data` (the pairing payload).
fn qr(ui: &mut Ui, data: &str, size: f32) {
    let Ok(code) = qrcode::QrCode::new(data.as_bytes()) else {
        ui.label(w::faint("(QR unavailable)"));
        return;
    };
    let n = code.width();
    let quiet = 2;
    let cells = n + 2 * quiet;
    let cell = (size / cells as f32).floor().max(1.0);
    let side = cell * cells as f32;
    let (r, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
    let p = ui.painter();
    p.rect_filled(r, 4, Color32::WHITE);
    let colors = code.to_colors();
    for y in 0..n {
        for x in 0..n {
            if colors[y * n + x] == qrcode::Color::Dark {
                let min = r.min + vec2((x + quiet) as f32 * cell, (y + quiet) as f32 * cell);
                p.rect_filled(
                    egui::Rect::from_min_size(min, vec2(cell, cell)),
                    0,
                    Color32::BLACK,
                );
            }
        }
    }
}
