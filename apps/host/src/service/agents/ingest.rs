use std::collections::VecDeque;

use anyhow::Context;
use tmux_agent_protocol::v1;

use super::{
    AgentRuntime, CodexReviewer, CodexTurnKey, CodexTurnReview, StoredAgent, adapters, identity,
    snapshot,
};
use crate::service::snapshot::{discover_authoritative, server_identity};

pub(super) const MAX_HOOK_BYTES: usize = 256 * 1024;
pub(super) const MAX_DEDUPE_IDS: usize = 512;
const MAX_CODEX_TURN_REVIEWS: usize = 64;

pub(crate) type HookIngestResult = Result<v1::AgentEvent, HookIngestFailure>;
type DeferredHookIngestResult = Result<IngestedHook, HookIngestFailure>;

struct IngestedHook {
    event: v1::AgentEvent,
    reply: Option<crate::service::voice::AgentReply>,
}

fn latest_codex_child_turn<'a>(
    turns: &'a VecDeque<CodexTurnKey>,
    agent_id: &str,
) -> Option<&'a str> {
    turns
        .iter()
        .rev()
        .find(|turn| turn.agent_id == agent_id)
        .map(|turn| turn.turn_id.as_str())
}

fn remember_codex_child_turn(turns: &mut VecDeque<CodexTurnKey>, turn: &CodexTurnKey) {
    if latest_codex_child_turn(turns, &turn.agent_id)
        .and_then(|latest| super::codex_authority::turn_order(&turn.turn_id, latest))
        == Some(std::cmp::Ordering::Less)
    {
        return;
    }
    turns.retain(|known| known.agent_id != turn.agent_id);
    turns.push_back(turn.clone());
    while turns.len() > super::MAX_CODEX_CHILD_TURN_HISTORY {
        turns.pop_front();
    }
}

pub(super) fn remember_codex_terminal_child_turn(
    turns: &mut VecDeque<CodexTurnKey>,
    turn: &CodexTurnKey,
) {
    turns.retain(|known| known != turn);
    turns.push_back(turn.clone());
    while turns.len() > super::MAX_CODEX_CHILD_TURN_HISTORY {
        turns.pop_front();
    }
}

/// A failed ingest is final only when its variant says so. Callers retain
/// `Retryable` input; duplicate and permanently malformed input are discarded.
#[derive(Debug)]
pub(crate) enum HookIngestFailure {
    Duplicate,
    /// A logical Codex thread that no longer owns its inherited tmux pane
    /// emitted a late hook. The event is final, but it must not replace the
    /// pane's current interactive thread.
    Superseded,
    Permanent(anyhow::Error),
    Retryable(anyhow::Error),
}

