//! A click on an agent notification: the exact pane it named, and only that
//! pane's attention generation acknowledged. Port of
//! `useAgentNotificationActivation.ts`.
//!
//! The click arrives on the notification daemon's thread, so it is queued and
//! polled from here rather than waking the app from that thread.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use desktop_core::notifications::{ActivationSink, resolve_notification_route};
use gpui_kit::{App, Global};
use regex::Regex;
use serde_json::{Value, json};

use crate::terminal::integration::routing::{
    PaneDestination, ResolvedNotificationRoute, notification_topology,
    pane_for_resolved_notification,
};

use super::{Agents, model::Generation, report, wire::RequestScope};

/// How often a click is looked for.
const POLL: Duration = Duration::from_millis(250);

#[derive(Clone, Default)]
pub struct ActivationQueue(Arc<Mutex<(bool, VecDeque<Value>)>>);

impl ActivationSink for ActivationQueue {
    fn focus_main_window(&self) {
        self.0.lock().unwrap().0 = true;
    }

    fn emit_activation(&self, payload: Value) {
        self.0.lock().unwrap().1.push_back(payload);
    }
}

pub fn poll(queue: ActivationQueue, cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(POLL).await;
            let (focus, routes) = {
                let mut queued = queue.0.lock().unwrap();
                (std::mem::take(&mut queued.0), std::mem::take(&mut queued.1))
            };
            if !focus && routes.is_empty() {
                continue;
            }
            cx.update(|cx| {
                if focus {
                    cx.activate(true);
                }
                for payload in routes {
                    match Route::parse(&payload) {
                        Some(route) => activate(route, false, cx),
                        None => eprintln!("muxflow: notification route is malformed"),
                    }
                }
            });
        }
    })
    .detach();
}

/// The route a notification carried, as `desktop-core` delivers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub host_profile: String,
    pub server_identity: String,
    pub session_id: String,
    pub session_name: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_id: String,
    pub agent_id: String,
    pub attention_generation: Generation,
}

impl Route {
    pub fn parse(payload: &Value) -> Option<Self> {
        let text = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_owned);
        Some(Self {
            host_profile: text("hostProfile")?,
            server_identity: text("serverIdentity")?,
            session_id: text("sessionId")?,
            session_name: text("sessionName")?,
            window_id: text("windowId")?,
            window_name: text("windowName")?,
            pane_id: text("paneId")?,
            agent_id: text("agentId")?,
            attention_generation: text("attentionGeneration")?.parse().ok()?,
        })
    }

    fn to_json(&self) -> Value {
        json!({
            "hostProfile": self.host_profile,
            "serverIdentity": self.server_identity,
            "sessionId": self.session_id,
            "sessionName": self.session_name,
            "windowId": self.window_id,
            "windowName": self.window_name,
            "paneId": self.pane_id,
            "agentId": self.agent_id,
            "attentionGeneration": self.attention_generation.to_string(),
        })
    }
}

/// An error that says the topology moved under the focus request.
pub fn is_stale_focus_error(error: &str) -> bool {
    Regex::new(
        r"(?i)stale|generation|no longer|not found|unknown (?:pane|window|session)|authoritative",
    )
    .expect("valid pattern")
    .is_match(error)
}

struct Pending {
    route: Route,
    retry_used: bool,
    previous_client: Option<String>,
    previous_epoch: u64,
}

#[derive(Default)]
struct PendingActivation(Option<Pending>);

impl Global for PendingActivation {}

fn active_scope(cx: &App) -> (String, Option<RequestScope>) {
    let Some(agents) = cx.try_global::<Agents>() else {
        return (String::new(), None);
    };
    let runtime = agents.runtime.read(cx);
    (
        runtime.inputs.focus.host_profile_id.clone(),
        runtime.active_scope().cloned(),
    )
}

