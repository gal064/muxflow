#[cfg(test)]
use std::sync::Arc;

use super::{TerminalClients, get_client};
use serde::Deserialize;
use serde_json::Value;
use tauri::State;
use tmux_agent_protocol::v1;

pub(crate) mod bulk_pool;
mod bulk_protocol;
mod cleanup;
mod clipboard_staging;
pub(crate) mod download_manager;
mod download_naming;
pub(crate) mod download_opener;
mod local_destination;
mod local_staging;
#[cfg(test)]
mod manager_acceptance;
pub(crate) mod native_clipboard;
pub use download_manager::DownloadManager;
pub(crate) mod editor_manager;
pub use editor_manager::FileIoManager;
mod serialization;
pub(crate) use serialization::{file_event_json, file_response_json};
mod scheduler;
mod transfer_event;
pub(crate) mod upload_manager;
pub use upload_manager::UploadManager;

pub(super) const BULK_CHUNK_BYTES: u32 = 1024 * 1024;
pub(super) const MAX_QUEUED_TRANSFERS: usize = 128;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileCommand {
    pub operation: String,
    #[serde(default)]
    pub operation_id: String,
    #[serde(default)]
    pub root: String,
    #[serde(default)]
    pub root_token: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub destination: String,
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub expected_server_identity: String,
    #[serde(default)]
    pub expected_topology_generation: String,
    #[serde(default)]
    pub mutation: String,
    #[serde(default)]
    pub create_directory: bool,
    #[serde(default)]
    pub overwrite_confirmed: bool,
    #[serde(default)]
    pub non_empty_confirmed: bool,
    #[serde(default)]
    pub file_generation: String,
    #[serde(default)]
    pub content: Vec<u8>,
    #[serde(default)]
    pub watch_id: String,
    #[serde(default)]
    pub page_token: String,
    #[serde(default)]
    pub page_size: u32,
}

/// Async so the host round trip never runs on the WebView's main thread: a
/// blocking command freezes every other interaction — typing included — for the
/// whole trip, which over SSH is an entire RTT.
#[tauri::command]
pub async fn file_request(
    client_id: String,
    command: FileCommand,
    clients: State<'_, TerminalClients>,
) -> Result<Value, String> {
    let operation = operation_from_name(&command.operation)?;
    let request = v1::FileServiceRequest {
        operation_id: command.operation_id,
        root: command.root,
        path: command.path,
        destination: command.destination,
        pane_id: command.pane_id,
        expected_server_identity: command.expected_server_identity,
        expected_topology_generation: parse_optional_u64(
            "expectedTopologyGeneration",
            &command.expected_topology_generation,
        )?,
        mutation: mutation_from_name(&command.mutation)?.into(),
        create_directory: command.create_directory,
        overwrite_confirmed: command.overwrite_confirmed,
        non_empty_confirmed: command.non_empty_confirmed,
        file_generation: parse_optional_u64("fileGeneration", &command.file_generation)?,
        content: command.content,
        watch_id: command.watch_id,
        root_token: command.root_token,
        page_token: command.page_token,
        page_size: command.page_size,
        ..Default::default()
    };
    let client = get_client(&clients, &client_id)?;
    let request = v1::Request {
        operation: operation.into(),
        file: Some(request),
        ..Default::default()
    };
    let response = tauri::async_runtime::spawn_blocking(move || client.request(request))
        .await
        .map_err(|error| format!("file request task failed: {error}"))??;
    let file = response.file.ok_or("host omitted file-service response")?;
    Ok(file_response_json(&file))
}

fn operation_from_name(value: &str) -> Result<v1::Operation, String> {
    match value {
        "resolveActiveRoot" => Ok(v1::Operation::ResolveActiveRoot),
        "listDirectory" => Ok(v1::Operation::ListDirectory),
        "watchDirectory" => Ok(v1::Operation::WatchDirectory),
        "unwatchDirectory" => Ok(v1::Operation::UnwatchDirectory),
        "mutate" => Ok(v1::Operation::FileMutation),
        "readFile" => Ok(v1::Operation::ReadFile),
        "writeFile" => Err("writeFile bodies must use start_file_write on the bulk route".into()),
        _ => Err(format!("unsupported file operation {value}")),
    }
}

