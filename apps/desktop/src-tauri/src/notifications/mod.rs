use phase0_core::{NotificationRoute, ResolvedRoute, SyntheticTopology, resolve_route};

/// The one notification a person can ask for, worded once for every backend.
pub(crate) const TEST_TITLE: &str = "Muxflow";
pub(crate) const TEST_BODY: &str = "Test notification — delivery works.";

/// The whole vocabulary `authorization_status` may answer with.
///
/// Every backend maps its own platform's states onto these, and the frontend
/// renders one sentence per word (`features/agents/notifications.ts`). Naming
/// them once here is what stops a backend inventing a sixth word that the UI
/// would render as an empty line.
pub(crate) const PERMISSION_STATUSES: [&str; 5] = [
    "authorized",
    "denied",
    "notDetermined",
    "provisional",
    "unsupported",
];

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod unsupported;

#[cfg(target_os = "linux")]
pub use linux::{NativeNotifications, NotificationReceipt};
#[cfg(target_os = "macos")]
pub use macos::{NativeNotifications, NotificationReceipt};
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub use unsupported::{NativeNotifications, NotificationReceipt};

/// What the application does when a person clicks a notification.
pub trait ActivationSink: Send + Sync + 'static {
    fn focus_main_window(&self);
    fn emit_activation(&self, payload: serde_json::Value);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn deliver_activation(sink: &dyn ActivationSink, route: NotificationRoute) {
    sink.focus_main_window();
    sink.emit_activation(serde_json::json!({
        "hostProfile": route.host_profile,
        "serverIdentity": route.server_identity,
        "sessionId": route.session_id,
        "sessionName": route.session_name,
        "windowId": route.window_id,
        "windowName": route.window_name,
        "paneId": route.pane_id,
        "agentId": route.agent_id,
        "attentionGeneration": route.attention_generation.to_string(),
    }));
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentNotificationRouteContent {
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

pub fn resolve_notification_route(
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
pub struct AgentNotificationContent {
    title: String,
    body: String,
    request_action: bool,
    /// Whether macOS should show this while the app itself is frontmost. The
    /// frontend is the only side that knows which pane the user is looking at.
    ///
    /// Defaulted to *presenting*, not to suppressing. A missing field is a bug
    /// either way, but its two failure modes are not equal: showing a banner
    /// for a pane already on screen is a duplicate, while suppressing one is
    /// silence — which is the exact defect this flag exists to end, and the
    /// kind nobody reports because nothing happens.
    #[serde(default = "present_by_default")]
    present_in_foreground: bool,
    route: AgentNotificationRouteContent,
}

fn present_by_default() -> bool {
    true
}

pub async fn emit_agent_notification(
    notification: AgentNotificationContent,
    notifications: &NativeNotifications,
) -> Result<NotificationReceipt, String> {
    // The semantic reducer intentionally supplies only agent/state/workspace/tab.
    // Keep a conservative bridge bound so an accidental payload cannot become a
    // large or multiline lock-screen disclosure.
    if !valid_notification_content(&notification.title, &notification.body) {
        return Err("native notification content is malformed".into());
    }
    let notifications = notifications.clone();
    let route = notification.route.into_route()?;
    crate::platform::spawn_blocking(move || {
        notifications.notify(
            &notification.title,
            &notification.body,
            route,
            notification.request_action,
            notification.present_in_foreground,
        )
    })
    .await
    .map_err(|error| format!("native notification worker failed: {error}"))?
}

/// What the OS says about this app's permission, so Settings can say it too.
///
/// Read-only and never prompts: a status line that raised a modal system
/// prompt just for being looked at would be a worse surface than none.
pub async fn notification_permission_status(
    notifications: &NativeNotifications,
) -> Result<String, String> {
    let notifications = notifications.clone();
    let status = crate::platform::spawn_blocking(move || notifications.authorization_status())
        .await
        .map_err(|error| format!("notification status worker failed: {error}"))??;
    // Three backends write this vocabulary independently and the UI renders one
    // sentence per word, so a sixth word would render as an empty status line.
    // The frontend re-checks it too; this is where a new backend finds out.
    debug_assert!(
        PERMISSION_STATUSES.contains(&status.as_str()),
        "{status} is not a notification permission the UI can render"
    );
    Ok(status)
}

/// The notification the user asks for from Settings.
///
/// The permission prompt itself is raised at launch; this is how a person
/// checks that banners actually arrive.
pub async fn emit_test_notification(
    notifications: &NativeNotifications,
) -> Result<NotificationReceipt, String> {
    let notifications = notifications.clone();
    crate::platform::spawn_blocking(move || notifications.send_test_notification())
        .await
        .map_err(|error| format!("native notification worker failed: {error}"))?
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
