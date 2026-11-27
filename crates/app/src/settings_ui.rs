//! The Settings window: a modal with a section list on the left. Changes apply
//! live and are saved as they are made (prompt-related ones reach new panes).

use egui::{Align, Color32, Frame, Key, Layout, Margin, RichText, Sense, Ui, vec2};
use egui_phosphor::regular as ph;
use sv_core::keymap::{ACTIONS, chords_for, pretty};
use sv_core::paths::prompt_dir;

use crate::app::App;
use crate::hub::UiEvent;
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
    Mcp,
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
    (Section::Mcp, ph::ROBOT, "MCP"),
    (Section::About, ph::QUESTION, "About"),
];

#[derive(Default)]
pub struct SettingsState {
    pub section: Section,
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
                                        Section::Mcp => self.section_mcp(ui),
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
        let host = host_name();
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
        ui.add_space(14.0);
        let sh = &mut self.settings.shell;
        let hub = self.hub.clone();
        field(ui, "Start in", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut sh.start_dir)
                    .hint_text("your home folder")
                    .font(theme::mono(12.0))
                    .desired_width(300.0),
            );
            if ui.button(format!("{} Browse…", ph::FOLDER_OPEN)).clicked() {
                let from = sh.start_dir();
                // Native dialogs block: keep them off the UI thread.
                std::thread::spawn(move || {
                    let mut d = rfd::FileDialog::new().set_title("Start in");
                    if let Some(f) = from {
                        d = d.set_directory(f);
                    }
                    if let Some(p) = d.pick_folder() {
                        hub.emit(UiEvent::StartDirPicked(p.to_string_lossy().into_owned()));
                    }
                });
            }
            if !sh.start_dir.is_empty() && ui.small_button("Clear").clicked() {
                sh.start_dir.clear();
            }
        });
        if !sh.start_dir.trim().is_empty() && sh.start_dir().is_none() {
            ui.label(
                RichText::new("Folder not found: panes start in your home folder.").color(WARN),
            );
        } else {
            ui.label(w::faint(
                "Where the first pane opens when SplitView starts. Splits keep the folder of the pane they split.",
            ));
        }
    }

    fn section_mcp(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "MCP server",
            "Let an AI agent on this computer (Claude Code, or any MCP client) list, open, close and read panes, run commands in them, and write on the scratchpad.",
        );
        let on = self.mcp.as_ref().map(|m| m.url());
        let label = match &on {
            Some(url) => format!("On at {url}"),
            None => "Off".into(),
        };
        w::switch(ui, &mut self.settings.mcp.enabled, &label);
        if let Some(e) = &self.mcp_error {
            ui.label(RichText::new(e).color(ERR));
        }
        ui.add_space(8.0);
        let m = &mut self.settings.mcp;
        field(ui, "Port", |ui| {
            ui.add_enabled(
                !m.enabled,
                egui::DragValue::new(&mut m.port).range(1024..=65535),
            );
            ui.label(w::faint("127.0.0.1 only; turn off to change"));
        });
        let mut copy = None;
        field(ui, "Token", |ui| {
            let shown = if m.token.len() > 8 {
                format!("{}…", m.token.chars().take(8).collect::<String>())
            } else {
                "made when first turned on".into()
            };
            ui.label(RichText::new(shown).monospace().color(TX1));
            if !m.token.is_empty() && ui.small_button("Copy").clicked() {
                copy = Some(m.token.clone());
            }
            if ui
                .small_button("New token")
                .on_hover_text("Clients using the old one stop working")
                .clicked()
            {
                m.token = crate::mcp::new_token();
            }
        });
        if let Some(url) = &on {
            ui.add_space(10.0);
            ui.label(w::dim("Add it to Claude Code (run once in a terminal):"));
            let cmd = crate::mcp::claude_command(url, &m.token);
            Frame::NONE
                .fill(self.skin.term.bg)
                .corner_radius(4)
                .inner_margin(Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(
                        egui::Label::new(
                            RichText::new(&cmd).font(theme::term_font(12.0)).color(TX0),
                        )
                        .wrap(),
                    );
                });
            ui.add_space(4.0);
            if ui.button(format!("{} Copy command", ph::COPY)).clicked() {
                copy = Some(cmd);
            }
        }
        if let Some(t) = copy {
            ui.ctx().copy_text(t);
        }
        ui.add_space(10.0);
        ui.label(w::faint(
            "Anything holding the token can type into your shells. Agents running in a pane see their own pane id in $SPLITVIEW_PANE.",
        ));
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

/// This machine's name for the prompt preview: env first, then
/// `/etc/hostname`, then a constant.
fn host_name() -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .chain(std::fs::read_to_string("/etc/hostname").ok())
        .map(|n| n.trim().to_owned())
        .find(|n| !n.is_empty())
        .unwrap_or_else(|| "splitview".into())
}
