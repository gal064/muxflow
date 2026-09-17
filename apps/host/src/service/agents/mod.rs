use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use tmux_agent_protocol::v1;

use super::{broadcast_control_event, snapshot::server_identity};

pub(crate) mod adapters;
mod codex_authority;
mod fallback;
mod hooks;
mod identity;
mod ingest;
mod process;
mod reconcile;
mod snapshot;
mod store;
pub(crate) use hooks::HookManager;
pub(crate) use ingest::HookIngestFailure;
use store::{CodexReviewer, CodexTurnKey, CodexTurnReview, StoredAgent, StoredRoute, StoredState};

/// How long a Working agent may go without a single lifecycle event before the
/// daemon stops claiming to know what it is doing.
///
/// Working is asserted by an event and only ever withdrawn by another one, so
/// an agent whose events stop — its process killed, its hooks removed
/// underneath it, a vendor upgrade that renamed an event — stays "working"
/// forever. That is the failure this phase exists to remove, and it is worse
/// than saying nothing.
///
/// The bound is set by the longest gap a *healthy* agent can leave. Claude Code
/// and Codex both emit a hook per tool call, so the gap is one tool call, and
/// the longest of those is a shell command at Claude's maximum configurable
/// `Bash` timeout of 600s; `PostToolUse` follows immediately after. Fifteen
/// minutes clears that ceiling by half again, so no healthy long-running tool
/// call can flap the state, while a genuinely dead agent stops lying within a
/// coffee break. Degrading is deliberately one-way per event: the next hook of
/// any kind restores real state.
const STALE_WORKING_TTL_MILLIS: i64 = 15 * 60 * 1_000;
/// A vendor-reported child may legitimately be silent far longer than one
/// tool call. Keep that stronger evidence for a day, but not forever: a lost
/// terminal hook must still have a bounded recovery path.
const STALE_SUBAGENT_WORKING_TTL_MILLIS: i64 = 24 * 60 * 60 * 1_000;
/** Three daemon maintenance passes (normally six seconds) make process absence conclusive. */
const DEPARTURE_MISSES_REQUIRED: u8 = 3;
const MAX_PENDING_CODEX_PERMISSIONS: usize = 64;
const PENDING_CODEX_PERMISSION_TTL_MILLIS: i64 = 24 * 60 * 60 * 1_000;
pub(crate) const MAX_CODEX_CHILDREN: usize = 64;
pub(crate) const MAX_CODEX_CHILD_TURN_HISTORY: usize = 512;
pub(crate) const MAX_CODEX_TERMINAL_ROOT_TURNS: usize = 64;
pub(crate) const MAX_CODEX_PANE_SESSION_DEPTH: usize = 16;
const MAX_CODEX_TRANSCRIPT_MONITORS: usize = 64;

