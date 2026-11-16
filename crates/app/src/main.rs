#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod alerts;
mod commands;
mod live;
mod settings;
mod state;
mod tray;
mod usual;
mod watcher;

use tauri::{Manager, RunEvent, WindowEvent};

/// Passed by the login item so the app starts in the menu bar only.
const BACKGROUND_ARG: &str = "--background";

fn main() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![BACKGROUND_ARG]),
        ))
        .manage(state::AppState::default())
        .manage(settings::SettingsState::default())
        .manage(alerts::Alerts::default())
        .manage(tray::TrayState::default())
        .setup(|app| {
            let handle = app.handle().clone();
            let s = settings::load(&handle);
            if let Err(e) = settings::apply_autostart(&handle, s.launch_at_login) {
                eprintln!("[settings] launch at login: {e}");
            }
            *app.state::<settings::SettingsState>().0.lock().unwrap() = s;
            tray::init(&handle)?;
            if !std::env::args().any(|a| a == BACKGROUND_ARG) {
                tray::show_main(&handle);
            }
            watcher::start(handle);
            Ok(())
        })
        .on_window_event(|w, ev| {
            // Closing the window keeps the app in the menu bar.
            if let WindowEvent::CloseRequested { api, .. } = ev {
                if w.label() == "main" {
                    api.prevent_close();
                    let _ = w.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::version,
            commands::refresh,
            commands::overview,
            commands::daily,
            commands::breakdown,
            commands::models,
            commands::session,
            commands::session_cached,
            commands::insights,
            commands::change,
            commands::price_info,
            commands::summary_preview,
            commands::export_summary,
            commands::compare,
            settings::get_settings,
            settings::set_settings,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Claude Usage Analytics");
    app.run(|app, ev| match ev {
        // No window left open is not a reason to quit; "Quit" in the menu is.
        RunEvent::ExitRequested {
            api, code: None, ..
        } => api.prevent_exit(),
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => tray::show_main(app),
        _ => {}
    });
}
