//! SplitView — a minimalistic, keyboard-driven terminal emulator with
//! tmux-style split panes. egui renders; alacritty_terminal emulates; real PTYs
//! (portable-pty) run the shells. See CLAUDE.md.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod chrome;
mod dialogs;
mod editor;
mod error;
mod files;
mod git;
mod hub;
mod keys;
mod panes;
mod profiles;
mod remote;
mod settings;
mod settings_ui;
mod shell;
mod shot;
mod sidebar;
mod store;
mod theme;
mod watch;
mod widgets;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("splitview {}", theme::VERSION);
        return Ok(());
    }
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let shot = arg("--shot")
        .map(|out| shot::Shot::new(out.into(), arg("--scene").unwrap_or_else(|| "main".into())));
    let size = if shot.is_some() {
        [1600.0, 920.0]
    } else {
        [1280.0, 800.0]
    };

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title("SplitView")
            .with_app_id("splitview")
            .with_icon(eframe::icon_data::from_png_bytes(theme::ICON_PNG).unwrap_or_default())
            .with_inner_size(size)
            .with_min_inner_size([640.0, 400.0]),
        // Reactive: frames come from input and PTY output, never a loop.
        run_and_return: false,
        ..Default::default()
    };
    eframe::run_native(
        "SplitView",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc, shot)))),
    )
}