fn mutation_from_name(value: &str) -> Result<v1::FileMutationKind, String> {
    match value {
        "" => Ok(v1::FileMutationKind::Unspecified),
        "create" => Ok(v1::FileMutationKind::Create),
        "rename" => Ok(v1::FileMutationKind::Rename),
        "move" => Ok(v1::FileMutationKind::Move),
        "duplicate" => Ok(v1::FileMutationKind::Duplicate),
        "delete" => Ok(v1::FileMutationKind::Delete),
        _ => Err(format!("unsupported file mutation {value}")),
    }
}

fn parse_optional_u64(label: &str, value: &str) -> Result<u64, String> {
    if value.is_empty() {
        Ok(0)
    } else {
        value
            .parse::<u64>()
            .map_err(|_| format!("{label} must be a decimal u64 string"))
    }
}

pub(super) fn parse_required_u64(label: &str, value: &str) -> Result<u64, String> {
    let parsed = parse_optional_u64(label, value)?;
    if parsed == 0 {
        Err(format!("{label} must be a non-zero decimal u64 string"))
    } else {
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::scheduler::{BulkBinding, CancelState, acquire_bulk_permit};
    use super::serialization::metadata_json;
    use super::*;
    use crate::connection::TerminalClient;
    use std::{process::Command, sync::atomic::Ordering};

    #[test]
    fn js_file_json_preserves_u64_values_above_safe_integer_as_strings() {
        let unsafe_integer = (1_u64 << 53) + 17;
        let value = metadata_json(&v1::FileMetadata {
            size: unsafe_integer,
            generation: u64::MAX,
            ..Default::default()
        });
        assert_eq!(value["size"], unsafe_integer.to_string());
        assert_eq!(value["generation"], u64::MAX.to_string());
        assert_eq!(
            parse_optional_u64("fileGeneration", &u64::MAX.to_string()).unwrap(),
            u64::MAX
        );
        let event = file_event_json(&v1::FileServiceEvent {
            root_token: "root-capability".into(),
            watch_id: "editor-parent".into(),
            ..Default::default()
        });
        assert_eq!(event["rootToken"], "root-capability");
        assert_eq!(event["watchId"], "editor-parent");
    }

    #[test]
    fn global_bulk_permit_never_exceeds_two_connections() {
        let _serial = scheduler::engine_test_lock();
        let cancellation = CancelState::new();
        let first = acquire_bulk_permit(&cancellation).unwrap();
        let second = acquire_bulk_permit(&cancellation).unwrap();
        let cancelled = CancelState::new();
        cancelled.cancel();
        assert!(acquire_bulk_permit(&cancelled).is_err());
        drop(first);
        drop(second);
    }

    #[test]
    fn bulk_binding_rejects_replaced_epoch_and_server() {
        let client = Arc::new(TerminalClient::new());
        client.ready.store(true, Ordering::Release);
        client.terminal_epoch.store(41, Ordering::Release);
        *client.server_identity.lock().unwrap() = "server-a".into();
        assert!(BulkBinding::capture(Arc::clone(&client), "server-a".into(), 41).is_ok());
        assert!(BulkBinding::capture(Arc::clone(&client), "server-a".into(), 40).is_err());
        assert!(BulkBinding::capture(client, "server-b".into(), 41).is_err());
    }

    #[test]
    fn user_cancellation_leaves_live_helper_for_cooperative_cleanup() {
        let cancellation = CancelState::new();
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let binding = cancellation.bind_process(child.id()).unwrap();
        cancellation.cancel();
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        let _ = child.wait();
        drop(binding);
    }

    #[test]
    fn finalizing_cancel_records_intent_without_killing_commit_helper() {
        let cancellation = CancelState::new();
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let binding = cancellation.bind_process(child.id()).unwrap();
        cancellation.prepare_finalize().unwrap();
        cancellation.cancel();
        assert!(cancellation.is_cancelled());
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
        drop(binding);
    }
}
