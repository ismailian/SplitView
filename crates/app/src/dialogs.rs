//! Modals and overlays: the shell picker, the backgrounded-panes picker,
//! confirmations, the command palette, toasts.
//! A modal owns the keyboard of its window while open; none of its keys leak
//! into the terminal underneath.

use std::time::Instant;

use egui::{
    Align, Align2, Color32, Frame, Id, Key, Layout, Margin, RichText, Sense, Ui, pos2, vec2,
};
use egui_phosphor::regular as ph;
use sv_core::fuzzy;
use sv_core::keymap::{ACTIONS, chords_for, pretty};
use sv_core::layout::{Direction, PaneId};

use crate::app::{App, Focus};
use crate::settings_ui::{Section, SettingsState};
use crate::theme::{self, tok::*};
use crate::widgets as w;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerMode {
    /// No shell chosen yet: the window waits for this.
    FirstRun,
    /// `<prefix> p`: open a pane with any shell, or change the default.
    Pick,
}

pub enum Modal {
    Picker {
        mode: PickerMode,
        sel: usize,
        message: Option<String>,
    },
    Background {
        sel: usize,
    },
    ConfirmClose(PaneId),
    ConfirmQuit(usize),
    ConfirmCloseWindow,
    ConfirmOverwrite(usize),
    ConfirmCloseTab(usize),
    Palette {
        query: String,
        sel: usize,
        focus: bool,
    },
    Settings(Box<SettingsState>),
    /// A newer release: Download / Close.
    Update(Box<sv_core::update::Release>),
}

impl Modal {
    pub fn picker(mode: PickerMode, message: Option<String>, _default: Option<String>) -> Self {
        Modal::Picker {
            mode,
            sel: 0,
            message,
        }
    }
    pub fn palette() -> Self {
        Modal::Palette {
            query: String::new(),
            sel: 0,
            focus: true,
        }
    }
    pub fn settings() -> Self {
        Modal::Settings(Box::default())
    }
    pub fn settings_at(s: Section) -> Self {
        Modal::Settings(Box::new(SettingsState { section: s }))
    }
}

/// A centered dialog frame over a scrim. Returns the inner value.
pub fn dialog<R>(ctx: &egui::Context, id: &str, width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let screen = ctx.content_rect();
    egui::Area::new(Id::new(("dialog", id)))
        .order(egui::Order::Foreground)
        .pivot(Align2::CENTER_TOP)
        .fixed_pos(pos2(
            screen.center().x,
            screen.top() + (screen.height() * 0.16).min(140.0),
        ))
        .show(ctx, |ui| {
            // The backdrop, on the dialog's own layer so it is always beneath
            // the dialog and above everything else.
            ui.ctx()
                .layer_painter(ui.layer_id())
                .rect_filled(screen, 0, SCRIM);
            Frame::NONE
                .fill(BG1)
                .stroke(egui::Stroke::new(1.0, LINE2))
                .corner_radius(6)
                .shadow(egui::Shadow {
                    offset: [0, 12],
                    blur: 40,
                    spread: 0,
                    color: Color32::from_black_alpha(128),
                })
                .inner_margin(Margin::same(0))
                .show(ui, |ui| {
                    ui.set_width(width.min(screen.width() - 32.0));
                    add(ui)
                })
                .inner
        })
        .inner
}

fn title_row(ui: &mut Ui, icon: &str, title: &str) {
    Frame::NONE
        .inner_margin(Margin {
            left: 16,
            right: 16,
            top: 14,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon).size(16.0).color(ACC));
                ui.label(RichText::new(title).size(14.5).color(TX0).strong());
            });
        });
    w::hline(ui);
}

fn footer_hints(ui: &mut Ui, hints: &[(&str, &str)]) {
    w::hline(ui);
    Frame::NONE
        .fill(BG2)
        .inner_margin(Margin::symmetric(14, 7))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                for (k, what) in hints {
                    w::kbd(ui, k);
                    ui.label(w::faint(*what));
                    ui.add_space(8.0);
                }
            });
        });
}

