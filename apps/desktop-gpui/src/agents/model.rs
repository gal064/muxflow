//! The adapter-neutral agent records and their store. Port of
//! `features/agents/types.ts`, `generation.ts`, `state.ts`, `selectors.ts` and
//! `runtimeMemory.ts`.
//!
//! Generations are the host's u64s. TypeScript carried them as decimal strings
//! to keep the full range; here they are plain integers, parsed losslessly at
//! the wire ([`super::wire`]).

use std::collections::{BTreeMap, HashMap};

pub type Generation = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lifecycle {
    Working,
    Blocked,
    Idle,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttentionKind {
    Blocked,
    Completed,
}

/// A lifecycle as drawn: `Done` is an idle agent with an unread completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DisplayState {
    Working,
    Blocked,
    Idle,
    Unknown,
    Done,
}

impl DisplayState {
    pub fn word(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Idle => "idle",
            Self::Unknown => "unknown",
            Self::Done => "done",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Window,
    Split,
}

impl Placement {
    pub fn word(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Split => "split",
        }
    }
}

/// What this host's configuration does with an adapter's lifecycle events, as
/// the daemon observed it. `Unavailable` is not `NotWired`: a configuration
/// nobody could read may be wired perfectly, and offering to write over it is
/// how unrelated hooks get lost. `Unspecified` is a host that did not say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookWiring {
    Wired,
    Partial,
    NotWired,
    Absent,
    Unavailable,
    Unspecified,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterDescriptor {
    pub id: String,
    pub display_name: String,
    pub supports_launch: bool,
    pub supports_resume: bool,
    pub supports_hooks: bool,
    pub supports_process_detection: bool,
    pub hook_config_path: String,
    pub hook_events: Vec<String>,
    pub hook_wiring: HookWiring,
    /// Why the wiring could not be read; empty in every other state.
    pub hook_wiring_detail: String,
    /// Whether the host says an install would act on this adapter.
    pub hook_setup_recommended: bool,
}

impl AdapterDescriptor {
    pub fn placements(&self) -> &'static [Placement] {
        if self.supports_launch {
            &[Placement::Window, Placement::Split]
        } else {
            &[]
        }
    }
}

/// The latest state the host supplied for one agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRecord {
    pub id: String,
    pub adapter_id: String,
    pub native_session_id: String,
    pub display_name: String,
    pub host_profile_id: String,
    pub server_identity: String,
    pub session_id: String,
    pub session_name: String,
    pub window_id: String,
    pub window_name: String,
    /// Exact routable pane; empty when the hook was absent from topology.
    pub pane_id: String,
    /// The agent pane's own title, which names the agent ahead of its window.
    pub pane_title: String,
    pub lifecycle: Lifecycle,
    pub lifecycle_generation: Generation,
    pub attention_generation: Generation,
    /// Persisted cause of the current attention generation.
    pub attention_kind: Option<AttentionKind>,
    pub seen_generation: Generation,
    pub updated_at: u64,
    /// Host-observed time when `lifecycle` last changed.
    pub lifecycle_changed_at: u64,
    /// Host-observed time when the current attention generation was first seen.
    pub attention_seen_at: u64,
    pub detected_manually: bool,
    pub present: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSnapshot {
    pub host_profile_id: String,
    pub server_identity: String,
    pub connection_epoch: u64,
    pub revision: Generation,
    /// Host barrier: every live mutation at or below this generation is included.
    pub accepted_generation: Generation,
    pub notification_watermark: Generation,
    pub agents: Vec<AgentRecord>,
    pub adapters: Vec<AdapterDescriptor>,
}

/// The connection a live event belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventScope {
    pub host_profile_id: String,
    pub server_identity: String,
    pub connection_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireEvent {
    Snapshot {
        snapshot: AgentSnapshot,
        replayed: bool,
    },
    Upsert {
        scope: EventScope,
        sequence: Generation,
        // Boxed: a record is several times the size of the other variants.
        record: Box<AgentRecord>,
        retired: Vec<String>,
        replayed: bool,
    },
    /// Retirements with no surviving record: the host saw the agent's process go.
    Retired {
        scope: EventScope,
        sequence: Generation,
        retired: Vec<String>,
        replayed: bool,
    },
}

