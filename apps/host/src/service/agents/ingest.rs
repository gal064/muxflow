use std::collections::VecDeque;

use anyhow::Context;
use tmux_agent_protocol::v1;

use super::{AgentRuntime, StoredAgent, adapters, identity, snapshot};
use crate::service::snapshot::{discover_authoritative, server_identity};

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
        let codex_turn_start = adapter.id() == "codex" && parsed.event_name == "UserPromptSubmit";
        let codex_permission = adapter.id() == "codex" && parsed.event_name == "PermissionRequest";
        let approval_turn_id = if codex_turn_start || codex_permission {
            payload
                .get(adapters::CODEX_APPROVAL_TURN_ID_FIELD)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        } else {
            ""
        };
        let approval_reviewer = if codex_turn_start {
            payload
                .get(adapters::CODEX_APPROVAL_REVIEWER_FIELD)
                .and_then(serde_json::Value::as_str)
        } else {
            None
        };
        let cached_auto_review = codex_permission
            && !approval_turn_id.is_empty()
            && previous
                .as_ref()
                .is_some_and(|record| record.codex_auto_review_turn_id == approval_turn_id);
        let previous_claude_has_running_subagent = previous
            .as_ref()
            .is_some_and(|record| record.claude_has_running_subagent);
        let claude_idle_prompt_during_subagent = adapter.id() == "claude-code"
            && previous_claude_has_running_subagent
            && previous_lifecycle != v1::AgentLifecycleState::Blocked
            && parsed.event_name == "Notification"
            && payload
                .get("notification_type")
                .and_then(serde_json::Value::as_str)
                == Some("idle_prompt");
        let parsed_lifecycle = if cached_auto_review || claude_idle_prompt_during_subagent {
            v1::AgentLifecycleState::Working
        } else {
            parsed.lifecycle
        };
        let terminal_late = previous.as_ref().is_some_and(|record| record.hook_terminal)
            && !matches!(
                parsed.event_name.as_str(),
                "SessionStart" | "UserPromptSubmit"
            );
        let lifecycle = if terminal_late {
            // `hook_terminal` means a terminal Stop was already committed.
            // A late tool/subagent event cannot revive that turn, and an
            // inconsistent store written by an older build must not preserve
            // Working forever merely because every later Stop is also "late".
            v1::AgentLifecycleState::Idle
        } else {
            parsed_lifecycle
        };
        let lifecycle_changed_at = previous
            .as_ref()
            .filter(|record| record.lifecycle == lifecycle as i32)
            .map_or(observed_now, |record| {
                record.lifecycle_changed_at_unix_millis
            });
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
        let superseded_seen_completion = lifecycle == v1::AgentLifecycleState::Working
            && previous.as_ref().is_some_and(|record| {
                record.attention_kind == "completed"
                    && record.seen_generation >= record.attention_generation
            });
        let attention_kind = if attention_transition {
            if lifecycle == v1::AgentLifecycleState::Blocked {
                "blocked".into()
            } else {
                "completed".into()
            }
        } else if resolved_seen_block || superseded_seen_completion {
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
        let codex_auto_review_turn_id = if codex_turn_start {
            if approval_reviewer == Some("auto_review") && !approval_turn_id.is_empty() {
                approval_turn_id.to_owned()
            } else {
                String::new()
            }
        } else {
            previous
                .as_ref()
                .map(|record| record.codex_auto_review_turn_id.clone())
                .unwrap_or_default()
        };
        let claude_has_running_subagent = if adapter.id() != "claude-code" {
            false
        } else if parsed.event_name == "Stop" {
            if terminal_late {
                previous_claude_has_running_subagent
            } else {
                payload
                    .get(adapters::CLAUDE_HAS_RUNNING_SUBAGENT_FIELD)
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
            }
        } else if matches!(parsed.event_name.as_str(), "SessionStart" | "StopFailure") {
            false
        } else {
            previous_claude_has_running_subagent
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
            attention_seen_at_unix_millis: if attention_transition || superseded_seen_completion {
                0
            } else {
                previous
                    .as_ref()
                    .map_or(0, |record| record.attention_seen_at_unix_millis)
            },
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
            claude_has_running_subagent,
            codex_auto_review_turn_id,
            lifecycle_observed_at_unix_millis: observed_now,
            lifecycle_changed_at_unix_millis: lifecycle_changed_at,
        };
        state.agents.insert(agent_id.clone(), record.clone());
        if let Err(error) = self.persist_locked(&state) {
            *state = original;
            return Err(HookIngestFailure::Retryable(error));
        }
        drop(state);
        if !retired_agent_ids.is_empty() {
            (self.identity_promotion_sink)(&retired_agent_ids, &agent_id);
        }
        // Voice mode (docs/mobile/voice-mode-plan.md §4.5): a `Stop` that ends
        // the turn hands the final message on, after the state is committed,
        // and keeps none of it. Claude's Stop while subagents still run leaves
        // the lifecycle Working and is skipped, so one turn speaks once.
        if parsed.event_name == "Stop"
            && lifecycle == v1::AgentLifecycleState::Idle
            && let Some(text) = payload
                .get(adapters::LAST_ASSISTANT_MESSAGE_FIELD)
                .and_then(serde_json::Value::as_str)
                .filter(|text| !text.trim().is_empty())
        {
            (self.reply_sink)(crate::service::voice::AgentReply {
                agent_id,
                text: text.to_owned(),
                truncated: payload
                    .get(adapters::LAST_ASSISTANT_MESSAGE_TRUNCATED_FIELD)
                    .and_then(serde_json::Value::as_bool)
                    == Some(true),
                state_generation: generation,
                occurred_at_unix_millis: occurred_at,
            });
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
