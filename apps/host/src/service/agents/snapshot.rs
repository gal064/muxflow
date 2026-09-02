use std::path::PathBuf;

use tmux_agent_protocol::v1;

use super::{StoredAgent, StoredState, adapters};

pub(super) fn build(
    state: &StoredState,
    server_identity: &str,
    wiring: &[super::hooks::ObservedAdapter],
) -> v1::AgentSnapshot {
    let agents = state
        .agents
        .values()
        .filter(|record| record.route.server_identity == server_identity)
        .map(record)
        .collect();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    v1::AgentSnapshot {
        generation: state.generation,
        agents,
        authoritative: true,
        notification_watermark: state.generation,
        adapters: adapters::descriptors(&home, wiring),
        accepted_generation: state.generation,
    }
}

pub(super) fn record(value: &StoredAgent) -> v1::AgentRecord {
    v1::AgentRecord {
        agent_id: value.agent_id.clone(),
        adapter: value.adapter,
        adapter_id: if value.adapter_id.is_empty() {
            adapters::adapter(v1::AgentAdapterKind::try_from(value.adapter).unwrap_or_default())
                .map(|adapter| adapter.id().to_owned())
                .unwrap_or_default()
        } else {
            value.adapter_id.clone()
        },
        native_session_id: value.native_session_id.clone(),
        display_name: value.display_name.clone(),
        route: Some(v1::AgentRoute {
            host_profile_id: value.route.host_profile_id.clone(),
            server_identity: value.route.server_identity.clone(),
            session_id: value.route.session_id.clone(),
            session_name_fallback: value.route.session_name_fallback.clone(),
            window_id: value.route.window_id.clone(),
            window_name_fallback: value.route.window_name_fallback.clone(),
            pane_id: value.route.pane_id.clone(),
            pane_index_fallback: value.route.pane_index_fallback,
            agent_id: value.agent_id.clone(),
            attention_generation: value.attention_generation,
        }),
        lifecycle: value.lifecycle,
        state_generation: value.state_generation,
        attention_generation: value.attention_generation,
        seen_generation: value.seen_generation,
        updated_at_unix_millis: value.updated_at_unix_millis,
        hook_authority_expires_at_unix_millis: value.hook_authority_expires_at_unix_millis,
        detected_manually: value.detected_manually,
        present: value.present,
        attention_kind: value.attention_kind.clone(),
        lifecycle_changed_at_unix_millis: value.lifecycle_changed_at_unix_millis,
        attention_seen_at_unix_millis: value.attention_seen_at_unix_millis,
    }
}
