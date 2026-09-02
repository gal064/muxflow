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
    /// What this host's agent configuration was last observed to do with
    /// lifecycle events. Re-read only when a configuration file changed.
    wiring: Mutex<hooks::WiringCache>,
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
                Arc::new(Self::load(crate::paths::runtime_dir().join("agents.json")))
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
            state: Mutex::new(state),
            wiring: Mutex::new(hooks::WiringCache::default()),
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

    /// Withdraw Working from agents that have gone silent past
    /// [`STALE_WORKING_TTL_MILLIS`], returning the events to publish.
    ///
    /// Only the lifecycle is touched. Attention and what was already seen both
    /// survive: an agent that went quiet after asking for a human is still
    /// asking for a human, and forgetting that would be a second lie in place
    /// of the first. Nothing outside the record is touched either — no signal
    /// reaches the agent's process, and `notify` stays false, so a healthy
    /// agent that merely ran one long silent tool call loses a label and
    /// regains it on its next hook.
    pub(crate) fn sweep_stale(&self) -> Vec<v1::AgentEvent> {
        let now = now_millis();
        let mut state = self.state.lock().unwrap();
        let stale: Vec<String> = state
            .agents
            .values()
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
    /// The bar is not a new one: `reconcile::topology` already deletes records
    /// on this same evidence. And retirement is not final — any later hook
    /// re-creates the agent.
    fn retire_departed(&self) -> Vec<v1::AgentEvent> {
        // The tmux fork happens outside the lock. `ingest_hook` takes the same
        // mutex, and holding it across a subprocess would stall live hook
        // ingestion behind a discovery that has nothing to do with it.
        if !self.has_claimed_work() {
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
        let departed: Vec<String> = state
            .agents
            .values()
            .filter(|record| {
                record.route.server_identity == identity
                    && !record.route.pane_id.is_empty()
                    && !record.adapter_id.is_empty()
                    && claims_work(record)
                    && !detected
                        .contains_key(&(record.route.pane_id.clone(), record.adapter_id.clone()))
            })
            .map(|record| record.agent_id.clone())
            .collect();
        if departed.is_empty() {
            return Vec::new();
        }
        let original = state.clone();
        for agent_id in &departed {
            state.agents.remove(agent_id);
        }
        state.generation = state.generation.saturating_add(1);
        let generation = state.generation;
        if self.persist_locked(&state).is_err() {
            *state = original;
            return Vec::new();
        }
        vec![v1::AgentEvent {
            agent: None,
            generation,
            notify: false,
            reason: "departed".into(),
            retired_agent_ids: departed,
        }]
    }

    /// Whether any record claims to be mid-turn, which is the only condition
    /// under which the retirement pass is worth a tmux round-trip. Idle hosts
    /// stay at zero periodic forks, which is what the topology actor's own
    /// no-op early-out exists to preserve.
    fn has_claimed_work(&self) -> bool {
        self.state.lock().unwrap().agents.values().any(claims_work)
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
    for event in runtime.retire_departed() {
        publish(event);
    }
    for event in runtime.sweep_stale() {
        publish(event);
    }
}

/// Whether this record is asserting something about a turn in flight, which is
/// the only claim a departed process can still be falsely making. `Idle` and
/// `Unknown` say nothing that outliving the process would turn into a lie.
fn claims_work(record: &StoredAgent) -> bool {
    record.lifecycle == v1::AgentLifecycleState::Working as i32
        || record.lifecycle == v1::AgentLifecycleState::Blocked as i32
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