impl WireEvent {
    pub fn host_profile_id(&self) -> &str {
        match self {
            Self::Snapshot { snapshot, .. } => &snapshot.host_profile_id,
            Self::Upsert { scope, .. } | Self::Retired { scope, .. } => &scope.host_profile_id,
        }
    }
}

/// One host's agent records. `host_profile_id` is unset until its first snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostAgentState {
    pub host_profile_id: Option<String>,
    pub server_identity: Option<String>,
    pub connection_epoch: Option<u64>,
    pub snapshot_revision: Generation,
    pub event_sequence: Generation,
    pub authoritative: bool,
    pub by_id: BTreeMap<String, AgentRecord>,
    pub adapters: Vec<AdapterDescriptor>,
}

impl HostAgentState {
    fn same_scope(&self, host_profile_id: &str, server_identity: &str) -> bool {
        self.host_profile_id.is_none()
            || (self.host_profile_id.as_deref() == Some(host_profile_id)
                && self.server_identity.as_deref() == Some(server_identity))
    }

    /// Applies one wire event. Returns whether anything changed.
    fn apply(&mut self, event: &WireEvent) -> bool {
        let (scope, sequence) = match event {
            WireEvent::Snapshot { snapshot, .. } => {
                if self.same_scope(&snapshot.host_profile_id, &snapshot.server_identity)
                    && (snapshot.revision < self.snapshot_revision
                        || snapshot.accepted_generation < self.event_sequence)
                {
                    return false;
                }
                *self = Self {
                    host_profile_id: Some(snapshot.host_profile_id.clone()),
                    server_identity: Some(snapshot.server_identity.clone()),
                    connection_epoch: Some(snapshot.connection_epoch),
                    snapshot_revision: snapshot.revision,
                    event_sequence: snapshot.accepted_generation,
                    authoritative: true,
                    by_id: snapshot
                        .agents
                        .iter()
                        .filter(|record| {
                            valid_record(
                                record,
                                &snapshot.host_profile_id,
                                &snapshot.server_identity,
                            )
                        })
                        .map(|record| (record.id.clone(), record.clone()))
                        .collect(),
                    adapters: snapshot.adapters.clone(),
                };
                return true;
            }
            WireEvent::Upsert {
                scope, sequence, ..
            }
            | WireEvent::Retired {
                scope, sequence, ..
            } => (scope, *sequence),
        };
        if !self.same_scope(&scope.host_profile_id, &scope.server_identity)
            || self.connection_epoch != Some(scope.connection_epoch)
            || !self.authoritative
            || sequence <= self.event_sequence
        {
            return false;
        }
        self.event_sequence = sequence;
        match event {
            WireEvent::Retired { retired, .. } => {
                for id in retired {
                    self.by_id.remove(id);
                }
            }
            WireEvent::Upsert {
                record, retired, ..
            } => {
                if !valid_record(record, &scope.host_profile_id, &scope.server_identity) {
                    return true;
                }
                if self.by_id.get(&record.id).is_some_and(|previous| {
                    record.lifecycle_generation < previous.lifecycle_generation
                }) {
                    return true;
                }
                for id in retired {
                    self.by_id.remove(id);
                }
                self.by_id.insert(record.id.clone(), (**record).clone());
            }
            WireEvent::Snapshot { .. } => unreachable!("handled above"),
        }
        true
    }

    /// Optimistically mirrors only the exact generation the host acknowledged.
    pub fn acknowledge_seen(
        &mut self,
        agent_id: &str,
        attention: Generation,
        seen_at: u64,
    ) -> bool {
        let Some(record) = self.by_id.get_mut(agent_id) else {
            return false;
        };
        if record.attention_generation != attention {
            return false;
        }
        let advances = record.seen_generation < attention;
        let supplies_timestamp = seen_at > 0 && record.attention_seen_at == 0;
        if advances {
            record.seen_generation = attention;
        }
        if supplies_timestamp {
            record.attention_seen_at = seen_at;
        }
        advances || supplies_timestamp
    }
}

fn valid_record(record: &AgentRecord, host_profile_id: &str, server_identity: &str) -> bool {
    let valid_route = record.pane_id.is_empty()
        || (!record.window_id.is_empty() && !record.session_id.is_empty());
    record.host_profile_id == host_profile_id
        && record.server_identity == server_identity
        && !record.id.is_empty()
        && valid_route
}

/// Every shown host's slice.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentStore {
    pub by_host: HashMap<String, HostAgentState>,
}

