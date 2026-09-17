#![allow(clippy::too_many_arguments)] // Freedesktop Notify has a fixed nine-argument D-Bus signature.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use phase0_core::NotificationRoute;

use super::{TEST_BODY, TEST_TITLE};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use zbus::{blocking::Connection, proxy, zvariant::Value};

#[proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait FreedesktopNotifications {
    fn get_capabilities(&self) -> zbus::Result<Vec<String>>;

    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, &Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;

    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
}

#[derive(Clone)]
pub struct NativeNotifications {
    connection: Result<Connection, String>,
    routes: Arc<Mutex<PendingNotificationRoutes>>,
    action_listener_healthy: Arc<AtomicBool>,
}

const MAX_PENDING_ROUTES: usize = 512;
const CLOSED_ACTION_GRACE: Duration = Duration::from_secs(5);
const LISTENER_START_TIMEOUT: Duration = Duration::from_secs(1);

struct PendingRoute {
    route: NotificationRoute,
    inserted_at: Instant,
    closed_at: Option<Instant>,
}

#[derive(Default)]
struct PendingNotificationRoutes {
    entries: HashMap<u32, PendingRoute>,
    retired_ids: HashMap<u32, Instant>,
}

enum RouteInstall {
    Pending,
    ReusedId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeActionPolicy {
    NonActionable,
    Actionable,
    ReplaceWithoutActions,
}

fn native_action_policy(server_actionable: bool, install: &RouteInstall) -> NativeActionPolicy {
    match (server_actionable, install) {
        (true, RouteInstall::ReusedId) => NativeActionPolicy::ReplaceWithoutActions,
        (true, _) => NativeActionPolicy::Actionable,
        (false, _) => NativeActionPolicy::NonActionable,
    }
}

impl PendingNotificationRoutes {
    fn insert(&mut self, id: u32, route: NotificationRoute, now: Instant) -> RouteInstall {
        self.remove_expired(now);
        if self.entries.remove(&id).is_some() || self.retired_ids.remove(&id).is_some() {
            // A daemon may recycle numeric IDs. Never let a delayed action for
            // the retired notification activate the replacement route.
            self.retired_ids.insert(id, now);
            return RouteInstall::ReusedId;
        }
        if self.entries.len() >= MAX_PENDING_ROUTES
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, pending)| pending.inserted_at)
                .map(|(id, _)| *id)
        {
            self.entries.remove(&oldest);
            self.retired_ids.insert(oldest, now);
        }
        self.entries.insert(
            id,
            PendingRoute {
                route,
                inserted_at: now,
                closed_at: None,
            },
        );
        RouteInstall::Pending
    }

    fn close(&mut self, id: u32, now: Instant) {
        self.remove_expired(now);
        if let Some(pending) = self.entries.get_mut(&id) {
            // Some daemons send NotificationClosed immediately before their
            // ActionInvoked signal. Retain a short tombstone so either legal
            // signal ordering resolves the same immutable route exactly once.
            pending.closed_at.get_or_insert(now);
        }
    }

    fn take_for_action(&mut self, id: u32, now: Instant) -> Option<NotificationRoute> {
        self.remove_expired(now);
        if let Some(pending) = self.entries.remove(&id) {
            self.retired_ids.insert(id, now);
            return Some(pending.route);
        }
        // Notification IDs are daemon-global. An action received before this
        // client has installed an exact route may belong to another process or
        // a recycled daemon ID, so it must fail closed rather than be retained.
        None
    }

    fn remove_expired(&mut self, now: Instant) {
        let mut expired = Vec::new();
        self.entries.retain(|id, pending| {
            let retain = pending
                .closed_at
                .is_none_or(|closed| now.duration_since(closed) <= CLOSED_ACTION_GRACE);
            if !retain {
                expired.push(*id);
            }
            retain
        });
        for id in expired {
            self.retired_ids.insert(id, now);
        }
        self.retired_ids
            .retain(|_, at| now.duration_since(*at) <= CLOSED_ACTION_GRACE);
        bound_oldest(&mut self.retired_ids);
    }
}