fn queue_for_fresh_connection(route: Route, retry_used: bool, cx: &mut App) {
    let (_, scope) = active_scope(cx);
    cx.set_global(PendingActivation(Some(Pending {
        route,
        retry_used,
        previous_client: scope.as_ref().map(|scope| scope.client_id.clone()),
        previous_epoch: scope.map_or(0, |scope| scope.connection_epoch),
    })));
}

/// A queued click, once its host is on screen over a fresh connection.
pub fn resume_pending(cx: &mut App) {
    let (host, scope) = active_scope(cx);
    let Some(scope) = scope else { return };
    let ready = cx
        .default_global::<PendingActivation>()
        .0
        .as_ref()
        .is_some_and(|waiting| {
            let fresh_client = waiting
                .previous_client
                .as_ref()
                .is_none_or(|previous| *previous != scope.client_id);
            waiting.route.host_profile == host
                && (fresh_client || scope.connection_epoch > waiting.previous_epoch)
        });
    if ready && let Some(waiting) = cx.default_global::<PendingActivation>().0.take() {
        activate(waiting.route, waiting.retry_used, cx);
    }
}

pub fn activate(route: Route, retry_used: bool, cx: &mut App) {
    let Some(agents) = cx.try_global::<Agents>() else {
        return;
    };
    let (controller, runtime) = (agents.controller.clone(), agents.runtime.clone());
    if route.pane_id.is_empty() {
        return report(
            cx,
            "This agent notification is unmapped and remains available in Agents; no terminal destination was focused.".into(),
        );
    }
    let (host, scope) = active_scope(cx);
    if route.host_profile != host {
        if !controller
            .read(cx)
            .profiles()
            .iter()
            .any(|profile| profile.id == route.host_profile)
        {
            return report(
                cx,
                "Notification host profile no longer exists; the in-app attention item remains available on its configured host.".into(),
            );
        }
        let target = route.host_profile.clone();
        queue_for_fresh_connection(route, retry_used, cx);
        controller.update(cx, |controller, cx| controller.activate_host(&target, cx));
        return;
    }
    let Some(captured) = scope else {
        queue_for_fresh_connection(route, retry_used, cx);
        controller.update(cx, |controller, cx| controller.reconnect_host(&host, cx));
        return;
    };
    let resolved = {
        let controller = controller.read(cx);
        let Some(link) = controller.link(&host) else {
            return;
        };
        let topology = notification_topology(&link.host_state.snapshot, &captured.server_identity);
        let topology = json!({
            "serverIdentity": topology.server_identity,
            "targets": topology.targets.iter().map(|target| json!({
                "sessionId": target.session_id,
                "sessionName": target.session_name,
                "windowId": target.window_id,
                "windowName": target.window_name,
                "paneIds": target.pane_ids,
            })).collect::<Vec<_>>(),
        });
        let resolved = serde_json::from_value(route.to_json())
            .map_err(|error| error.to_string())
            .and_then(|content| {
                let topology =
                    serde_json::from_value(topology).map_err(|error| error.to_string())?;
                resolve_notification_route(content, topology)
            })
            .and_then(|value| {
                resolved_route(&value).ok_or_else(|| "unrecognised resolution".to_owned())
            });
        match resolved {
            Ok(resolved) => {
                match pane_for_resolved_notification(&link.host_state.snapshot, &resolved) {
                    PaneDestination::Target(pane) => Ok(pane.clone()),
                    PaneDestination::Unavailable(reason) => {
                        Err(format!("Notification destination unavailable: {reason}."))
                    }
                }
            }
            Err(error) => Err(format!("Could not resolve notification: {error}")),
        }
    };
    let pane = match resolved {
        Ok(pane) => pane,
        Err(message) => return report(cx, message),
    };
    controller.update(cx, |controller, cx| {
        controller.select_window(&host, &pane.session_id, &pane.window_id, cx);
    });
    let focus = runtime.read(cx).transport().tmux_action(
        captured.client_id.clone(),
        json!({
            "kind": "focusPane",
            "session_id": pane.session_id,
            "window_id": pane.window_id,
            "pane_id": pane.id,
            "expected_server_identity": captured.server_identity,
        }),
    );
    cx.spawn(async move |cx| {
        let focused = focus.await;
        cx.update(|cx| {
            if let Err(error) = focused {
                if !retry_used && is_stale_focus_error(&error) {
                    queue_for_fresh_connection(route, true, cx);
                    report(
                        cx,
                        "Notification topology changed while focusing; refreshing once before resolving the exact destination…".into(),
                    );
                    controller.update(cx, |controller, cx| controller.reconnect_host(&host, cx));
                } else {
                    report(cx, format!("Notification destination could not be focused: {error}"));
                }
                return;
            }
            // Only over the connection the route was resolved on.
            let (_, exact) = active_scope(cx);
            let Some(exact) = exact.filter(|exact| {
                exact.client_id == captured.client_id
                    && exact.connection_epoch == captured.connection_epoch
                    && exact.host_profile_id == captured.host_profile_id
                    && exact.server_identity == captured.server_identity
            }) else {
                return;
            };
            let seen = runtime.update(cx, |runtime, cx| {
                runtime.mark_seen(exact, route.agent_id.clone(), route.attention_generation, None, cx)
            });
            cx.spawn(async move |cx| {
                if let Err(error) = seen.await {
                    cx.update(|cx| {
                        report(
                            cx,
                            format!("Notification opened, but its exact attention generation was not acknowledged: {error}"),
                        )
                    });
                }
            })
            .detach();
        });
    })
    .detach();
}

