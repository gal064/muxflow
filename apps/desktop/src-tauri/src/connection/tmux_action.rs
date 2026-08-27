use serde::{Deserialize, Serialize};
use tauri::State;
use tmux_agent_protocol::v1;

use super::{TerminalClients, get_client};

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TmuxActionKindWire {
    CreateSession,
    RenameSession,
    ReorderSession,
    SelectSession,
    CloseSession,
    CreateWindow,
    RenameWindow,
    ReorderWindow,
    SelectWindow,
    CloseWindow,
    SplitPaneRight,
    SplitPaneDown,
    FocusPane,
    ResizePaneLeft,
    ResizePaneRight,
    ResizePaneUp,
    ResizePaneDown,
    ZoomPane,
    ClosePane,
}

impl From<TmuxActionKindWire> for v1::TmuxActionKind {
    fn from(value: TmuxActionKindWire) -> Self {
        match value {
            TmuxActionKindWire::CreateSession => Self::CreateSession,
            TmuxActionKindWire::RenameSession => Self::RenameSession,
            TmuxActionKindWire::ReorderSession => Self::ReorderSession,
            TmuxActionKindWire::SelectSession => Self::SelectSession,
            TmuxActionKindWire::CloseSession => Self::CloseSession,
            TmuxActionKindWire::CreateWindow => Self::CreateWindow,
            TmuxActionKindWire::RenameWindow => Self::RenameWindow,
            TmuxActionKindWire::ReorderWindow => Self::ReorderWindow,
            TmuxActionKindWire::SelectWindow => Self::SelectWindow,
            TmuxActionKindWire::CloseWindow => Self::CloseWindow,
            TmuxActionKindWire::SplitPaneRight => Self::SplitPaneRight,
            TmuxActionKindWire::SplitPaneDown => Self::SplitPaneDown,
            TmuxActionKindWire::FocusPane => Self::FocusPane,
            TmuxActionKindWire::ResizePaneLeft => Self::ResizePaneLeft,
            TmuxActionKindWire::ResizePaneRight => Self::ResizePaneRight,
            TmuxActionKindWire::ResizePaneUp => Self::ResizePaneUp,
            TmuxActionKindWire::ResizePaneDown => Self::ResizePaneDown,
            TmuxActionKindWire::ZoomPane => Self::ZoomPane,
            TmuxActionKindWire::ClosePane => Self::ClosePane,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WindowRelativePositionWire {
    #[default]
    Unspecified,
    Before,
    After,
}

impl From<WindowRelativePositionWire> for v1::WindowRelativePosition {
    fn from(value: WindowRelativePositionWire) -> Self {
        match value {
            WindowRelativePositionWire::Unspecified => Self::Unspecified,
            WindowRelativePositionWire::Before => Self::Before,
            WindowRelativePositionWire::After => Self::After,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct TmuxActionWire {
    kind: TmuxActionKindWire,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    window_id: String,
    #[serde(default)]
    pane_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    index: u32,
    #[serde(default)]
    split_size: u32,
    #[serde(default)]
    resize_cells: u32,
    #[serde(default)]
    zoomed: bool,
    #[serde(default)]
    expected_server_identity: String,
    #[serde(default)]
    expected_generation: u64,
    #[serde(default)]
    confirmed: bool,
    #[serde(default)]
    target_window_id: String,
    #[serde(default)]
    relative_position: WindowRelativePositionWire,
    /// The created session's start directory, resolved on the host. Empty
    /// means "wherever tmux would have started it".
    #[serde(default)]
    directory: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TmuxActionResultWire {
    #[serde(skip_serializing_if = "String::is_empty")]
    session_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    window_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pane_id: String,
    topology_generation: u64,
}

/// Async so a create, split, or tab switch never freezes the WebView's main
/// thread for the length of the host round trip. The user's report that creates
/// take seconds was mostly this: the UI could not repaint while it waited.
#[tauri::command]
pub async fn tmux_action(
    client_id: String,
    action: TmuxActionWire,
    clients: State<'_, TerminalClients>,
) -> Result<TmuxActionResultWire, String> {
    let client = get_client(&clients, &client_id)?;
    let request = v1::Request {
        operation: v1::Operation::TmuxAction.into(),
        tmux_action: Some(v1::TmuxAction {
            kind: v1::TmuxActionKind::from(action.kind).into(),
            session_id: action.session_id,
            window_id: action.window_id,
            pane_id: action.pane_id,
            name: action.name,
            index: action.index,
            split_size: action.split_size,
            resize_cells: action.resize_cells,
            zoomed: action.zoomed,
            expected_server_identity: action.expected_server_identity,
            expected_generation: action.expected_generation,
            confirmed: action.confirmed,
            target_window_id: action.target_window_id,
            relative_position: v1::WindowRelativePosition::from(action.relative_position).into(),
            directory: action.directory,
        }),
        ..Default::default()
    };
    let response = tauri::async_runtime::spawn_blocking(move || {
        client.flush_input()?;
        client.request(request)
    })
    .await
    .map_err(|error| format!("tmux action task failed: {error}"))??;
    let result = response
        .tmux_action_result
        .ok_or_else(|| "host omitted tmux action result".to_owned())?;
    Ok(TmuxActionResultWire {
        session_id: result.session_id,
        window_id: result.window_id,
        pane_id: result.pane_id,
        topology_generation: result.topology_generation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_matches_frontend_shape_and_relative_reorder_contracts() {
        let action: TmuxActionWire = serde_json::from_value(serde_json::json!({
            "kind": "reorderWindow",
            "session_id": "$1",
            "window_id": "@5",
            "target_window_id": "@3",
            "relative_position": "before",
            "expected_server_identity": "socket:42",
            "expected_generation": 9,
            "confirmed": true,
            "directory": "/work/projects"
        }))
        .unwrap();
        assert!(matches!(action.kind, TmuxActionKindWire::ReorderWindow));
        assert_eq!(action.target_window_id, "@3");
        assert!(matches!(
            action.relative_position,
            WindowRelativePositionWire::Before
        ));
        assert_eq!(action.expected_generation, 9);
        assert!(action.confirmed);
        assert_eq!(action.directory, "/work/projects");
    }
}
