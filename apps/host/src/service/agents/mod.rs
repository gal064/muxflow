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
mod tests;