impl AgentStore {
    pub fn apply(&mut self, event: &WireEvent) -> bool {
        self.by_host
            .entry(event.host_profile_id().to_owned())
            .or_default()
            .apply(event)
    }

    pub fn seen_ack(
        &mut self,
        scope: &EventScope,
        agent_id: &str,
        attention: Generation,
        seen_at: u64,
    ) -> bool {
        let Some(host) = self.by_host.get_mut(&scope.host_profile_id) else {
            return false;
        };
        if host.host_profile_id.as_deref() != Some(&scope.host_profile_id)
            || host.server_identity.as_deref() != Some(&scope.server_identity)
            || host.connection_epoch != Some(scope.connection_epoch)
        {
            return false;
        }
        host.acknowledge_seen(agent_id, attention, seen_at)
    }

    /// The host's link went down: its records stay, its authority goes.
    pub fn disconnect(&mut self, host_profile_id: &str) -> bool {
        match self.by_host.get_mut(host_profile_id) {
            Some(host) if host.authoritative => {
                host.authoritative = false;
                true
            }
            _ => false,
        }
    }

    /// The host is no longer shown: its slice goes, not just its authority.
    pub fn remove(&mut self, host_profile_id: &str) -> bool {
        self.by_host.remove(host_profile_id).is_some()
    }

    pub fn host(&self, host_profile_id: &str) -> Option<&HostAgentState> {
        self.by_host.get(host_profile_id)
    }

    /// The host's authoritative agents on `server_identity`, loudest first.
    pub fn agents_for_scope(
        &self,
        host_profile_id: &str,
        server_identity: Option<&str>,
    ) -> Vec<&AgentRecord> {
        let Some(state) = self.by_host.get(host_profile_id) else {
            return Vec::new();
        };
        if !state.authoritative
            || state.host_profile_id.as_deref() != Some(host_profile_id)
            || state.server_identity.as_deref() != server_identity
        {
            return Vec::new();
        }
        let mut agents: Vec<_> = state.by_id.values().collect();
        agents.sort_by(|left, right| compare_agents(left, right));
        agents
    }
}

pub fn display_state(agent: &AgentRecord) -> DisplayState {
    match agent.lifecycle {
        Lifecycle::Idle
            if agent.attention_kind == Some(AttentionKind::Completed)
                && agent.attention_generation > agent.seen_generation =>
        {
            DisplayState::Done
        }
        Lifecycle::Idle => DisplayState::Idle,
        Lifecycle::Working => DisplayState::Working,
        Lifecycle::Blocked => DisplayState::Blocked,
        Lifecycle::Unknown => DisplayState::Unknown,
    }
}

/// None ranks lowest, then idle, unknown, working, done, blocked.
pub fn priority(state: Option<DisplayState>) -> u8 {
    match state {
        None => 0,
        Some(DisplayState::Idle) => 1,
        Some(DisplayState::Unknown) => 2,
        Some(DisplayState::Working) => 3,
        Some(DisplayState::Done) => 4,
        Some(DisplayState::Blocked) => 5,
    }
}

pub fn compare_agents(left: &AgentRecord, right: &AgentRecord) -> std::cmp::Ordering {
    priority(Some(display_state(right)))
        .cmp(&priority(Some(display_state(left))))
        .then(right.updated_at.cmp(&left.updated_at))
        .then_with(|| left.display_name.cmp(&right.display_name))
        .then_with(|| left.id.cmp(&right.id))
}

/// A group of agents as one state: its loudest agent's, with that agent's
/// adapter, so a mark never shows one agent's icon with another's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rollup {
    /// `None` for an empty group.
    pub state: Option<DisplayState>,
    pub adapter_id: Option<String>,
    pub blocked: u32,
    pub working: u32,
    pub done: u32,
    pub unknown: u32,
    pub idle: u32,
    pub total: u32,
}

impl Rollup {
    fn single(agent: &AgentRecord) -> Self {
        let state = display_state(agent);
        let mut rollup = Self {
            state: Some(state),
            adapter_id: Some(agent.adapter_id.clone()),
            total: 1,
            ..Self::default()
        };
        *rollup.count_mut(state) = 1;
        rollup
    }

