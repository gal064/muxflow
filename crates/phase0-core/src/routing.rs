use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationRoute {
    pub host_profile: String,
    pub server_identity: String,
    pub session_id: String,
    pub session_name: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_id: String,
    pub agent_id: String,
    pub attention_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteTarget {
    pub session_id: String,
    pub session_name: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyntheticTopology {
    pub server_identity: String,
    pub targets: Vec<RouteTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "resolution")]
pub enum ResolvedRoute {
    Exact {
        session_id: String,
        window_id: String,
        pane_id: String,
        attention_generation: u64,
    },
    Expired,
    WrongServer,
}

pub fn resolve_route(route: &NotificationRoute, topology: &SyntheticTopology) -> ResolvedRoute {
    if route.server_identity != topology.server_identity {
        return ResolvedRoute::WrongServer;
    }

    let Some(target) = topology
        .targets
        .iter()
        .find(|target| target.pane_ids.contains(&route.pane_id))
    else {
        return ResolvedRoute::Expired;
    };

    ResolvedRoute::Exact {
        session_id: target.session_id.clone(),
        window_id: target.window_id.clone(),
        pane_id: route.pane_id.clone(),
        attention_generation: route.attention_generation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> NotificationRoute {
        NotificationRoute {
            host_profile: "local".into(),
            server_identity: "server-a".into(),
            session_id: "$1".into(),
            session_name: "work".into(),
            window_id: "@2".into(),
            window_name: "agent".into(),
            pane_id: "%3".into(),
            agent_id: "codex:abc".into(),
            attention_generation: 42,
        }
    }

    #[test]
    fn stable_ids_survive_renames() {
        let topology = SyntheticTopology {
            server_identity: "server-a".into(),
            targets: vec![RouteTarget {
                session_id: "$1".into(),
                session_name: "renamed workspace".into(),
                window_id: "@2".into(),
                window_name: "renamed window".into(),
                pane_ids: vec!["%3".into()],
            }],
        };
        assert!(matches!(
            resolve_route(&route(), &topology),
            ResolvedRoute::Exact { .. }
        ));
    }

    #[test]
    fn missing_pane_expires_without_guessing_another_destination() {
        let topology = SyntheticTopology {
            server_identity: "server-a".into(),
            targets: vec![RouteTarget {
                session_id: "$1".into(),
                session_name: "work".into(),
                window_id: "@2".into(),
                window_name: "agent".into(),
                pane_ids: vec!["%9".into()],
            }],
        };
        assert_eq!(resolve_route(&route(), &topology), ResolvedRoute::Expired);
    }

    #[test]
    fn matching_pane_number_on_another_server_is_rejected() {
        let topology = SyntheticTopology {
            server_identity: "server-b".into(),
            targets: vec![RouteTarget {
                session_id: "$1".into(),
                session_name: "work".into(),
                window_id: "@2".into(),
                window_name: "agent".into(),
                pane_ids: vec!["%3".into()],
            }],
        };

        assert_eq!(
            resolve_route(&route(), &topology),
            ResolvedRoute::WrongServer
        );
    }

    #[test]
    fn exact_pane_identity_survives_a_move_between_windows() {
        let topology = SyntheticTopology {
            server_identity: "server-a".into(),
            targets: vec![RouteTarget {
                session_id: "$9".into(),
                session_name: "elsewhere".into(),
                window_id: "@9".into(),
                window_name: "moved".into(),
                pane_ids: vec!["%3".into()],
            }],
        };
        assert_eq!(
            resolve_route(&route(), &topology),
            ResolvedRoute::Exact {
                session_id: "$9".into(),
                window_id: "@9".into(),
                pane_id: "%3".into(),
                attention_generation: 42,
            }
        );
    }
}
