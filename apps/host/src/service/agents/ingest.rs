use std::collections::VecDeque;

use anyhow::Context;
use tmux_agent_protocol::v1;

use super::{AgentRuntime, StoredAgent, adapters, identity, snapshot};
use crate::service::snapshot::{discover_authoritative, server_identity};
use adapters::ApprovalEffect;

pub(super) const MAX_HOOK_BYTES: usize = 256 * 1024;
const MAX_DEDUPE_IDS: usize = 512;

pub(crate) type HookIngestResult = Result<v1::AgentEvent, HookIngestFailure>;

/// A failed ingest is final only when its variant says so. Callers retain
/// `Retryable` input; duplicate and permanently malformed input are discarded.
#[derive(Debug)]
pub(crate) enum HookIngestFailure {
    Duplicate,
    Permanent(anyhow::Error),
    Retryable(anyhow::Error),
}

impl HookIngestFailure {
    pub(crate) fn disposition(&self) -> v1::HookIngestDisposition {
        match self {
            Self::Duplicate | Self::Permanent(_) => v1::HookIngestDisposition::Discarded,
            Self::Retryable(_) => v1::HookIngestDisposition::Retryable,
        }
    }
}

impl AgentRuntime {
    /// Older durable input always drains before a newer live hook is accepted.
    pub(crate) fn ingest_live_hook(&self, event: &v1::AgentHookEvent) -> HookIngestResult {
        self.ingest_after_replay(event, super::ingest_fallbacks)
    }

    pub(super) fn ingest_after_replay(
        &self,
        event: &v1::AgentHookEvent,
        replay: impl FnOnce() -> anyhow::Result<usize>,
    ) -> HookIngestResult {
        replay().map_err(HookIngestFailure::Retryable)?;
        self.ingest_hook(event)
    }

    #[cfg(test)]
    pub(super) fn ingest_after_replay_with_context(
        &self,
        event: &v1::AgentHookEvent,
        active_server_identity: &str,
        topology: Option<&tmux_control::TmuxSnapshot>,
        replay: impl FnOnce() -> anyhow::Result<usize>,
    ) -> HookIngestResult {
        replay().map_err(HookIngestFailure::Retryable)?;
        self.ingest_hook_with_context(event, active_server_identity, topology)
    }

    pub(crate) fn ingest_hook(&self, event: &v1::AgentHookEvent) -> HookIngestResult {
        let identity = server_identity();
        let topology = discover_authoritative()
            .ok()
            .filter(|(_, discovered_identity)| discovered_identity == &identity)
            .map(|(topology, _)| topology);
        self.ingest_hook_with_context(event, &identity, topology.as_ref())
    }

    pub(super) fn ingest_hook_with_context(
        &self,
        event: &v1::AgentHookEvent,
        active_server_identity: &str,
        topology: Option<&tmux_control::TmuxSnapshot>,
    ) -> HookIngestResult {
        self.try_ingest_hook_with_context(event, active_server_identity, topology)
    }

