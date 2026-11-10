mod commands;
#[cfg(test)]
mod golden_paths;
mod linux_window;
mod macos_window;
#[cfg(target_os = "macos")]
mod menu;
mod tauri_adapters;

use std::sync::Arc;

use desktop_core::{
    app_state, connection, incidents, notifications, platform, platform::AppPaths, power_events,
    update_check,
};
use tauri::Manager;

use tauri_adapters::{AppEvent, DESKTOP_RESUMED, TauriApp};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // One pool for the commands and the backend they call into.
    tauri::async_runtime::set(platform::runtime_handle());
    let builder = tauri::Builder::default();
    // Only macOS: Tauri's default menu is itself macOS-gated, so replacing it
    // anywhere else would add a menu bar to a window that never had one.
    #[cfg(target_os = "macos")]
    let builder = builder.menu(menu::build);
    builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri::plugin::Builder::<tauri::Wry>::new("navigation-policy")
                .on_navigation(|_, url| {
                    url.scheme() == "tauri"
                        || (cfg!(debug_assertions)
                            && matches!(url.scheme(), "http" | "https")
                            && matches!(url.host_str(), Some("localhost" | "127.0.0.1")))
                })
                .build(),
        )
        .setup(|app| {
            power_events::start(Arc::new(AppEvent::new(
                app.handle().clone(),
                DESKTOP_RESUMED,
            )));
            // Startup, because a kill or a crash never runs `RunEvent::Exit`:
            // the debris those exits leave can only be cleaned by a later launch.
            connection::spawn_orphan_reaper();
            let tauri_app = TauriApp(app.handle().clone());
            let config_dir = tauri_app.app_config_dir()?;
            let profile_path = config_dir.join("profiles.json");
            app.manage(connection::ProfileStore::load(profile_path)?);
            let app_state = app_state::AppStateStore::load(config_dir.join("app-state.json"));
            let theme = app_state.theme();
            let ui_scale = app_state.ui_scale();
            app.manage(app_state);
            app.manage(incidents::IncidentJournal::new(tauri_app.app_log_dir()?));
            app.manage(connection::TerminalClients::default());
            app.manage(connection::files::DownloadManager::default());
            app.manage(connection::files::FileIoManager);
            app.manage(connection::files::UploadManager);
            app.manage(connection::ports::PortForwards::default());
            let notifications = notifications::NativeNotifications::new(
                Arc::new(TauriApp(app.handle().clone())),
                tauri_app.app_data_dir().ok(),
            );
            // At launch, not at the first agent event, so the permission prompt
            // is there while the person is looking. A no-op once they answered.
            let prompt = notifications.clone();
            tauri::async_runtime::spawn_blocking(move || prompt.request_authorization());
            app.manage(notifications);
            // Created here rather than from the config alone so the page can be
            // handed the saved theme and zoom before its first script runs:
            // `public/theme.js` reads them to paint the right palette on the
            // first frame. A user script is not subject to the page's CSP.
            let window_config = app
                .config()
                .app
                .windows
                .first()
                .ok_or("tauri.conf.json declares no window")?
                .clone();
            tauri::WebviewWindowBuilder::from_config(app.handle(), &window_config)?
                .initialization_script(app_state::boot_script(theme, ui_scale))
                .build()?;
            for window in app.webview_windows().values() {
                // The page sets its own colors only once its script runs; until then
                // the platform paints its default, which is dark on a dark OS. Painting
                // the saved theme's background spares a light user that dark frame.
                let _ = window.set_background_color(Some(window_background(theme, window)));
                let _ = window.set_zoom(ui_scale);
                macos_window::enable_native_full_screen(window)?;
                // Cosmetic: a window that keeps GTK's bar is still a usable one,
                // so a failure here must not stop the app from starting.
                if let Err(error) = linux_window::apply(window) {
                    eprintln!("muxflow: {error}");
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::connection::list_host_profiles,
            commands::connection::save_host_profile,
            commands::connection::reset_host_profiles,
            commands::connection::delete_host_profile,
            commands::connection::set_last_profile_id,
            commands::app::load_app_state,
            commands::app::save_app_state,
            commands::app::reset_app_state,
            commands::app::set_ui_scale,
            commands::connection::probe_remote_helper,
            commands::connection::install_remote_helper,
            commands::app::resolve_notification_route,
            commands::app::emit_agent_notification,
            commands::app::emit_test_notification,
            commands::app::notification_permission_status,
            commands::app::record_incident,
            linux_window::window_chrome,
            commands::connection::start_terminal,
            commands::connection::stop_terminal,
            commands::connection::prewarm_terminal_bulk,
            commands::connection::acknowledge_terminal_delivery,
            commands::app::finalize_bridge_measurement,
            commands::app::bridge_final_totals,
            commands::connection::send_terminal_input,
            commands::connection::send_terminal_input_bytes,
            commands::connection::resize_terminal_client,
            commands::connection::probe_terminal_link,
            commands::connection::discard_ssh_masters,
            commands::connection::set_terminal_visibility,
            commands::connection::set_terminal_colors,
            commands::connection::request_terminal_seed,
            commands::connection::request_terminal_history,
            commands::connection::terminal_link_stats,
            commands::connection::input_latency_stats,
            commands::connection::tmux_action,
            commands::files::file_request,
            commands::files::cancel_file_request,
            commands::connection::git_request,
            commands::connection::agent_request,
            commands::connection::automation_request,
            commands::connection::cancel_git_request,
            commands::connection::read_git_diff_content,
            commands::connection::cancel_git_diff_content,
            commands::connection::ports_forward,
            commands::connection::ports_stop,
            commands::connection::ports_stop_host,
            commands::connection::ports_list,
            commands::connection::ports_detect,
            commands::files::start_download,
            commands::files::cancel_download,
            commands::files::suggest_download_destination,
            commands::files::open_download,
            commands::files::reveal_download,
            commands::files::start_file_read,
            commands::files::start_file_write,
            commands::files::cancel_file_io,
            commands::files::start_terminal_upload_preflight,
            commands::files::cancel_terminal_upload_preflight,
            commands::files::inspect_local_terminal_paths,
            commands::files::start_terminal_upload,
            commands::files::cancel_terminal_upload,
            commands::files::stage_clipboard_png,
            commands::files::read_native_terminal_clipboard,
            commands::files::write_native_terminal_clipboard,
            commands::app::open_external_link,
            commands::app::check_for_update,
            commands::app::install_update,
            commands::app::perf_log_enabled,
            commands::app::append_perf_log,
            commands::app::acknowledge_bridge_events,
            commands::app::sample_native_measurements,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Muxflow")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                app.state::<connection::ports::PortForwards>().stop_all();
                connection::close_all_control_masters();
                if update_check::relaunch_on_exit() {
                    tauri::process::restart(&app.env());
                }
            }
        });
}

/// The chrome background (`--bg` in palette.css) of the theme the page will
/// draw, resolving System the way the page's `prefers-color-scheme` will.
fn window_background(
    theme: app_state::ThemePreference,
    window: &tauri::WebviewWindow,
) -> tauri::window::Color {
    let dark = match theme {
        app_state::ThemePreference::Dark => true,
        app_state::ThemePreference::Light => false,
        app_state::ThemePreference::System => window
            .theme()
            .map_or(true, |theme| theme == tauri::Theme::Dark),
    };
    if dark {
        tauri::window::Color(0x28, 0x2c, 0x34, 0xff)
    } else {
        tauri::window::Color(0xfa, 0xfa, 0xfa, 0xff)
    }
}
