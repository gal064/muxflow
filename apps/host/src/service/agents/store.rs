use std::{
    collections::{BTreeMap, VecDeque},
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
    /// Opaque correlation digests for approvals that have not produced a
    /// matching resolution event. Empty on records written before this field
    /// existed; ingest treats that legacy blocked state conservatively.
    #[serde(default)]
    pub pending_approval_keys: Vec<String>,
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
        // A terminal hook is proof that the turn ended. Normalize the invalid
        // combination observed in a live schema-2 store (`hook_terminal: true`
        // with `lifecycle: working`) so the next daemon snapshot repairs the
        // UI immediately instead of waiting up to the stale-working TTL.
        if record.hook_terminal {
            record.lifecycle = tmux_agent_protocol::v1::AgentLifecycleState::Idle as i32;
            record.pending_approval_keys.clear();
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
        assert!(record.pending_approval_keys.is_empty());
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
                  "seen_generation": 1,
                  "updated_at_unix_millis": 1786000000000,
                  "hook_authority_expires_at_unix_millis": 0,
                  "detected_manually": false,
                  "present": true
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
        assert_eq!(record.lifecycle, 3);
        assert_eq!(record.attention_kind, "completed");
        assert_eq!(record.seen_generation, 1);
        assert_eq!(record.route.pane_id, "%1");
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
                updated_at_unix_millis: 1,
                hook_authority_expires_at_unix_millis: 1,
                detected_manually: false,
                source_event_ids: VecDeque::new(),
                latest_source_generation: 0,
                present: true,
                hook_terminal: true,
                pending_approval_keys: Vec::new(),
                lifecycle_observed_at_unix_millis: 1,
            },
        );
        persist(&path, &state).unwrap();

        let loaded = load(&path);
        assert_eq!(
            loaded.agents["codex:field"].lifecycle,
            tmux_agent_protocol::v1::AgentLifecycleState::Idle as i32,
        );
        fs::remove_file(path).unwrap();
    }
}
