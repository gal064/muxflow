use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use tmux_agent_protocol::v1;

use super::{broadcast_control_event, snapshot::server_identity};

pub(crate) mod adapters;
mod fallback;
mod hooks;
mod identity;
mod ingest;
mod process;
mod reconcile;
mod screen;
mod snapshot;
mod store;
pub(crate) use hooks::HookManager;
pub(crate) use ingest::HookIngestFailure;
use store::{StoredAgent, StoredRoute, StoredState};

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

pub(crate) struct AgentRuntime {
    state_path: PathBuf,
    state: Mutex<StoredState>,
    screen_observer: Mutex<screen::ScreenObserver>,
    /// What this host's agent configuration was last observed to do with
    /// lifecycle events. Re-read only when a configuration file changed.
    wiring: Mutex<hooks::WiringCache>,
}

static GLOBAL: OnceLock<Arc<AgentRuntime>> = OnceLock::new();

impl AgentRuntime {
    pub(crate) fn global() -> Arc<Self> {
        Arc::clone(
            GLOBAL.get_or_init(|| {
                Arc::new(Self::load(crate::paths::runtime_dir().join("agents.json")))
            }),
        )
    }

    fn load(state_path: PathBuf) -> Self {
        let state = store::load(&state_path);
        Self {
            state_path,
            state: Mutex::new(state),
            screen_observer: Mutex::new(screen::ScreenObserver::default()),
            wiring: Mutex::new(hooks::WiringCache::default()),
        }
    }

    #[cfg(test)]
    fn isolated(state_path: PathBuf) -> Self {
        Self::load(state_path)
    }

    pub(super) fn snapshot(&self) -> v1::AgentSnapshot {
        self.snapshot_for(&server_identity())
    }

