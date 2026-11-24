//! The record file: what the host last saw, and the switch.
//!
//! One JSON file in the state directory, written atomically through
//! [`crate::service::snapshot::save_sidecar`] like every other piece of host
//! state. The version field refuses a file from a future layout rather than
//! half-trusting it, and everything read back is re-checked against the same
//! bounds the writer keeps, so a hand-edited or corrupt file cannot smuggle a
//! path or a name the recreate step would run with.

use serde::{Deserialize, Serialize};

pub(super) const RECORD_VERSION: u32 = 1;
/// Sessions one record may hold. Generously past any tmux server a person
/// really runs, low enough that a corrupt file cannot grow without bound.
const MAX_SESSIONS: usize = 256;
/// Windows one remembered session may hold, mirroring the desktop's cap.
const MAX_WINDOWS: usize = 64;
/// A remembered directory longer than this is not a directory anyone typed.
const MAX_DIRECTORY_BYTES: usize = 4096;
/// A session name is bounded by tmux itself; this is the record's own ceiling.
const MAX_NAME_BYTES: usize = 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct Record {
    version: u32,
    pub enabled: bool,
    /// The `server_identity` the sessions were last seen on. A different one
    /// at the next look means the server died and another is here.
    pub server_identity: String,
    pub sessions: Vec<RememberedSession>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RememberedSession {
    pub name: String,
    /// In window-strip order. The first entry is the session's own first
    /// window; its directory is what `new-session -c` gets.
    pub windows: Vec<RememberedWindow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RememberedWindow {
    /// Empty is tmux's own choice, the same rule `TmuxAction.directory`
    /// follows.
    pub directory: String,
}

pub(super) struct Paths {
    pub record: std::path::PathBuf,
}

impl Paths {
    pub(super) fn under(state_dir: &std::path::Path) -> Self {
        Self {
            record: state_dir.join("workspace-restore.json"),
        }
    }
}

impl Record {
    /// The file as it must be read: absent is default, a wrong version is
    /// default (a future layout must not be half-trusted), and every entry is
    /// re-checked against the writer's bounds. A file that cannot parse is
    /// reported, and the caller keeps the last good record.
    /// A record with the version this host writes. The version field stays
    /// private so no caller can stamp a file with one it does not own.
    pub(super) fn current(
        enabled: bool,
        server_identity: String,
        sessions: Vec<RememberedSession>,
    ) -> Self {
        Self {
            version: RECORD_VERSION,
            enabled,
            server_identity,
            sessions,
        }
    }

    pub(super) fn load(paths: &Paths) -> anyhow::Result<Self> {
        let read: Record =
            crate::service::snapshot::load_sidecar(&paths.record, "workspace restore")?;
        Ok(read.sanitized())
    }

    pub(super) fn save(&self, paths: &Paths) -> anyhow::Result<()> {
        debug_assert_eq!(self.version, RECORD_VERSION);
        crate::service::snapshot::save_sidecar(&paths.record, "workspace restore", self)
    }

    pub(super) fn sanitized(self) -> Self {
        if self.version != RECORD_VERSION {
            return Self::default();
        }
        let mut sessions = Vec::new();
        let mut names = std::collections::BTreeSet::new();
        for session in self.sessions.into_iter().take(MAX_SESSIONS) {
            if session.name.is_empty()
                || session.name.len() > MAX_NAME_BYTES
                || session.name.chars().any(char::is_control)
                || session.windows.is_empty()
                || session.windows.len() > MAX_WINDOWS
                // Names match by name when the restore runs, so two of one
                // name is a record nothing could honor.
                || !names.insert(session.name.clone())
                || session.windows.iter().any(|window| {
                    !window.directory.is_empty()
                        && (window.directory.len() > MAX_DIRECTORY_BYTES
                            || window.directory.chars().any(char::is_control))
                })
            {
                continue;
            }
            sessions.push(session);
        }
        Self { sessions, ..self }
    }
}
