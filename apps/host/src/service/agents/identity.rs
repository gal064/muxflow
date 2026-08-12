use super::{StoredRoute, StoredState};

pub(super) struct HookCandidates {
    pub pane: Option<String>,
    pub native: Option<String>,
}

pub(super) fn hook_candidates(
    state: &StoredState,
    adapter_id: &str,
    active_server_identity: &str,
    native_session_id: &str,
    route: &StoredRoute,
    agent_id: &str,
    route_verified: bool,
) -> HookCandidates {
    if !route_verified {
        return HookCandidates {
            pane: state.agents.contains_key(agent_id).then(|| agent_id.into()),
            native: None,
        };
    }
    let pane = state
        .agents
        .values()
        .find(|record| {
            record.adapter_id == adapter_id
                && record.route.server_identity == active_server_identity
                && ((!route.pane_id.is_empty() && record.route.pane_id == route.pane_id)
                    || record.agent_id == agent_id)
        })
        .map(|record| record.agent_id.clone());
    let native = if native_session_id.is_empty() {
        None
    } else {
        state
            .agents
            .values()
            .find(|record| {
                record.adapter_id == adapter_id
                    && record.route.server_identity == active_server_identity
                    && record.native_session_id == native_session_id
            })
            .map(|record| record.agent_id.clone())
    };
    HookCandidates { pane, native }
}

pub(super) fn manual_agent_id(adapter_id: &str, server_identity: &str, pane_id: &str) -> String {
    hashed_id(adapter_id, &["manual", server_identity, pane_id])
}

pub(super) fn native_agent_id(
    adapter_id: &str,
    server_identity: &str,
    native_session_id: &str,
) -> String {
    hashed_id(adapter_id, &["native", server_identity, native_session_id])
}

pub(super) fn unmapped_hook_agent_id(
    adapter_id: &str,
    origin_server_identity: &str,
    pane_id: &str,
    native_session_id: &str,
) -> String {
    if native_session_id.is_empty() {
        hashed_id(
            adapter_id,
            &["unmapped-manual", origin_server_identity, pane_id],
        )
    } else {
        hashed_id(
            adapter_id,
            &[
                "unmapped-native",
                origin_server_identity,
                pane_id,
                native_session_id,
            ],
        )
    }
}

fn hashed_id(adapter_id: &str, components: &[&str]) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(b"ade-agent-id-v2");
    for component in std::iter::once(adapter_id).chain(components.iter().copied()) {
        hash.update(&(component.len() as u64).to_be_bytes());
        hash.update(component.as_bytes());
    }
    format!("{adapter_id}:{}", &hash.finalize().to_hex()[..24])
}

pub(super) fn direct_route(
    topology: &tmux_control::TmuxSnapshot,
    identity: &str,
    pane_id: &str,
) -> Option<StoredRoute> {
    route_to_pane(topology, identity, pane_id)
}

pub(super) fn hook_route(
    topology: Option<&tmux_control::TmuxSnapshot>,
    identity: &str,
    pane_id: &str,
) -> StoredRoute {
    topology
        .and_then(|topology| direct_route(topology, identity, pane_id))
        .unwrap_or_else(|| unmapped_route(identity))
}

fn route_to_pane(
    topology: &tmux_control::TmuxSnapshot,
    identity: &str,
    pane_id: &str,
) -> Option<StoredRoute> {
    let pane = topology.panes.iter().find(|pane| pane.id == pane_id)?;
    Some(StoredRoute {
        host_profile_id: String::new(),
        server_identity: identity.into(),
        session_id: pane.session_id.clone(),
        session_name_fallback: topology
            .sessions
            .iter()
            .find(|session| session.id == pane.session_id)
            .map(|session| session.name.clone())
            .unwrap_or_default(),
        window_id: pane.window_id.clone(),
        window_name_fallback: topology
            .windows
            .iter()
            .find(|window| window.id == pane.window_id)
            .map(|window| window.name.clone())
            .unwrap_or_default(),
        pane_id: pane.id.clone(),
        pane_index_fallback: pane.index,
    })
}

fn unmapped_route(identity: &str) -> StoredRoute {
    StoredRoute {
        host_profile_id: String::new(),
        server_identity: identity.into(),
        session_id: String::new(),
        session_name_fallback: String::new(),
        window_id: String::new(),
        window_name_fallback: String::new(),
        pane_id: String::new(),
        pane_index_fallback: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmapped_hook_never_invents_a_destination() {
        let route = hook_route(None, "server", "%99");
        assert!(route.session_id.is_empty());
        assert!(route.window_id.is_empty());
        assert!(route.pane_id.is_empty());
    }

    #[test]
    fn agent_id_components_have_unambiguous_boundaries() {
        let first = hashed_id("codex", &["native", "server:12", "3x"]);
        let second = hashed_id("codex", &["native", "server:123", "x"]);

        assert_ne!(first, second);
    }
}
