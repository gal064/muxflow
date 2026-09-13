use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

use anyhow::Context;
use serde::{Deserialize, Serialize};

const STATE_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct StoredRoute {
    pub host_profile_id: String,
    pub server_identity: String,
    pub session_id: String,
    pub session_name_fallback: String,
    pub window_id: String,
    pub window_name_fallback: String,
    pub pane_id: String,
    pub pane_index_fallback: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(super) struct CodexTurnKey {
    pub agent_id: String,
    pub turn_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CodexReviewer {
    AutoReview,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct CodexTurnReview {
    pub turn: CodexTurnKey,
    pub reviewer: CodexReviewer,
}

/// Deliberately tolerant of keys it does not know: **do not add
/// `#[serde(deny_unknown_fields)]`.**
///
/// Every store already written to disk carries `"authority"`, a field removed
/// when hooks became the only writer of `lifecycle`. Serde ignoring it is the
/// entire migration — there is no schema bump and no rewrite. Denying unknown
/// fields would turn every one of those files into a parse failure, and `load`
/// answers a parse failure by discarding the state, so every user would lose
/// their agent list on upgrade. `a_store_written_with_the_removed_authority_field_still_loads`
/// pins this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StoredAgent {
    pub agent_id: String,
    pub adapter: i32,
    #[serde(default)]
    pub adapter_id: String,
    pub native_session_id: String,
    pub display_name: String,
    pub route: StoredRoute,
    pub lifecycle: i32,
    pub state_generation: u64,
    pub attention_generation: u64,
    #[serde(default)]
    pub attention_kind: String,
    pub seen_generation: u64,
    pub updated_at_unix_millis: i64,
    pub hook_authority_expires_at_unix_millis: i64,
    pub detected_manually: bool,
    #[serde(default)]
    pub source_event_ids: VecDeque<String>,
    #[serde(default)]
    pub latest_source_generation: u64,
    #[serde(default = "default_present")]
    pub present: bool,
    #[serde(default)]
    pub hook_terminal: bool,
    /// The latest root-scoped Codex turn that produced lifecycle activity.
    /// Automatic goal continuations start a fresh turn without emitting
    /// `UserPromptSubmit`, so this cannot be derived from the terminal bit.
    #[serde(default)]
    pub codex_active_root_turn_id: String,
    /// Root turns for which a terminal hook has already been observed. This
    /// session-scoped history lets a fresh automatic continuation reopen
    /// Working without allowing late activity from an older turn to do so.
    #[serde(default)]
    pub codex_terminal_root_turn_ids: BTreeSet<String>,
    /// Root turn that owned each active child when its latest activity arrived.
    /// Transcript fallback edges carry the root turn, so an old root cannot
    /// clear a same-ID child that resumed under its successor.
    #[serde(default)]
    pub codex_subagent_root_turn_ids: BTreeMap<String, String>,
    /// Resumed children observed after the active root had already completed.
    /// Their new owner is the next root hook, not that completed root.
    #[serde(default)]
    pub codex_subagents_awaiting_root: BTreeSet<String>,
    /// Bounded recent exact child turns, including inactive children.
    /// Retaining them distinguishes a same-ID resume from a first-seen child
    /// whose parent root is unknowable.
    #[serde(default)]
    pub codex_latest_subagent_turns: VecDeque<CodexTurnKey>,
    /// Recently completed exact child turns. A late activity hook for one of
    /// these turns cannot resurrect the child after its terminal arrived.
    #[serde(default)]
    pub codex_terminal_subagent_turns: VecDeque<CodexTurnKey>,
    /// Claude's parent has stopped while at least one background subagent is
    /// still running. Retained across daemon restarts so Claude's routine idle
    /// notification cannot turn that live work into a false blocked state.
    #[serde(default)]
    pub claude_has_running_subagent: bool,
    /// Codex has no aggregate child count on its parent `Stop`, so each stable
    /// child ID is retained with its exact active turn across daemon restarts.
    /// The turn prevents a late terminal hook from clearing a resumed child.
    /// An empty turn is the bounded migration state for a version-4 ID set.
    #[serde(
        default,
        alias = "codex_running_subagent_ids",
        deserialize_with = "deserialize_codex_running_subagents"
    )]
    pub codex_running_subagents: BTreeMap<String, String>,
    /// More children were observed than the bounded ID set can represent.
    /// Keep Working conservatively until a fresh session or stale-evidence
    /// recovery; guessing which unmatched stop clears overflow would lie.
    #[serde(default)]
    pub codex_subagent_capacity_exceeded: bool,
    /// The Codex parent has stopped but remains live until the map above is
    /// empty. A parent that waits for its children never sets this flag.
    #[serde(default)]
    pub codex_parent_stopped_for_subagents: bool,
    /// When either vendor last supplied direct evidence for the retained child
    /// guard. Kept separate from general lifecycle traffic so idle nags or
    /// unrelated parent hooks cannot make a lost child stop live forever.
    #[serde(default)]
    pub subagent_evidence_observed_at_unix_millis: i64,
    /// Exact Codex turns whose context identified native auto-review. Child
    /// turns overlap, so one scalar would let the latest child evict valid
    /// evidence for its siblings. Ingest keeps this deque strictly bounded.
    #[serde(default)]
    pub codex_turn_reviews: VecDeque<CodexTurnReview>,
    /// When something last said what this agent was *doing*.
    ///
    /// Distinct from `updated_at_unix_millis`, which also moves when the agent
    /// merely changes pane — reconciliation rewrites it on a pure route change,
    /// so moving a pane between windows reset a dead agent's staleness clock
    /// with no lifecycle evidence involved. Zero means a record written before
    /// this field existed; the staleness sweep falls back to `updated_at` for
    /// those rather than treating them as infinitely old.
    #[serde(default)]
    pub lifecycle_observed_at_unix_millis: i64,
    /// When `lifecycle` last changed, rather than when it was last observed.
    ///
    /// Zero is the on-disk compatibility value for records written before the
    /// field existed. `load` repairs it from the best timestamp those records
    /// have, then every real lifecycle transition advances it exactly once.
    #[serde(default)]
    pub lifecycle_changed_at_unix_millis: i64,
    /// When the current seen attention generation was first acknowledged.
    ///
    /// Repeated acknowledgements do not move this clock. It is the stable
    /// origin for a completed agent's post-read Recent window.
    #[serde(default)]
    pub attention_seen_at_unix_millis: i64,
}