fn remember_terminal_root_turn(turns: &mut std::collections::BTreeSet<String>, turn_id: &str) {
    if turn_id.is_empty() {
        return;
    }
    turns.insert(turn_id.to_owned());
    while turns.len() > MAX_CODEX_TERMINAL_ROOT_TURNS {
        let Some(oldest) = turns.first().cloned() else {
            break;
        };
        turns.remove(&oldest);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PendingCodexPermissionKey {
    record_id: String,
    turn: CodexTurnKey,
}

struct PendingCodexPermission {
    monitor: crate::hook::codex_transcript::TurnMonitor,
    terminal: Option<crate::hook::codex_transcript::TurnTerminal>,
    lifecycle_changed_at_unix_millis: i64,
    observed_at_unix_millis: i64,
    root_turn_id: String,
}

struct CodexChildMonitor {
    turn: CodexTurnKey,
    monitor: crate::hook::codex_transcript::TurnMonitor,
    terminal: Option<crate::hook::codex_transcript::TurnTerminal>,
}

pub(crate) struct AgentRuntime {
    state_path: PathBuf,
    /// Orders persisted hook state through identity side effects and event
    /// publication, so concurrent hook connections cannot publish backwards.
    ingest_order: Mutex<()>,
    state: Mutex<StoredState>,
    /// What this host's agent configuration was last observed to do with
    /// lifecycle events. Re-read only when a configuration file changed.
    wiring: Mutex<hooks::WiringCache>,
    /** Consecutive process-scan misses, reset by detection or a newer hook generation. */
    departure_misses: Mutex<BTreeMap<String, (u64, u8)>>,
    /// Confirmed manual Codex permissions that need transcript-terminal
    /// recovery because Codex emits no hook when the user cancels its dialog.
    /// Entries and transcript handles are memory-only and strictly bounded.
    pending_codex_permissions: Mutex<BTreeMap<PendingCodexPermissionKey, PendingCodexPermission>>,
    /// One exact-turn transcript descriptor per active Codex child. Paths and
    /// transcript contents never enter persisted state.
    codex_child_monitors: Mutex<BTreeMap<(String, String), CodexChildMonitor>>,
    /// Where a `Stop` hands the agent's final message. Voice mode in
    /// production; a recorder in tests, so no test needs the voice service.
    reply_sink: ReplySink,
}

type ReplySink = Box<dyn Fn(super::voice::AgentReply) + Send + Sync>;

static GLOBAL: OnceLock<Arc<AgentRuntime>> = OnceLock::new();

impl AgentRuntime {
    pub(crate) fn global() -> Arc<Self> {
        Arc::clone(
            GLOBAL.get_or_init(|| {
                Arc::new(Self::load(crate::paths::state_dir().join("agents.json")))
            }),
        )
    }

    fn load(state_path: PathBuf) -> Self {
        Self::load_with_sink(state_path, Box::new(super::voice::on_agent_reply))
    }

    fn load_with_sink(state_path: PathBuf, reply_sink: ReplySink) -> Self {
        let state = store::load(&state_path);
        Self {
            state_path,
            ingest_order: Mutex::new(()),
            state: Mutex::new(state),
            wiring: Mutex::new(hooks::WiringCache::default()),
            departure_misses: Mutex::new(BTreeMap::new()),
            pending_codex_permissions: Mutex::new(BTreeMap::new()),
            codex_child_monitors: Mutex::new(BTreeMap::new()),
            reply_sink,
        }
    }

    #[cfg(test)]
    fn isolated(state_path: PathBuf) -> Self {
        Self::load(state_path)
    }

    #[cfg(test)]
    fn isolated_with_sink(state_path: PathBuf, reply_sink: ReplySink) -> Self {
        Self::load_with_sink(state_path, reply_sink)
    }

    pub(super) fn snapshot(&self) -> v1::AgentSnapshot {
        self.snapshot_for(&server_identity())
    }

    fn track_codex_permission(
        &self,
        record_id: &str,
        event_name: &str,
        turn: Option<CodexTurnKey>,
        monitor: Option<crate::hook::codex_transcript::TurnMonitor>,
        lifecycle_changed_at_unix_millis: i64,
        root: (&str, Option<&StoredAgent>),
    ) {
        let (root_turn_id, current_record) = root;
        let observed_at_unix_millis = now_millis();
        let current_children = turn
            .as_ref()
            .filter(|turn| turn.agent_id.is_empty())
            .and(current_record)
            .map(|record| {
                (
                    record.codex_running_subagents.clone(),
                    record.codex_subagent_root_turn_ids.clone(),
                )
            })
            .unwrap_or_default();
        let mut pending = self.pending_codex_permissions.lock().unwrap();
        pending.retain(|_, value| {
            observed_at_unix_millis.saturating_sub(value.observed_at_unix_millis)
                <= PENDING_CODEX_PERMISSION_TTL_MILLIS
        });
        if matches!(
            event_name,
            "SessionStart" | "UserPromptSubmit" | "Interrupt" | "SessionEnd"
        ) {
            pending.retain(|key, _| key.record_id != record_id);
        } else if let Some(active_root) = turn.as_ref().filter(|turn| turn.agent_id.is_empty()) {
            for (key, value) in pending.iter_mut() {
                if key.record_id == record_id
                    && !key.turn.agent_id.is_empty()
                    && value.root_turn_id.is_empty()
                    && current_children.0.get(&key.turn.agent_id) == Some(&key.turn.turn_id)
                    && current_children.1.get(&key.turn.agent_id) == Some(&active_root.turn_id)
                {
                    value.root_turn_id.clone_from(&active_root.turn_id);
                }
            }
            pending.retain(|key, value| {
                key.record_id != record_id
                    || key.turn.agent_id.is_empty()
                    || !value.root_turn_id.is_empty()
                    || current_children.0.get(&key.turn.agent_id) == Some(&key.turn.turn_id)
                        && current_children.1.get(&key.turn.agent_id) == Some(&active_root.turn_id)
            });
            pending.retain(|key, _| {
                key.record_id != record_id
                    || !key.turn.agent_id.is_empty()
                    || key.turn.turn_id == active_root.turn_id
            });
        }
        let Some(turn) = turn else {
            return;
        };
        let key = PendingCodexPermissionKey {
            record_id: record_id.to_owned(),
            turn,
        };
        pending.remove(&key);
        if event_name == "PermissionRequest"
            && let Some(monitor) = monitor
        {
            pending.insert(
                key,
                PendingCodexPermission {
                    monitor,
                    terminal: None,
                    lifecycle_changed_at_unix_millis,
                    observed_at_unix_millis,
                    root_turn_id: root_turn_id.to_owned(),
                },
            );
        }
        while pending.len() > MAX_PENDING_CODEX_PERMISSIONS {
            let Some(oldest) = pending
                .iter()
                .min_by_key(|(_, value)| value.observed_at_unix_millis)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            pending.remove(&oldest);
        }
    }

    fn has_codex_child_monitor(&self, record_id: &str, turn: &CodexTurnKey) -> bool {
        self.codex_child_monitors
            .lock()
            .unwrap()
            .get(&(record_id.to_owned(), turn.agent_id.clone()))
            .is_some_and(|entry| entry.turn == *turn)
    }

    fn track_codex_child(
        &self,
        record_id: &str,
        turn: Option<CodexTurnKey>,
        monitor: Option<crate::hook::codex_transcript::TurnMonitor>,
        active: bool,
        clear_record: bool,
    ) {
        let mut monitors = self.codex_child_monitors.lock().unwrap();
        if clear_record {
            monitors.retain(|(stored_record, _), _| stored_record != record_id);
            return;
        }
        let Some(turn) = turn else {
            return;
        };
        let key = (record_id.to_owned(), turn.agent_id.clone());
        if !active {
            if monitors.get(&key).is_some_and(|entry| entry.turn == turn) {
                monitors.remove(&key);
            }
            return;
        }
        if monitors.get(&key).is_some_and(|entry| entry.turn != turn) {
            monitors.remove(&key);
        }
        let Some(monitor) = monitor else {
            return;
        };
        if monitors.contains_key(&key) || monitors.len() < MAX_CODEX_TRANSCRIPT_MONITORS {
            monitors.insert(
                key,
                CodexChildMonitor {
                    turn,
                    monitor,
                    terminal: None,
                },
            );
        }
    }

    fn retire_codex_runtime_state(&self, record_ids: &[String]) {
        if record_ids.is_empty() {
            return;
        }
        self.pending_codex_permissions
            .lock()
            .unwrap()
            .retain(|key, _| !record_ids.contains(&key.record_id));
        self.codex_child_monitors
            .lock()
            .unwrap()
            .retain(|(record_id, _), _| !record_ids.contains(record_id));
    }

    /// Reconcile the terminal record for each exact Codex child turn. Codex
    /// writes `turn_aborted` when a child is interrupted but emits no matching
    /// `SubagentStop`; the child transcript is the authoritative repair path.
    fn sweep_codex_child_terminals(&self) -> Vec<v1::AgentEvent> {
        let resolved = {
            let mut monitors = self.codex_child_monitors.lock().unwrap();
            monitors.retain(|_, entry| entry.monitor.is_readable());
            for entry in monitors.values_mut() {
                if entry.terminal.is_none() {
                    entry.terminal = entry.monitor.poll_terminal();
                }
            }
            monitors
                .iter()
                .filter_map(|(key, entry)| {
                    entry
                        .terminal
                        .map(|terminal| (key.clone(), entry.turn.clone(), terminal))
                })
                .collect::<Vec<_>>()
        };
        if resolved.is_empty() {
            return Vec::new();
        }

        let now = now_millis();
        let mut state = self.state.lock().unwrap();
        let original = state.clone();
        let mut events = Vec::new();
        let mut decisions = Vec::new();
        let mut resolved_keys = Vec::new();
        for ((record_id, _), turn, terminal) in &resolved {
            let Some(before) = state.agents.get(record_id).cloned() else {
                if state.codex_predecessor_record(record_id).is_none() {
                    resolved_keys.push((record_id.clone(), turn.agent_id.clone()));
                }
                continue;
            };
            resolved_keys.push((record_id.clone(), turn.agent_id.clone()));
            if before.adapter_id != "codex"
                || before.codex_running_subagents.get(&turn.agent_id) != Some(&turn.turn_id)
            {
                continue;
            }
            let blocked = before.lifecycle == v1::AgentLifecycleState::Blocked as i32;
            let mut children = before.codex_running_subagents.clone();
            let mut child_roots = before.codex_subagent_root_turn_ids.clone();
            let mut awaiting_root = before.codex_subagents_awaiting_root.clone();
            children.remove(&turn.agent_id);
            child_roots.remove(&turn.agent_id);
            awaiting_root.remove(&turn.agent_id);
            let has_children = !children.is_empty() || before.codex_subagent_capacity_exceeded;
            let completed = !blocked && !has_children && before.codex_parent_stopped_for_subagents;
            let lifecycle = if blocked {
                v1::AgentLifecycleState::Blocked
            } else if completed {
                v1::AgentLifecycleState::Idle
            } else {
                v1::AgentLifecycleState::try_from(before.lifecycle).unwrap_or_default()
            };

            state.generation = state.generation.saturating_add(1);
            let generation = state.generation;
            let record = state.agents.get_mut(record_id).unwrap();
            record.codex_running_subagents = children;
            record.codex_subagent_root_turn_ids = child_roots;
            record.codex_subagents_awaiting_root = awaiting_root;
            ingest::remember_codex_terminal_child_turn(
                &mut record.codex_terminal_subagent_turns,
                turn,
            );
            record.codex_parent_stopped_for_subagents = if completed {
                false
            } else {
                record.codex_parent_stopped_for_subagents
            };
            record.hook_terminal = completed;
            if completed && !record.codex_active_root_turn_id.is_empty() {
                let active_root_turn_id = record.codex_active_root_turn_id.clone();
                remember_terminal_root_turn(
                    &mut record.codex_terminal_root_turn_ids,
                    &active_root_turn_id,
                );
            }
            record.lifecycle = lifecycle as i32;
            record.lifecycle_observed_at_unix_millis = now;
            record.updated_at_unix_millis = now;
            record.state_generation = generation;
            record.subagent_evidence_observed_at_unix_millis = if has_children { now } else { 0 };
            if completed {
                record.lifecycle_changed_at_unix_millis = now;
                record.attention_generation = record.attention_generation.saturating_add(1);
                record.attention_kind = "completed".into();
                record.attention_seen_at_unix_millis = 0;
            }
            let reason = match terminal {
                crate::hook::codex_transcript::TurnTerminal::Completed => "child_turn_completed",
                crate::hook::codex_transcript::TurnTerminal::Aborted => "child_turn_aborted",
            };
            decisions.push((
                record.agent_id.clone(),
                record.route.pane_id.clone(),
                !record.native_session_id.is_empty(),
                lifecycle_label(before.lifecycle),
                lifecycle_label(record.lifecycle),
                reason,
                record.codex_running_subagents.len(),
            ));
            events.push(v1::AgentEvent {
                agent: Some(snapshot::record(record)),
                generation,
                notify: completed,
                reason: reason.into(),
                retired_agent_ids: Vec::new(),
            });
        }
        if !events.is_empty() && self.persist_locked(&state).is_err() {
            *state = original;
            return Vec::new();
        }
        for (agent_id, pane_id, has_session_id, before, after, reason, active_children) in
            &decisions
        {
            crate::diagnostics::record_lifecycle_decision(crate::diagnostics::LifecycleDecision {
                input_origin: "unknown",
                pane_id,
                agent_id,
                event: "TranscriptChildTerminal",
                child: true,
                has_session_id: *has_session_id,
                has_turn_id: true,
                session_relation: "current",
                turn_relation: "current",
                authority: "accepted",
                lifecycle_before: before,
                lifecycle_after: after,
                reason,
                active_children: *active_children,
            });
        }
        drop(state);
        let mut monitors = self.codex_child_monitors.lock().unwrap();
        for key in resolved_keys {
            monitors.remove(&key);
        }
        events
    }

    /// Resolve confirmed manual permission turns that Codex terminated without
    /// a lifecycle hook. Codex 0.153.4 appends an exact-turn `turn_aborted` on
    /// Escape and `task_complete` on completion, but emits neither PostToolUse
    /// nor Stop for the cancelled dialog. The open transcript handle is
    /// memory-only; maintenance reads only after its length changes.
    fn sweep_codex_permission_terminals(&self) -> Vec<v1::AgentEvent> {
        // The ordinary auto-review/yolo path pays one uncontended mutex check,
        // then no state clone, filesystem metadata call or transcript read.
        if self.pending_codex_permissions.lock().unwrap().is_empty() {
            return Vec::new();
        }
        let now = now_millis();
        let current: BTreeMap<String, StoredAgent> = {
            let state = self.state.lock().unwrap();
            state
                .agents
                .iter()
                .map(|(id, record)| (id.clone(), record.clone()))
                .chain(
                    state
                        .codex_predecessors
                        .values()
                        .map(|record| (record.agent_id.clone(), record.clone())),
                )
                .collect()
        };
        let mut pending = self.pending_codex_permissions.lock().unwrap();
        pending.retain(|key, value| {
            now.saturating_sub(value.observed_at_unix_millis) <= PENDING_CODEX_PERMISSION_TTL_MILLIS
                && current.get(&key.record_id).is_some_and(|record| {
                    let exact_child_is_current = key.turn.agent_id.is_empty()
                        || record.codex_running_subagents.get(&key.turn.agent_id)
                            == Some(&key.turn.turn_id);
                    if value.root_turn_id.is_empty()
                        && exact_child_is_current
                        && !key.turn.agent_id.is_empty()
                        && record.codex_subagent_root_turn_ids.get(&key.turn.agent_id)
                            == Some(&record.codex_active_root_turn_id)
                    {
                        value
                            .root_turn_id
                            .clone_from(&record.codex_active_root_turn_id);
                    }
                    let root_is_current = if value.root_turn_id.is_empty() {
                        !key.turn.agent_id.is_empty()
                            && record
                                .codex_subagents_awaiting_root
                                .contains(&key.turn.agent_id)
                    } else {
                        record.codex_active_root_turn_id == value.root_turn_id
                    };
                    record.lifecycle == v1::AgentLifecycleState::Blocked as i32
                        && record.lifecycle_changed_at_unix_millis
                            == value.lifecycle_changed_at_unix_millis
                        && exact_child_is_current
                        && root_is_current
                })
        });
        let resolved: Vec<_> = pending
            .iter_mut()
            .filter_map(|(key, value)| {
                if value.root_turn_id.is_empty() {
                    return None;
                }
                if value.terminal.is_none() {
                    value.terminal = value.monitor.poll_terminal();
                }
                value.terminal.map(|terminal| {
                    (
                        key.clone(),
                        terminal,
                        value.lifecycle_changed_at_unix_millis,
                        value.root_turn_id.clone(),
                    )
                })
            })
            .collect();
        drop(pending);
        if resolved.is_empty() {
            return Vec::new();
        }

        let mut state = self.state.lock().unwrap();
        let original = state.clone();
        let mut events = Vec::new();
        let mut diagnostic_scopes = Vec::new();
        let mut resolved_keys = Vec::new();
        for (key, terminal, expected_changed_at, expected_root_turn_id) in resolved {
            let Some(current) = state.agents.get(&key.record_id) else {
                if state.codex_predecessor_record(&key.record_id).is_none() {
                    resolved_keys.push(key);
                }
                continue;
            };
            resolved_keys.push(key.clone());
            if current.lifecycle != v1::AgentLifecycleState::Blocked as i32
                || current.lifecycle_changed_at_unix_millis != expected_changed_at
                || current.codex_active_root_turn_id != expected_root_turn_id
                || !key.turn.agent_id.is_empty()
                    && current.codex_running_subagents.get(&key.turn.agent_id)
                        != Some(&key.turn.turn_id)
            {
                continue;
            }
            state.generation = state.generation.saturating_add(1);
            let generation = state.generation;
            let record = state.agents.get_mut(&key.record_id).unwrap();
            if !key.turn.agent_id.is_empty()
                && record.codex_running_subagents.get(&key.turn.agent_id) == Some(&key.turn.turn_id)
            {
                record.codex_running_subagents.remove(&key.turn.agent_id);
                record
                    .codex_subagent_root_turn_ids
                    .remove(&key.turn.agent_id);
                record
                    .codex_subagents_awaiting_root
                    .remove(&key.turn.agent_id);
            }
            if !key.turn.agent_id.is_empty() {
                ingest::remember_codex_terminal_child_turn(
                    &mut record.codex_terminal_subagent_turns,
                    &key.turn,
                );
            }
            let idle = record.codex_running_subagents.is_empty()
                && !record.codex_subagent_capacity_exceeded
                && (key.turn.agent_id.is_empty() || record.codex_parent_stopped_for_subagents);
            if key.turn.agent_id.is_empty() && !idle {
                record.codex_parent_stopped_for_subagents = true;
            }
            record.lifecycle = if idle {
                v1::AgentLifecycleState::Idle as i32
            } else {
                v1::AgentLifecycleState::Working as i32
            };
            record.hook_terminal = idle;
            if key.turn.agent_id.is_empty() {
                remember_terminal_root_turn(
                    &mut record.codex_terminal_root_turn_ids,
                    &key.turn.turn_id,
                );
            }
            if idle {
                record.codex_parent_stopped_for_subagents = false;
                if !key.turn.agent_id.is_empty() && !record.codex_active_root_turn_id.is_empty() {
                    let active_root_turn_id = record.codex_active_root_turn_id.clone();
                    remember_terminal_root_turn(
                        &mut record.codex_terminal_root_turn_ids,
                        &active_root_turn_id,
                    );
                }
            }
            record
                .codex_turn_reviews
                .retain(|cached| cached.turn != key.turn);
            record.lifecycle_observed_at_unix_millis = now;
            record.lifecycle_changed_at_unix_millis = now;
            record.updated_at_unix_millis = now;
            record.state_generation = generation;
            diagnostic_scopes.push((
                !key.turn.agent_id.is_empty(),
                record.codex_running_subagents.len(),
            ));
            events.push(v1::AgentEvent {
                agent: Some(snapshot::record(record)),
                generation,
                notify: false,
                reason: match terminal {
                    crate::hook::codex_transcript::TurnTerminal::Completed => {
                        "permission_turn_completed"
                    }
                    crate::hook::codex_transcript::TurnTerminal::Aborted => {
                        "permission_turn_aborted"
                    }
                }
                .into(),
                retired_agent_ids: Vec::new(),
            });
        }
        if events.is_empty() {
            let mut pending = self.pending_codex_permissions.lock().unwrap();
            for key in resolved_keys {
                pending.remove(&key);
            }
            return events;
        }
        if self.persist_locked(&state).is_err() {
            *state = original;
            return Vec::new();
        }
        for (event, (child, active_children)) in events.iter().zip(diagnostic_scopes) {
            let Some(record) = event.agent.as_ref() else {
                continue;
            };
            let Some(route) = record.route.as_ref() else {
                continue;
            };
            crate::diagnostics::record_lifecycle_decision(crate::diagnostics::LifecycleDecision {
                input_origin: "unknown",
                pane_id: &route.pane_id,
                agent_id: &record.agent_id,
                event: "TranscriptTerminal",
                child,
                has_session_id: !record.native_session_id.is_empty(),
                has_turn_id: true,
                session_relation: "current",
                turn_relation: "current",
                authority: "accepted",
                lifecycle_before: "blocked",
                lifecycle_after: lifecycle_label(record.lifecycle),
                reason: &event.reason,
                active_children,
            });
        }
        let mut pending = self.pending_codex_permissions.lock().unwrap();
        for key in resolved_keys {
            pending.remove(&key);
        }
        events
    }

    /// Withdraw Working from agents that have gone silent past
    /// [`STALE_WORKING_TTL_MILLIS`], returning the events to publish.
    ///
    /// Only the lifecycle is touched. Attention and what was already seen both
    /// survive: an agent that went quiet after asking for a human is still
    /// asking for a human, and forgetting that would be a second lie in place
    /// of the first. Nothing outside the record is touched either — no signal
    /// reaches the agent's process, and `notify` stays false, so a healthy
    /// agent that merely ran one long silent tool call loses a label and
    /// regains it on its next hook. A retained vendor subagent guard is direct
    /// evidence of live work and is exempt until its matching terminal event.
    pub(crate) fn sweep_stale(&self) -> Vec<v1::AgentEvent> {
        let now = now_millis();
        let monitored_codex: BTreeSet<String> = self
            .codex_child_monitors
            .lock()
            .unwrap()
            .keys()
            .map(|(record_id, _)| record_id.clone())
            .collect();
        let mut state = self.state.lock().unwrap();
        let stale: Vec<String> = state
            .agents
            .values()
            // The clock is the lifecycle observation, not `updated_at`, which
            // reconciliation also moves when the agent merely changes pane.
            .filter(|record| {
                let lifecycle_observed = match record.lifecycle_observed_at_unix_millis {
                    0 => record.updated_at_unix_millis,
                    value => value,
                };
                let subagent_evidence = record.claude_has_running_subagent
                    || !record.codex_running_subagents.is_empty()
                    || record.codex_subagent_capacity_exceeded;
                let (observed, ttl) = if subagent_evidence {
                    (
                        record.subagent_evidence_observed_at_unix_millis,
                        STALE_SUBAGENT_WORKING_TTL_MILLIS,
                    )
                } else {
                    (lifecycle_observed, STALE_WORKING_TTL_MILLIS)
                };
                record.lifecycle == v1::AgentLifecycleState::Working as i32
                    && !monitored_codex.contains(&record.agent_id)
                    && now.saturating_sub(observed) > ttl
            })
            .map(|record| record.agent_id.clone())
            .collect();
        if stale.is_empty() {
            return Vec::new();
        }
        let original = state.clone();
        let mut events = Vec::new();
        for agent_id in stale {
            state.generation = state.generation.saturating_add(1);
            let generation = state.generation;
            let record = state.agents.get_mut(&agent_id).expect("collected above");
            record.lifecycle = v1::AgentLifecycleState::Unknown as i32;
            record.claude_has_running_subagent = false;
            record.codex_running_subagents.clear();
            record.codex_subagent_root_turn_ids.clear();
            record.codex_subagents_awaiting_root.clear();
            record.codex_subagent_capacity_exceeded = false;
            record.codex_parent_stopped_for_subagents = false;
            record.subagent_evidence_observed_at_unix_millis = 0;
            record.lifecycle_changed_at_unix_millis = now;
            record.state_generation = generation;
            events.push(v1::AgentEvent {
                agent: Some(snapshot::record(record)),
                generation,
                notify: false,
                reason: "stale".into(),
                retired_agent_ids: Vec::new(),
            });
        }
        if self.persist_locked(&state).is_err() {
            *state = original;
            return Vec::new();
        }
        for event in &events {
            let Some(record) = event.agent.as_ref() else {
                continue;
            };
            let Some(route) = record.route.as_ref() else {
                continue;
            };
            crate::diagnostics::record_lifecycle_decision(crate::diagnostics::LifecycleDecision {
                input_origin: "unknown",
                pane_id: &route.pane_id,
                agent_id: &record.agent_id,
                event: "WorkingDecay",
                child: false,
                has_session_id: !record.native_session_id.is_empty(),
                has_turn_id: false,
                session_relation: "current",
                turn_relation: "missing",
                authority: "accepted",
                lifecycle_before: "working",
                lifecycle_after: "unknown",
                reason: "stale_working_decay",
                active_children: 0,
            });
        }
        events
    }

    /// Retire agents whose process is no longer on the host, returning the
    /// events to publish.
    ///
    /// This is what replaces screen scraping as the way out of `Working`, and
    /// it is the only one that works for a pane nobody is looking at. It is
    /// also the only route by which a killed agent's row disappears without
    /// waiting on a tmux topology change: `reconcile_topology` runs only when
    /// the snapshot *differs*, and an agent found through the process tree
    /// (`node …/cli.js` under a shell) leaves `current_command` untouched when
    /// it exits, so its record would otherwise claim `Working` indefinitely
    /// even with a desktop connected.
    ///
    /// Deliberately conservative in three places, because a false positive
    /// deletes a live agent's row:
    ///
    /// - Records belonging to another tmux server are never judged. This
    ///   snapshot is evidence about this host and says nothing about theirs.
    /// - A record with no pane is hook-only and unmapped; there is no pane to
    ///   look for, so it survives on its hook lease exactly as reconciliation
    ///   already lets it.
    /// - A record with no adapter id cannot be matched against detection
    ///   evidence at all, so no evidence can convict it.
    ///
    /// Process absence has to repeat across several successful scans. One scan
    /// can race a process-tree transition; a later positive scan or a newer
    /// hook generation resets the count. Retirement is not final — any later
    /// hook re-creates the agent.
    fn retire_departed(&self) -> Vec<v1::AgentEvent> {
        // The tmux fork happens outside the lock. `ingest_hook` takes the same
        // mutex, and holding it across a subprocess would stall live hook
        // ingestion behind a discovery that has nothing to do with it.
        if !self.has_mapped_agents() {
            self.departure_misses.lock().unwrap().clear();
            return Vec::new();
        }
        let Ok((topology, identity)) = super::snapshot::discover_consistent() else {
            // No answer is not evidence of absence. A tmux server that is
            // briefly unreachable must not empty the agent list.
            return Vec::new();
        };
        self.retire_departed_from(&topology, &identity)
    }

    /// The decision half of [`Self::retire_departed`], separated from the tmux
    /// discovery so the conservatism guards can be tested without a live
    /// server.
    fn retire_departed_from(
        &self,
        topology: &tmux_control::TmuxSnapshot,
        identity: &str,
    ) -> Vec<v1::AgentEvent> {
        let detected = reconcile::detect_all(topology);
        let mut state = self.state.lock().unwrap();
        let candidates: Vec<(String, u64)> = state
            .agents
            .values()
            .filter(|record| {
                record.route.server_identity == identity
                    && !record.route.pane_id.is_empty()
                    && !record.adapter_id.is_empty()
                    && !detected
                        .contains_key(&(record.route.pane_id.clone(), record.adapter_id.clone()))
            })
            .map(|record| (record.agent_id.clone(), record.state_generation))
            .collect();
        let mut misses = self.departure_misses.lock().unwrap();
        misses.retain(|agent_id, _| {
            candidates
                .iter()
                .any(|(candidate, _)| candidate == agent_id)
        });
        let mut departed = Vec::new();
        for (agent_id, generation) in candidates {
            let entry = misses.entry(agent_id.clone()).or_insert((generation, 0));
            if entry.0 != generation {
                *entry = (generation, 0);
            }
            entry.1 = entry.1.saturating_add(1);
            if entry.1 >= DEPARTURE_MISSES_REQUIRED {
                departed.push(agent_id);
            }
        }
        if departed.is_empty() {
            return Vec::new();
        }
        let original = state.clone();
        let mut runtime_departed = departed.clone();
        for agent_id in &departed {
            state.agents.remove(agent_id);
            runtime_departed.extend(state.drain_codex_predecessors(agent_id));
        }
        state.unbind_agents(&departed);
        state.generation = state.generation.saturating_add(1);
        let generation = state.generation;
        if self.persist_locked(&state).is_err() {
            *state = original;
            return Vec::new();
        }
        for agent_id in &departed {
            misses.remove(agent_id);
        }
        drop(misses);
        drop(state);
        self.retire_codex_runtime_state(&runtime_departed);
        vec![v1::AgentEvent {
            agent: None,
            generation,
            notify: false,
            reason: "departed".into(),
            retired_agent_ids: departed,
        }]
    }

    /// Whether any routed row needs process-presence maintenance. Hosts with
    /// no mapped agents stay at zero periodic discovery forks.
    fn has_mapped_agents(&self) -> bool {
        self.state
            .lock()
            .unwrap()
            .agents
            .values()
            .any(|record| !record.route.pane_id.is_empty() && !record.adapter_id.is_empty())
    }

    /// Voice-origin terminal input is accepted while any supported root agent
    /// is still present on this pane. The caller holds this lock through the
    /// terminal-input fence so a confirmed departure cannot race submission.
    pub(super) fn with_valid_input_pane<T>(
        &self,
        server_identity: &str,
        pane_id: &str,
        process_present: bool,
        record_submission: bool,
        action: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let state = self.state.lock().unwrap();
        if !process_present {
            bail!("pane no longer contains a supported agent");
        }
        let result = action();
        if result.is_ok() && record_submission {
            record_input_diagnostic_locked(&state, server_identity, pane_id, "voice");
        }
        result
    }

    pub(super) fn with_input_diagnostic<T>(
        &self,
        server_identity: &str,
        pane_id: &str,
        action: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let state = self.state.lock().unwrap();
        let result = action();
        if result.is_ok() {
            record_input_diagnostic_locked(&state, server_identity, pane_id, "terminal");
        }
        result
    }

    pub(super) fn snapshot_for(&self, server_identity: &str) -> v1::AgentSnapshot {
        let state = self.state.lock().unwrap();
        // Which agents are actually running here, which is what corrects a
        // configuration probe that could not see the agent's executable.
        let running: BTreeSet<&str> = state
            .agents
            .values()
            .filter(|record| record.route.server_identity == server_identity)
            .map(|record| record.adapter_id.as_str())
            .collect();
        let wiring = self.wiring.lock().unwrap().current(&running);
        snapshot::build(&state, server_identity, &wiring)
    }

    pub(super) fn reconcile_topology(
        &self,
        topology: &tmux_control::TmuxSnapshot,
        identity: &str,
    ) -> anyhow::Result<bool> {
        let mut state = self.state.lock().unwrap();
        let original = state.clone();
        let result = reconcile::topology(&mut state, topology, identity, now_millis());
        if result.changed
            && let Err(error) = self.persist_locked(&state)
        {
            *state = original;
            return Err(error);
        }
        // Reconciliation and the maintenance loop observe the same process
        // evidence. A positive observation in either path makes earlier
        // negative maintenance scans non-consecutive.
        let detected = reconcile::detect_all(topology);
        let observed: BTreeSet<String> = state
            .agents
            .values()
            .filter(|record| {
                record.route.server_identity == identity
                    && detected
                        .contains_key(&(record.route.pane_id.clone(), record.adapter_id.clone()))
            })
            .map(|record| record.agent_id.clone())
            .collect();
        if !observed.is_empty() {
            self.departure_misses
                .lock()
                .unwrap()
                .retain(|agent_id, _| !observed.contains(agent_id));
        }
        let retired_codex_agent_ids = result.retired_codex_agent_ids.clone();
        drop(state);
        self.retire_codex_runtime_state(&retired_codex_agent_ids);
        Ok(result.changed)
    }

    pub(super) fn mark_seen(
        &self,
        agent_id: &str,
        attention_generation: u64,
    ) -> anyhow::Result<v1::AgentEvent> {
        let mut state = self.state.lock().unwrap();
        let original = state.clone();
        let current = state
            .agents
            .get(agent_id)
            .context("agent no longer exists")?;
        if attention_generation != current.attention_generation {
            bail!("attention generation is stale");
        }
        let first_seen = current.seen_generation < attention_generation;
        state.generation = state.generation.saturating_add(1);
        let generation = state.generation;
        let record = state
            .agents
            .get_mut(agent_id)
            .context("agent no longer exists")?;
        record.seen_generation = record.seen_generation.max(attention_generation);
        if first_seen {
            record.attention_seen_at_unix_millis = now_millis();
        }
        record.state_generation = generation;
        let result = snapshot::record(record);
        if let Err(error) = self.persist_locked(&state) {
            *state = original;
            return Err(error);
        }
        Ok(v1::AgentEvent {
            agent: Some(result),
            generation,
            notify: false,
            reason: "seen".into(),
            retired_agent_ids: Vec::new(),
        })
    }

    pub(super) fn rename(&self, agent_id: &str, name: &str) -> anyhow::Result<v1::AgentEvent> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 128 || name.chars().any(char::is_control) {
            bail!("agent name must be 1-128 printable characters");
        }
        let mut state = self.state.lock().unwrap();
        let original = state.clone();
        state.generation = state.generation.saturating_add(1);
        let generation = state.generation;
        let record = state
            .agents
            .get_mut(agent_id)
            .context("agent no longer exists")?;
        record.display_name = name.into();
        record.state_generation = generation;
        let result = snapshot::record(record);
        if let Err(error) = self.persist_locked(&state) {
            *state = original;
            return Err(error);
        }
        Ok(v1::AgentEvent {
            agent: Some(result),
            generation,
            notify: false,
            reason: "renamed".into(),
            retired_agent_ids: Vec::new(),
        })
    }

    fn persist_locked(&self, state: &StoredState) -> anyhow::Result<()> {
        store::persist(&self.state_path, state)
    }
}

pub(super) fn pane_has_supported_process(pane: &tmux_control::Pane) -> bool {
    process::detect_live(pane).is_some()
}

fn record_input_diagnostic_locked(
    state: &StoredState,
    server_identity: &str,
    pane_id: &str,
    origin: &str,
) {
    let Some(record) = state.agents.values().find(|record| {
        record.present
            && record.route.server_identity == server_identity
            && record.route.pane_id == pane_id
    }) else {
        return;
    };
    let lifecycle = lifecycle_label(record.lifecycle);
    crate::diagnostics::record_lifecycle_decision(crate::diagnostics::LifecycleDecision {
        input_origin: origin,
        pane_id,
        agent_id: &record.agent_id,
        event: "InputSubmit",
        child: false,
        has_session_id: !record.native_session_id.is_empty(),
        has_turn_id: !record.codex_active_root_turn_id.is_empty(),
        session_relation: "current",
        turn_relation: if record.codex_active_root_turn_id.is_empty() {
            "missing"
        } else {
            "current"
        },
        authority: "accepted",
        lifecycle_before: lifecycle,
        lifecycle_after: lifecycle,
        reason: "input_accepted",
        active_children: record.codex_running_subagents.len(),
    });
}

pub(crate) fn publish(event: v1::AgentEvent) {
    broadcast_control_event(v1::HostEvent {
        kind: v1::EventKind::AgentState.into(),
        scope: event
            .agent
            .as_ref()
            .map(|agent| agent.agent_id.clone())
            .unwrap_or_default(),
        agent: Some(event),
        ..Default::default()
    });
}

pub(crate) fn ingest_fallbacks() -> anyhow::Result<usize> {
    fallback::ingest()
}

/// The daemon's periodic honesty pass: retire agents whose process is gone,
/// then withdraw Working from whatever is left that has gone silent.
///
/// This is the *only* caller of either. It runs on the daemon's own timer, not
/// on a subscriber's wakeup, because both failures are properties of the host
/// and neither stops happening when nobody is looking. The previous callers
/// both sat behind a connected desktop — the topology actor's loop after
/// `if !self.subscribed { continue; }`, and the snapshot request itself — so a
/// daemon left alone accrued Working states that nothing could ever withdraw.
///
/// Retirement runs first. Removing a record makes any staleness question about
/// it moot, and the reverse order would publish a withdrawal for an agent that
/// is about to disappear in the same pass.
pub(crate) fn maintain() {
    let runtime = AgentRuntime::global();
    {
        // A transcript observation and a live hook update the same lifecycle
        // record. Hold the hook ingest fence through publication so an older
        // transcript edge can neither overwrite nor publish after a newer
        // hook. Reads are bounded to one MiB per growing monitored file.
        let _order = runtime.ingest_order.lock().unwrap();
        for event in runtime.sweep_codex_permission_terminals() {
            publish(event);
        }
        for event in runtime.sweep_codex_child_terminals() {
            publish(event);
        }
    }
    for event in runtime.retire_departed() {
        publish(event);
    }
    for event in runtime.sweep_stale() {
        publish(event);
    }
}

fn lifecycle_label(value: i32) -> &'static str {
    match v1::AgentLifecycleState::try_from(value).unwrap_or_default() {
        v1::AgentLifecycleState::Working => "working",
        v1::AgentLifecycleState::Blocked => "blocked",
        v1::AgentLifecycleState::Idle => "idle",
        v1::AgentLifecycleState::Unknown | v1::AgentLifecycleState::Unspecified => "unknown",
    }
}

fn validate_pane_id(pane_id: &str) -> anyhow::Result<()> {
    if pane_id.strip_prefix('%').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        Ok(())
    } else {
        bail!("hook TMUX_PANE must be an exact tmux % pane ID")
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests;