fn resolved_route(value: &Value) -> Option<ResolvedNotificationRoute> {
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    match value.get("resolution").and_then(Value::as_str)? {
        "exact" => Some(ResolvedNotificationRoute::Exact {
            session_id: text("sessionId")?,
            window_id: text("windowId")?,
            pane_id: text("paneId")?,
            attention_generation: text("attentionGeneration")?,
        }),
        "expired" => Some(ResolvedNotificationRoute::Expired),
        "wrongServer" => Some(ResolvedNotificationRoute::WrongServer),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_route_desktop_core_delivers_with_its_full_generation() {
        let payload = json!({
            "hostProfile": "local", "serverIdentity": "server-a", "sessionId": "$1",
            "sessionName": "work", "windowId": "@1", "windowName": "agent", "paneId": "%1",
            "agentId": "clicked", "attentionGeneration": "18446744073709551614"
        });
        let route = Route::parse(&payload).unwrap();
        assert_eq!(route.attention_generation, u64::MAX - 1);
        assert_eq!(route.to_json(), payload);
        assert_eq!(Route::parse(&json!({ "paneId": "%1" })), None);
    }

    #[test]
    fn treats_a_moved_topology_as_stale_and_anything_else_as_final() {
        for stale in [
            "stale generation",
            "pane %4 no longer exists",
            "unknown pane",
            "Not Found",
        ] {
            assert!(is_stale_focus_error(stale), "{stale}");
        }
        assert!(!is_stale_focus_error("permission denied"));
    }

    #[test]
    fn reads_every_resolution_the_host_gives() {
        assert_eq!(
            resolved_route(
                &json!({ "resolution": "exact", "sessionId": "$1", "windowId": "@1",
                "paneId": "%1", "attentionGeneration": "4" })
            ),
            Some(ResolvedNotificationRoute::Exact {
                session_id: "$1".into(),
                window_id: "@1".into(),
                pane_id: "%1".into(),
                attention_generation: "4".into(),
            })
        );
        assert_eq!(
            resolved_route(&json!({ "resolution": "expired" })),
            Some(ResolvedNotificationRoute::Expired)
        );
        assert_eq!(
            resolved_route(&json!({ "resolution": "wrongServer" })),
            Some(ResolvedNotificationRoute::WrongServer)
        );
        assert_eq!(resolved_route(&json!({ "resolution": "maybe" })), None);
    }
}