fn deserialize_codex_running_subagents<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StoredChildren {
        Turns(BTreeMap<String, String>),
        LegacyIds(BTreeSet<String>),
    }

    Ok(match StoredChildren::deserialize(deserializer)? {
        StoredChildren::Turns(turns) => turns,
        StoredChildren::LegacyIds(ids) => ids.into_iter().map(|id| (id, String::new())).collect(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StoredState {
    #[serde(default)]
    pub schema_version: u32,
    pub generation: u64,
    pub agents: BTreeMap<String, StoredAgent>,
}

impl Default for StoredState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            generation: 0,
            agents: BTreeMap::new(),
        }
    }
}

pub(super) fn load(path: &Path) -> StoredState {
    let mut state = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|state: &StoredState| state.schema_version == STATE_SCHEMA_VERSION)
        .unwrap_or_default();
    for record in state.agents.values_mut() {
        while record.codex_terminal_root_turn_ids.len() > super::MAX_CODEX_TERMINAL_ROOT_TURNS {
            let Some(oldest) = record.codex_terminal_root_turn_ids.first().cloned() else {
                break;
            };
            record.codex_terminal_root_turn_ids.remove(&oldest);
        }
        if record.codex_running_subagents.len() > super::MAX_CODEX_CHILDREN {
            record.codex_running_subagents = record
                .codex_running_subagents
                .iter()
                .take(super::MAX_CODEX_CHILDREN)
                .map(|(child, turn)| (child.clone(), turn.clone()))
                .collect();
            record.codex_subagent_capacity_exceeded = true;
        }
        record
            .codex_subagent_root_turn_ids
            .retain(|child_id, _| record.codex_running_subagents.contains_key(child_id));
        record
            .codex_subagents_awaiting_root
            .retain(|child_id| record.codex_running_subagents.contains_key(child_id));
        while record.codex_latest_subagent_turns.len() > super::MAX_CODEX_CHILD_TURN_HISTORY {
            record.codex_latest_subagent_turns.pop_front();
        }
        while record.codex_terminal_subagent_turns.len() > super::MAX_CODEX_CHILD_TURN_HISTORY {
            record.codex_terminal_subagent_turns.pop_front();
        }
        if record.lifecycle_changed_at_unix_millis == 0 {
            record.lifecycle_changed_at_unix_millis =
                if record.lifecycle_observed_at_unix_millis > 0 {
                    record.lifecycle_observed_at_unix_millis
                } else {
                    record.updated_at_unix_millis
                };
        }
        // Stores written before this field existed cannot recover the actual
        // acknowledgement time. Preserve their previous Recent/Idle behavior
        // by using the completion transition as the one-time repair baseline.
        if record.attention_seen_at_unix_millis == 0
            && record.attention_kind == "completed"
            && record.attention_generation > 0
            && record.seen_generation >= record.attention_generation
        {
            record.attention_seen_at_unix_millis = record.lifecycle_changed_at_unix_millis;
        }
        if record.subagent_evidence_observed_at_unix_millis == 0
            && (record.claude_has_running_subagent
                || !record.codex_running_subagents.is_empty()
                || record.codex_subagent_capacity_exceeded)
        {
            record.subagent_evidence_observed_at_unix_millis =
                if record.lifecycle_observed_at_unix_millis > 0 {
                    record.lifecycle_observed_at_unix_millis
                } else {
                    record.updated_at_unix_millis
                };
        }
        // A terminal hook is proof that the turn ended. Normalize the invalid
        // combination observed in a live schema-2 store (`hook_terminal: true`
        // with `lifecycle: working`) so the next daemon snapshot repairs the
        // UI immediately instead of waiting up to the stale-working TTL.
        if record.hook_terminal {
            record.claude_has_running_subagent = false;
            record.codex_running_subagents.clear();
            record.codex_subagent_root_turn_ids.clear();
            record.codex_subagents_awaiting_root.clear();
            record.codex_subagent_capacity_exceeded = false;
            record.codex_parent_stopped_for_subagents = false;
            record.subagent_evidence_observed_at_unix_millis = 0;
            if record.lifecycle != tmux_agent_protocol::v1::AgentLifecycleState::Idle as i32 {
                record.lifecycle = tmux_agent_protocol::v1::AgentLifecycleState::Idle as i32;
            }
        }
    }
    state
}