    fn count_mut(&mut self, state: DisplayState) -> &mut u32 {
        match state {
            DisplayState::Blocked => &mut self.blocked,
            DisplayState::Working => &mut self.working,
            DisplayState::Done => &mut self.done,
            DisplayState::Unknown => &mut self.unknown,
            DisplayState::Idle => &mut self.idle,
        }
    }

    /// Ties go to `self`, the accumulator: the first agent at the winning
    /// priority, in the caller's order.
    fn combine(&mut self, other: &Self) {
        if priority(other.state) > priority(self.state) {
            self.state = other.state;
            self.adapter_id.clone_from(&other.adapter_id);
        }
        self.blocked += other.blocked;
        self.working += other.working;
        self.done += other.done;
        self.unknown += other.unknown;
        self.idle += other.idle;
        self.total += other.total;
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rollups {
    pub by_pane: HashMap<String, Rollup>,
    pub by_window: HashMap<String, Rollup>,
    pub by_workspace: HashMap<String, Rollup>,
}

pub fn derive_rollups<'a>(agents: impl IntoIterator<Item = &'a AgentRecord>) -> Rollups {
    let mut rollups = Rollups::default();
    for agent in agents {
        let own = Rollup::single(agent);
        for (map, key) in [
            (&mut rollups.by_pane, &agent.pane_id),
            (&mut rollups.by_window, &agent.window_id),
            (&mut rollups.by_workspace, &agent.session_id),
        ] {
            if !key.is_empty() {
                map.entry(key.clone()).or_default().combine(&own);
            }
        }
    }
    rollups
}

/// The agents in `pane_id` whose attention is unread, with the exact
/// generation each would acknowledge.
pub fn agents_matching_pane<'a>(
    agents: impl IntoIterator<Item = &'a AgentRecord>,
    pane_id: &str,
) -> Vec<(String, Generation)> {
    if pane_id.is_empty() {
        return Vec::new();
    }
    agents
        .into_iter()
        .filter(|agent| {
            agent.pane_id == pane_id && agent.attention_generation > agent.seen_generation
        })
        .map(|agent| (agent.id.clone(), agent.attention_generation))
        .collect()
}

pub const MAX_AGENT_RUNTIME_SCOPES: usize = 16;
pub const MAX_PROCESSED_ATTENTION: usize = 4096;
pub const MAX_RECONNECT_RECORDS_PER_SCOPE: usize = 2048;

/// A map that forgets its least recently touched entry past `capacity`.
struct Lru<V> {
    entries: HashMap<String, (V, u64)>,
    tick: u64,
    capacity: usize,
}

impl<V> Lru<V> {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            tick: 0,
            capacity,
        }
    }

    fn peek(&self, key: &str) -> Option<&V> {
        self.entries.get(key).map(|(value, _)| value)
    }

    fn touch(&mut self, key: &str) -> Option<&V> {
        self.tick += 1;
        let tick = self.tick;
        self.entries.get_mut(key).map(|entry| {
            entry.1 = tick;
            &entry.0
        })
    }

    /// Returns the keys evicted to make room.
    fn insert(&mut self, key: String, value: V) -> Vec<String> {
        self.tick += 1;
        self.entries.insert(key, (value, self.tick));
        let mut evicted = Vec::new();
        while self.entries.len() > self.capacity {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (_, tick))| *tick)
                .map(|(key, _)| key.clone())
                .expect("over capacity, so not empty");
            self.entries.remove(&oldest);
            evicted.push(oldest);
        }
        evicted
    }
}

pub struct ScopeMemory {
    pub watermark: Generation,
    pub records: BTreeMap<String, AgentRecord>,
}

/// Bounded memory used only to reconcile notifications across a reconnect.
pub struct RuntimeMemory {
    processed: Lru<Generation>,
    scopes: Lru<ScopeMemory>,
}

impl Default for RuntimeMemory {
    fn default() -> Self {
        Self {
            processed: Lru::new(MAX_PROCESSED_ATTENTION),
            scopes: Lru::new(MAX_AGENT_RUNTIME_SCOPES),
        }
    }
}

impl RuntimeMemory {
    pub fn processed_generation(&self, record_key: &str) -> Option<Generation> {
        self.processed.peek(record_key).copied()
    }

    pub fn mark_processed(&mut self, record_key: String, generation: Generation) {
        self.processed.insert(record_key, generation);
    }

    pub fn scope(&mut self, scope_key: &str) -> Option<&ScopeMemory> {
        self.scopes.touch(scope_key)
    }