/// A list row: selected rows get the accent bar.
fn list_row(ui: &mut Ui, selected: bool, h: f32, add: impl FnOnce(&mut Ui)) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    if selected {
        ui.painter().rect_filled(r, 0, BG3);
        ui.painter()
            .rect_filled(egui::Rect::from_min_size(r.min, vec2(2.0, h)), 0, ACC);
    } else if resp.hovered() {
        ui.painter().rect_filled(r, 0, BG2);
    }
    let mut c = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(r.shrink2(vec2(16.0, 4.0)))
            .layout(Layout::top_down(Align::Min)),
    );
    add(&mut c);
    resp
}

enum Answer {
    Yes,
    No,
}

/// A yes/no confirmation. Enter/y = yes, Esc/n = no.
fn confirm(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    body: &str,
    yes: &str,
    danger: bool,
) -> Option<Answer> {
    let (keys, _) = crate::keys::take_all(ctx);
    let mut ans = None;
    for (k, _) in keys {
        match k {
            Key::Enter | Key::Y => ans = Some(Answer::Yes),
            Key::Escape | Key::N => ans = Some(Answer::No),
            _ => {}
        }
    }
    dialog(ctx, id, 420.0, |ui| {
        title_row(ui, if danger { ph::WARNING } else { ph::QUESTION }, title);
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.label(RichText::new(body).color(TX1));
            });
        w::hline(ui);
        Frame::NONE
            .fill(BG2)
            .inner_margin(Margin::symmetric(14, 9))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let yes_btn = if danger {
                        ui.add(
                            egui::Button::new(RichText::new(yes).color(Color32::WHITE)).fill(ERR),
                        )
                    } else {
                        w::primary(ui, yes)
                    };
                    if yes_btn.clicked() {
                        ans = Some(Answer::Yes);
                    }
                    if ui.button("Cancel").clicked() {
                        ans = Some(Answer::No);
                    }
                    ui.label(w::faint("Enter to confirm · Esc to cancel"));
                });
            });
    });
    ans
}

/// "SplitView x.y.z is available": the changelog, Download / Close.
fn update_prompt(ctx: &egui::Context, rel: &sv_core::update::Release) -> Option<Answer> {
    let (keys, _) = crate::keys::take_all(ctx);
    let mut ans = None;
    for (k, _) in keys {
        match k {
            Key::Enter => ans = Some(Answer::Yes),
            Key::Escape => ans = Some(Answer::No),
            _ => {}
        }
    }
    dialog(ctx, "update", 480.0, |ui| {
        title_row(
            ui,
            ph::DOWNLOAD_SIMPLE,
            &format!("SplitView {} is available", rel.version),
        );
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(format!(
                        "You have {}. The installer is {:.1} MB; SplitView closes while it runs.",
                        theme::VERSION,
                        rel.size as f64 / (1024.0 * 1024.0)
                    ))
                    .color(TX1),
                );
                if !rel.changelog.trim().is_empty() {
                    ui.add_space(8.0);
                    egui::ScrollArea::vertical()
                        .max_height(240.0)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(rel.changelog.trim()).color(TX2));
                        });
                }
            });
        w::hline(ui);
        Frame::NONE
            .fill(BG2)
            .inner_margin(Margin::symmetric(14, 9))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if w::primary(ui, "Download").clicked() {
                        ans = Some(Answer::Yes);
                    }
                    if ui.button("Close").clicked() {
                        ans = Some(Answer::No);
                    }
                    ui.label(w::faint("Enter to download · Esc to close"));
                });
            });
    });
    ans
}

impl App {
    pub fn show_overlays(&mut self, ctx: &egui::Context, wi: usize) {
        if self
            .modal
            .as_ref()
            .is_some_and(|(k, _)| *k == self.windows[wi].key)
        {
            self.show_modal(ctx, wi);
        }
        if wi == self.active_window() {
            self.show_toast(ctx);
        }
    }

