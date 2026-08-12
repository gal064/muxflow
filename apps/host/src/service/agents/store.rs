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
    pub authority: i32,
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
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|state: &StoredState| state.schema_version == STATE_SCHEMA_VERSION)
        .unwrap_or_default()
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
}
