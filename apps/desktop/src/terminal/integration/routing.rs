//! Port of `paneRouting.ts`: which pane an agent row or a notification leads
//! to. Only a pane in the current authoritative topology is a destination; a
//! missing one is said to be gone, never guessed at.

use tmux_control::{Pane, TmuxSnapshot};

#[derive(Debug, PartialEq, Eq)]
pub enum PaneDestination<'a> {
    Target(&'a Pane),
    Unavailable(String),
}

pub fn resolve_terminal_destination<'a>(panes: &'a [Pane], pane_id: &str) -> PaneDestination<'a> {
    match panes.iter().find(|pane| pane.id == pane_id) {
        Some(pane) => PaneDestination::Target(pane),
        None => PaneDestination::Unavailable(format!("pane {pane_id} is no longer available")),
    }
}

/// What the host resolved a notification's route to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedNotificationRoute {
    Exact {
        session_id: String,
        window_id: String,
        pane_id: String,
        attention_generation: String,
    },
    Expired,
    WrongServer,
}

/// One window's panes, named, for the host to resolve a route against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationTarget {
    pub session_id: String,
    pub session_name: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationTopology {
    pub server_identity: String,
    pub targets: Vec<NotificationTarget>,
}

pub fn notification_topology(
    snapshot: &TmuxSnapshot,
    server_identity: &str,
) -> NotificationTopology {
    let targets = snapshot
        .windows
        .iter()
        .map(|window| NotificationTarget {
            session_id: window.session_id.clone(),
            session_name: snapshot
                .sessions
                .iter()
                .find(|session| session.id == window.session_id)
                .map_or_else(|| window.session_id.clone(), |session| session.name.clone()),
            window_id: window.id.clone(),
            window_name: window.name.clone(),
            pane_ids: snapshot
                .panes
                .iter()
                .filter(|pane| pane.window_id == window.id)
                .map(|pane| pane.id.clone())
                .collect(),
        })
        .collect();
    NotificationTopology {
        server_identity: server_identity.to_owned(),
        targets,
    }
}

/// The exact pane a resolved route names, and no other.
pub fn pane_for_resolved_notification<'a>(
    snapshot: &'a TmuxSnapshot,
    route: &ResolvedNotificationRoute,
) -> PaneDestination<'a> {
    let ResolvedNotificationRoute::Exact {
        session_id,
        window_id,
        pane_id,
        ..
    } = route
    else {
        let reason = if *route == ResolvedNotificationRoute::Expired {
            "notification destination expired"
        } else {
            "notification belongs to a replaced tmux server"
        };
        return PaneDestination::Unavailable(reason.into());
    };
    snapshot
        .panes
        .iter()
        .find(|pane| {
            pane.id == *pane_id && pane.window_id == *window_id && pane.session_id == *session_id
        })
        .map_or_else(
            || {
                PaneDestination::Unavailable(
                    "exact notification destination is no longer available".into(),
                )
            },
            PaneDestination::Target,
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tmux_control::{Session, Window};

    fn pane(id: &str) -> Pane {
        Pane {
            id: id.into(),
            session_id: "$1".into(),
            window_id: "@1".into(),
            active: true,
            width: 80,
            height: 24,
            index: 0,
            left: 0,
            top: 0,
            current_path: "/tmp".into(),
            current_command: "bash".into(),
            pane_pid: 1,
            start_command: String::new(),
            title: String::new(),
        }
    }

    fn snapshot(panes: Vec<Pane>) -> TmuxSnapshot {
        TmuxSnapshot {
            sessions: vec![Session {
                id: "$1".into(),
                name: "one".into(),
                window_count: 1,
                attached_clients: 0,
                order: 0,
                pinned: false,
            }],
            windows: vec![Window {
                id: "@1".into(),
                session_id: "$1".into(),
                index: 0,
                name: "shell".into(),
                active: true,
                layout: String::new(),
                zoomed: false,
                pinned: false,
            }],
            panes,
        }
    }

    #[test]
    fn routes_exact_panes_and_never_guesses_missing_pane_ids() {
        let panes = [pane("%1"), pane("%2")];
        assert_eq!(
            resolve_terminal_destination(&panes, "%1"),
            PaneDestination::Target(&panes[0])
        );
        assert_eq!(
            resolve_terminal_destination(&panes, "%2"),
            PaneDestination::Target(&panes[1])
        );
        assert!(matches!(
            resolve_terminal_destination(&panes[..1], "%404"),
            PaneDestination::Unavailable(_)
        ));
    }

    #[test]
    fn maps_only_a_canonical_exact_route_onto_a_visible_pane() {
        let snapshot = snapshot(vec![pane("%1"), pane("%2")]);
        let topology = notification_topology(&snapshot, "server-a");
        assert_eq!(topology.targets[0].session_name, "one");
        assert_eq!(topology.targets[0].pane_ids, ["%1", "%2"]);
        let exact = ResolvedNotificationRoute::Exact {
            session_id: "$1".into(),
            window_id: "@1".into(),
            pane_id: "%2".into(),
            attention_generation: "1".into(),
        };
        assert_eq!(
            pane_for_resolved_notification(&snapshot, &exact),
            PaneDestination::Target(&snapshot.panes[1])
        );
        for route in [
            ResolvedNotificationRoute::Expired,
            ResolvedNotificationRoute::WrongServer,
        ] {
            assert!(matches!(
                pane_for_resolved_notification(&snapshot, &route),
                PaneDestination::Unavailable(_)
            ));
        }
    }

    #[test]
    fn never_substitutes_a_surviving_pane_for_a_closed_target() {
        let snapshot = snapshot(vec![pane("%1")]);
        let closed = ResolvedNotificationRoute::Exact {
            session_id: "$1".into(),
            window_id: "@1".into(),
            pane_id: "%2".into(),
            attention_generation: "1".into(),
        };
        assert!(matches!(
            pane_for_resolved_notification(&snapshot, &closed),
            PaneDestination::Unavailable(_)
        ));
    }
}