impl HookIngestFailure {
    pub(crate) fn disposition(&self) -> v1::HookIngestDisposition {
        match self {
            Self::Duplicate | Self::Superseded | Self::Permanent(_) => {
                v1::HookIngestDisposition::Discarded
            }
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
        let mut route = identity::hook_route(
            origin_matches.then_some(topology).flatten(),
            active_server_identity,
            &event.pane_id,
        );
        let mut state = self.state.lock().unwrap();
        let original = state.clone();
        let route_verified = origin_matches && !route.pane_id.is_empty();
        let mut restored_route_authority = false;
        // A native session has one identity for its entire lifetime. Topology
        // decides whether its route is usable; it must never decide identity.
        // Keeping those concerns separate prevents a brief tmux outage from
        // creating a second agent that later has to be ranked and merged.
        let identity_server = if origin_matches {
            active_server_identity
        } else {
            &event.origin_server_identity
        };
        let mut agent_id = if !native_session_id.is_empty() {
            identity::native_agent_id(adapter_id, identity_server, &native_session_id)
        } else if route_verified {
            identity::manual_agent_id(adapter_id, active_server_identity, &event.pane_id)
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
        let mut pane_record_id = candidates.pane;
        let mut native_record_id = if adapter.id() == "codex" && !native_session_id.is_empty() {
            state
                .agents
                .contains_key(&agent_id)
                .then(|| agent_id.clone())
        } else {
            candidates.native
        };
        // Session authority is decided before dedupe repair or any other state
        // mutation. A delayed fork hook must not use a coincident event ID or
        // transcript edge to mutate the interactive pane owner's turn state.
        let canonical_native_agent_id = (!native_session_id.is_empty())
            .then(|| identity::native_agent_id(adapter_id, identity_server, &native_session_id));
        // Topology discovery can fail while the hook still carries the exact
        // same-server pane inherited from tmux. That pane is not trusted as a
        // destination, but an existing mapped owner may use it as negative
        // evidence that a different logical session is a superseded emitter.
        let stored_pane_owner_id = origin_matches
            .then(|| {
                state
                    .pane_owner_id(adapter.id(), active_server_identity, &event.pane_id)
                    .map(str::to_owned)
            })
            .flatten();
        let observed_pane_owner = stored_pane_owner_id
            .as_ref()
            .and_then(|owner_id| state.agents.get(owner_id))
            .or_else(|| {
                (adapter.id() != "codex")
                    .then(|| {
                        pane_record_id
                            .as_ref()
                            .and_then(|pane_id| state.agents.get(pane_id))
                    })
                    .flatten()
            });
        let observed_pane_owner_id = observed_pane_owner.map(|owner| owner.agent_id.clone());
        let observed_pane_owner_record = observed_pane_owner.cloned();
        let known_mapped_native_session = native_record_id.as_ref().is_some_and(|record_id| {
            state.agents[record_id].present && state.agent_is_bound(record_id)
        });
        let session_authority = if adapter.id() == "codex" {
            super::codex_authority::session(
                &native_session_id,
                known_mapped_native_session,
                observed_pane_owner.map(|owner| owner.native_session_id.as_str()),
                route_verified,
                &parsed.event_name,
            )
        } else {
            super::codex_authority::SessionAuthority::Current
        };
        if session_authority == super::codex_authority::SessionAuthority::Superseded {
            crate::diagnostics::write_codex_hook_superseded_log(
                &event.pane_id,
                observed_pane_owner.map(|owner| owner.agent_id.as_str()),
                &agent_id,
                &parsed.event_name,
            );
            return Err(HookIngestFailure::Superseded);
        }
        // Keep the current session's last verified identity and route through
        // topology loss. TMUX_PANE is evidence that the hook still belongs to
        // this owner, but never becomes a new unverified destination.
        if adapter.id() == "codex"
            && session_authority == super::codex_authority::SessionAuthority::Current
            && !route_verified
        {
            let exact_pane_owner = observed_pane_owner;
            let continuity_id = exact_pane_owner
                .filter(|owner| owner.native_session_id == native_session_id)
                .map(|owner| owner.agent_id.clone())
                .or_else(|| native_record_id.clone());
            if exact_pane_owner.is_some_and(|owner| {
                owner.native_session_id.is_empty() && !owner.route.pane_id.is_empty()
            }) && !native_session_id.is_empty()
            {
                let owner = exact_pane_owner.expect("manual pane owner is present");
                agent_id = canonical_native_agent_id
                    .clone()
                    .expect("nonempty native session has a canonical identity");
                route = owner.route.clone();
                pane_record_id = Some(owner.agent_id.clone());
                restored_route_authority = true;
            } else if let Some(continuity_id) = continuity_id {
                let continuity = &state.agents[&continuity_id];
                agent_id.clone_from(&continuity.agent_id);
                route = continuity.route.clone();
                native_record_id = Some(continuity.agent_id.clone());
                pane_record_id = None;
            }
        }
        let trusted_route = route_verified || restored_route_authority;
        // A replacement is a new native-session boundary. The displaced
        // pane owner's dedupe, sequence, lifecycle, attention, root and child
        // state belong to the old session and cannot seed the new owner.
        let mut session_record_ids = Vec::new();
        if matches!(
            session_authority,
            super::codex_authority::SessionAuthority::Current
                | super::codex_authority::SessionAuthority::Move
        ) {
            for record_id in [native_record_id.as_ref(), pane_record_id.as_ref()]
                .into_iter()
                .flatten()
            {
                if session_authority == super::codex_authority::SessionAuthority::Move
                    && pane_record_id.as_ref() == Some(record_id)
                {
                    continue;
                }
                if !session_record_ids.contains(&record_id) {
                    session_record_ids.push(record_id);
                }
            }
        }
        let duplicate_id = session_record_ids
            .iter()
            .find(|candidate| {
                state.agents[candidate.as_str()]
                    .source_event_ids
                    .contains(&event.source_event_id)
            })
            .map(|id| (*id).clone());
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
        let latest_sequence = session_record_ids
            .iter()
            .copied()
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
        let previous = session_record_ids
            .first()
            .and_then(|id| state.agents.get(*id))
            .cloned();
        let metadata_previous =
            if session_authority == super::codex_authority::SessionAuthority::Replacement {
                observed_pane_owner_record
            } else {
                session_record_ids
                    .first()
                    .and_then(|id| state.agents.get(*id))
                    .cloned()
            };
        let mut source_ids = VecDeque::new();
        for candidate in session_record_ids
            .iter()
            .filter_map(|id| state.agents.get(id.as_str()))
        {
            for id in &candidate.source_event_ids {
                if !source_ids.contains(id) {
                    source_ids.push_back(id.clone());
                }
            }
        }
        let mut retirement_candidates: Vec<String> = if trusted_route {
            [native_record_id.as_ref(), pane_record_id.as_ref()]
                .into_iter()
                .flatten()
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        if matches!(
            session_authority,
            super::codex_authority::SessionAuthority::Replacement
                | super::codex_authority::SessionAuthority::Move
        ) && let Some(owner_id) = observed_pane_owner_id
            && !retirement_candidates.contains(&owner_id)
        {
            retirement_candidates.push(owner_id);
        }
        let mut voice_preferred_retired_agent_ids = Vec::new();
        if !native_session_id.is_empty() {
            for old_id in &retirement_candidates {
                if old_id == &agent_id {
                    continue;
                }
                let Some(record) = state.agents.get(old_id) else {
                    continue;
                };
                if !record.present {
                    continue;
                }
                if record.native_session_id.is_empty() {
                    voice_preferred_retired_agent_ids.push(old_id.clone());
                }
            }
        }
        let mut retired_agent_ids = Vec::new();
        for old_id in retirement_candidates {
            if old_id != agent_id
                && !retired_agent_ids.contains(&old_id)
                && state.agents.remove(&old_id).is_some()
            {
                retired_agent_ids.push(old_id);
            }
        }
        state.unbind_agents(&retired_agent_ids);
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
        let mut codex_subagent_root_turn_ids = previous
            .as_ref()
            .map(|record| record.codex_subagent_root_turn_ids.clone())
            .unwrap_or_default();
        let mut codex_subagents_awaiting_root = previous
            .as_ref()
            .map(|record| record.codex_subagents_awaiting_root.clone())
            .unwrap_or_default();
        let mut codex_latest_subagent_turns = previous
            .as_ref()
            .map(|record| record.codex_latest_subagent_turns.clone())
            .unwrap_or_default();
        let mut codex_terminal_subagent_turns = previous
            .as_ref()
            .map(|record| record.codex_terminal_subagent_turns.clone())
            .unwrap_or_default();
        let mut codex_subagent_capacity_exceeded = previous
            .as_ref()
            .is_some_and(|record| record.codex_subagent_capacity_exceeded);
        let codex_subagent_turn = approval_turn
            .as_ref()
            .filter(|turn| adapter.id() == "codex" && !turn.agent_id.is_empty())
            .cloned();
        let codex_child_event = codex_subagent_turn.is_some();
        let codex_child_stop = codex_child_event
            && matches!(
                parsed.event_name.as_str(),
                "SubagentStop" | "Stop" | "Interrupt" | "SessionEnd"
            );
        let codex_stale_child_activity = codex_subagent_turn.as_ref().is_some_and(|turn| {
            let latest_for_child =
                latest_codex_child_turn(&codex_latest_subagent_turns, &turn.agent_id);
            let inactive_latest_replay = previous_lifecycle != v1::AgentLifecycleState::Unknown
                && codex_running_subagents.get(&turn.agent_id) != Some(&turn.turn_id)
                && latest_for_child == Some(turn.turn_id.as_str());
            let older_than_child_watermark = latest_for_child
                .and_then(|latest| super::codex_authority::turn_order(&turn.turn_id, latest))
                == Some(std::cmp::Ordering::Less);
            codex_terminal_subagent_turns.contains(turn)
                || inactive_latest_replay
                || older_than_child_watermark
        }) && !codex_child_stop;
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
        let codex_child_activity = codex_child_event
            && !codex_child_stop
            && !child_transcript_terminal
            && !codex_stale_child_activity;
        let codex_root_turn_id = approval_turn
            .as_ref()
            .filter(|turn| adapter.id() == "codex" && turn.agent_id.is_empty())
            .map(|turn| turn.turn_id.as_str());
        let mut codex_active_root_turn_id = previous
            .as_ref()
            .map(|record| record.codex_active_root_turn_id.clone())
            .unwrap_or_default();
        let mut codex_terminal_root_turn_ids = previous
            .as_ref()
            .map(|record| record.codex_terminal_root_turn_ids.clone())
            .unwrap_or_default();
        if adapter.id() == "codex" && parsed.event_name == "SessionStart" {
            codex_active_root_turn_id.clear();
            codex_terminal_root_turn_ids.clear();
            codex_subagent_root_turn_ids.clear();
            codex_subagents_awaiting_root.clear();
            codex_latest_subagent_turns.clear();
            codex_terminal_subagent_turns.clear();
        }
        let codex_root_terminal_event = codex_root_turn_id.is_some()
            && matches!(
                parsed.event_name.as_str(),
                "Stop" | "Interrupt" | "SessionEnd"
            );
        let codex_child_terminal_event = codex_child_stop;
        let codex_root_activity = codex_root_turn_id.is_some()
            && !matches!(
                parsed.event_name.as_str(),
                "SessionStart" | "Stop" | "Interrupt" | "SessionEnd"
            );
        let codex_explicit_root_start = codex_turn_start && codex_root_turn_id.is_some();
        let root_authority = if adapter.id() == "codex" {
            super::codex_authority::root_turn(super::codex_authority::RootTurnInput {
                event_name: &parsed.event_name,
                turn_id: codex_root_turn_id,
                active_turn_id: &codex_active_root_turn_id,
                terminal_turn_ids: &codex_terminal_root_turn_ids,
                previous_hook_terminal,
                child_event: codex_child_event,
            })
        } else {
            super::codex_authority::RootTurnAuthority::Unscoped
        };
        let codex_session_terminal_event =
            root_authority == super::codex_authority::RootTurnAuthority::SessionTerminal;
        // Goal continuations begin a fresh Codex turn without another
        // UserPromptSubmit. Their first observable hook can be activity or the
        // terminal itself when the continuation needed no tool.
        let codex_new_root_observed =
            root_authority == super::codex_authority::RootTurnAuthority::Successor;
        let mut codex_root_owner_changed = false;
        if codex_new_root_observed {
            let next_root_turn_id = codex_root_turn_id
                .expect("Codex root continuation has a turn")
                .to_owned();
            for child_id in &codex_subagents_awaiting_root {
                if codex_running_subagents.contains_key(child_id) {
                    codex_subagent_root_turn_ids
                        .insert(child_id.clone(), next_root_turn_id.clone());
                }
            }
            codex_subagents_awaiting_root.clear();
            super::remember_terminal_root_turn(
                &mut codex_terminal_root_turn_ids,
                &codex_active_root_turn_id,
            );
            codex_active_root_turn_id = next_root_turn_id;
            codex_root_owner_changed = true;
        }
        let codex_root_continuation =
            codex_new_root_observed && codex_root_activity && !codex_explicit_root_start;
        if codex_root_terminal_event && !codex_session_terminal_event {
            let turn_id = codex_root_turn_id.expect("Codex root terminal has a turn");
            if codex_active_root_turn_id.is_empty() {
                codex_active_root_turn_id = turn_id.to_owned();
            }
            super::remember_terminal_root_turn(&mut codex_terminal_root_turn_ids, turn_id);
        }
        // Current Codex Stop hooks carry an exact turn. SessionEnd and
        // Interrupt are session-authoritative even when no turn is attached.
        let codex_unscoped_root_terminal = adapter.id() == "codex"
            && !codex_child_event
            && codex_root_turn_id.is_none()
            && !codex_active_root_turn_id.is_empty()
            && parsed.event_name == "Stop";
        let codex_ignored_root_event = root_authority
            == super::codex_authority::RootTurnAuthority::Stale
            || codex_unscoped_root_terminal;
        let codex_ignored_lifecycle_event = codex_ignored_root_event || codex_stale_child_activity;
        if adapter.id() == "codex"
            && (!previous_hook_terminal
                || parsed.event_name == "SessionStart"
                || codex_turn_start
                || codex_child_activity
                || codex_child_stop
                || child_transcript_terminal)
        {
            match parsed.event_name.as_str() {
                "SessionStart" => {
                    codex_running_subagents.clear();
                    codex_subagent_root_turn_ids.clear();
                    codex_subagents_awaiting_root.clear();
                    codex_latest_subagent_turns.clear();
                    codex_terminal_subagent_turns.clear();
                    codex_subagent_capacity_exceeded = false;
                }
                _ if codex_child_stop || child_transcript_terminal => {
                    if let Some(turn) = codex_subagent_turn.as_ref() {
                        match codex_running_subagents.get(&turn.agent_id) {
                            Some(active_turn)
                                if active_turn.is_empty() || active_turn == &turn.turn_id =>
                            {
                                codex_running_subagents.remove(&turn.agent_id);
                                codex_subagent_root_turn_ids.remove(&turn.agent_id);
                                codex_subagents_awaiting_root.remove(&turn.agent_id);
                                remember_codex_child_turn(&mut codex_latest_subagent_turns, turn);
                                remember_codex_terminal_child_turn(
                                    &mut codex_terminal_subagent_turns,
                                    turn,
                                );
                            }
                            None => {
                                remember_codex_child_turn(&mut codex_latest_subagent_turns, turn);
                                remember_codex_terminal_child_turn(
                                    &mut codex_terminal_subagent_turns,
                                    turn,
                                );
                            }
                            Some(_) => remember_codex_terminal_child_turn(
                                &mut codex_terminal_subagent_turns,
                                turn,
                            ),
                        }
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
                    let prior_turn =
                        latest_codex_child_turn(&codex_latest_subagent_turns, &turn.agent_id);
                    let resumed = prior_turn.is_some_and(|prior| prior != turn.turn_id.as_str());
                    codex_running_subagents.insert(turn.agent_id.clone(), turn.turn_id.clone());
                    let owner = if resumed {
                        if codex_terminal_root_turn_ids.contains(&codex_active_root_turn_id) {
                            codex_subagents_awaiting_root.insert(turn.agent_id.clone());
                            String::new()
                        } else {
                            codex_subagents_awaiting_root.remove(&turn.agent_id);
                            codex_active_root_turn_id.clone()
                        }
                    } else {
                        codex_subagent_root_turn_ids
                            .get(&turn.agent_id)
                            .cloned()
                            .unwrap_or_default()
                    };
                    codex_subagent_root_turn_ids.insert(turn.agent_id.clone(), owner);
                    remember_codex_child_turn(&mut codex_latest_subagent_turns, turn);
                }
                _ if codex_child_activity => codex_subagent_capacity_exceeded = true,
                _ => {}
            }
        }
        let mut transcript_child_event = false;
        if adapter.id() == "codex" {
            for child_id in sanitized_child_terminals_for_root(
                &payload,
                &codex_active_root_turn_id,
                &codex_subagent_root_turn_ids,
                &codex_subagents_awaiting_root,
            ) {
                if let Some(turn_id) = codex_running_subagents.remove(&child_id) {
                    codex_subagent_root_turn_ids.remove(&child_id);
                    codex_subagents_awaiting_root.remove(&child_id);
                    remember_codex_terminal_child_turn(
                        &mut codex_terminal_subagent_turns,
                        &CodexTurnKey {
                            agent_id: child_id.clone(),
                            turn_id,
                        },
                    );
                    transcript_child_event = true;
                }
            }
        }
        let codex_parent_stop_during_subagents = adapter.id() == "codex"
            && parsed.event_name == "Stop"
            && !codex_child_event
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
            } else if codex_child_terminal_event {
                previous_lifecycle
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
            && !codex_child_activity
            && !codex_root_continuation;
        let lifecycle = if codex_ignored_lifecycle_event {
            // Preserve the newer root turn when an event from a completed
            // turn arrives after it. A transcript repair carried by that
            // stale event may complete only a parent that had already stopped.
            if codex_final_unwaited_subagent_stop {
                v1::AgentLifecycleState::Idle
            } else {
                previous_lifecycle
            }
        } else if terminal_late {
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
        let hook_terminal = if codex_ignored_lifecycle_event {
            codex_final_unwaited_subagent_stop || previous_hook_terminal
        } else if codex_child_terminal_event && !codex_final_unwaited_subagent_stop {
            previous_hook_terminal
        } else if matches!(
            parsed.event_name.as_str(),
            "SessionStart" | "UserPromptSubmit"
        ) || codex_child_activity
            || codex_root_continuation
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
        let codex_tool_free_continuation_completed = codex_new_root_observed
            && codex_root_terminal_event
            && previous_hook_terminal
            && lifecycle == v1::AgentLifecycleState::Idle;
        let attention_transition = lifecycle == v1::AgentLifecycleState::Blocked
            && previous_lifecycle != v1::AgentLifecycleState::Blocked
            || previous_lifecycle == v1::AgentLifecycleState::Working
                && lifecycle == v1::AgentLifecycleState::Idle
            || codex_tool_free_continuation_completed;
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
        if !source_ids.contains(&event.source_event_id) {
            source_ids.push_back(event.source_event_id.clone());
        }
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
        if adapter.id() == "codex" && !codex_ignored_lifecycle_event {
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
        let mut codex_parent_stopped_for_subagents = if adapter.id() != "codex" {
            false
        } else if codex_ignored_lifecycle_event {
            previous_codex_parent_stopped && !codex_final_unwaited_subagent_stop
        } else if codex_root_owner_changed && parsed.event_name != "Stop" || terminal_late {
            false
        } else if parsed.event_name == "Stop" && !codex_child_event {
            !codex_running_subagents.is_empty() || codex_subagent_capacity_exceeded
        } else if lifecycle == v1::AgentLifecycleState::Blocked {
            previous_codex_parent_stopped
        } else {
            match parsed.event_name.as_str() {
                "SessionStart" => false,
                "UserPromptSubmit" if !codex_child_event => false,
                _ if (codex_child_stop || child_transcript_terminal)
                    && codex_running_subagents.is_empty()
                    && !codex_subagent_capacity_exceeded =>
                {
                    false
                }
                _ if codex_resolved_block_after_children => false,
                _ if codex_child_activity && previous_hook_terminal => codex_subagent_turn
                    .as_ref()
                    .is_none_or(|turn| !codex_subagents_awaiting_root.contains(&turn.agent_id)),
                _ => previous_codex_parent_stopped,
            }
        };
        if hook_terminal {
            if adapter.id() == "codex" {
                if !codex_active_root_turn_id.is_empty() {
                    super::remember_terminal_root_turn(
                        &mut codex_terminal_root_turn_ids,
                        &codex_active_root_turn_id,
                    );
                }
                for (child_id, turn_id) in &codex_running_subagents {
                    remember_codex_terminal_child_turn(
                        &mut codex_terminal_subagent_turns,
                        &CodexTurnKey {
                            agent_id: child_id.clone(),
                            turn_id: turn_id.clone(),
                        },
                    );
                }
            }
            codex_running_subagents.clear();
            codex_subagent_root_turn_ids.clear();
            codex_subagents_awaiting_root.clear();
            codex_subagent_capacity_exceeded = false;
            codex_parent_stopped_for_subagents = false;
        }
        let subagent_evidence_observed_at_unix_millis = if hook_terminal {
            0
        } else if adapter.id() == "codex" {
            if codex_running_subagents.is_empty() && !codex_subagent_capacity_exceeded {
                0
            } else if (codex_child_event && !codex_ignored_lifecycle_event)
                || transcript_child_event
            {
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
            display_name: metadata_previous
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
            detected_manually: metadata_previous
                .as_ref()
                .is_some_and(|record| record.detected_manually),
            source_event_ids: source_ids,
            latest_source_generation,
            present: true,
            hook_terminal,
            codex_active_root_turn_id: codex_active_root_turn_id.clone(),
            codex_terminal_root_turn_ids,
            codex_subagent_root_turn_ids,
            codex_subagents_awaiting_root,
            codex_latest_subagent_turns,
            codex_terminal_subagent_turns,
            claude_has_running_subagent,
            codex_running_subagents,
            codex_subagent_capacity_exceeded,
            codex_parent_stopped_for_subagents,
            subagent_evidence_observed_at_unix_millis,
            codex_turn_reviews,
            lifecycle_observed_at_unix_millis: if codex_ignored_lifecycle_event
                && !transcript_child_event
            {
                previous.as_ref().map_or(observed_now, |record| {
                    record.lifecycle_observed_at_unix_millis
                })
            } else {
                observed_now
            },
            lifecycle_changed_at_unix_millis: lifecycle_changed_at,
        };
        state.agents.insert(agent_id.clone(), record.clone());
        let explicit_session_claim = adapter.id() == "codex"
            && matches!(
                parsed.event_name.as_str(),
                "SessionStart" | "UserPromptSubmit"
            );
        if origin_matches && (trusted_route || explicit_session_claim && topology.is_none()) {
            state.bind_pane(
                adapter.id(),
                active_server_identity,
                &event.pane_id,
                &agent_id,
            );
        }
        if let Err(error) = self.persist_locked(&state) {
            *state = original;
            return Err(HookIngestFailure::Retryable(error));
        }
        drop(state);
        if adapter.id() == "codex" {
            self.retire_codex_runtime_state(&retired_agent_ids);
        }
        if adapter.id() == "codex" {
            if !codex_ignored_lifecycle_event {
                let permission_root_turn_id = if approval_turn.as_ref().is_some_and(|turn| {
                    !turn.agent_id.is_empty()
                        && record
                            .codex_subagents_awaiting_root
                            .contains(&turn.agent_id)
                }) {
                    String::new()
                } else {
                    codex_active_root_turn_id.clone()
                };
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
                    (&permission_root_turn_id, Some(&record)),
                );
            }
            self.track_codex_child(
                &agent_id,
                codex_subagent_turn,
                child_monitor,
                codex_child_activity,
                parsed.event_name == "SessionStart" || record.hook_terminal,
            );
        }
        if !voice_preferred_retired_agent_ids.is_empty() {
            (self.identity_promotion_sink)(&voice_preferred_retired_agent_ids, &agent_id);
        }
        // Voice mode (docs/mobile/voice-mode-plan.md §4.5): Claude produces a
        // final aggregate Stop after its background children, so its
        // intermediate Stop is skipped. Codex does not produce another parent
        // Stop when an unwaited child finishes; speaking its one parent reply
        // here preserves the existing voice behavior without storing content.
        let reply = if parsed.event_name == "Stop"
            && !codex_ignored_root_event
            && !codex_child_event
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

fn sanitized_child_terminals_for_root(
    payload: &serde_json::Value,
    active_root_turn_id: &str,
    child_roots: &std::collections::BTreeMap<String, String>,
    awaiting_root: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    let root_turn_id = payload
        .get(adapters::CODEX_APPROVAL_TURN_ID_FIELD)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if root_turn_id.is_empty() {
        return Vec::new();
    }
    sanitized_child_terminals(payload)
        .into_iter()
        .filter(|child_id| {
            !awaiting_root.contains(child_id)
                && (root_turn_id == active_root_turn_id
                    || child_roots
                        .get(child_id)
                        .is_some_and(|owner| !owner.is_empty() && owner == root_turn_id))
        })
        .collect()
}

fn reconcile_duplicate_transcript_child_states(
    state: &mut super::StoredState,
    record_id: &str,
    payload: &serde_json::Value,
    now: i64,
) -> Option<v1::AgentEvent> {
    let before = state.agents.get(record_id)?.clone();
    let transitions = sanitized_child_terminals_for_root(
        payload,
        &before.codex_active_root_turn_id,
        &before.codex_subagent_root_turn_ids,
        &before.codex_subagents_awaiting_root,
    );
    if transitions.is_empty() {
        return None;
    }
    let mut children = before.codex_running_subagents.clone();
    let mut child_roots = before.codex_subagent_root_turn_ids.clone();
    let mut awaiting_root = before.codex_subagents_awaiting_root.clone();
    let mut terminal_child_turns = before.codex_terminal_subagent_turns.clone();
    let capacity_exceeded = before.codex_subagent_capacity_exceeded;
    for child_id in transitions {
        if let Some(turn_id) = children.remove(&child_id) {
            remember_codex_terminal_child_turn(
                &mut terminal_child_turns,
                &CodexTurnKey {
                    agent_id: child_id.clone(),
                    turn_id,
                },
            );
        }
        child_roots.remove(&child_id);
        awaiting_root.remove(&child_id);
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
    record.codex_subagent_root_turn_ids = child_roots;
    record.codex_subagents_awaiting_root = awaiting_root;
    record.codex_terminal_subagent_turns = terminal_child_turns;
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
    if completed && !record.codex_active_root_turn_id.is_empty() {
        let active_root_turn_id = record.codex_active_root_turn_id.clone();
        super::remember_terminal_root_turn(
            &mut record.codex_terminal_root_turn_ids,
            &active_root_turn_id,
        );
    }
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