fn bound_oldest(values: &mut HashMap<u32, Instant>) {
    while values.len() > MAX_PENDING_ROUTES {
        let Some(oldest) = values.iter().min_by_key(|(_, at)| *at).map(|(id, _)| *id) else {
            break;
        };
        values.remove(&oldest);
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationReceipt {
    pub id: u32,
    pub actionable: bool,
}

impl NativeNotifications {
    pub fn new(app: AppHandle) -> Self {
        let connection = Connection::session().map_err(|error| error.to_string());
        let routes = Arc::new(Mutex::new(PendingNotificationRoutes::default()));
        let action_listener_healthy = Arc::new(AtomicBool::new(false));
        if let Ok(listener_connection) = connection.clone() {
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let action_routes = Arc::clone(&routes);
            let action_health = Arc::clone(&action_listener_healthy);
            let action_app = app.clone();
            let spawned = thread::Builder::new()
                .name("notification-actions".into())
                .spawn(move || {
                    listen_for_actions(
                        listener_connection,
                        action_routes,
                        action_app,
                        action_health,
                        ready_tx,
                    )
                });
            if spawned.is_ok() {
                let _ = ready_rx.recv_timeout(LISTENER_START_TIMEOUT);
            }
            let closed_routes = Arc::clone(&routes);
            if let Ok(closed_connection) = connection.clone() {
                let _ = thread::Builder::new()
                    .name("notification-closures".into())
                    .spawn(move || listen_for_closures(closed_connection, closed_routes));
            }
        }
        Self {
            connection,
            routes,
            action_listener_healthy,
        }
    }

    pub fn notify(
        &self,
        title: &str,
        body: &str,
        route: NotificationRoute,
        request_action: bool,
        // Freedesktop has no foreground-suppression hook: the daemon decides
        // what a notification does while its client is focused, and there is
        // no delegate to consult. The frontend's answer is accepted and
        // ignored here so the command has one shape on every platform.
        _present_in_foreground: bool,
    ) -> Result<NotificationReceipt, String> {
        self.post(title, body, request_action.then_some(route))
    }

    /// The one notification a person can ask for directly. No route: "did a
    /// banner appear" is the whole question, and an Open button on it would
    /// have nowhere to go.
    pub fn send_test_notification(&self) -> Result<NotificationReceipt, String> {
        self.post(TEST_TITLE, TEST_BODY, None)
    }

    /// Freedesktop has no per-app permission model — a notification daemon
    /// either answers or there is nothing to deliver through.
    pub fn authorization_status(&self) -> Result<String, String> {
        let Ok(connection) = self.connection.as_ref() else {
            return Ok("unsupported".into());
        };
        let reachable = FreedesktopNotificationsProxyBlocking::new(connection)
            .and_then(|proxy| proxy.get_capabilities())
            .is_ok();
        Ok(if reachable {
            "authorized"
        } else {
            "unsupported"
        }
        .into())
    }

    fn post(
        &self,
        title: &str,
        body: &str,
        route: Option<NotificationRoute>,
    ) -> Result<NotificationReceipt, String> {
        let connection = self.connection.as_ref().map_err(Clone::clone)?;
        let proxy = FreedesktopNotificationsProxyBlocking::new(connection)
            .map_err(|error| error.to_string())?;
        let capabilities = proxy
            .get_capabilities()
            .map_err(|error| error.to_string())?;
        let actionable = notification_actionable(
            route.is_some(),
            capabilities
                .iter()
                .any(|capability| capability == "actions"),
            &self.action_listener_healthy,
        );
        let actions = if actionable {
            &["default", "Open"] as &[&str]
        } else {
            &[]
        };
        let id = proxy
            .notify(
                "Muxflow",
                0,
                "utilities-terminal",
                title,
                body,
                actions,
                HashMap::new(),
                15_000,
            )
            .map_err(|error| error.to_string())?;
        let install = match route {
            Some(route) if actionable => {
                self.routes
                    .lock()
                    .unwrap()
                    .insert(id, route, Instant::now())
            }
            _ => RouteInstall::Pending,
        };
        let action_policy = native_action_policy(actionable, &install);
        let id = if action_policy == NativeActionPolicy::ReplaceWithoutActions {
            // The daemon recycled an ID while an old action may still be in
            // flight. Replace the visible notification without actions so we
            // never advertise an Open button whose route must be rejected.
            proxy
                .notify(
                    "Muxflow",
                    id,
                    "utilities-terminal",
                    title,
                    body,
                    &[],
                    HashMap::new(),
                    15_000,
                )
                .map_err(|error| error.to_string())?
        } else {
            id
        };
        Ok(NotificationReceipt {
            id,
            actionable: action_policy == NativeActionPolicy::Actionable,
        })
    }
}

fn listener_actionable(server_supports_actions: bool, health: &AtomicBool) -> bool {
    server_supports_actions && health.load(Ordering::Acquire)
}

fn notification_actionable(
    request_action: bool,
    server_supports_actions: bool,
    health: &AtomicBool,
) -> bool {
    request_action && listener_actionable(server_supports_actions, health)
}

fn listen_for_closures(connection: Connection, routes: Arc<Mutex<PendingNotificationRoutes>>) {
    let Ok(proxy) = FreedesktopNotificationsProxyBlocking::new(&connection) else {
        return;
    };
    let Ok(mut closures) = proxy.receive_notification_closed() else {
        return;
    };
    for closed in &mut closures {
        if let Ok(arguments) = closed.args() {
            routes
                .lock()
                .unwrap()
                .close(*arguments.id(), Instant::now());
        }
    }
}

fn listen_for_actions(
    connection: Connection,
    routes: Arc<Mutex<PendingNotificationRoutes>>,
    app: AppHandle,
    health: Arc<AtomicBool>,
    ready: mpsc::SyncSender<bool>,
) {
    let Ok(proxy) = FreedesktopNotificationsProxyBlocking::new(&connection) else {
        let _ = ready.send(false);
        return;
    };
    let Ok(mut actions) = proxy.receive_action_invoked() else {
        let _ = ready.send(false);
        return;
    };
    health.store(true, Ordering::Release);
    let _guard = ListenerHealthGuard(Arc::clone(&health));
    let _ = ready.send(true);
    for action in &mut actions {
        let Ok(arguments) = action.args() else {
            continue;
        };
        if arguments.action_key() != "default" {
            continue;
        }
        let route = routes
            .lock()
            .unwrap()
            .take_for_action(*arguments.id(), Instant::now());
        if let Some(route) = route {
            deliver_activation(&app, route);
        }
    }
}

struct ListenerHealthGuard(Arc<AtomicBool>);

impl Drop for ListenerHealthGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

trait ActivationSink {
    fn focus_main_window(&self);
    fn emit_activation(&self, payload: serde_json::Value);
}

impl ActivationSink for AppHandle {
    fn focus_main_window(&self) {
        if let Some(window) = self.get_webview_window("main") {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
    }

    fn emit_activation(&self, payload: serde_json::Value) {
        let _ = self.emit("notification-activated", payload);
    }
}

fn deliver_activation(sink: &impl ActivationSink, route: NotificationRoute) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestSink {
        focused: AtomicBool,
        payloads: Mutex<Vec<serde_json::Value>>,
    }

    impl ActivationSink for TestSink {
        fn focus_main_window(&self) {
            self.focused.store(true, Ordering::Release);
        }

        fn emit_activation(&self, payload: serde_json::Value) {
            assert!(self.focused.load(Ordering::Acquire));
            self.payloads.lock().unwrap().push(payload);
        }
    }

    fn route(agent_id: &str) -> NotificationRoute {
        NotificationRoute {
            host_profile: "local".into(),
            server_identity: "server".into(),
            session_id: "$1".into(),
            session_name: "work".into(),
            window_id: "@1".into(),
            window_name: "agent".into(),
            pane_id: "%1".into(),
            agent_id: agent_id.into(),
            attention_generation: 1,
        }
    }

    #[test]
    fn action_before_close_consumes_the_route_once() {
        let now = Instant::now();
        let mut routes = PendingNotificationRoutes::default();
        assert!(matches!(
            routes.insert(1, route("agent-1"), now),
            RouteInstall::Pending
        ));
        assert_eq!(routes.take_for_action(1, now).unwrap().agent_id, "agent-1");
        routes.close(1, now);
        assert!(routes.take_for_action(1, now).is_none());
    }

    #[test]
    fn close_before_action_retains_the_route_during_daemon_signal_grace() {
        let now = Instant::now();
        let mut routes = PendingNotificationRoutes::default();
        assert!(matches!(
            routes.insert(1, route("agent-1"), now),
            RouteInstall::Pending
        ));
        routes.close(1, now + Duration::from_millis(1));
        assert_eq!(
            routes
                .take_for_action(1, now + Duration::from_millis(2))
                .unwrap()
                .agent_id,
            "agent-1"
        );
    }

    #[test]
    fn closed_route_expires_without_an_action() {
        let now = Instant::now();
        let mut routes = PendingNotificationRoutes::default();
        assert!(matches!(
            routes.insert(1, route("agent-1"), now),
            RouteInstall::Pending
        ));
        routes.close(1, now);
        assert!(
            routes
                .take_for_action(1, now + CLOSED_ACTION_GRACE + Duration::from_millis(1))
                .is_none()
        );
    }

    #[test]
    fn unknown_early_action_fails_closed_and_cannot_attach_to_a_later_route() {
        let now = Instant::now();
        let mut routes = PendingNotificationRoutes::default();
        assert!(routes.take_for_action(41, now).is_none());
        assert!(matches!(
            routes.insert(41, route("later"), now + Duration::from_millis(1)),
            RouteInstall::Pending
        ));
        assert_eq!(
            routes
                .take_for_action(41, now + Duration::from_millis(2))
                .unwrap()
                .agent_id,
            "later"
        );
    }

    #[test]
    fn reused_id_never_consumes_a_delayed_action_from_the_retired_route() {
        let now = Instant::now();
        let mut routes = PendingNotificationRoutes::default();
        assert!(matches!(
            routes.insert(9, route("old"), now),
            RouteInstall::Pending
        ));
        assert_eq!(routes.take_for_action(9, now).unwrap().agent_id, "old");
        assert!(
            routes
                .take_for_action(9, now + Duration::from_millis(1))
                .is_none()
        );
        assert!(matches!(
            routes.insert(9, route("new"), now + Duration::from_millis(2)),
            RouteInstall::ReusedId
        ));
        assert!(
            routes
                .take_for_action(9, now + Duration::from_millis(3))
                .is_none()
        );
        assert_eq!(
            native_action_policy(true, &RouteInstall::ReusedId),
            NativeActionPolicy::ReplaceWithoutActions,
        );
        assert_eq!(
            native_action_policy(false, &RouteInstall::ReusedId),
            NativeActionPolicy::NonActionable,
        );
    }

    #[test]
    fn actionability_requires_a_live_initialized_listener_and_degrades_on_exit() {
        let health = Arc::new(AtomicBool::new(false));
        assert!(!listener_actionable(true, &health));
        health.store(true, Ordering::Release);
        assert!(listener_actionable(true, &health));
        assert!(notification_actionable(true, true, &health));
        assert!(!notification_actionable(false, true, &health));
        assert!(!listener_actionable(false, &health));
        drop(ListenerHealthGuard(Arc::clone(&health)));
        assert!(!listener_actionable(true, &health));
    }

    #[test]
    fn activation_focuses_the_app_before_emitting_the_lossless_route() {
        let sink = TestSink::default();
        deliver_activation(&sink, route("agent-1"));
        assert!(sink.focused.load(Ordering::Acquire));
        let payloads = sink.payloads.lock().unwrap();
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0]["agentId"], "agent-1");
        assert_eq!(payloads[0]["attentionGeneration"], "1");
    }
}