    /// Withdraw Working from agents that have gone silent past
    /// [`STALE_WORKING_TTL_MILLIS`], returning the events to publish.
    ///
    /// Only the lifecycle is touched. Attention, what was already seen, and the
    /// authority that supplied the last real evidence all survive: an agent
    /// that went quiet after asking for a human is still asking for a human,
    /// and forgetting that would be a second lie in place of the first.
    pub(crate) fn sweep_stale(&self) -> Vec<v1::AgentEvent> {
        let now = now_millis();
        let mut state = self.state.lock().unwrap();
        let stale: Vec<String> = state
            .agents
            .values()
            // Authority is deliberately not consulted. Every Working state in
            // the store came from evidence — a hook, or a screen the observer
            // confirmed — and both go silent the same way; process detection
            // only ever writes Unknown, so there is no third case to carve out.
            //
            // The clock is the lifecycle observation, not `updated_at`, which
            // reconciliation also moves when the agent merely changes pane.
            .filter(|record| {
                let observed = match record.lifecycle_observed_at_unix_millis {
                    0 => record.updated_at_unix_millis,
                    value => value,
                };
                record.lifecycle == v1::AgentLifecycleState::Working as i32
                    && now.saturating_sub(observed) > STALE_WORKING_TTL_MILLIS
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
        events
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
    ) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        let original = state.clone();
        let result = reconcile::topology(&mut state, topology, identity, now_millis());
        if result.changed
            && let Err(error) = self.persist_locked(&state)
        {
            *state = original;
            return Err(error);
        }
        drop(state);
        let live_panes: BTreeSet<_> = topology.panes.iter().map(|pane| pane.id.as_str()).collect();
        self.screen_observer
            .lock()
            .unwrap()
            .retain_panes(live_panes.iter().copied());
        Ok(())
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
        state.generation = state.generation.saturating_add(1);
        let generation = state.generation;
        let record = state
            .agents
            .get_mut(agent_id)
            .context("agent no longer exists")?;
        record.seen_generation = record.seen_generation.max(attention_generation);
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

    pub(super) fn observe_screen(
        &self,
        pane_id: &str,
        bytes: &[u8],
        reset: bool,
    ) -> anyhow::Result<()> {
        let now = now_millis();
        let (agent_id, adapter_kind) = {
            let state = self.state.lock().unwrap();
            let Some(current) = state.agents.values().find(|record| {
                record.present
                    && record.route.pane_id == pane_id
                    && adapters::adapter(
                        v1::AgentAdapterKind::try_from(record.adapter).unwrap_or_default(),
                    )
                    .is_some()
            }) else {
                return Ok(());
            };
            if current.authority == v1::AgentAuthority::Hook as i32
                && current.hook_authority_expires_at_unix_millis > now
            {
                return Ok(());
            }
            (
                current.agent_id.clone(),
                v1::AgentAdapterKind::try_from(current.adapter).unwrap_or_default(),
            )
        };
        let Some(screen) = self
            .screen_observer
            .lock()
            .unwrap()
            .observe(pane_id, bytes, reset, now)
        else {
            return Ok(());
        };
        let Some(lifecycle) =
            adapters::adapter(adapter_kind).and_then(|adapter| adapter.fallback_screen(&screen))
        else {
            return Ok(());
        };
        if !self
            .screen_observer
            .lock()
            .unwrap()
            .confirms(pane_id, lifecycle)
        {
            return Ok(());
        }
        let mut state = self.state.lock().unwrap();
        let original = state.clone();
        let Some(current) = state.agents.get(&agent_id) else {
            return Ok(());
        };
        if current.authority == v1::AgentAuthority::Hook as i32
            && current.hook_authority_expires_at_unix_millis > now
        {
            return Ok(());
        }
        if current.authority == v1::AgentAuthority::Screen as i32
            && current.lifecycle == lifecycle as i32
        {
            return Ok(());
        }
        let previous_lifecycle =
            v1::AgentLifecycleState::try_from(current.lifecycle).unwrap_or_default();
        state.generation = state.generation.saturating_add(1);
        let generation = state.generation;
        let record = state.agents.get_mut(&agent_id).unwrap();
        let notify = lifecycle == v1::AgentLifecycleState::Blocked
            && previous_lifecycle != v1::AgentLifecycleState::Blocked
            || previous_lifecycle == v1::AgentLifecycleState::Working
                && lifecycle == v1::AgentLifecycleState::Idle;
        if notify {
            record.attention_generation = record.attention_generation.saturating_add(1);
            record.attention_kind = if lifecycle == v1::AgentLifecycleState::Blocked {
                "blocked"
            } else {
                "completed"
            }
            .into();
        } else if previous_lifecycle == v1::AgentLifecycleState::Blocked
            && lifecycle == v1::AgentLifecycleState::Idle
            && record.attention_kind == "blocked"
            && record.seen_generation >= record.attention_generation
        {
            record.attention_kind.clear();
        }
        record.lifecycle = lifecycle as i32;
        record.authority = v1::AgentAuthority::Screen as i32;
        record.state_generation = generation;
        record.updated_at_unix_millis = now;
        record.lifecycle_observed_at_unix_millis = now;
        let record = snapshot::record(record);
        if let Err(error) = self.persist_locked(&state) {
            *state = original;
            return Err(error);
        }
        drop(state);
        publish(v1::AgentEvent {
            agent: Some(record),
            generation,
            notify,
            reason: if previous_lifecycle == v1::AgentLifecycleState::Working
                && lifecycle == v1::AgentLifecycleState::Idle
            {
                "completed"
            } else if lifecycle == v1::AgentLifecycleState::Blocked {
                "blocked"
            } else {
                "screen_recovery"
            }
            .into(),
            retired_agent_ids: Vec::new(),
        });
        Ok(())
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

/// Runs [`AgentRuntime::sweep_stale`] and broadcasts whatever it withdrew.
///
/// Called from the topology actor's own wakeups so a connected desktop sees a
/// silent agent stop claiming to work without asking for a new snapshot, and
/// from the snapshot request so a desktop that reconnects after the daemon sat
/// alone never receives a stale Working in the first place.
pub(crate) fn sweep_stale_and_publish() {
    for event in AgentRuntime::global().sweep_stale() {
        publish(event);
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
mod tests {
    use super::*;
    use prost::Message;
    use std::fs;

    fn runtime(name: &str) -> AgentRuntime {
        let path = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase6-agent-{name}-{}", uuid::Uuid::new_v4()))
            .join("agents.json");
        AgentRuntime::isolated(path)
    }

    fn event(id: &str, generation: u64, name: &str) -> v1::AgentHookEvent {
        v1::AgentHookEvent {
            adapter: v1::AgentAdapterKind::Codex.into(),
            source_event_id: id.into(),
            source_generation: generation,
            native_session_id: "native-1".into(),
            pane_id: "%7".into(),
            payload_json: serde_json::to_vec(&serde_json::json!({"hook_event_name": name}))
                .unwrap(),
            occurred_at_unix_millis: now_millis(),
            origin_server_identity: "server-a".into(),
            ..Default::default()
        }
    }

    fn topology(command: &str) -> tmux_control::TmuxSnapshot {
        tmux_control::TmuxSnapshot {
            sessions: vec![tmux_control::Session {
                id: "$1".into(),
                name: "workspace".into(),
                window_count: 1,
                attached_clients: 0,
                order: 0,
            }],
            windows: vec![tmux_control::Window {
                id: "@2".into(),
                session_id: "$1".into(),
                index: 0,
                name: "agent".into(),
                active: true,
                layout: String::new(),
                zoomed: false,
            }],
            panes: vec![tmux_control::Pane {
                id: "%7".into(),
                session_id: "$1".into(),
                window_id: "@2".into(),
                index: 0,
                active: true,
                width: 80,
                height: 24,
                left: 0,
                top: 0,
                current_path: "/work".into(),
                current_command: command.into(),
                pane_pid: 0,
                start_command: String::new(),
            }],
        }
    }

    #[test]
    fn hook_generations_dedupe_and_attention_are_monotonic() {
        let runtime = runtime("transitions");
        let working = runtime
            .ingest_hook(&event("e1", 1, "UserPromptSubmit"))
            .unwrap();
        assert!(!working.notify);
        let done = runtime.ingest_hook(&event("e2", 2, "Stop")).unwrap();
        assert!(done.notify);
        let record = done.agent.unwrap();
        assert_eq!(record.attention_generation, 1);
        assert_eq!(record.attention_kind, "completed");
        assert_eq!(record.seen_generation, 0);
        assert!(matches!(
            runtime.ingest_hook(&event("e2", 2, "Stop")),
            Err(HookIngestFailure::Duplicate)
        ));
        assert!(
            runtime
                .ingest_hook(&event("older-sequence", 1, "Stop"))
                .is_ok()
        );
        let seen = runtime.mark_seen(&record.agent_id, 1).unwrap();
        assert_eq!(seen.reason, "seen");
        assert!(!seen.notify);
        assert!(seen.generation > done.generation);
        let snapshot = runtime.snapshot();
        assert!(snapshot.authoritative);
        assert_eq!(snapshot.notification_watermark, snapshot.generation);
        assert_eq!(snapshot.agents[0].seen_generation, 1);
    }

    #[test]
    fn rename_returns_a_canonical_published_event_generation() {
        let runtime = runtime("rename-event");
        let topology_snapshot = topology("codex");
        runtime
            .reconcile_topology(&topology_snapshot, "server-a")
            .unwrap();
        let before = runtime.snapshot_for("server-a");
        let renamed = runtime
            .rename(&before.agents[0].agent_id, "Build agent")
            .unwrap();
        assert_eq!(renamed.reason, "renamed");
        assert!(!renamed.notify);
        assert!(renamed.generation > before.generation);
        assert_eq!(renamed.agent.unwrap().display_name, "Build agent");
    }

    #[test]
    fn persist_failure_rolls_back_runtime_mutations() {
        let seeded = runtime("transaction-seed");
        let topology_snapshot = topology("codex");
        seeded
            .reconcile_topology(&topology_snapshot, "server-a")
            .unwrap();
        let baseline_state = seeded.state.lock().unwrap().clone();
        let blocker = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase6-state-blocker-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(blocker.parent().unwrap()).unwrap();
        fs::write(&blocker, b"not a directory").unwrap();
        let failing = || AgentRuntime {
            state_path: blocker.join("agents.json"),
            state: Mutex::new(baseline_state.clone()),
            screen_observer: Mutex::new(screen::ScreenObserver::default()),
            wiring: Mutex::new(hooks::WiringCache::default()),
        };
        let agent_id = baseline_state.agents.keys().next().unwrap().clone();

        let runtime = failing();
        assert!(runtime.rename(&agent_id, "must rollback").is_err());
        assert_eq!(
            runtime.state.lock().unwrap().generation,
            baseline_state.generation
        );

        let runtime = failing();
        assert!(runtime.mark_seen(&agent_id, 0).is_err());
        assert_eq!(
            runtime.state.lock().unwrap().generation,
            baseline_state.generation
        );

        let runtime = failing();
        assert!(
            runtime
                .reconcile_topology(&topology("claude"), "server-a")
                .is_err()
        );
        let state = runtime.state.lock().unwrap();
        assert_eq!(state.generation, baseline_state.generation);
        assert_eq!(
            state.agents.keys().collect::<Vec<_>>(),
            baseline_state.agents.keys().collect::<Vec<_>>()
        );
        drop(state);

        let runtime = failing();
        assert!(matches!(
            runtime.ingest_hook_with_context(
                &event("transaction-hook", 0, "PermissionRequest"),
                "server-a",
                Some(&topology_snapshot),
            ),
            Err(HookIngestFailure::Retryable(_))
        ));
        assert_eq!(
            runtime.state.lock().unwrap().generation,
            baseline_state.generation
        );
    }

    #[test]
    fn snapshot_exposes_raw_records_for_frontend_rollups() {
        let runtime = runtime("raw-records");
        let mut topology = topology("codex");
        let mut second_pane = topology.panes[0].clone();
        second_pane.id = "%8".into();
        topology.panes.push(second_pane);
        runtime.reconcile_topology(&topology, "server-a").unwrap();
        runtime
            .ingest_hook_with_context(
                &event("e1", 1, "UserPromptSubmit"),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        let mut second = event("e2", 1, "PermissionRequest");
        second.native_session_id = "native-2".into();
        second.pane_id = "%8".into();
        runtime
            .ingest_hook_with_context(&second, "server-a", Some(&topology))
            .unwrap();
        let snapshot = runtime.snapshot_for("server-a");
        assert_eq!(snapshot.agents.len(), 2);
        assert!(
            snapshot
                .agents
                .iter()
                .any(|agent| { agent.lifecycle == v1::AgentLifecycleState::Blocked as i32 })
        );
        assert!(
            snapshot
                .agents
                .iter()
                .any(|agent| { agent.lifecycle == v1::AgentLifecycleState::Working as i32 })
        );
    }

    #[test]
    fn unexpired_hook_authority_survives_process_reconciliation() {
        let runtime = runtime("authority");
        let topology = topology("codex");
        runtime
            .ingest_hook_with_context(
                &event("e1", 1, "PermissionRequest"),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        runtime.reconcile_topology(&topology, "server-a").unwrap();
        let snapshot = runtime.snapshot_for("server-a");
        assert_eq!(snapshot.agents.len(), 1);
        let record = &snapshot.agents[0];
        assert_eq!(record.authority, v1::AgentAuthority::Hook as i32);
        assert_eq!(record.lifecycle, v1::AgentLifecycleState::Blocked as i32);
    }

    #[test]
    fn screen_fallback_classifies_but_cannot_override_current_hook() {
        let runtime = runtime("screen-authority");
        runtime
            .ingest_hook(&event("e1", 1, "UserPromptSubmit"))
            .unwrap();
        runtime
            .observe_screen("%7", b"Permission required: Allow command?", true)
            .unwrap();
        let current = &runtime.snapshot().agents[0];
        assert_eq!(current.authority, v1::AgentAuthority::Hook as i32);
        assert_eq!(current.lifecycle, v1::AgentLifecycleState::Working as i32);
    }

    #[test]
    fn reconciliation_retires_process_exit_pane_close_and_adapter_replacement() {
        let runtime = runtime("retire");
        runtime
            .reconcile_topology(&topology("codex"), "server-a")
            .unwrap();
        let codex_id = runtime.snapshot_for("server-a").agents[0].agent_id.clone();

        runtime
            .reconcile_topology(&topology("claude"), "server-a")
            .unwrap();
        let replaced = runtime.snapshot_for("server-a");
        assert_eq!(replaced.agents.len(), 1);
        assert_ne!(replaced.agents[0].agent_id, codex_id);
        assert_eq!(
            replaced.agents[0].adapter,
            v1::AgentAdapterKind::ClaudeCode as i32
        );

        runtime
            .reconcile_topology(&topology("bash"), "server-a")
            .unwrap();
        assert!(runtime.snapshot_for("server-a").agents.is_empty());

        runtime
            .reconcile_topology(&topology("codex"), "server-a")
            .unwrap();
        runtime
            .reconcile_topology(&tmux_control::TmuxSnapshot::default(), "server-a")
            .unwrap();
        assert!(runtime.snapshot_for("server-a").agents.is_empty());
    }

    #[test]
    fn server_replacement_filters_and_retires_foreign_records() {
        let runtime = runtime("server-replace");
        runtime
            .reconcile_topology(&topology("codex"), "server-a")
            .unwrap();
        assert_eq!(runtime.snapshot_for("server-a").agents.len(), 1);
        assert!(runtime.snapshot_for("server-b").agents.is_empty());
        runtime
            .reconcile_topology(&topology("codex"), "server-b")
            .unwrap();
        assert!(runtime.snapshot_for("server-a").agents.is_empty());
        assert_eq!(runtime.snapshot_for("server-b").agents.len(), 1);
    }

    #[test]
    fn hook_atomically_promotes_manual_pane_identity_without_duplicates() {
        let runtime = runtime("promotion");
        let topology = topology("codex");
        runtime.reconcile_topology(&topology, "server-a").unwrap();
        let manual_id = runtime.snapshot_for("server-a").agents[0].agent_id.clone();
        let promoted = runtime
            .ingest_hook_with_context(
                &event("hook-1", 0, "UserPromptSubmit"),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        let snapshot = runtime.snapshot_for("server-a");
        assert_eq!(snapshot.agents.len(), 1);
        assert_eq!(snapshot.agents[0].native_session_id, "native-1");
        assert_ne!(snapshot.agents[0].agent_id, manual_id);
        assert_eq!(promoted.retired_agent_ids, [manual_id]);
        assert_eq!(snapshot.agents[0].route.as_ref().unwrap().pane_id, "%7");
    }

    #[test]
    fn foreign_hook_is_visible_but_never_invents_a_destination() {
        let runtime = runtime("foreign-route");
        let mut foreign = event("foreign", 0, "PermissionRequest");
        foreign.pane_id = "%99".into();
        runtime
            .ingest_hook_with_context(&foreign, "server-a", Some(&topology("codex")))
            .unwrap();
        let snapshot = runtime.snapshot_for("server-a");
        let route = snapshot.agents[0].route.as_ref().unwrap().clone();
        assert!(route.pane_id.is_empty());
        assert!(route.session_id.is_empty());
        assert!(route.window_id.is_empty());
    }

    #[test]
    fn same_numbered_pane_from_another_server_remains_unmapped() {
        let runtime = runtime("foreign-server-collision");
        let topology = topology("codex");
        runtime.reconcile_topology(&topology, "server-a").unwrap();
        let direct_before = runtime.snapshot_for("server-a").agents[0].clone();
        let mut foreign = event("foreign-collision", 0, "UserPromptSubmit");
        foreign.origin_server_identity = "server-b".into();
        foreign.native_session_id = direct_before.native_session_id.clone();
        runtime
            .ingest_hook_with_context(&foreign, "server-a", Some(&topology))
            .unwrap();
        assert!(
            runtime
                .ingest_hook_with_context(&foreign, "server-a", Some(&topology))
                .is_err(),
            "foreign exact-ID continuity must retain source-event dedupe"
        );
        let mut completed = foreign.clone();
        completed.source_event_id = "foreign-completed".into();
        completed.payload_json =
            serde_json::to_vec(&serde_json::json!({"hook_event_name": "Stop"})).unwrap();
        runtime
            .ingest_hook_with_context(&completed, "server-a", Some(&topology))
            .unwrap();
        let snapshot = runtime.snapshot_for("server-a");
        let direct_after = snapshot
            .agents
            .iter()
            .find(|agent| agent.agent_id == direct_before.agent_id)
            .unwrap();
        assert_eq!(direct_after, &direct_before);
        let unmapped = snapshot
            .agents
            .iter()
            .find(|agent| agent.agent_id != direct_before.agent_id)
            .unwrap();
        let route = unmapped.route.as_ref().unwrap();
        assert!(
            route.pane_id.is_empty() && route.session_id.is_empty() && route.window_id.is_empty()
        );
        assert_eq!(unmapped.lifecycle, v1::AgentLifecycleState::Idle as i32);
        assert_eq!(unmapped.attention_kind, "completed");
        assert_eq!(unmapped.attention_generation, 1);
    }

    #[test]
    fn missing_same_server_pane_cannot_replace_a_mapped_native_agent() {
        let runtime = runtime("same-server-missing-pane");
        let topology = topology("codex");
        runtime.reconcile_topology(&topology, "server-a").unwrap();
        runtime
            .ingest_hook_with_context(
                &event("mapped-working", 0, "UserPromptSubmit"),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        let direct_before = runtime.snapshot_for("server-a").agents[0].clone();

        let mut stale = event("stale-working", 0, "UserPromptSubmit");
        stale.pane_id = "%99".into();
        stale.native_session_id = direct_before.native_session_id.clone();
        runtime
            .ingest_hook_with_context(&stale, "server-a", Some(&topology))
            .unwrap();
        assert!(
            runtime
                .ingest_hook_with_context(&stale, "server-a", Some(&topology))
                .is_err(),
            "same-server unmapped continuity must retain source-event dedupe"
        );
        let mut completed = stale.clone();
        completed.source_event_id = "stale-completed".into();
        completed.payload_json =
            serde_json::to_vec(&serde_json::json!({"hook_event_name": "Stop"})).unwrap();
        runtime
            .ingest_hook_with_context(&completed, "server-a", Some(&topology))
            .unwrap();

        let snapshot = runtime.snapshot_for("server-a");
        let direct_after = snapshot
            .agents
            .iter()
            .find(|agent| agent.agent_id == direct_before.agent_id)
            .unwrap();
        assert_eq!(direct_after, &direct_before);
        let unmapped = snapshot
            .agents
            .iter()
            .find(|agent| agent.agent_id != direct_before.agent_id)
            .unwrap();
        let route = unmapped.route.as_ref().unwrap();
        assert!(
            route.pane_id.is_empty() && route.session_id.is_empty() && route.window_id.is_empty()
        );
        assert_eq!(unmapped.native_session_id, "native-1");
        assert_eq!(unmapped.lifecycle, v1::AgentLifecycleState::Idle as i32);
        assert_eq!(unmapped.attention_kind, "completed");
        assert_eq!(unmapped.attention_generation, 1);
    }

    #[test]
    fn blocked_to_idle_never_becomes_a_completed_attention() {
        let unseen = runtime("blocked-idle-unseen");
        unseen
            .ingest_hook_with_context(
                &event("blocked", 0, "PermissionRequest"),
                "server-a",
                Some(&topology("codex")),
            )
            .unwrap();
        let idle = unseen
            .ingest_hook_with_context(
                &event("idle", 0, "Stop"),
                "server-a",
                Some(&topology("codex")),
            )
            .unwrap();
        assert!(!idle.notify);
        let idle = idle.agent.unwrap();
        assert_eq!(idle.lifecycle, v1::AgentLifecycleState::Idle as i32);
        assert_eq!(idle.attention_kind, "blocked");
        assert_ne!(idle.attention_kind, "completed");

        let seen = runtime("blocked-idle-seen");
        let blocked = seen
            .ingest_hook_with_context(
                &event("blocked-seen", 0, "PermissionRequest"),
                "server-a",
                Some(&topology("codex")),
            )
            .unwrap()
            .agent
            .unwrap();
        seen.mark_seen(&blocked.agent_id, blocked.attention_generation)
            .unwrap();
        let idle = seen
            .ingest_hook_with_context(
                &event("idle-seen", 0, "Stop"),
                "server-a",
                Some(&topology("codex")),
            )
            .unwrap();
        assert!(!idle.notify);
        assert_eq!(idle.agent.unwrap().attention_kind, "");
    }

    #[test]
    fn concurrent_same_millisecond_hooks_do_not_use_wall_clock_ordering() {
        let runtime = Arc::new(runtime("concurrent"));
        let topology = Arc::new(topology("codex"));
        runtime.reconcile_topology(&topology, "server-a").unwrap();
        let occurred = now_millis();
        let handles: Vec<_> = ["concurrent-a", "concurrent-b"]
            .into_iter()
            .map(|id| {
                let runtime = Arc::clone(&runtime);
                let topology = Arc::clone(&topology);
                std::thread::spawn(move || {
                    let mut event = event(id, 0, "UserPromptSubmit");
                    event.occurred_at_unix_millis = occurred;
                    runtime.ingest_hook_with_context(&event, "server-a", Some(&topology))
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        let snapshot = runtime.snapshot_for("server-a");
        assert_eq!(snapshot.agents.len(), 1);
        assert!(snapshot.generation >= 3);
    }

    #[test]
    fn authoritative_vendor_sequence_rejects_replay_but_unsequenced_hooks_remain_concurrent() {
        let runtime = runtime("vendor-sequence");
        let topology = topology("codex");
        let mut sequenced = event("vendor-10", 10, "UserPromptSubmit");
        sequenced.source_sequence_authoritative = true;
        runtime
            .ingest_hook_with_context(&sequenced, "server-a", Some(&topology))
            .unwrap();
        let mut older = event("vendor-9", 9, "PermissionRequest");
        older.source_sequence_authoritative = true;
        assert!(
            runtime
                .ingest_hook_with_context(&older, "server-a", Some(&topology))
                .is_err()
        );
        let unsequenced = event("parallel-no-sequence", 0, "PermissionRequest");
        assert!(
            runtime
                .ingest_hook_with_context(&unsequenced, "server-a", Some(&topology))
                .is_ok()
        );
    }

    #[test]
    fn unsequenced_late_tool_events_cannot_regress_a_completed_phase() {
        let runtime = runtime("phase-reducer");
        let topology = topology("codex");
        for (id, name) in [
            ("prompt", "UserPromptSubmit"),
            ("stop", "Stop"),
            ("late-permission", "PermissionRequest"),
            ("late-post", "PostToolUse"),
        ] {
            runtime
                .ingest_hook_with_context(&event(id, 0, name), "server-a", Some(&topology))
                .unwrap();
        }
        let completed = &runtime.snapshot_for("server-a").agents[0];
        assert_eq!(completed.lifecycle, v1::AgentLifecycleState::Idle as i32);
        assert_eq!(completed.attention_generation, 1);

        runtime
            .ingest_hook_with_context(
                &event("next-prompt", 0, "UserPromptSubmit"),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        assert_eq!(
            runtime.snapshot_for("server-a").agents[0].lifecycle,
            v1::AgentLifecycleState::Working as i32
        );
    }

    #[test]
    fn completed_phase_survives_runtime_reload_before_late_fallback() {
        let path = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase6-agent-reload-{}", uuid::Uuid::new_v4()))
            .join("agents.json");
        let topology = topology("codex");
        {
            let runtime = AgentRuntime::isolated(path.clone());
            runtime
                .ingest_hook_with_context(
                    &event("prompt", 0, "UserPromptSubmit"),
                    "server-a",
                    Some(&topology),
                )
                .unwrap();
            runtime
                .ingest_hook_with_context(&event("stop", 0, "Stop"), "server-a", Some(&topology))
                .unwrap();
        }
        let runtime = AgentRuntime::isolated(path);
        runtime
            .ingest_hook_with_context(&event("late", 0, "PreToolUse"), "server-a", Some(&topology))
            .unwrap();
        assert_eq!(
            runtime.snapshot_for("server-a").agents[0].lifecycle,
            v1::AgentLifecycleState::Idle as i32
        );
    }

    #[test]
    fn hook_expiry_retires_an_unmapped_record_without_touching_direct_detection() {
        let runtime = runtime("expired-evidence");
        let topology = topology("codex");
        runtime.reconcile_topology(&topology, "server-a").unwrap();
        let mut unmapped = event("unmapped-expiry", 0, "PermissionRequest");
        unmapped.native_session_id = "unmapped-session".into();
        unmapped.pane_id = "%101".into();
        runtime
            .ingest_hook_with_context(&unmapped, "server-a", Some(&topology))
            .unwrap();
        assert_eq!(runtime.snapshot_for("server-a").agents.len(), 2);
        {
            let mut state = runtime.state.lock().unwrap();
            state.agents.values_mut().for_each(|record| {
                if record.route.pane_id.is_empty() {
                    record.hook_authority_expires_at_unix_millis = 1;
                }
            });
        }
        runtime.reconcile_topology(&topology, "server-a").unwrap();
        let snapshot = runtime.snapshot_for("server-a");
        assert_eq!(snapshot.agents.len(), 1);
        assert_eq!(snapshot.agents[0].route.as_ref().unwrap().pane_id, "%7");
    }

    /// The transition sequence the whole phase is judged on, driven only by
    /// hook events, with the manual detection that precedes them.
    #[test]
    fn manual_detection_is_unknown_and_only_hooks_move_an_agent_through_its_turn() {
        let runtime = runtime("honest-lifecycle");
        let topology = topology("codex");
        runtime.reconcile_topology(&topology, "server-a").unwrap();
        let detected = &runtime.snapshot_for("server-a").agents[0];
        assert_eq!(
            detected.lifecycle,
            v1::AgentLifecycleState::Unknown as i32,
            "a running process proves presence, never activity"
        );
        assert!(detected.detected_manually);
        assert_eq!(detected.attention_generation, 0);

        let hook = |id: &str, name: &str| {
            runtime
                .ingest_hook_with_context(&event(id, 0, name), "server-a", Some(&topology))
                .unwrap()
                .agent
                .unwrap()
        };
        assert_eq!(
            hook("prompt", "UserPromptSubmit").lifecycle,
            v1::AgentLifecycleState::Working as i32
        );
        let blocked = hook("permission", "PermissionRequest");
        assert_eq!(blocked.lifecycle, v1::AgentLifecycleState::Blocked as i32);
        assert_eq!(blocked.attention_kind, "blocked");
        assert!(blocked.attention_generation > blocked.seen_generation);
        runtime
            .mark_seen(&blocked.agent_id, blocked.attention_generation)
            .unwrap();
        assert_eq!(
            hook("resumed", "PostToolUse").lifecycle,
            v1::AgentLifecycleState::Working as i32
        );
        let done = hook("stop", "Stop");
        assert_eq!(done.lifecycle, v1::AgentLifecycleState::Idle as i32);
        assert_eq!(done.attention_kind, "completed");
        assert!(
            done.attention_generation > done.seen_generation,
            "a finished turn stays unread until its pane is looked at"
        );
        runtime
            .mark_seen(&done.agent_id, done.attention_generation)
            .unwrap();
        let seen = &runtime.snapshot_for("server-a").agents[0];
        assert_eq!(seen.seen_generation, seen.attention_generation);
        assert_eq!(
            hook("next-prompt", "UserPromptSubmit").lifecycle,
            v1::AgentLifecycleState::Working as i32
        );
    }

    /// Claude Code's `StopFailure` ends a turn exactly like `Stop`. Before it
    /// was taken, a failed turn left the agent working with nothing left to
    /// arrive that could ever end it.
    #[test]
    fn a_failed_turn_ends_the_turn_and_asks_for_a_human() {
        let runtime = runtime("stop-failure");
        let topology = topology("claude");
        let claude = |id: &str, name: &str| {
            let mut value = event(id, 0, name);
            value.adapter = v1::AgentAdapterKind::ClaudeCode.into();
            value.adapter_id = "claude-code".into();
            runtime
                .ingest_hook_with_context(&value, "server-a", Some(&topology))
                .unwrap()
        };
        claude("prompt", "UserPromptSubmit");
        let failed = claude("stop-failure", "StopFailure");
        assert!(failed.notify);
        let record = failed.agent.unwrap();
        assert_eq!(record.lifecycle, v1::AgentLifecycleState::Idle as i32);
        assert_eq!(record.attention_kind, "completed");
        // And it is terminal: a late tool event cannot revive the turn.
        claude("late-post", "PostToolUse");
        assert_eq!(
            runtime.snapshot_for("server-a").agents[0].lifecycle,
            v1::AgentLifecycleState::Idle as i32
        );
    }

    /// A subagent finishing is the parent still working, not the parent done.
    #[test]
    fn a_finished_subagent_does_not_end_its_parents_turn() {
        let runtime = runtime("subagent-stop");
        let topology = topology("codex");
        for (id, name) in [("prompt", "UserPromptSubmit"), ("sub", "SubagentStop")] {
            runtime
                .ingest_hook_with_context(&event(id, 0, name), "server-a", Some(&topology))
                .unwrap();
        }
        let record = &runtime.snapshot_for("server-a").agents[0];
        assert_eq!(record.lifecycle, v1::AgentLifecycleState::Working as i32);
        assert_eq!(record.attention_generation, 0);
    }

    #[test]
    fn a_working_agent_that_stops_reporting_stops_claiming_to_work() {
        let runtime = runtime("stale-working");
        let topology = topology("codex");
        runtime
            .ingest_hook_with_context(
                &event("prompt", 0, "UserPromptSubmit"),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        let working = runtime.snapshot_for("server-a").agents[0].clone();
        assert_eq!(working.lifecycle, v1::AgentLifecycleState::Working as i32);
        assert!(
            runtime.sweep_stale().is_empty(),
            "an agent that reported a moment ago is not stale"
        );

        // One millisecond inside the window is still not stale; one outside is.
        let age = |millis: i64| {
            let mut state = runtime.state.lock().unwrap();
            let record = state.agents.get_mut(&working.agent_id).unwrap();
            record.lifecycle = v1::AgentLifecycleState::Working as i32;
            record.updated_at_unix_millis = now_millis() - millis;
            record.lifecycle_observed_at_unix_millis = now_millis() - millis;
        };
        age(STALE_WORKING_TTL_MILLIS);
        assert!(runtime.sweep_stale().is_empty());

        // Moving the agent's pane is not evidence of what it is doing, and
        // reconciliation writes `updated_at` when it happens. A dead agent
        // whose pane moved must not get its silence clock reset.
        age(STALE_WORKING_TTL_MILLIS + 1_000);
        {
            let mut state = runtime.state.lock().unwrap();
            state
                .agents
                .get_mut(&working.agent_id)
                .unwrap()
                .updated_at_unix_millis = now_millis();
        }
        assert_eq!(
            runtime.sweep_stale().len(),
            1,
            "a route change reset the staleness clock"
        );
        runtime
            .ingest_hook_with_context(
                &event("re-working", 0, "UserPromptSubmit"),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        age(STALE_WORKING_TTL_MILLIS + 1_000);
        let events = runtime.sweep_stale();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reason, "stale");
        assert!(!events[0].notify, "going quiet is not an event to chase");
        let stale = &runtime.snapshot_for("server-a").agents[0];
        assert_eq!(stale.lifecycle, v1::AgentLifecycleState::Unknown as i32);
        assert!(runtime.sweep_stale().is_empty(), "degrading is done once");

        // The next hook of any kind restores real state.
        runtime
            .ingest_hook_with_context(
                &event("recovered", 0, "PostToolUse"),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        assert_eq!(
            runtime.snapshot_for("server-a").agents[0].lifecycle,
            v1::AgentLifecycleState::Working as i32
        );
    }

    /// Attention that was already earned is not erased by silence: an agent
    /// that asked for a human and then went quiet is still asking.
    #[test]
    fn staleness_never_touches_attention_or_a_blocked_agent() {
        let runtime = runtime("stale-preserves-attention");
        let topology = topology("codex");
        runtime
            .ingest_hook_with_context(
                &event("blocked", 0, "PermissionRequest"),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        {
            let mut state = runtime.state.lock().unwrap();
            for record in state.agents.values_mut() {
                record.updated_at_unix_millis = now_millis() - STALE_WORKING_TTL_MILLIS * 10;
            }
        }
        assert!(runtime.sweep_stale().is_empty());
        let record = &runtime.snapshot_for("server-a").agents[0];
        assert_eq!(record.lifecycle, v1::AgentLifecycleState::Blocked as i32);
        assert_eq!(record.attention_kind, "blocked");
    }

    /// The disconnect catch-up contract: everything that happened while the
    /// desktop was away is in the persisted store, and the snapshot a
    /// reconnecting desktop asks for carries it — unread, and attributed.
    #[test]
    fn attention_earned_while_the_desktop_was_away_survives_a_daemon_restart() {
        let path = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase13-catch-up-{}", uuid::Uuid::new_v4()))
            .join("agents.json");
        let mut topology = topology("codex");
        let mut second_pane = topology.panes[0].clone();
        second_pane.id = "%8".into();
        topology.panes.push(second_pane);
        {
            let runtime = AgentRuntime::isolated(path.clone());
            for (id, name) in [("prompt", "UserPromptSubmit"), ("stop", "Stop")] {
                runtime
                    .ingest_hook_with_context(&event(id, 0, name), "server-a", Some(&topology))
                    .unwrap();
            }
            let mut blocked = event("blocked", 0, "PermissionRequest");
            blocked.pane_id = "%8".into();
            blocked.native_session_id = "native-2".into();
            runtime
                .ingest_hook_with_context(&blocked, "server-a", Some(&topology))
                .unwrap();
        }

        // A fresh runtime is what a restarted daemon, or a desktop reconnecting
        // to one that never stopped, actually reads.
        let reconnected = AgentRuntime::isolated(path);
        let snapshot = reconnected.snapshot_for("server-a");
        assert_eq!(snapshot.agents.len(), 2);
        let done = snapshot
            .agents
            .iter()
            .find(|agent| agent.attention_kind == "completed")
            .expect("the finished turn is still unread");
        assert_eq!(done.lifecycle, v1::AgentLifecycleState::Idle as i32);
        assert!(done.attention_generation > done.seen_generation);
        let blocked = snapshot
            .agents
            .iter()
            .find(|agent| agent.attention_kind == "blocked")
            .expect("the blocked agent is still blocked");
        assert_eq!(blocked.lifecycle, v1::AgentLifecycleState::Blocked as i32);
        assert!(blocked.attention_generation > blocked.seen_generation);
        assert_eq!(
            snapshot.notification_watermark, snapshot.generation,
            "the watermark a reconnecting desktop replays against is the store's own"
        );
    }

    /// Routing is by exact pane ID and nothing else. On a stock tmux every
    /// agent window is called `claude`, so a name that participated in routing
    /// would send one agent's state to another agent's pane.
    #[test]
    fn identical_window_names_never_route_one_agents_state_to_another() {
        let runtime = runtime("duplicate-window-names");
        let mut topology = topology("claude");
        topology.windows[0].name = "claude".into();
        topology.windows.push(tmux_control::Window {
            id: "@3".into(),
            session_id: "$1".into(),
            index: 1,
            name: "claude".into(),
            active: false,
            layout: String::new(),
            zoomed: false,
        });
        let mut second = topology.panes[0].clone();
        second.id = "%8".into();
        second.window_id = "@3".into();
        topology.panes.push(second);
        runtime.reconcile_topology(&topology, "server-a").unwrap();

        let mut blocked = event("blocked-in-second-window", 0, "PermissionRequest");
        blocked.adapter = v1::AgentAdapterKind::ClaudeCode.into();
        blocked.adapter_id = "claude-code".into();
        blocked.pane_id = "%8".into();
        blocked.native_session_id = "native-2".into();
        runtime
            .ingest_hook_with_context(&blocked, "server-a", Some(&topology))
            .unwrap();

        let snapshot = runtime.snapshot_for("server-a");
        assert_eq!(snapshot.agents.len(), 2);
        let routed = snapshot
            .agents
            .iter()
            .find(|agent| agent.lifecycle == v1::AgentLifecycleState::Blocked as i32)
            .unwrap();
        let route = routed.route.as_ref().unwrap();
        assert_eq!(route.pane_id, "%8");
        assert_eq!(route.window_id, "@3");
        assert!(
            snapshot
                .agents
                .iter()
                .filter(|agent| agent.route.as_ref().unwrap().window_id == "@2")
                .all(|agent| agent.lifecycle == v1::AgentLifecycleState::Unknown as i32),
            "the identically named window kept its own state"
        );
    }

    /// A turn that starts and then blocks while the daemon is down.
    ///
    /// The mailbox used to keep one event per pane, so only the block survived
    /// — and the daemon then correctly ignored it, because the prompt that
    /// opened the new turn had been overwritten and the previous turn was
    /// already finished. The user came back to the old result and no sign that
    /// an agent was waiting on them.
    #[test]
    fn a_turn_that_starts_and_blocks_offline_replays_in_the_order_it_happened() {
        let dir = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase13-offline-turn-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let runtime = AgentRuntime::isolated(dir.join("agents.json"));
        let topology = topology("codex");
        for (id, name) in [("prompt", "UserPromptSubmit"), ("stop", "Stop")] {
            runtime
                .ingest_hook_with_context(&event(id, 0, name), "server-a", Some(&topology))
                .unwrap();
        }

        // Written out of order on purpose: the replay must be driven by the
        // names, not by whatever order the directory happens to hand back.
        for (name, event_name) in [
            (
                "hook-fallback-codex-7-00000000000000000002-b.pb",
                "PermissionRequest",
            ),
            (
                "hook-fallback-codex-7-00000000000000000001-a.pb",
                "UserPromptSubmit",
            ),
        ] {
            fs::write(
                dir.join(name),
                event(event_name, 0, event_name).encode_to_vec(),
            )
            .unwrap();
        }
        let mut replayed = Vec::new();
        let report = fallback::consume(&dir, |event| {
            replayed.push(event.source_event_id.clone());
            match runtime.ingest_hook_with_context(&event, "server-a", Some(&topology)) {
                Ok(_) => fallback::HookReplayDisposition::Applied,
                Err(HookIngestFailure::Duplicate | HookIngestFailure::Permanent(_)) => {
                    fallback::HookReplayDisposition::Discarded
                }
                Err(HookIngestFailure::Retryable(_)) => fallback::HookReplayDisposition::Retryable,
            }
        })
        .unwrap();
        assert_eq!(report.applied, 2);
        assert_eq!(report.retained, 0);
        assert_eq!(replayed, ["UserPromptSubmit", "PermissionRequest"]);
        let record = &runtime.snapshot_for("server-a").agents[0];
        assert_eq!(record.lifecycle, v1::AgentLifecycleState::Blocked as i32);
        assert_eq!(record.attention_kind, "blocked");
        assert!(record.attention_generation > record.seen_generation);
        fs::remove_dir_all(dir).unwrap();
    }

    /// The configuration probe reads the *daemon process's* `PATH`, and a
    /// daemon started by launchd or a non-login SSH exec has one without
    /// `~/.local/bin`. A running agent is proof its vendor is installed here —
    /// reported as `Absent`, the desktop offers nothing and says nothing,
    /// which is the original failure with the volume turned down.
    ///
    /// Asserted as an invariant over the running set rather than by observing
    /// a machine without the agent installed: whether *this* machine has Codex
    /// on its `PATH` is not something a test may depend on. The `Absent` half
    /// is covered by `an_agent_that_is_not_on_this_host_is_absent_rather_than_unwired`,
    /// which passes the search path in.
    #[test]
    fn a_running_agent_is_never_reported_as_an_agent_this_host_does_not_have() {
        let home = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase13-running-absent-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let manager = hooks::HookManager::for_home(&home);
        let running: BTreeSet<&str> = adapters::all().map(|adapter| adapter.id()).collect();
        // Nothing is configured here at all, so every answer would otherwise be
        // whatever the search path happened to say.
        for (adapter, observed) in manager.wiring_with_running(&running) {
            assert_eq!(
                observed.state,
                v1::AgentHookWiring::NotWired,
                "{} is running and was reported as {:?}",
                adapter.id(),
                observed.state
            );
        }
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn malformed_fallback_is_removed_and_does_not_stop_the_scan() {
        let dir = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase6-fallback-scan-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("hook-fallback-a-1.pb"), b"malformed").unwrap();
        fs::write(
            dir.join("hook-fallback-b-2.pb"),
            event("valid", 0, "Stop").encode_to_vec(),
        )
        .unwrap();
        let mut seen = Vec::new();
        let report = fallback::consume(&dir, |event| {
            seen.push(event.source_event_id);
            fallback::HookReplayDisposition::Applied
        })
        .unwrap();
        assert_eq!(report.applied, 1);
        assert_eq!(report.retained, 0);
        assert_eq!(seen, ["valid"]);
        assert!(fs::read_dir(&dir).unwrap().next().is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn retryable_fallback_replay_stays_queued_until_a_later_disposition() {
        let dir = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase14-fallback-retain-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hook-fallback-codex-7-00000000000000000001-a.pb");
        let later = dir.join("hook-fallback-codex-7-00000000000000000002-b.pb");
        fs::write(
            &path,
            event("retryable", 0, "PermissionRequest").encode_to_vec(),
        )
        .unwrap();
        fs::write(&later, event("later", 0, "Stop").encode_to_vec()).unwrap();

        let mut attempts = 0;
        let retained = fallback::consume(&dir, |_| {
            attempts += 1;
            fallback::HookReplayDisposition::Retryable
        })
        .unwrap();
        assert_eq!(retained.applied, 0);
        assert_eq!(retained.retained, 2);
        assert_eq!(
            attempts, 1,
            "later events must not overtake a retained event"
        );
        assert!(path.exists(), "retryable replay must remain durable");
        assert!(later.exists(), "later replay must remain ordered behind it");

        let discarded =
            fallback::consume(&dir, |_| fallback::HookReplayDisposition::Discarded).unwrap();
        assert_eq!(discarded.applied, 0);
        assert_eq!(discarded.retained, 0);
        assert!(!path.exists(), "a permanent/duplicate disposition is final");
        assert!(!later.exists(), "later permanent input is also discarded");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn recovered_mailbox_applies_before_the_next_live_event_without_a_restart() {
        let root = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase14-live-recovery-{}", uuid::Uuid::new_v4()));
        let state_parent = root.join("state");
        let mailbox = root.join("mailbox");
        fs::create_dir_all(&mailbox).unwrap();
        fs::write(&state_parent, b"blocks persistence").unwrap();
        let runtime = AgentRuntime::isolated(state_parent.join("agents.json"));
        let topology = topology("codex");
        let retained = event("retained-a", 0, "PermissionRequest");
        assert!(matches!(
            runtime.ingest_hook_with_context(&retained, "server-a", Some(&topology)),
            Err(HookIngestFailure::Retryable(_))
        ));
        let queued = mailbox.join("hook-fallback-codex-7-00000000000000000001-a.pb");
        fs::write(&queued, retained.encode_to_vec()).unwrap();

        fs::remove_file(&state_parent).unwrap();
        fs::create_dir_all(&state_parent).unwrap();
        let live = event("live-b", 0, "Stop");
        let live_event = runtime
            .ingest_after_replay_with_context(&live, "server-a", Some(&topology), || {
                let report = fallback::consume(&mailbox, |event| {
                    match runtime.ingest_hook_with_context(&event, "server-a", Some(&topology)) {
                        Ok(_) => fallback::HookReplayDisposition::Applied,
                        Err(HookIngestFailure::Duplicate | HookIngestFailure::Permanent(_)) => {
                            fallback::HookReplayDisposition::Discarded
                        }
                        Err(HookIngestFailure::Retryable(_)) => {
                            fallback::HookReplayDisposition::Retryable
                        }
                    }
                })?;
                if report.retained > 0 {
                    anyhow::bail!("mailbox is still retained");
                }
                Ok(report.applied)
            })
            .unwrap();

        assert!(!queued.exists());
        let record = live_event.agent.unwrap();
        assert_eq!(record.lifecycle, v1::AgentLifecycleState::Idle as i32);
        let stored = &runtime.state.lock().unwrap().agents[&record.agent_id];
        assert_eq!(
            stored.source_event_ids.iter().cloned().collect::<Vec<_>>(),
            ["retained-a", "live-b"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incomplete_mailbox_sweep_rejects_live_input_after_partial_progress() {
        let root = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase14-incomplete-sweep-{}", uuid::Uuid::new_v4()));
        let mailbox = root.join("mailbox");
        let failed_root = root.join("failed-root");
        fs::create_dir_all(&mailbox).unwrap();
        fs::write(&failed_root, b"not a directory").unwrap();
        let runtime = AgentRuntime::isolated(root.join("state/agents.json"));
        let topology = topology("codex");
        let queued = mailbox.join("hook-fallback-codex-7-00000000000000000001-a.pb");
        fs::write(
            &queued,
            event("replayed-a", 0, "UserPromptSubmit").encode_to_vec(),
        )
        .unwrap();

        let roots = [mailbox, failed_root];
        let live = event("live-b", 0, "Stop");
        let result =
            runtime.ingest_after_replay_with_context(&live, "server-a", Some(&topology), || {
                fallback::consume_roots(&roots, |event| {
                    match runtime.ingest_hook_with_context(&event, "server-a", Some(&topology)) {
                        Ok(_) => fallback::HookReplayDisposition::Applied,
                        Err(HookIngestFailure::Duplicate | HookIngestFailure::Permanent(_)) => {
                            fallback::HookReplayDisposition::Discarded
                        }
                        Err(HookIngestFailure::Retryable(_)) => {
                            fallback::HookReplayDisposition::Retryable
                        }
                    }
                })
            });

        assert!(matches!(result, Err(HookIngestFailure::Retryable(_))));
        assert!(
            !queued.exists(),
            "successfully replayed input is acknowledged"
        );
        let state = runtime.state.lock().unwrap();
        let stored = state.agents.values().next().unwrap();
        assert_eq!(
            stored.source_event_ids.iter().cloned().collect::<Vec<_>>(),
            ["replayed-a"],
            "live input must wait until every mailbox root was inspected"
        );
        drop(state);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lost_ack_replay_discards_duplicate_without_republishing_or_advancing_state() {
        let root = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase14-lost-ack-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let runtime = AgentRuntime::isolated(root.join("state/agents.json"));
        let topology = topology("codex");
        let applied = event("ack-lost", 0, "PermissionRequest");
        runtime
            .ingest_hook_with_context(&applied, "server-a", Some(&topology))
            .unwrap();
        let generation = runtime.state.lock().unwrap().generation;
        let queued = root.join("hook-fallback-codex-7-00000000000000000001-a.pb");
        fs::write(&queued, applied.encode_to_vec()).unwrap();

        let report = fallback::consume(&root, |event| {
            match runtime.ingest_hook_with_context(&event, "server-a", Some(&topology)) {
                Ok(_) => fallback::HookReplayDisposition::Applied,
                Err(HookIngestFailure::Duplicate | HookIngestFailure::Permanent(_)) => {
                    fallback::HookReplayDisposition::Discarded
                }
                Err(HookIngestFailure::Retryable(_)) => fallback::HookReplayDisposition::Retryable,
            }
        })
        .unwrap();
        assert_eq!(report.applied, 0);
        assert_eq!(report.retained, 0);
        assert_eq!(runtime.state.lock().unwrap().generation, generation);
        assert!(!queued.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
