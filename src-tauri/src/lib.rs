//! SplitView backend entry point.
//!
//! Rust owns PTYs and process lifecycle; the webview frontend owns rendering,
//! the split layout, and the keymap (CLAUDE.md §2.5).

mod commands;
mod error;
mod files;
mod profiles;
mod pty;
mod remote;
mod shell;
mod state;
mod watch;

use remote::RemoteState;
use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .manage(RemoteState::new())
        // A window's sidebar watch — and its entry in the remote roster — go
        // with the window, even if its page never got to say so (killed,
        // crashed, closed mid-load).
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                if let Some(state) = window.try_state::<AppState>() {
                    let watch = state
                        .watchers
                        .lock()
                        .ok()
                        .and_then(|mut w| w.remove(window.label()));
                    drop(watch); // after the lock is released

                    if let Some(remote) = window.try_state::<RemoteState>() {
                        // A remote client must not be shown panes belonging to
                        // a window that no longer exists.
                        if let Ok(seqs) = state.output_seqs() {
                            remote.forget_window(window.label(), &seqs);
                        }
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::spawn_pty,
            commands::write_pty,
            commands::resize_pty,
            commands::close_pty,
            commands::list_panes,
            commands::list_shell_profiles,
            commands::attach_pty,
            commands::stash_handoff,
            commands::take_handoff,
            commands::pick_file,
            commands::read_dir,
            commands::fs_watch,
            commands::fs_watch_dirs,
            commands::fs_unwatch,
            commands::read_text_file,
            commands::file_stamp,
            commands::write_text_file,
            commands::publish_panes,
            commands::provide_snapshot,
            commands::remote_status,
            commands::remote_enable,
            commands::remote_disable,
            commands::remote_pairing_start,
            commands::remote_pairing_cancel,
            commands::remote_pairing_respond,
            commands::remote_devices,
            commands::remote_revoke_device,
            commands::remote_disconnect_session,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