    pub fn commit_scope(
        &mut self,
        scope_key: String,
        watermark: Generation,
        records: &BTreeMap<String, AgentRecord>,
    ) {
        let records = if records.len() <= MAX_RECONNECT_RECORDS_PER_SCOPE {
            records.clone()
        } else {
            let mut newest: Vec<_> = records.values().collect();
            newest.sort_by_key(|record| std::cmp::Reverse(record.updated_at));
            newest
                .into_iter()
                .take(MAX_RECONNECT_RECORDS_PER_SCOPE)
                .map(|record| (record.id.clone(), record.clone()))
                .collect()
        };
        for evicted in self
            .scopes
            .insert(scope_key, ScopeMemory { watermark, records })
        {
            let prefix = format!("{evicted}\0");
            self.processed
                .entries
                .retain(|key, _| !key.starts_with(&prefix));
        }
    }

    #[cfg(test)]
    pub fn sizes(&self) -> (usize, usize) {
        (self.processed.entries.len(), self.scopes.entries.len())
    }
}

pub fn scope_key(host_profile_id: &str, server_identity: &str) -> String {
    format!("{host_profile_id}\0{server_identity}")
}

pub fn record_key(record: &AgentRecord) -> String {
    format!(
        "{}\0{}",
        scope_key(&record.host_profile_id, &record.server_identity),
        record.id
    )
}

#[cfg(test)]
pub mod fixtures {
    use super::*;

    /// `testFixtures.ts`'s agent: a working Codex in `%1`.
    pub fn agent() -> AgentRecord {
        AgentRecord {
            id: "agent-1".into(),
            adapter_id: "codex".into(),
            native_session_id: "native-1".into(),
            display_name: "Codex one".into(),
            host_profile_id: "local".into(),
            server_identity: "server-a".into(),
            session_id: "$1".into(),
            session_name: "work".into(),
            window_id: "@1".into(),
            window_name: "agent".into(),
            pane_id: "%1".into(),
            pane_title: String::new(),
            lifecycle: Lifecycle::Working,
            lifecycle_generation: 3,
            attention_generation: 0,
            attention_kind: None,
            seen_generation: 0,
            updated_at: 100,
            lifecycle_changed_at: 100,
            attention_seen_at: 0,
            detected_manually: false,
            present: true,
        }
    }

    pub fn adapter(id: &str, wiring: HookWiring) -> AdapterDescriptor {
        AdapterDescriptor {
            id: id.into(),
            display_name: id.into(),
            supports_launch: true,
            supports_resume: true,
            supports_hooks: true,
            supports_process_detection: true,
            hook_config_path: format!("~/.{id}/settings.json"),
            hook_events: Vec::new(),
            hook_wiring: wiring,
            hook_wiring_detail: String::new(),
            hook_setup_recommended: matches!(wiring, HookWiring::NotWired | HookWiring::Partial),
        }
    }

    pub fn snapshot(agents: Vec<AgentRecord>, revision: u64, host: &str) -> AgentSnapshot {
        AgentSnapshot {
            host_profile_id: host.into(),
            server_identity: "server-a".into(),
            connection_epoch: 1,
            revision,
            accepted_generation: revision,
            notification_watermark: revision,
            agents,
            adapters: Vec::new(),
        }
    }

