//! SplitView backend entry point.
//!
//! Rust owns PTYs and process lifecycle; the webview frontend owns rendering,
//! the split layout, and the keymap (CLAUDE.md §2.5).

mod commands;
mod error;
mod pty;
mod shell;
mod state;

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::spawn_pty,
            commands::write_pty,
            commands::resize_pty,
            commands::close_pty,
            commands::list_panes,
            commands::attach_pty,
            commands::stash_handoff,
            commands::take_handoff,
            commands::pick_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
