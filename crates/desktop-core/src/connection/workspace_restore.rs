//! Workspace restore: one host's remembering of its tmux sessions, read and
//! switched through `OPERATION_WORKSPACE_RESTORE`. The host owns the record;
//! this only asks it what it holds and whether the switch is on.

use serde::Serialize;
use tmux_agent_protocol::v1;

use super::{TerminalClients, get_client};

/// One remembered window: the directory its first pane was last in. Empty is
/// tmux's own choice.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RememberedWindow {
    pub directory: String,
}

/// One remembered session, named the way tmux names it.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RememberedSession {
    pub name: String,
    pub windows: Vec<RememberedWindow>,
}

/// What a host answers for `GET` and `SET_ENABLED`: the switch, and the
/// layout the host would recreate after its tmux server dies.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRestoreState {
    pub enabled: bool,
    pub sessions: Vec<RememberedSession>,
}

/// Read the host's switch and record (`set_enabled` is `None`), or move the
/// switch to `set_enabled` and answer with the record that follows.
pub async fn workspace_restore_request(
    client_id: String,
    set_enabled: Option<bool>,
    clients: &TerminalClients,
) -> Result<WorkspaceRestoreState, String> {
    let request = v1::Request {
        operation: v1::Operation::WorkspaceRestore.into(),
        workspace_restore: Some(v1::WorkspaceRestoreRequest {
            action: match set_enabled {
                None => v1::WorkspaceRestoreAction::Get,
                Some(_) => v1::WorkspaceRestoreAction::SetEnabled,
            }
            .into(),
            enabled: set_enabled.unwrap_or(false),
        }),
        ..Default::default()
    };
    let client = get_client(clients, &client_id)?;
    let response = crate::platform::spawn_blocking(move || client.request(request))
        .await
        .map_err(|error| format!("workspace restore request task failed: {error}"))??;
    response
        .workspace_restore
        .as_ref()
        .map(state_from)
        .ok_or_else(|| "host omitted workspace restore response".into())
}

fn state_from(answer: &v1::WorkspaceRestoreResponse) -> WorkspaceRestoreState {
    WorkspaceRestoreState {
        enabled: answer.enabled,
        sessions: answer
            .sessions
            .iter()
            .map(|session| RememberedSession {
                name: session.name.clone(),
                windows: session
                    .windows
                    .iter()
                    .map(|window| RememberedWindow {
                        directory: window.directory.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}