    pub fn scope(host: &str, server: &str, epoch: u64) -> EventScope {
        EventScope {
            host_profile_id: host.into(),
            server_identity: server.into(),
            connection_epoch: epoch,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    fn snap(store: &mut AgentStore, agents: Vec<AgentRecord>, revision: u64, host: &str) -> bool {
        store.apply(&WireEvent::Snapshot {
            snapshot: snapshot(agents, revision, host),
            replayed: false,
        })
    }

    fn upsert(
        host: &str,
        server: &str,
        epoch: u64,
        sequence: u64,
        record: AgentRecord,
    ) -> WireEvent {
        WireEvent::Upsert {
            scope: scope(host, server, epoch),
            sequence,
            record: Box::new(record),
            retired: Vec::new(),
            replayed: false,
        }
    }

    fn retired(host: &str, server: &str, epoch: u64, sequence: u64, ids: &[&str]) -> WireEvent {
        WireEvent::Retired {
            scope: scope(host, server, epoch),
            sequence,
            retired: ids.iter().map(|id| (*id).to_owned()).collect(),
            replayed: false,
        }
    }

    fn local(store: &AgentStore) -> &HostAgentState {
        store.host("local").unwrap()
    }

    #[test]
    fn replaces_reconnect_state_with_an_authoritative_snapshot_without_merging_stale_agents() {
        let mut store = AgentStore::default();
        snap(&mut store, vec![agent()], 4, "local");
        store.disconnect("local");
        assert!(!local(&store).authoritative);
        snap(&mut store, vec![], 5, "local");
        assert!(local(&store).authoritative);
        assert!(local(&store).by_id.is_empty());
    }

    #[test]
    fn rejects_stale_cross_server_and_weaker_lifecycle_generations() {
        let mut store = AgentStore::default();
        snap(&mut store, vec![agent()], 4, "local");
        let weaker = AgentRecord {
            lifecycle: Lifecycle::Unknown,
            lifecycle_generation: 2,
            updated_at: 120,
            ..agent()
        };
        store.apply(&upsert("local", "server-a", 1, 5, weaker));
        assert_eq!(local(&store).by_id["agent-1"].lifecycle, Lifecycle::Working);
        assert!(!store.apply(&retired("local", "server-b", 1, 6, &["agent-1"])));
        assert!(!store.apply(&retired("local", "server-a", 1, 5, &["agent-1"])));
    }

    #[test]
    fn applies_a_retirement_that_carries_no_surviving_record() {
        let mut store = AgentStore::default();
        snap(&mut store, vec![agent()], 4, "local");
        assert!(store.apply(&retired("local", "server-a", 1, 5, &["agent-1"])));
        assert!(!local(&store).by_id.contains_key("agent-1"));
        assert_eq!(local(&store).event_sequence, 5);
    }

    #[test]
    fn ignores_a_retirement_from_another_server_and_one_for_an_agent_it_never_had() {
        let mut store = AgentStore::default();
        snap(&mut store, vec![agent()], 4, "local");
        assert!(!store.apply(&retired("local", "server-b", 1, 5, &["agent-1"])));
        store.apply(&retired("local", "server-a", 1, 5, &["never-existed"]));
        assert!(local(&store).by_id.contains_key("agent-1"));
        assert_eq!(local(&store).event_sequence, 5);
    }

    #[test]
    fn atomically_retires_a_manually_detected_identity_when_the_native_identity_arrives() {
        let mut store = AgentStore::default();
        let manual = AgentRecord {
            id: "manual".into(),
            native_session_id: String::new(),
            detected_manually: true,
            ..agent()
        };
        snap(&mut store, vec![manual], 4, "local");
        let native = AgentRecord {
            id: "native".into(),
            native_session_id: "session-1".into(),
            lifecycle_generation: 5,
            ..agent()
        };
        store.apply(&WireEvent::Upsert {
            scope: scope("local", "server-a", 1),
            sequence: 5,
            record: Box::new(native),
            retired: vec!["manual".into()],
            replayed: false,
        });
        assert_eq!(
            local(&store).by_id.keys().collect::<Vec<_>>(),
            [&"native".to_owned()]
        );
    }

    #[test]
    fn retains_an_unmapped_record_without_inventing_topology() {
        let mut store = AgentStore::default();
        let unmapped = AgentRecord {
            session_id: String::new(),
            window_id: String::new(),
            pane_id: String::new(),
            ..agent()
        };
        snap(&mut store, vec![unmapped], 4, "local");
        assert!(local(&store).by_id["agent-1"].pane_id.is_empty());
        let rollups = derive_rollups(local(&store).by_id.values());
        assert!(rollups.by_pane.is_empty());
        assert!(rollups.by_window.is_empty());
        assert!(rollups.by_workspace.is_empty());
    }

    #[test]
    fn acknowledges_only_the_exact_current_attention_generation() {
        let mut store = AgentStore::default();
        let unread = AgentRecord {
            lifecycle: Lifecycle::Idle,
            attention_generation: 8,
            seen_generation: 3,
            ..agent()
        };
        snap(&mut store, vec![unread], 4, "local");
        let mut host = local(&store).clone();
        assert!(!host.acknowledge_seen("agent-1", 7, 1));
        assert!(host.acknowledge_seen("agent-1", 8, 1));
        assert_eq!(host.by_id["agent-1"].seen_generation, 8);
    }

    #[test]
    fn merges_an_overtaken_acknowledgement_timestamp_without_lowering_the_event_barrier() {
        let mut store = AgentStore::default();
        let completed = AgentRecord {
            lifecycle: Lifecycle::Idle,
            attention_kind: Some(AttentionKind::Completed),
            attention_generation: 8,
            seen_generation: 3,
            ..agent()
        };
        snap(&mut store, vec![completed.clone()], 4, "local");
        store.apply(&upsert(
            "local",
            "server-a",
            1,
            6,
            AgentRecord {
                id: "other".into(),
                lifecycle_generation: 6,
                ..agent()
            },
        ));
        let mut other_epoch = store.clone();
        assert!(store.seen_ack(&scope("local", "server-a", 1), &completed.id, 8, 5_000));
        assert_eq!(local(&store).event_sequence, 6);
        assert_eq!(local(&store).by_id[&completed.id].seen_generation, 8);
        assert_eq!(local(&store).by_id[&completed.id].attention_seen_at, 5_000);
        assert!(!other_epoch.seen_ack(&scope("local", "server-a", 2), &completed.id, 8, 5_000));
    }

    #[test]
    fn rejects_a_snapshot_whose_accepted_barrier_is_behind_an_already_applied_live_event() {
        let mut store = AgentStore::default();
        snap(&mut store, vec![agent()], 5, "local");
        let renamed = AgentRecord {
            display_name: "Live rename".into(),
            ..agent()
        };
        store.apply(&upsert("local", "server-a", 1, 7, renamed));
        let late = AgentSnapshot {
            accepted_generation: 6,
            ..snapshot(
                vec![AgentRecord {
                    display_name: "Stale name".into(),
                    ..agent()
                }],
                6,
                "local",
            )
        };
        assert!(!store.apply(&WireEvent::Snapshot {
            snapshot: late,
            replayed: false
        }));
    }

    #[test]
    fn rejects_a_late_mutation_from_a_previous_connection_epoch() {
        let mut store = AgentStore::default();
        snap(&mut store, vec![agent()], 5, "local");
        let stale = AgentRecord {
            display_name: "Old connection".into(),
            ..agent()
        };
        assert!(!store.apply(&upsert("local", "server-a", 0, 6, stale)));
    }

    fn both() -> AgentStore {
        let mut store = AgentStore::default();
        snap(&mut store, vec![agent()], 4, "local");
        let remote = AgentRecord {
            host_profile_id: "remote".into(),
            ..agent()
        };
        snap(&mut store, vec![remote], 4, "remote");
        store
    }

    #[test]
    fn keeps_the_same_agent_id_on_two_hosts_as_two_records() {
        let store = both();
        assert_eq!(
            store.host("local").unwrap().by_id["agent-1"].host_profile_id,
            "local"
        );
        assert_eq!(
            store.host("remote").unwrap().by_id["agent-1"].host_profile_id,
            "remote"
        );
    }

    #[test]
    fn routes_a_live_event_to_its_own_host_and_checks_it_against_that_host() {
        let mut store = both();
        let before_local = store.host("local").unwrap().clone();
        store.apply(&upsert(
            "remote",
            "server-a",
            1,
            5,
            AgentRecord {
                host_profile_id: "remote".into(),
                display_name: "Renamed remotely".into(),
                ..agent()
            },
        ));
        assert_eq!(store.host("local").unwrap(), &before_local);
        assert_eq!(
            store.host("remote").unwrap().by_id["agent-1"].display_name,
            "Renamed remotely"
        );

        let mut store = both();
        store.apply(&upsert(
            "local",
            "server-a",
            1,
            9,
            AgentRecord {
                display_name: "Far ahead".into(),
                ..agent()
            },
        ));
        assert!(store.apply(&retired("remote", "server-a", 1, 5, &["agent-1"])));
        assert!(store.host("remote").unwrap().by_id.is_empty());
        assert!(!both().apply(&retired("remote", "server-a", 2, 5, &["agent-1"])));
        assert!(!both().apply(&retired("remote", "server-b", 1, 5, &["agent-1"])));
    }

    #[test]
    fn disconnects_one_host_without_touching_the_other_and_removes_a_host_outright() {
        let mut store = both();
        assert!(store.disconnect("remote"));
        assert!(!store.host("remote").unwrap().authoritative);
        assert!(store.host("local").unwrap().authoritative);
        assert!(!store.disconnect("never-seen"));
        assert!(store.remove("remote"));
        assert!(store.host("remote").is_none());
        assert!(!store.remove("remote"));
    }

    #[test]
    fn represents_done_only_as_idle_with_an_unseen_persisted_completion() {
        let idle = |kind, seen| AgentRecord {
            lifecycle: Lifecycle::Idle,
            attention_generation: 4,
            attention_kind: kind,
            seen_generation: seen,
            ..agent()
        };
        assert_eq!(
            display_state(&idle(Some(AttentionKind::Completed), 3)),
            DisplayState::Done
        );
        assert_eq!(
            display_state(&idle(Some(AttentionKind::Completed), 4)),
            DisplayState::Idle
        );
        assert_eq!(
            display_state(&idle(Some(AttentionKind::Blocked), 3)),
            DisplayState::Idle
        );
        assert_eq!(display_state(&idle(None, 3)), DisplayState::Idle);
    }

    #[test]
    fn rolls_the_loudest_agent_up_to_pane_window_and_workspace_with_its_own_adapter() {
        let agents = [
            AgentRecord {
                id: "working".into(),
                pane_id: "%1".into(),
                ..agent()
            },
            AgentRecord {
                id: "done".into(),
                lifecycle: Lifecycle::Idle,
                pane_id: "%2".into(),
                attention_generation: 2,
                attention_kind: Some(AttentionKind::Completed),
                seen_generation: 1,
                ..agent()
            },
            AgentRecord {
                id: "blocked".into(),
                adapter_id: "claude-code".into(),
                lifecycle: Lifecycle::Blocked,
                pane_id: "%2".into(),
                attention_generation: 3,
                ..agent()
            },
        ];
        let rollups = derive_rollups(&agents);
        assert_eq!(rollups.by_pane["%1"].state, Some(DisplayState::Working));
        let pane = &rollups.by_pane["%2"];
        assert_eq!(
            (pane.state, pane.blocked, pane.done),
            (Some(DisplayState::Blocked), 1, 1)
        );
        assert_eq!(
            rollups.by_window["@1"].adapter_id.as_deref(),
            Some("claude-code")
        );
        assert_eq!(rollups.by_workspace["$1"].total, 3);

        let tied = derive_rollups(&[
            AgentRecord {
                id: "first".into(),
                adapter_id: "claude-code".into(),
                ..agent()
            },
            AgentRecord {
                id: "second".into(),
                ..agent()
            },
        ]);
        assert_eq!(
            tied.by_window["@1"].adapter_id.as_deref(),
            Some("claude-code")
        );
    }

    #[test]
    fn marks_only_the_exact_routable_pane() {
        let direct = AgentRecord {
            pane_id: "%outer".into(),
            attention_generation: 7,
            seen_generation: 2,
            ..agent()
        };
        assert_eq!(
            agents_matching_pane([&direct], "%outer"),
            [("agent-1".to_owned(), 7)]
        );
        assert!(agents_matching_pane([&direct], "%other").is_empty());
    }

    #[test]
    fn bounds_processed_generations_host_scopes_and_reconnect_records() {
        let mut memory = RuntimeMemory::default();
        for index in 0..MAX_PROCESSED_ATTENTION + 20 {
            memory.mark_processed(format!("record-{index}"), index as u64);
        }
        assert_eq!(memory.sizes().0, MAX_PROCESSED_ATTENTION);
        assert_eq!(memory.processed_generation("record-0"), None);
        for scope in 0..MAX_AGENT_RUNTIME_SCOPES + 2 {
            let records = (0..MAX_RECONNECT_RECORDS_PER_SCOPE + 5)
                .map(|index| {
                    let record = AgentRecord {
                        id: format!("{scope}-{index}"),
                        updated_at: index as u64,
                        ..agent()
                    };
                    (record.id.clone(), record)
                })
                .collect();
            memory.commit_scope(format!("host-{scope}"), scope as u64, &records);
        }
        assert_eq!(memory.sizes().1, MAX_AGENT_RUNTIME_SCOPES);
        assert!(memory.scope("host-0").is_none());
        let newest = format!("host-{}", MAX_AGENT_RUNTIME_SCOPES + 1);
        assert_eq!(
            memory.scope(&newest).unwrap().records.len(),
            MAX_RECONNECT_RECORDS_PER_SCOPE
        );
    }
}
