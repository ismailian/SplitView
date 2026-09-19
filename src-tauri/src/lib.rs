//! SplitView backend entry point.
//!
//! Rust owns PTYs and process lifecycle; the webview frontend owns rendering,
//! the split layout, and the keymap (CLAUDE.md §2.5).

mod commands;
mod error;
mod pty;
mod shell;
mod state;
mod watch;

use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        // A window's sidebar watch goes with the window, even if its page never
        // got to unsubscribe (killed, crashed, closed mid-load).
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                if let Some(state) = window.try_state::<AppState>() {
                    let watch = state
                        .watchers
                        .lock()
                        .ok()
                        .and_then(|mut w| w.remove(window.label()));
                    drop(watch); // after the lock is released
                }
            }
        })
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
            commands::read_dir,
            commands::fs_watch,
            commands::fs_watch_dirs,
            commands::fs_unwatch,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