    fn show_modal(&mut self, ctx: &egui::Context, wi: usize) {
        let Some((key, mut modal)) = self.modal.take() else {
            return;
        };
        let keep = match &mut modal {
            Modal::Picker { mode, sel, message } => {
                self.picker(ctx, wi, *mode, sel, message.clone())
            }
            Modal::Background { sel } => self.background_picker(ctx, wi, sel),
            Modal::ConfirmClose(id) => {
                let id = *id;
                match confirm(
                    ctx,
                    "close-pane",
                    "Close this pane?",
                    "Its shell — and anything running in it — will be ended.",
                    "Close pane",
                    true,
                ) {
                    Some(Answer::Yes) => {
                        self.close_pane(id);
                        false
                    }
                    Some(Answer::No) => false,
                    None => true,
                }
            }
            Modal::ConfirmQuit(n) => {
                let body = format!(
                    "{} unsaved change{} in the editor. Quit anyway?",
                    if *n == 1 {
                        "One file has".to_owned()
                    } else {
                        format!("{n} files have")
                    },
                    if *n == 1 { "" } else { "s" }
                );
                match confirm(
                    ctx,
                    "quit",
                    "Unsaved changes",
                    &body,
                    "Quit without saving",
                    true,
                ) {
                    Some(Answer::Yes) => {
                        self.request_quit();
                        false
                    }
                    Some(Answer::No) => false,
                    None => true,
                }
            }
            Modal::ConfirmCloseWindow => {
                match confirm(
                    ctx,
                    "close-win",
                    "Unsaved changes",
                    "This window's editor has unsaved changes. Close anyway?",
                    "Close window",
                    true,
                ) {
                    Some(Answer::Yes) => {
                        self.close_window(wi);
                        false
                    }
                    Some(Answer::No) => false,
                    None => true,
                }
            }
            Modal::ConfirmOverwrite(i) => {
                let i = *i;
                let name = self.windows[wi].editor.tab_name(i);
                match confirm(
                    ctx,
                    "overwrite",
                    "File changed on disk",
                    &format!(
                        "{name} was changed by something else since you opened it. Overwrite it with your version?"
                    ),
                    "Overwrite",
                    true,
                ) {
                    Some(Answer::Yes) => {
                        if let Some(a) = self.windows[wi].editor.save(i, true) {
                            self.editor_actions(wi, vec![a]);
                        }
                        false
                    }
                    Some(Answer::No) => false,
                    None => true,
                }
            }
            Modal::ConfirmCloseTab(i) => {
                let i = *i;
                let name = self.windows[wi].editor.tab_name(i);
                match confirm(
                    ctx,
                    "close-tab",
                    "Discard changes?",
                    &format!("{name} has unsaved changes."),
                    "Discard",
                    true,
                ) {
                    Some(Answer::Yes) => {
                        if let Some(a) = self.windows[wi].editor.close_tab(i, true) {
                            self.editor_actions(wi, vec![a]);
                        }
                        false
                    }
                    Some(Answer::No) => false,
                    None => true,
                }
            }
            Modal::Palette { query, sel, focus } => self.palette(ctx, wi, query, sel, focus),
            Modal::Settings(state) => self.settings_window(ctx, wi, state),
            Modal::Update(rel) => match update_prompt(ctx, rel) {
                Some(Answer::Yes) => {
                    #[cfg(windows)]
                    crate::update::install(self.hub.clone(), (**rel).clone());
                    self.toast(format!("Downloading SplitView {}…", rel.version), false);
                    false
                }
                Some(Answer::No) => false,
                None => true,
            },
        };
        // An action run from the modal may have opened another one.
        if keep && self.modal.is_none() {
            self.modal = Some((key, modal));
        }
    }