    fn try_ingest_hook_with_context(
        &self,
        event: &v1::AgentHookEvent,
        active_server_identity: &str,
        topology: Option<&tmux_control::TmuxSnapshot>,
    ) -> Result<v1::AgentEvent, HookIngestFailure> {
        if event.payload_json.len() > MAX_HOOK_BYTES {
            return Err(HookIngestFailure::Permanent(anyhow::anyhow!(
                "hook payload exceeds the {MAX_HOOK_BYTES}-byte limit"
            )));
        }
        if event.source_event_id.is_empty() {
            return Err(HookIngestFailure::Permanent(anyhow::anyhow!(
                "hook source_event_id is required"
            )));
        }
        super::validate_pane_id(&event.pane_id).map_err(HookIngestFailure::Permanent)?;
        let adapter = (if event.adapter_id.is_empty() {
            adapters::adapter(v1::AgentAdapterKind::try_from(event.adapter).unwrap_or_default())
        } else {
            adapters::by_id(&event.adapter_id)
        })
        .context("supported agent adapter is required")
        .map_err(HookIngestFailure::Permanent)?;
        let adapter_id = adapter.kind();
        let payload: serde_json::Value = serde_json::from_slice(&event.payload_json)
            .context("parse hook JSON")
            .map_err(HookIngestFailure::Permanent)?;
        let parsed = adapter
            .parse_hook(&payload)
            .map_err(anyhow::Error::msg)
            .map_err(HookIngestFailure::Permanent)?;
        let observed_now = super::now_millis();
        let occurred_at = if event.occurred_at_unix_millis > 0 {
            event.occurred_at_unix_millis
        } else {
            observed_now
        };
        let native_session_id = if event.native_session_id.is_empty() {
            parsed.native_session_id
        } else {
            event.native_session_id.clone()
        };
        let origin_matches = event.origin_server_identity == active_server_identity;
        let route = identity::hook_route(
            origin_matches.then_some(topology).flatten(),
            active_server_identity,
            &event.pane_id,
        );
        let mut state = self.state.lock().unwrap();
        let original = state.clone();
        let route_verified = origin_matches && !route.pane_id.is_empty();
        let agent_id = if route_verified && native_session_id.is_empty() {
            identity::manual_agent_id(adapter_id, active_server_identity, &event.pane_id)
        } else if route_verified {
            identity::native_agent_id(adapter_id, active_server_identity, &native_session_id)
        } else {
            identity::unmapped_hook_agent_id(
                adapter_id,
                &event.origin_server_identity,
                &event.pane_id,
                &native_session_id,
            )
        };
        let candidates = identity::hook_candidates(
            &state,
            adapter_id,
            active_server_identity,
            &native_session_id,
            &route,
            &agent_id,
            route_verified,
        );
        let pane_record_id = candidates.pane;
        let native_record_id = candidates.native;
        let previous_id = native_record_id.clone().or(pane_record_id.clone());
        for candidate in [native_record_id.as_ref(), pane_record_id.as_ref()]
            .into_iter()
            .flatten()
        {
            if state.agents[candidate]
                .source_event_ids
                .contains(&event.source_event_id)
            {
                return Err(HookIngestFailure::Duplicate);
            }
        }
        let latest_sequence = [native_record_id.as_ref(), pane_record_id.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(|id| state.agents.get(id))
            .map(|record| record.latest_source_generation)
            .max()
            .unwrap_or_default();
        if event.source_sequence_authoritative {
            if event.source_generation == 0 {
                return Err(HookIngestFailure::Permanent(anyhow::anyhow!(
                    "authoritative hook source sequence must be nonzero"
                )));
            }
            if event.source_generation <= latest_sequence {
                return Err(HookIngestFailure::Duplicate);
            }
        }
        let previous = previous_id
            .as_ref()
            .and_then(|id| state.agents.get(id))
            .cloned();
        let mut source_ids = VecDeque::new();
        for candidate in [native_record_id.as_ref(), pane_record_id.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(|id| state.agents.get(id))
        {
            for id in &candidate.source_event_ids {
                if !source_ids.contains(id) {
                    source_ids.push_back(id.clone());
                }
            }
        }
        let mut retired_agent_ids = Vec::new();
        for old_id in [native_record_id, pane_record_id].into_iter().flatten() {
            if old_id != agent_id && state.agents.remove(&old_id).is_some() {
                retired_agent_ids.push(old_id);
            }
        }
        state.generation = state.generation.saturating_add(1);
        let generation = state.generation;
        let previous_lifecycle = previous
            .as_ref()
            .and_then(|record| v1::AgentLifecycleState::try_from(record.lifecycle).ok())
            .unwrap_or(v1::AgentLifecycleState::Unknown);
        let attention = previous
            .as_ref()
            .map_or(0, |record| record.attention_generation);
        let terminal_late = previous.as_ref().is_some_and(|record| record.hook_terminal)
            && !matches!(
                parsed.event_name.as_str(),
                "SessionStart" | "UserPromptSubmit"
            );
        let mut pending_approval_keys = previous
            .as_ref()
            .map(|record| record.pending_approval_keys.clone())
            .unwrap_or_default();
        let legacy_pending = previous_lifecycle == v1::AgentLifecycleState::Blocked
            && pending_approval_keys.is_empty();
        let approval_resolved = if terminal_late {
            pending_approval_keys.clear();
            true
        } else {
            match parsed.approval_effect {
                ApprovalEffect::Pending => {
                    if !parsed.approval_key.is_empty()
                        && !pending_approval_keys.contains(&parsed.approval_key)
                    {
                        pending_approval_keys.push(parsed.approval_key.clone());
                    }
                    false
                }
                ApprovalEffect::ResolveMatching => {
                    if legacy_pending {
                        true
                    } else if let Some(index) = pending_approval_keys
                        .iter()
                        .position(|key| key == &parsed.approval_key)
                    {
                        pending_approval_keys.remove(index);
                        pending_approval_keys.is_empty()
                    } else {
                        false
                    }
                }
                ApprovalEffect::ResolveAll => {
                    pending_approval_keys.clear();
                    true
                }
                ApprovalEffect::None => false,
            }
        };
        let approval_pending = (previous_lifecycle == v1::AgentLifecycleState::Blocked
            && !approval_resolved)
            || parsed.approval_effect == ApprovalEffect::Pending;
        let lifecycle = if terminal_late {
            // `hook_terminal` means a terminal Stop was already committed.
            // A late tool/subagent event cannot revive that turn, and an
            // inconsistent store written by an older build must not preserve
            // Working forever merely because every later Stop is also "late".
            v1::AgentLifecycleState::Idle
        } else if approval_pending {
            v1::AgentLifecycleState::Blocked
        } else {
            parsed.lifecycle
        };
        let hook_terminal = if matches!(
            parsed.event_name.as_str(),
            "SessionStart" | "UserPromptSubmit"
        ) {
            false
        } else if matches!(parsed.event_name.as_str(), "Stop" | "StopFailure")
            && parsed.lifecycle == v1::AgentLifecycleState::Idle
        {
            true
        } else {
            previous.as_ref().is_some_and(|record| record.hook_terminal)
        };
        let attention_transition = lifecycle == v1::AgentLifecycleState::Blocked
            && previous_lifecycle != v1::AgentLifecycleState::Blocked
            || previous_lifecycle == v1::AgentLifecycleState::Working
                && lifecycle == v1::AgentLifecycleState::Idle;
        let attention_generation = if attention_transition {
            attention.saturating_add(1)
        } else {
            attention
        };
        let resolved_seen_block = previous_lifecycle == v1::AgentLifecycleState::Blocked
            && lifecycle == v1::AgentLifecycleState::Idle
            && previous.as_ref().is_some_and(|record| {
                record.attention_kind == "blocked"
                    && record.seen_generation >= record.attention_generation
            });
        let attention_kind = if attention_transition {
            if lifecycle == v1::AgentLifecycleState::Blocked {
                "blocked".into()
            } else {
                "completed".into()
            }
        } else if resolved_seen_block {
            String::new()
        } else {
            previous
                .as_ref()
                .map(|record| record.attention_kind.clone())
                .unwrap_or_default()
        };
        source_ids.push_back(event.source_event_id.clone());
        while source_ids.len() > MAX_DEDUPE_IDS {
            source_ids.pop_front();
        }
        let latest_source_generation = if event.source_sequence_authoritative {
            event.source_generation
        } else {
            latest_sequence
        };
        let record = StoredAgent {
            agent_id: agent_id.clone(),
            adapter: adapter.legacy_kind() as i32,
            adapter_id: adapter.id().into(),
            native_session_id,
            display_name: previous
                .as_ref()
                .map(|record| record.display_name.clone())
                .unwrap_or_else(|| adapter.display_name().into()),
            route,
            lifecycle: lifecycle as i32,
            state_generation: generation,
            attention_generation,
            attention_kind,
            seen_generation: previous.as_ref().map_or(0, |record| record.seen_generation),
            updated_at_unix_millis: occurred_at,
            hook_authority_expires_at_unix_millis: observed_now
                .saturating_add(parsed.authority_millis),
            detected_manually: previous
                .as_ref()
                .is_some_and(|record| record.detected_manually),
            source_event_ids: source_ids,
            latest_source_generation,
            present: true,
            hook_terminal,
            pending_approval_keys,
            lifecycle_observed_at_unix_millis: observed_now,
        };
        state.agents.insert(agent_id, record.clone());
        if let Err(error) = self.persist_locked(&state) {
            *state = original;
            return Err(HookIngestFailure::Retryable(error));
        }
        let reason = if lifecycle == v1::AgentLifecycleState::Blocked {
            "blocked"
        } else if previous_lifecycle == v1::AgentLifecycleState::Working
            && lifecycle == v1::AgentLifecycleState::Idle
        {
            "completed"
        } else {
            "state_changed"
        };
        Ok(v1::AgentEvent {
            agent: Some(snapshot::record(&record)),
            generation,
            notify: attention_transition,
            reason: reason.into(),
            retired_agent_ids,
        })
    }
}