pub(super) fn persist(path: &Path, state: &StoredState) -> anyhow::Result<()> {
    let parent = path.parent().context("agent state has no parent")?;
    crate::paths::prepare_runtime_dir(parent)?;
    let temporary = parent.join(format!(".agents-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(state)?)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn default_present() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store written by the build that shipped before this phase.
    ///
    /// It is schema 2, so it is *not* invalidated — the records must load with
    /// their manual detections, their attention and their seen generations
    /// intact, and the fields this phase added must degrade to defaults rather
    /// than taking the whole file down. This is exactly the file sitting on the
    /// user's machine right now.
    #[test]
    fn a_store_written_before_this_phase_loads_with_its_records_intact() {
        let path = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase13-agent-state-{}.json", uuid::Uuid::new_v4()));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            br#"{
              "schema_version": 2,
              "generation": 7,
              "agents": {
                "claude-code:field": {
                  "agent_id": "claude-code:field",
                  "adapter": 2,
                  "adapter_id": "claude-code",
                  "native_session_id": "",
                  "display_name": "Claude Code",
                  "route": {
                    "host_profile_id": "",
                    "server_identity": "tmux:server",
                    "session_id": "$1",
                    "session_name_fallback": "inductive",
                    "window_id": "@4",
                    "window_name_fallback": "claude",
                    "pane_id": "%120",
                    "pane_index_fallback": 0
                  },
                  "lifecycle": 4,
                  "authority": 2,
                  "state_generation": 7,
                  "attention_generation": 2,
                  "attention_kind": "blocked",
                  "seen_generation": 1,
                  "updated_at_unix_millis": 1786000000000,
                  "hook_authority_expires_at_unix_millis": 0,
                  "detected_manually": true,
                  "source_event_ids": [],
                  "latest_source_generation": 0,
                  "present": true,
                  "hook_terminal": false
                }
              }
            }"#,
        )
        .unwrap();
        let state = load(&path);
        assert_eq!(state.schema_version, STATE_SCHEMA_VERSION);
        assert_eq!(state.generation, 7);
        let record = &state.agents["claude-code:field"];
        assert!(record.detected_manually);
        assert_eq!(record.attention_generation, 2);
        assert_eq!(record.seen_generation, 1);
        assert_eq!(record.attention_kind, "blocked");
        assert_eq!(record.route.pane_id, "%120");
        // The field this phase added is absent from the file, and its default
        // is what the staleness sweep reads as "fall back to `updated_at`".
        assert_eq!(record.lifecycle_observed_at_unix_millis, 0);
        assert_eq!(record.lifecycle_changed_at_unix_millis, 1786000000000);
        assert!(!record.claude_has_running_subagent);
        assert!(record.codex_running_subagents.is_empty());
        assert!(!record.codex_subagent_capacity_exceeded);
        assert!(!record.codex_parent_stopped_for_subagents);
        assert_eq!(record.subagent_evidence_observed_at_unix_millis, 0);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn state_without_the_current_schema_is_invalidated_for_topology_rebuild() {
        let path = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase9-agent-state-{}.json", uuid::Uuid::new_v4()));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            br#"{
              "schema_version": 1,
              "generation": 42,
              "agents": {
                "codex:old": {
                  "agent_id": "codex:old",
                  "adapter": 1,
                  "adapter_id": "codex",
                  "native_session_id": "native-old",
                  "display_name": "Old agent",
                  "route": {
                    "host_profile_id": "",
                    "server_identity": "server-a",
                    "session_id": "$1",
                    "session_name_fallback": "work",
                    "window_id": "@1",
                    "window_name_fallback": "agent",
                    "pane_id": "%1",
                    "pane_index_fallback": 0,
                    "nested_provenance": 2,
                    "reported_pane_id": "%99"
                  },
                  "lifecycle": 1,
                  "authority": 1,
                  "state_generation": 42,
                  "attention_generation": 1,
                  "seen_generation": 0,
                  "updated_at_unix_millis": 1,
                  "hook_authority_expires_at_unix_millis": 2,
                  "detected_manually": false
                }
              }
            }"#,
        )
        .unwrap();
        let state = load(&path);
        assert_eq!(state.schema_version, STATE_SCHEMA_VERSION);
        assert_eq!(state.generation, 0);
        assert!(state.agents.is_empty());
        fs::remove_file(path).unwrap();
    }

    /// The entire migration for removing `authority`: serde ignores the key.
    ///
    /// There is no schema bump and no rewrite, which is only true while
    /// [`StoredAgent`] stays tolerant of unknown fields. Adding
    /// `#[serde(deny_unknown_fields)]` would turn every store already on disk
    /// into a parse failure, and `load` answers a parse failure by discarding
    /// the state — so every user would open the app to an empty agent list.
    /// This is the test that fails first if someone adds it.
    #[test]
    fn a_store_written_with_the_removed_authority_field_still_loads() {
        let path = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("authority-removal-{}.json", uuid::Uuid::new_v4()));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            br#"{
              "schema_version": 2,
              "generation": 9,
              "agents": {
                "codex:kept": {
                  "agent_id": "codex:kept",
                  "adapter": 1,
                  "adapter_id": "codex",
                  "native_session_id": "native-kept",
                  "display_name": "Kept agent",
                  "route": {
                    "host_profile_id": "",
                    "server_identity": "server-a",
                    "session_id": "$1",
                    "session_name_fallback": "work",
                    "window_id": "@1",
                    "window_name_fallback": "agent",
                    "pane_id": "%1",
                    "pane_index_fallback": 0
                  },
                  "lifecycle": 3,
                  "authority": 1,
                  "state_generation": 9,
                  "attention_generation": 2,
                  "attention_kind": "completed",
                  "seen_generation": 2,
                  "updated_at_unix_millis": 1786000000000,
                  "hook_authority_expires_at_unix_millis": 0,
                  "detected_manually": false,
                  "present": true,
                  "codex_running_subagent_ids": ["legacy-child"]
                }
              }
            }"#,
        )
        .unwrap();
        let state = load(&path);
        assert_eq!(state.generation, 9, "the store was kept, not discarded");
        let record = state
            .agents
            .get("codex:kept")
            .expect("the record survived the removed field");
        assert_eq!(
            record.codex_running_subagents.get("legacy-child"),
            Some(&String::new())
        );
        assert_eq!(record.lifecycle, 3);
        assert_eq!(record.attention_kind, "completed");
        assert_eq!(record.seen_generation, 2);
        assert_eq!(
            record.attention_seen_at_unix_millis, record.lifecycle_changed_at_unix_millis,
            "an already-seen completion uses its transition as the migration baseline"
        );
        assert_eq!(record.route.pane_id, "%1");
        assert!(record.codex_turn_reviews.is_empty());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_terminal_record_cannot_reload_as_working() {
        let path = std::env::current_dir().unwrap().join("tmp").join(format!(
            "terminal-working-agent-state-{}.json",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut state = StoredState::default();
        state.agents.insert(
            "codex:field".into(),
            StoredAgent {
                agent_id: "codex:field".into(),
                adapter: 1,
                adapter_id: "codex".into(),
                native_session_id: "session".into(),
                display_name: "Codex".into(),
                route: StoredRoute {
                    host_profile_id: String::new(),
                    server_identity: "server-a".into(),
                    session_id: "$1".into(),
                    session_name_fallback: "test".into(),
                    window_id: "@1".into(),
                    window_name_fallback: "codex".into(),
                    pane_id: "%1".into(),
                    pane_index_fallback: 0,
                },
                lifecycle: tmux_agent_protocol::v1::AgentLifecycleState::Working as i32,
                state_generation: 1,
                attention_generation: 1,
                attention_kind: "completed".into(),
                seen_generation: 1,
                attention_seen_at_unix_millis: 1,
                updated_at_unix_millis: 1,
                hook_authority_expires_at_unix_millis: 1,
                detected_manually: false,
                source_event_ids: VecDeque::new(),
                latest_source_generation: 0,
                present: true,
                hook_terminal: true,
                codex_active_root_turn_id: String::new(),
                codex_terminal_root_turn_ids: (0..100)
                    .map(|index| format!("00000000-{index:04x}-7000-8000-000000000000"))
                    .collect(),
                codex_subagent_root_turn_ids: BTreeMap::new(),
                codex_subagents_awaiting_root: BTreeSet::new(),
                codex_latest_subagent_turns: VecDeque::new(),
                codex_terminal_subagent_turns: VecDeque::new(),
                claude_has_running_subagent: false,
                codex_running_subagents: BTreeMap::new(),
                codex_subagent_capacity_exceeded: false,
                codex_parent_stopped_for_subagents: false,
                subagent_evidence_observed_at_unix_millis: 0,
                codex_turn_reviews: VecDeque::new(),
                lifecycle_observed_at_unix_millis: 1,
                lifecycle_changed_at_unix_millis: 1,
            },
        );
        persist(&path, &state).unwrap();

        let loaded = load(&path);
        assert_eq!(
            loaded.agents["codex:field"].lifecycle,
            tmux_agent_protocol::v1::AgentLifecycleState::Idle as i32,
        );
        assert_eq!(
            loaded.agents["codex:field"]
                .codex_terminal_root_turn_ids
                .len(),
            crate::service::agents::MAX_CODEX_TERMINAL_ROOT_TURNS
        );
        assert_eq!(
            loaded.agents["codex:field"]
                .codex_terminal_root_turn_ids
                .first()
                .map(String::as_str),
            Some("00000000-0024-7000-8000-000000000000")
        );
        assert_eq!(
            loaded.agents["codex:field"].lifecycle_changed_at_unix_millis, 1,
            "normalizing legacy state preserves its durable event time",
        );
        let reloaded = load(&path);
        assert_eq!(
            reloaded.agents["codex:field"].lifecycle_changed_at_unix_millis, 1,
            "reloading the same legacy store cannot make the agent newly recent",
        );
        fs::remove_file(path).unwrap();
    }
}