    fn picker(
        &mut self,
        ctx: &egui::Context,
        wi: usize,
        mode: PickerMode,
        sel: &mut usize,
        message: Option<String>,
    ) -> bool {
        let n = self.profiles.len();
        let (keys, _) = crate::keys::take_all(ctx);
        let mut choose: Option<(usize, bool)> = None; // index, make default
        let mut cancel = false;
        let default_id = self.settings.shell.default_profile.clone();
        for (k, _) in keys {
            match k {
                Key::J | Key::ArrowDown => *sel = (*sel + 1).min(n.saturating_sub(1)),
                Key::K | Key::ArrowUp => *sel = sel.saturating_sub(1),
                // First run: Enter both starts it and makes it the default.
                // Otherwise Enter opens a pane, `d` only changes the default.
                Key::Enter | Key::Space => choose = Some((*sel, mode == PickerMode::FirstRun)),
                Key::D => choose = Some((*sel, true)),
                Key::Escape | Key::Q => cancel = true,
                _ => {}
            }
        }
        let title = match mode {
            PickerMode::FirstRun => "Choose your shell",
            PickerMode::Pick => "Open a pane with…",
        };
        dialog(ctx, "picker", 520.0, |ui| {
            title_row(ui, ph::TERMINAL_WINDOW, title);
            if let Some(m) = &message {
                Frame::NONE
                    .fill(ERR_DIM)
                    .inner_margin(Margin::symmetric(16, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(m).color(ERR).size(12.0));
                    });
            } else if mode == PickerMode::FirstRun {
                Frame::NONE.inner_margin(Margin::symmetric(16, 8)).show(ui, |ui| {
                    ui.label(w::dim("New panes will run this shell. You can change it later in Settings → Shell, or open any shell with prefix p."));
                });
            }
            ui.add_space(4.0);
            for (i, p) in self.profiles.iter().enumerate() {
                let resp = list_row(ui, i == *sel, 44.0, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&p.name).color(TX0).size(13.0));
                        if default_id.as_deref() == Some(p.id.as_str()) {
                            w::badge(ui, "default", ACC, ACC_DIM);
                        }
                        let fam = match p.family {
                            sv_core::quote::ShellFamily::Posix => "posix",
                            sv_core::quote::ShellFamily::Powershell => "powershell",
                            sv_core::quote::ShellFamily::Cmd => "cmd",
                        };
                        w::chip(ui, fam, TX2);
                    });
                    ui.label(RichText::new(&p.program).monospace().size(11.0).color(TX3));
                });
                if resp.clicked() {
                    *sel = i;
                    choose = Some((i, mode == PickerMode::FirstRun));
                }
            }
            if self.profiles.is_empty() {
                Frame::NONE.inner_margin(Margin::symmetric(16, 10)).show(ui, |ui| {
                    ui.label(w::dim("No shells found. Install Git for Windows (Git Bash) or set `shell` in config.toml."));
                });
            }
            ui.add_space(4.0);
            let hints: &[(&str, &str)] = match mode {
                PickerMode::FirstRun => &[("j/k", "move"), ("Enter", "use this shell")],
                PickerMode::Pick => &[
                    ("j/k", "move"),
                    ("Enter", "open pane"),
                    ("d", "make default"),
                    ("Esc", "cancel"),
                ],
            };
            footer_hints(ui, hints);
        });
        if cancel && mode == PickerMode::Pick {
            return false;
        }
        if cancel && mode == PickerMode::FirstRun && n > 0 {
            choose = Some((*sel, true));
        }
        let Some((i, make_default)) = choose else {
            return true;
        };
        let Some(p) = self.profiles.get(i).cloned() else {
            return false;
        };
        if make_default {
            self.set_default_profile(&p.id);
        }
        match mode {
            PickerMode::FirstRun => {
                if self.windows[wi].layout.is_empty()
                    && let Err(e) =
                        self.split_into(wi, Direction::Right, None, Some(p.program.clone()), true)
                {
                    self.modal = Some((
                        self.windows[wi].key,
                        Modal::picker(
                            PickerMode::FirstRun,
                            Some(format!("Couldn't start {}: {e}", p.name)),
                            None,
                        ),
                    ));
                }
                false
            }
            PickerMode::Pick => {
                if make_default {
                    self.toast(format!("{} is now the default shell", p.name), false);
                } else if let Err(e) =
                    self.split_into(wi, Direction::Right, None, Some(p.program.clone()), true)
                {
                    self.toast(format!("Couldn't start {}: {e}", p.name), true);
                }
                false
            }
        }
    }

    fn background_picker(&mut self, ctx: &egui::Context, wi: usize, sel: &mut usize) -> bool {
        let n = self.windows[wi].background.len();
        if n == 0 {
            return false;
        }
        *sel = (*sel).min(n - 1);
        let (keys, _) = crate::keys::take_all(ctx);
        let mut restore = None;
        let mut kill = None;
        let mut close = false;
        for (k, _) in keys {
            match k {
                Key::J | Key::ArrowDown => *sel = (*sel + 1).min(n - 1),
                Key::K | Key::ArrowUp => *sel = sel.saturating_sub(1),
                Key::Enter | Key::Space => restore = Some(*sel),
                Key::X | Key::Delete => kill = Some(*sel),
                Key::Escape | Key::Q => close = true,
                _ => {}
            }
        }
        let rows: Vec<(PaneId, String, String, u64)> = self.windows[wi]
            .background
            .iter()
            .rev()
            .filter_map(|(id, since)| {
                let p = self.panes.get(id)?;
                Some((
                    *id,
                    p.title(),
                    p.term.preview_line(),
                    since.elapsed().as_secs(),
                ))
            })
            .collect();
        dialog(ctx, "background", 600.0, |ui| {
            title_row(ui, ph::STACK, "Backgrounded panes");
            ui.add_space(4.0);
            for (i, (_, title, preview, age)) in rows.iter().enumerate() {
                let resp = list_row(ui, i == *sel, 46.0, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(title).color(TX0).size(13.0));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(w::faint(age_text(*age)));
                        });
                    });
                    let pv = if preview.is_empty() {
                        "(no output yet)"
                    } else {
                        preview.as_str()
                    };
                    ui.label(RichText::new(pv).monospace().size(11.0).color(TX2));
                });
                if resp.double_clicked() || resp.clicked() {
                    *sel = i;
                    if resp.double_clicked() {
                        restore = Some(i);
                    }
                }
            }
            ui.add_space(4.0);
            footer_hints(
                ui,
                &[
                    ("j/k", "move"),
                    ("Enter", "restore"),
                    ("x", "kill"),
                    ("Esc", "close"),
                ],
            );
        });
        // Rows are shown newest first.
        let index_of = |shown: usize| n - 1 - shown;
        if let Some(i) = kill {
            let id = self.windows[wi].background[index_of(i)].0;
            self.close_pane(id);
            return !self.windows.get(wi).is_none_or(|w| w.background.is_empty());
        }
        if let Some(i) = restore {
            self.restore_background(wi, index_of(i));
            return false;
        }
        !close
    }

    fn palette(
        &mut self,
        ctx: &egui::Context,
        wi: usize,
        query: &mut String,
        sel: &mut usize,
        focus: &mut bool,
    ) -> bool {
        // Items: every action, a pane per installed shell, every color preset.
        let km = self.keymap.config().clone();
        let mut items: Vec<(String, String, PaletteItem)> = Vec::new();
        for (name, label, cat) in ACTIONS {
            let chords = chords_for(&km, name);
            let hint = chords
                .iter()
                .find(|(_, c)| c.len() <= 12)
                .map(|(pre, c)| {
                    if *pre {
                        format!("{} {}", pretty(&km.prefix), pretty(c))
                    } else {
                        pretty(c)
                    }
                })
                .unwrap_or_default();
            items.push((format!("{cat}: {label}"), hint, PaletteItem::Action(name)));
        }
        for p in &self.profiles {
            items.push((
                format!("New pane: {}", p.name),
                String::new(),
                PaletteItem::Shell(p.program.clone()),
            ));
        }
        for (i, p) in crate::settings::PRESETS.iter().enumerate() {
            items.push((
                format!("Theme: {}", p.name),
                String::new(),
                PaletteItem::Preset(i),
            ));
        }
        let mut scored: Vec<(i32, usize)> = items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| fuzzy::score(query, &it.0).map(|s| (s, i)))
            .collect();
        if !query.is_empty() {
            scored.sort_by_key(|a| std::cmp::Reverse(a.0));
        }
        let n = scored.len();
        let mut run = None;
        let mut close = false;
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, Key::ArrowDown) {
                *sel = (*sel + 1).min(n.saturating_sub(1));
            }
            if i.consume_key(egui::Modifiers::NONE, Key::ArrowUp) {
                *sel = sel.saturating_sub(1);
            }
            if i.consume_key(egui::Modifiers::NONE, Key::Enter) {
                run = Some(*sel);
            }
            if i.consume_key(egui::Modifiers::NONE, Key::Escape) {
                close = true;
            }
        });
        dialog(ctx, "palette", 560.0, |ui| {
            Frame::NONE
                .inner_margin(Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(ph::MAGNIFYING_GLASS).color(TX2).size(15.0));
                        let r = ui.add(
                            egui::TextEdit::singleline(query)
                                .hint_text("Type an action, a shell or a theme…")
                                .frame(Frame::NONE)
                                .desired_width(f32::INFINITY)
                                .font(theme::prop(14.0)),
                        );
                        if *focus {
                            r.request_focus();
                            *focus = false;
                        }
                        if r.changed() {
                            *sel = 0;
                        }
                    });
                });
            w::hline(ui);
            let max_rows = 12usize;
            let start = sel.saturating_sub(max_rows - 1);
            for (row, &(_, idx)) in scored.iter().enumerate().skip(start).take(max_rows) {
                let (label, hint, _) = &items[idx];
                let resp = list_row(ui, row == *sel, 28.0, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(label).color(if row == *sel { TX0 } else { TX1 }));
                        if !hint.is_empty() {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                w::kbd(ui, hint);
                            });
                        }
                    });
                });
                if resp.clicked() {
                    run = Some(row);
                }
            }
            if n == 0 {
                Frame::NONE
                    .inner_margin(Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.label(w::faint("No matches"));
                    });
            }
            footer_hints(ui, &[("↑↓", "move"), ("Enter", "run"), ("Esc", "close")]);
        });
        if close {
            return false;
        }
        if let Some(r) = run
            && let Some(&(_, idx)) = scored.get(r)
        {
            match items[idx].2.clone() {
                PaletteItem::Action(a) => {
                    self.run_action(wi, a);
                }
                PaletteItem::Shell(prog) => {
                    if let Err(e) = self.split_into(wi, Direction::Right, None, Some(prog), true) {
                        self.toast(e, true);
                    }
                }
                PaletteItem::Preset(i) => {
                    let p = &crate::settings::PRESETS[i];
                    let d = &mut self.settings.display;
                    d.bg = p.bg.into();
                    d.fg = p.fg.into();
                    d.cursor = p.cursor.into();
                    d.accent = p.accent.into();
                    d.divider = p.divider.into();
                    d.divider_active = p.divider_active.into();
                    self.apply_settings();
                }
            }
            return false;
        }
        true
    }

    fn show_toast(&mut self, ctx: &egui::Context) {
        let Some(t) = &self.toast else { return };
        let screen = ctx.content_rect();
        let fade = 1.0 - (t.at.elapsed().as_secs_f32() - 3.6).max(0.0) / 0.4;
        egui::Area::new(Id::new("toast"))
            .order(egui::Order::Tooltip)
            .pivot(Align2::RIGHT_BOTTOM)
            .fixed_pos(pos2(
                screen.right() - 16.0,
                screen.bottom() - theme::STATUS_H - 14.0,
            ))
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_opacity(fade.clamp(0.0, 1.0));
                Frame::NONE
                    .fill(BG2)
                    .stroke(egui::Stroke::new(1.0, if t.error { ERR } else { LINE2 }))
                    .corner_radius(4)
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_max_width(420.0);
                        ui.horizontal(|ui| {
                            let (icon, c) = if t.error {
                                (ph::WARNING_CIRCLE, ERR)
                            } else {
                                (ph::INFO, ACC)
                            };
                            ui.label(RichText::new(icon).color(c));
                            ui.label(RichText::new(&t.text).color(TX1).size(12.0));
                        });
                    });
            });
        if t.at.elapsed().as_secs_f32() > 3.6 {
            ctx.request_repaint();
        }
        let _ = Instant::now();
    }

    pub fn focus_terminal(&mut self, wi: usize) {
        self.windows[wi].focus = Focus::Terminal;
    }
}

#[derive(Clone)]
enum PaletteItem {
    Action(&'static str),
    Shell(String),
    Preset(usize),
}

fn age_text(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
    }
}
