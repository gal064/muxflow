use std::collections::VecDeque;

use anyhow::Context;
use tmux_agent_protocol::v1;

use super::{
    AgentRuntime, CodexReviewer, CodexTurnKey, CodexTurnReview, StoredAgent, adapters, identity,
    snapshot,
};
use crate::service::snapshot::{discover_authoritative, server_identity};

pub(super) const MAX_HOOK_BYTES: usize = 256 * 1024;
const MAX_DEDUPE_IDS: usize = 512;
const MAX_CODEX_TURN_REVIEWS: usize = 64;

pub(crate) type HookIngestResult = Result<v1::AgentEvent, HookIngestFailure>;
type DeferredHookIngestResult = Result<IngestedHook, HookIngestFailure>;

struct IngestedHook {
    event: v1::AgentEvent,
    reply: Option<crate::service::voice::AgentReply>,
}

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
        super::ingest_fallbacks().map_err(HookIngestFailure::Retryable)?;
        self.ingest_and_publish(event)
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

    #[cfg(test)]
    pub(crate) fn ingest_hook(&self, event: &v1::AgentHookEvent) -> HookIngestResult {
        let ingested = self.ingest_hook_deferred(event)?;
        self.dispatch_reply(ingested.reply);
        Ok(ingested.event)
    }

    #[cfg(test)]
    fn ingest_hook_deferred(&self, event: &v1::AgentHookEvent) -> DeferredHookIngestResult {
        let identity = server_identity();
        let topology = discover_authoritative()
            .ok()
            .filter(|(_, discovered_identity)| discovered_identity == &identity)
            .map(|(topology, _)| topology);
        self.try_ingest_hook_with_context(event, &identity, topology.as_ref())
    }

    pub(super) fn ingest_and_publish(&self, event: &v1::AgentHookEvent) -> HookIngestResult {
        let identity = server_identity();
        let topology = discover_authoritative()
            .ok()
            .filter(|(_, discovered_identity)| discovered_identity == &identity)
            .map(|(topology, _)| topology);
        self.ingest_and_publish_with_context(event, &identity, topology.as_ref())
    }

    #[cfg(test)]
    pub(super) fn ingest_hook_with_context(
        &self,
        event: &v1::AgentHookEvent,
        active_server_identity: &str,
        topology: Option<&tmux_control::TmuxSnapshot>,
    ) -> HookIngestResult {
        let ingested =
            self.try_ingest_hook_with_context(event, active_server_identity, topology)?;
        self.dispatch_reply(ingested.reply);
        Ok(ingested.event)
    }

    pub(super) fn ingest_and_publish_with_context(
        &self,
        event: &v1::AgentHookEvent,
        active_server_identity: &str,
        topology: Option<&tmux_control::TmuxSnapshot>,
    ) -> HookIngestResult {
        let _order = self.ingest_order.lock().unwrap();
        let ingested =
            self.try_ingest_hook_with_context(event, active_server_identity, topology)?;
        let event = ingested.event.clone();
        super::publish(ingested.event);
        self.dispatch_reply(ingested.reply);
        Ok(event)
    }

    fn try_ingest_hook_with_context(
        &self,
        event: &v1::AgentHookEvent,
        active_server_identity: &str,
        topology: Option<&tmux_control::TmuxSnapshot>,
    ) -> DeferredHookIngestResult {
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
        let duplicate_id = [native_record_id.as_ref(), pane_record_id.as_ref()]
            .into_iter()
            .flatten()
            .find(|candidate| {
                state.agents[*candidate]
                    .source_event_ids
                    .contains(&event.source_event_id)
            })
            .cloned();
        if let Some(duplicate_id) = duplicate_id {
            if adapter.id() == "codex"
                && let Some(reconciled) = reconcile_duplicate_transcript_child_states(
                    &mut state,
                    &duplicate_id,
                    &payload,
                    observed_now,
                )
            {
                if let Err(error) = self.persist_locked(&state) {
                    *state = original;
                    return Err(HookIngestFailure::Retryable(error));
                }
                return Ok(IngestedHook {
                    event: reconciled,
                    reply: None,
                });
            }
            return Err(HookIngestFailure::Duplicate);
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
        let voice_retired_agent_ids: Vec<String> = if native_session_id.is_empty() {
            Vec::new()
        } else {
            [native_record_id.as_ref(), pane_record_id.as_ref()]
                .into_iter()
                .flatten()
                .filter(|old_id| old_id.as_str() != agent_id)
                .filter(|old_id| {
                    state
                        .agents
                        .get(old_id.as_str())
                        .is_some_and(|record| record.native_session_id.is_empty())
                })
                .cloned()
                .collect()
        };
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
        let approval_turn_id = if adapter.id() == "codex" {
            payload
                .get(adapters::CODEX_APPROVAL_TURN_ID_FIELD)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        } else {
            ""
        };
        let approval_turn = (!approval_turn_id.is_empty()).then(|| CodexTurnKey {
            agent_id: payload
                .get(adapters::CODEX_SUBAGENT_ID_FIELD)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            turn_id: approval_turn_id.to_owned(),
        });
        let embedded_approval_reviewer = if codex_turn_start || codex_permission {
            payload
                .get(adapters::CODEX_APPROVAL_REVIEWER_FIELD)
                .and_then(serde_json::Value::as_str)
        } else {
            None
        };
        let cached_reviewer = codex_permission
            .then(|| {
                previous
                    .as_ref()?
                    .codex_turn_reviews
                    .iter()
                    .find_map(|review| {
                        (Some(&review.turn) == approval_turn.as_ref()).then_some(
                            match review.reviewer {
                                CodexReviewer::AutoReview => {
                                    crate::hook::codex_transcript::ApprovalReviewer::AutoReview
                                }
                                CodexReviewer::User => {
                                    crate::hook::codex_transcript::ApprovalReviewer::User
                                }
                            },
                        )
                    })
            })
            .flatten();
        let cached_auto_review =
            cached_reviewer == Some(crate::hook::codex_transcript::ApprovalReviewer::AutoReview);
        let mut permission_monitor = (codex_permission
            && !cached_auto_review
            && matches!(embedded_approval_reviewer, None | Some("user")))
        .then(|| {
            let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
            crate::hook::codex_transcript::TurnMonitor::open(
                &serde_json::json!({
                    "turn_id": approval_turn_id,
                    "transcript_path": payload
                        .get(adapters::CODEX_TRANSCRIPT_PATH_FIELD)
                        .and_then(serde_json::Value::as_str)?,
                }),
                &home,
            )
        })
        .flatten();
        let permission_review = codex_permission.then(|| {
            classify_permission_review(cached_reviewer, embedded_approval_reviewer, || {
                permission_monitor.as_mut()?.approval_reviewer()
            })
        });
        let observed_auto_review = permission_review == Some(PermissionReview::ObservedAuto);
        let observed_user_review = permission_review == Some(PermissionReview::ObservedUser);
        let previous_claude_has_running_subagent = previous
            .as_ref()
            .is_some_and(|record| record.claude_has_running_subagent);
        let previous_hook_terminal = previous.as_ref().is_some_and(|record| record.hook_terminal);
        let previous_codex_parent_stopped = previous
            .as_ref()
            .is_some_and(|record| record.codex_parent_stopped_for_subagents);
        let previous_subagent_evidence_observed_at = previous
            .as_ref()
            .map_or(0, |record| record.subagent_evidence_observed_at_unix_millis);
        let mut codex_running_subagents = previous
            .as_ref()
            .map(|record| record.codex_running_subagents.clone())
            .unwrap_or_default();
        let mut codex_subagent_capacity_exceeded = previous
            .as_ref()
            .is_some_and(|record| record.codex_subagent_capacity_exceeded);
        let codex_subagent_turn = approval_turn
            .as_ref()
            .filter(|turn| adapter.id() == "codex" && !turn.agent_id.is_empty())
            .cloned();
        let codex_child_event = codex_subagent_turn.is_some();
        let codex_child_stop = codex_child_event && parsed.event_name == "SubagentStop";
        let needs_child_monitor = codex_subagent_turn.as_ref().is_some_and(|turn| {
            !codex_child_stop
                && (codex_running_subagents.get(&turn.agent_id) != Some(&turn.turn_id)
                    || !self.has_codex_child_monitor(&agent_id, turn))
        });
        let mut child_monitor = needs_child_monitor
            .then(|| {
                let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
                crate::hook::codex_transcript::TurnMonitor::open(
                    &serde_json::json!({
                        "turn_id": codex_subagent_turn.as_ref()?.turn_id,
                        "transcript_path": payload
                            .get(adapters::CODEX_TRANSCRIPT_PATH_FIELD)
                            .and_then(serde_json::Value::as_str)?,
                    }),
                    &home,
                )
            })
            .flatten();
        // A hook delivered after its own turn ended is stale activity, not a
        // resumed child. Codex resumes the same child with a fresh turn ID.
        let child_transcript_terminal = child_monitor
            .as_mut()
            .and_then(crate::hook::codex_transcript::TurnMonitor::poll_terminal)
            .is_some();
        let codex_child_activity =
            codex_child_event && !codex_child_stop && !child_transcript_terminal;
        if adapter.id() == "codex"
            && (!previous_hook_terminal
                || parsed.event_name == "SessionStart"
                || codex_turn_start
                || codex_child_activity)
        {
            match parsed.event_name.as_str() {
                "SessionStart" => {
                    codex_running_subagents.clear();
                    codex_subagent_capacity_exceeded = false;
                }
                _ if codex_child_stop || child_transcript_terminal => {
                    if let Some(turn) = codex_subagent_turn.as_ref()
                        && codex_running_subagents
                            .get(&turn.agent_id)
                            .is_some_and(|active_turn| {
                                active_turn.is_empty() || active_turn == &turn.turn_id
                            })
                    {
                        codex_running_subagents.remove(&turn.agent_id);
                    }
                }
                _ if codex_child_activity
                    && (codex_running_subagents.contains_key(
                        &codex_subagent_turn
                            .as_ref()
                            .expect("child activity has a turn")
                            .agent_id,
                    ) || codex_running_subagents.len() < super::MAX_CODEX_CHILDREN) =>
                {
                    let turn = codex_subagent_turn
                        .as_ref()
                        .expect("child activity has a turn");
                    codex_running_subagents.insert(turn.agent_id.clone(), turn.turn_id.clone());
                }
                _ if codex_child_activity => codex_subagent_capacity_exceeded = true,
                _ => {}
            }
        }
        let mut transcript_child_event = false;
        if adapter.id() == "codex" {
            for child_id in sanitized_child_terminals(&payload) {
                transcript_child_event = true;
                codex_running_subagents.remove(&child_id);
            }
        }
        let codex_parent_stop_during_subagents = adapter.id() == "codex"
            && parsed.event_name == "Stop"
            && (!codex_running_subagents.is_empty() || codex_subagent_capacity_exceeded);
        let codex_final_unwaited_subagent_stop = adapter.id() == "codex"
            && (codex_child_stop || child_transcript_terminal || transcript_child_event)
            && previous_codex_parent_stopped
            && previous_lifecycle != v1::AgentLifecycleState::Blocked
            && codex_running_subagents.is_empty()
            && !codex_subagent_capacity_exceeded;
        let claude_idle_prompt_during_subagent = adapter.id() == "claude-code"
            && previous_claude_has_running_subagent
            && previous_lifecycle != v1::AgentLifecycleState::Blocked
            && parsed.event_name == "Notification"
            && payload
                .get("notification_type")
                .and_then(serde_json::Value::as_str)
                == Some("idle_prompt");
        let subagent_bookkeeping_during_block = previous_lifecycle
            == v1::AgentLifecycleState::Blocked
            && (codex_child_event
                || transcript_child_event
                || adapter.id() == "claude-code" && parsed.event_name == "SubagentStop"
                || codex_parent_stop_during_subagents
                || adapter.id() == "claude-code"
                    && parsed.event_name == "Stop"
                    && parsed.lifecycle == v1::AgentLifecycleState::Working);
        let codex_resolved_block_after_children = adapter.id() == "codex"
            && previous_lifecycle == v1::AgentLifecycleState::Blocked
            && previous_codex_parent_stopped
            && codex_running_subagents.is_empty()
            && !codex_subagent_capacity_exceeded
            && parsed.lifecycle != v1::AgentLifecycleState::Blocked
            && !matches!(
                parsed.event_name.as_str(),
                "SessionStart" | "UserPromptSubmit"
            )
            && !codex_child_event
            && !transcript_child_event
            && !codex_child_activity;
        let parsed_lifecycle =
            if codex_final_unwaited_subagent_stop || codex_resolved_block_after_children {
                v1::AgentLifecycleState::Idle
            } else if subagent_bookkeeping_during_block {
                v1::AgentLifecycleState::Blocked
            } else if codex_parent_stop_during_subagents
                || matches!(
                    permission_review,
                    Some(
                        PermissionReview::CachedAuto
                            | PermissionReview::ObservedAuto
                            | PermissionReview::Unknown
                    )
                )
                || claude_idle_prompt_during_subagent
            {
                v1::AgentLifecycleState::Working
            } else {
                parsed.lifecycle
            };
        let terminal_late = previous_hook_terminal
            && !matches!(
                parsed.event_name.as_str(),
                "SessionStart" | "UserPromptSubmit"
            )
            && !codex_child_activity;
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
        ) || codex_child_activity
        {
            false
        } else if codex_final_unwaited_subagent_stop
            || codex_resolved_block_after_children
            || matches!(
                parsed.event_name.as_str(),
                "Stop" | "StopFailure" | "Interrupt" | "SessionEnd"
            ) && parsed_lifecycle == v1::AgentLifecycleState::Idle
        {
            true
        } else {
            previous_hook_terminal
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
        let mut codex_turn_reviews = previous
            .as_ref()
            .map(|record| record.codex_turn_reviews.clone())
            .unwrap_or_default();
        if adapter.id() == "codex" {
            if parsed.event_name == "SessionStart"
                || codex_turn_start
                    && approval_turn
                        .as_ref()
                        .is_none_or(|turn| turn.agent_id.is_empty())
            {
                codex_turn_reviews.clear();
            }
            if let Some(turn) = approval_turn.as_ref()
                && (codex_turn_start || observed_auto_review || observed_user_review)
            {
                codex_turn_reviews.retain(|cached| &cached.turn != turn);
                let reviewer =
                    if embedded_approval_reviewer == Some("auto_review") || observed_auto_review {
                        Some(CodexReviewer::AutoReview)
                    } else if embedded_approval_reviewer == Some("user") || observed_user_review {
                        Some(CodexReviewer::User)
                    } else {
                        None
                    };
                if let Some(reviewer) = reviewer {
                    codex_turn_reviews.push_back(CodexTurnReview {
                        turn: turn.clone(),
                        reviewer,
                    });
                }
            }
            while codex_turn_reviews.len() > MAX_CODEX_TURN_REVIEWS {
                codex_turn_reviews.pop_front();
            }
        }
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
        let mut codex_parent_stopped_for_subagents = if adapter.id() != "codex" || terminal_late {
            false
        } else if lifecycle == v1::AgentLifecycleState::Blocked {
            previous_codex_parent_stopped
        } else {
            match parsed.event_name.as_str() {
                "SessionStart" => false,
                "UserPromptSubmit" if !codex_child_event => false,
                "Stop" => !codex_running_subagents.is_empty() || codex_subagent_capacity_exceeded,
                _ if (codex_child_stop || child_transcript_terminal)
                    && codex_running_subagents.is_empty()
                    && !codex_subagent_capacity_exceeded =>
                {
                    false
                }
                _ if codex_resolved_block_after_children => false,
                _ if codex_child_activity && previous_hook_terminal => true,
                _ => previous_codex_parent_stopped,
            }
        };
        if hook_terminal {
            codex_running_subagents.clear();
            codex_subagent_capacity_exceeded = false;
            codex_parent_stopped_for_subagents = false;
        }
        let subagent_evidence_observed_at_unix_millis = if hook_terminal {
            0
        } else if adapter.id() == "codex" {
            if codex_running_subagents.is_empty() && !codex_subagent_capacity_exceeded {
                0
            } else if codex_child_event || transcript_child_event {
                observed_now
            } else {
                previous_subagent_evidence_observed_at
            }
        } else if adapter.id() == "claude-code" {
            if !claude_has_running_subagent {
                0
            } else if parsed.event_name == "Stop" && !terminal_late {
                observed_now
            } else {
                previous_subagent_evidence_observed_at
            }
        } else {
            0
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
            codex_running_subagents,
            codex_subagent_capacity_exceeded,
            codex_parent_stopped_for_subagents,
            subagent_evidence_observed_at_unix_millis,
            codex_turn_reviews,
            lifecycle_observed_at_unix_millis: observed_now,
            lifecycle_changed_at_unix_millis: lifecycle_changed_at,
        };
        state.agents.insert(agent_id.clone(), record.clone());
        if let Err(error) = self.persist_locked(&state) {
            *state = original;
            return Err(HookIngestFailure::Retryable(error));
        }
        drop(state);
        if adapter.id() == "codex" {
            self.track_codex_permission(
                &agent_id,
                &parsed.event_name,
                approval_turn,
                (matches!(
                    permission_review,
                    Some(PermissionReview::CachedUser | PermissionReview::ObservedUser)
                ))
                .then_some(permission_monitor)
                .flatten(),
                lifecycle_changed_at,
                observed_now,
            );
            self.track_codex_child(
                &agent_id,
                codex_subagent_turn,
                child_monitor,
                codex_child_activity,
                parsed.event_name == "SessionStart" || record.hook_terminal,
            );
        }
        if !voice_retired_agent_ids.is_empty() {
            (self.identity_promotion_sink)(&voice_retired_agent_ids, &agent_id);
        }
        // Voice mode (docs/mobile/voice-mode-plan.md §4.5): Claude produces a
        // final aggregate Stop after its background children, so its
        // intermediate Stop is skipped. Codex does not produce another parent
        // Stop when an unwaited child finishes; speaking its one parent reply
        // here preserves the existing voice behavior without storing content.
        let reply = if parsed.event_name == "Stop"
            && (lifecycle == v1::AgentLifecycleState::Idle || adapter.id() == "codex")
            && let Some(text) = payload
                .get(adapters::LAST_ASSISTANT_MESSAGE_FIELD)
                .and_then(serde_json::Value::as_str)
                .filter(|text| !text.trim().is_empty())
        {
            Some(crate::service::voice::AgentReply {
                agent_id: agent_id.clone(),
                text: text.to_owned(),
                truncated: payload
                    .get(adapters::LAST_ASSISTANT_MESSAGE_TRUNCATED_FIELD)
                    .and_then(serde_json::Value::as_bool)
                    == Some(true),
                state_generation: generation,
                occurred_at_unix_millis: occurred_at,
            })
        } else {
            None
        };
        let reason = if lifecycle == v1::AgentLifecycleState::Blocked {
            "blocked"
        } else if previous_lifecycle == v1::AgentLifecycleState::Working
            && lifecycle == v1::AgentLifecycleState::Idle
        {
            "completed"
        } else {
            "state_changed"
        };
        Ok(IngestedHook {
            event: v1::AgentEvent {
                agent: Some(snapshot::record(&record)),
                generation,
                notify: attention_transition,
                reason: reason.into(),
                retired_agent_ids,
            },
            reply,
        })
    }

    pub(super) fn dispatch_reply(&self, reply: Option<crate::service::voice::AgentReply>) {
        if let Some(reply) = reply {
            (self.reply_sink)(reply);
        }
    }
}

