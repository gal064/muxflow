mod app_state;
mod connection;
mod external_links;
mod notifications;
mod power_events;

use phase0_core::{NotificationRoute, ResolvedRoute, SyntheticTopology, resolve_route};
use tauri::Manager;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentNotificationRouteContent {
    host_profile: String,
    server_identity: String,
    session_id: String,
    session_name: String,
    window_id: String,
    window_name: String,
    pane_id: String,
    agent_id: String,
    attention_generation: String,
}

impl AgentNotificationRouteContent {
    fn into_route(self) -> Result<NotificationRoute, String> {
        Ok(NotificationRoute {
            host_profile: self.host_profile,
            server_identity: self.server_identity,
            session_id: self.session_id,
            session_name: self.session_name,
            window_id: self.window_id,
            window_name: self.window_name,
            pane_id: self.pane_id,
            agent_id: self.agent_id,
            attention_generation: self
                .attention_generation
                .parse()
                .map_err(|_| "attentionGeneration must be a decimal u64 string")?,
        })
    }
}

#[tauri::command]
fn resolve_notification_route(
    route: AgentNotificationRouteContent,
    topology: SyntheticTopology,
) -> Result<serde_json::Value, String> {
    let resolved = resolve_route(&route.into_route()?, &topology);
    Ok(match resolved {
        ResolvedRoute::Exact {
            session_id,
            window_id,
            pane_id,
            attention_generation,
        } => serde_json::json!({
            "resolution": "exact", "sessionId": session_id, "windowId": window_id,
            "paneId": pane_id, "attentionGeneration": attention_generation.to_string(),
        }),
        ResolvedRoute::Expired => serde_json::json!({ "resolution": "expired" }),
        ResolvedRoute::WrongServer => serde_json::json!({ "resolution": "wrongServer" }),
    })
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentNotificationContent {
    title: String,
    body: String,
    request_action: bool,
    route: AgentNotificationRouteContent,
}

#[tauri::command]
fn emit_agent_notification(
    notification: AgentNotificationContent,
    notifications: tauri::State<'_, notifications::NativeNotifications>,
) -> Result<notifications::NotificationReceipt, String> {
    // The semantic reducer intentionally supplies only agent/state/workspace/tab.
    // Keep a conservative bridge bound so an accidental payload cannot become a
    // large or multiline lock-screen disclosure.
    if !valid_notification_content(&notification.title, &notification.body) {
        return Err("native notification content is malformed".into());
    }
    notifications.notify(
        &notification.title,
        &notification.body,
        notification.route.into_route()?,
        notification.request_action,
    )
}

fn valid_notification_content(title: &str, body: &str) -> bool {
    !title.is_empty()
        && title.len() <= 160
        && body.len() <= 320
        && !title
            .chars()
            .any(|character| matches!(character, '\r' | '\n'))
        && !body
            .chars()
            .any(|character| matches!(character, '\r' | '\n'))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
        .on_window_event(|_, event| {
            if matches!(event, tauri::WindowEvent::Destroyed) {
                connection::close_all_control_masters();
            }
        })
        .setup(|app| {
            power_events::start(app.handle().clone());
            let config_dir = app.path().app_config_dir()?;
            let profile_path = config_dir.join("profiles.json");
            app.manage(connection::ProfileStore::load(profile_path)?);
            app.manage(app_state::AppStateStore::load(
                config_dir.join("app-state.json"),
            ));
            app.manage(connection::TerminalClients::default());
            app.manage(connection::files::DownloadManager);
            app.manage(connection::files::FileIoManager);
            app.manage(connection::files::UploadManager);
            app.manage(notifications::NativeNotifications::new(
                app.handle().clone(),
            ));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connection::profiles::list_host_profiles,
            connection::profiles::save_host_profile,
            connection::profiles::reset_host_profiles,
            app_state::load_app_state,
            app_state::save_app_state,
            app_state::reset_app_state,
            connection::helper::probe_remote_helper,
            connection::helper::install_remote_helper,
            resolve_notification_route,
            emit_agent_notification,
            connection::start_terminal,
            connection::stop_terminal,
            connection::send_terminal_input,
            connection::send_terminal_input_bytes,
            connection::resize_terminal_client,
            connection::set_terminal_visibility,
            connection::request_terminal_seed,
            connection::tmux_action::tmux_action,
            connection::files::file_request,
            connection::git::git_request,
            connection::agent::agent_request,
            connection::git::cancel_git_request,
            connection::files::download_manager::start_download,
            connection::files::download_manager::cancel_download,
            connection::files::editor_manager::start_file_read,
            connection::files::editor_manager::start_file_write,
            connection::files::editor_manager::cancel_file_io,
            connection::files::upload_manager::start_terminal_upload_preflight,
            connection::files::upload_manager::cancel_terminal_upload_preflight,
            connection::files::upload_manager::inspect_local_terminal_paths,
            connection::files::upload_manager::start_terminal_upload,
            connection::files::upload_manager::cancel_terminal_upload,
            connection::files::upload_manager::stage_clipboard_png,
            connection::files::native_clipboard::read_native_terminal_clipboard,
            external_links::open_external_link,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run tmux Agent IDE");
}

#[cfg(test)]
mod notification_content_tests {
    use super::{AgentNotificationRouteContent, valid_notification_content};

    #[test]
    fn native_notification_content_is_bounded_and_single_line() {
        assert!(valid_notification_content(
            "Codex needs attention",
            "work · agent · Blocked"
        ));
        assert!(!valid_notification_content("", "work"));
        assert!(!valid_notification_content("agent\nprompt", "work"));
        assert!(!valid_notification_content("agent", "output\nsecret"));
        assert!(!valid_notification_content(&"a".repeat(161), "work"));
        assert!(!valid_notification_content("agent", &"a".repeat(321)));
    }

    #[test]
    fn notification_attention_generation_is_parsed_losslessly() {
        let route = AgentNotificationRouteContent {
            host_profile: "local".into(),
            server_identity: "server".into(),
            session_id: "$1".into(),
            session_name: "work".into(),
            window_id: "@1".into(),
            window_name: "agent".into(),
            pane_id: "%1".into(),
            agent_id: "agent-1".into(),
            attention_generation: u64::MAX.to_string(),
        }
        .into_route()
        .expect("valid generation");
        assert_eq!(route.attention_generation, u64::MAX);
    }
}
