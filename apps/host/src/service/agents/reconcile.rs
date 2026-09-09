use std::collections::{BTreeMap, VecDeque};

use tmux_agent_protocol::v1;

use super::{StoredAgent, StoredRoute, StoredState, identity};

#[derive(Debug, Default)]
pub(super) struct ReconcileResult {
    pub changed: bool,
}

/// Every `(pane_id, adapter_id)` this tmux server is currently running an agent
/// on, as proven by the pane's command or its process tree.
///
/// Shared with the daemon's maintenance pass so "is this agent still alive"
/// is answered by exactly one piece of evidence. Two implementations of that
/// question would eventually disagree, and the disagreement would show up as a
/// row that either vanished while its agent worked or lingered after it died.
pub(super) fn detect_all(
    snapshot: &tmux_control::TmuxSnapshot,
) -> BTreeMap<
    (String, String),
    (
        &tmux_control::Pane,
        &'static dyn super::adapters::AgentAdapter,
    ),
> {
    snapshot
        .panes
        .iter()
        .filter_map(|pane| {
            super::process::detect(pane).map(|adapter| {
                (
                    (pane.id.clone(), adapter.kind().to_owned()),
                    (pane, adapter),
                )
            })
        })
        .collect()
}

pub(super) fn topology(
    state: &mut StoredState,
    snapshot: &tmux_control::TmuxSnapshot,
    server_identity: &str,
    now: i64,
) -> ReconcileResult {
    let detected = detect_all(snapshot);

    let mut retired = Vec::new();
    state.agents.retain(|agent_id, record| {
        let same_server = record.route.server_identity == server_identity;
        let live = if record.route.pane_id.is_empty() {
            record.hook_authority_expires_at_unix_millis > now
        } else {
            detected.contains_key(&(record.route.pane_id.clone(), record.adapter_id.clone()))
        };
        // A live pane is stronger evidence than one negative process scan.
        // The daemon's maintenance loop confirms absence across multiple
        // scans before retirement. A closed pane or a different detected
        // adapter in the same pane remains conclusive.
        let pane_still_exists = snapshot
            .panes
            .iter()
            .any(|pane| pane.id == record.route.pane_id);
        let replaced = detected
            .keys()
            .any(|(pane_id, _)| pane_id == &record.route.pane_id);
        let retain = same_server && (live || (pane_still_exists && !replaced));
        if !retain {
            retired.push(agent_id.clone());
        }
        retain
    });

    let mut changed = !retired.is_empty();
    for ((pane_id, adapter_id), (_pane, adapter)) in detected {
        let existing_id = state
            .agents
            .values()
            .find(|record| {
                record.route.server_identity == server_identity
                    && record.route.pane_id == pane_id
                    && record.adapter_id == adapter_id
            })
            .map(|record| record.agent_id.clone());
        if let Some(existing_id) = existing_id {
            let fresh_route = identity::direct_route(snapshot, server_identity, &pane_id)
                .expect("detected pane belongs to snapshot");
            if !same_route_for_reconciliation(&state.agents[&existing_id].route, &fresh_route) {
                state.generation = state.generation.saturating_add(1);
                let generation = state.generation;
                let record = state.agents.get_mut(&existing_id).unwrap();
                record.route = fresh_route;
                record.state_generation = generation;
                record.updated_at_unix_millis = now;
                changed = true;
            }
            // Process discovery proves presence, not lifecycle: it never
            // erases the lifecycle or attention a hook established.
            continue;
        }

        state.generation = state.generation.saturating_add(1);
        let agent_id = identity::manual_agent_id(adapter.kind(), server_identity, &pane_id);
        state.agents.insert(
            agent_id.clone(),
            StoredAgent {
                agent_id,
                adapter: adapter.legacy_kind() as i32,
                adapter_id,
                native_session_id: String::new(),
                display_name: adapter.display_name().into(),
                route: identity::direct_route(snapshot, server_identity, &pane_id)
                    .expect("detected pane belongs to snapshot"),
                lifecycle: v1::AgentLifecycleState::Unknown as i32,
                state_generation: state.generation,
                attention_generation: 0,
                attention_kind: String::new(),
                seen_generation: 0,
                attention_seen_at_unix_millis: 0,
                updated_at_unix_millis: now,
                hook_authority_expires_at_unix_millis: 0,
                detected_manually: true,
                source_event_ids: VecDeque::new(),
                latest_source_generation: 0,
                present: true,
                hook_terminal: false,
                claude_has_running_subagent: false,
                codex_running_subagent_ids: Default::default(),
                codex_subagent_capacity_exceeded: false,
                codex_parent_stopped_for_subagents: false,
                subagent_evidence_observed_at_unix_millis: 0,
                codex_turn_reviews: VecDeque::new(),
                // Process detection proves a process exists; it is not an
                // observation of what that process is doing, so it starts no
                // staleness clock.
                lifecycle_observed_at_unix_millis: 0,
                lifecycle_changed_at_unix_millis: now,
            },
        );
        changed = true;
    }

    if changed && !retired.is_empty() {
        state.generation = state.generation.saturating_add(1);
    }
    ReconcileResult { changed }
}

/// Agent route fallbacks follow the same title semantics as topology
/// publication. Persisting every animation glyph would make agent generation
/// churn even after the topology generation stopped doing so; every routing
/// identity and non-title fallback remains exact.
fn same_route_for_reconciliation(previous: &StoredRoute, current: &StoredRoute) -> bool {
    previous.host_profile_id == current.host_profile_id
        && previous.server_identity == current.server_identity
        && previous.session_id == current.session_id
        && previous.session_name_fallback == current.session_name_fallback
        && previous.window_id == current.window_id
        && super::super::topology::equivalent_agent_title(
            &previous.window_name_fallback,
            &current.window_name_fallback,
        )
        && previous.pane_id == current.pane_id
        && previous.pane_index_fallback == current.pane_index_fallback
}