fn sanitized_child_terminals(payload: &serde_json::Value) -> Vec<String> {
    let Some(transitions) = payload
        .get(adapters::CODEX_CHILD_TRANSITIONS_FIELD)
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    let mut latest: VecDeque<String> = VecDeque::new();
    for transition in transitions {
        let Some(child_id) = transition
            .get("agent_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= adapters::MAX_CODEX_SUBAGENT_ID_BYTES)
        else {
            continue;
        };
        if transition
            .get("active")
            .and_then(serde_json::Value::as_bool)
            != Some(false)
        {
            continue;
        }
        if let Some(index) = latest.iter().position(|id| id == child_id) {
            latest.remove(index);
        }
        latest.push_back(child_id.to_owned());
        while latest.len() > super::MAX_CODEX_CHILDREN {
            latest.pop_front();
        }
    }
    latest.into()
}

fn reconcile_duplicate_transcript_child_states(
    state: &mut super::StoredState,
    record_id: &str,
    payload: &serde_json::Value,
    now: i64,
) -> Option<v1::AgentEvent> {
    let transitions = sanitized_child_terminals(payload);
    if transitions.is_empty() {
        return None;
    }
    let before = state.agents.get(record_id)?.clone();
    let mut children = before.codex_running_subagents.clone();
    let capacity_exceeded = before.codex_subagent_capacity_exceeded;
    for child_id in transitions {
        children.remove(&child_id);
    }
    let has_children = !children.is_empty() || capacity_exceeded;
    let was_blocked = before.lifecycle == v1::AgentLifecycleState::Blocked as i32;
    let resumed = has_children && before.hook_terminal;
    let completed = !has_children && before.codex_parent_stopped_for_subagents && !was_blocked;
    let lifecycle = if was_blocked {
        v1::AgentLifecycleState::Blocked
    } else if has_children {
        v1::AgentLifecycleState::Working
    } else if completed {
        v1::AgentLifecycleState::Idle
    } else {
        v1::AgentLifecycleState::try_from(before.lifecycle).unwrap_or_default()
    };
    let changed = children != before.codex_running_subagents
        || capacity_exceeded != before.codex_subagent_capacity_exceeded
        || lifecycle as i32 != before.lifecycle
        || resumed
        || completed;
    if !changed {
        return None;
    }
    state.generation = state.generation.saturating_add(1);
    let generation = state.generation;
    let record = state.agents.get_mut(record_id)?;
    let previous_lifecycle =
        v1::AgentLifecycleState::try_from(record.lifecycle).unwrap_or_default();
    record.codex_running_subagents = children;
    record.codex_subagent_capacity_exceeded = capacity_exceeded;
    record.codex_parent_stopped_for_subagents = if completed {
        false
    } else if resumed {
        true
    } else {
        record.codex_parent_stopped_for_subagents
    };
    record.hook_terminal = if completed {
        true
    } else if resumed {
        false
    } else {
        record.hook_terminal
    };
    record.lifecycle = lifecycle as i32;
    record.lifecycle_observed_at_unix_millis = now;
    record.updated_at_unix_millis = now;
    record.state_generation = generation;
    record.subagent_evidence_observed_at_unix_millis = if has_children { now } else { 0 };
    if previous_lifecycle != lifecycle {
        record.lifecycle_changed_at_unix_millis = now;
    }
    let notify = previous_lifecycle == v1::AgentLifecycleState::Working
        && lifecycle == v1::AgentLifecycleState::Idle;
    if notify {
        record.attention_generation = record.attention_generation.saturating_add(1);
        record.attention_kind = "completed".into();
        record.attention_seen_at_unix_millis = 0;
    } else if lifecycle == v1::AgentLifecycleState::Working
        && record.attention_kind == "completed"
        && record.seen_generation >= record.attention_generation
    {
        record.attention_kind.clear();
        record.attention_seen_at_unix_millis = 0;
    }
    Some(v1::AgentEvent {
        agent: Some(snapshot::record(record)),
        generation,
        notify,
        reason: if notify {
            "completed"
        } else {
            "transcript_child_state"
        }
        .into(),
        retired_agent_ids: Vec::new(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PermissionReview {
    CachedAuto,
    CachedUser,
    ObservedAuto,
    ObservedUser,
    Unknown,
}

fn classify_permission_review(
    cached_reviewer: Option<crate::hook::codex_transcript::ApprovalReviewer>,
    embedded_reviewer: Option<&str>,
    revalidate: impl FnOnce() -> Option<crate::hook::codex_transcript::ApprovalReviewer>,
) -> PermissionReview {
    match cached_reviewer {
        Some(crate::hook::codex_transcript::ApprovalReviewer::AutoReview) => {
            return PermissionReview::CachedAuto;
        }
        Some(crate::hook::codex_transcript::ApprovalReviewer::User) => {
            return PermissionReview::CachedUser;
        }
        Some(crate::hook::codex_transcript::ApprovalReviewer::Unknown) | None => {}
    }
    let reviewer = embedded_reviewer.map(|reviewer| match reviewer {
        "auto_review" => crate::hook::codex_transcript::ApprovalReviewer::AutoReview,
        "user" => crate::hook::codex_transcript::ApprovalReviewer::User,
        _ => crate::hook::codex_transcript::ApprovalReviewer::Unknown,
    });
    match reviewer.or_else(revalidate) {
        Some(crate::hook::codex_transcript::ApprovalReviewer::AutoReview) => {
            PermissionReview::ObservedAuto
        }
        Some(crate::hook::codex_transcript::ApprovalReviewer::User) => {
            PermissionReview::ObservedUser
        }
        Some(crate::hook::codex_transcript::ApprovalReviewer::Unknown) | None => {
            PermissionReview::Unknown
        }
    }
}

#[cfg(test)]
mod permission_review_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn exact_positive_cache_hit_never_runs_revalidation() {
        let calls = Cell::new(0);
        let review = classify_permission_review(
            Some(crate::hook::codex_transcript::ApprovalReviewer::AutoReview),
            None,
            || {
                calls.set(calls.get() + 1);
                Some(crate::hook::codex_transcript::ApprovalReviewer::User)
            },
        );

        assert_eq!(review, PermissionReview::CachedAuto);
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn cache_miss_revalidates_once_and_only_positive_results_are_observed_auto() {
        let calls = Cell::new(0);
        let auto = classify_permission_review(None, None, || {
            calls.set(calls.get() + 1);
            Some(crate::hook::codex_transcript::ApprovalReviewer::AutoReview)
        });
        assert_eq!(auto, PermissionReview::ObservedAuto);
        assert_eq!(calls.get(), 1);

        let user = classify_permission_review(None, None, || {
            Some(crate::hook::codex_transcript::ApprovalReviewer::User)
        });
        assert_eq!(user, PermissionReview::ObservedUser);

        assert_eq!(
            classify_permission_review(None, None, || None),
            PermissionReview::Unknown
        );
    }
}
