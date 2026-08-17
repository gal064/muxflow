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
mod destination_lease;
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
pub(super) use scheduler::invalidate_bulk_scope;
mod transfer_event;
pub(crate) mod upload_manager;
pub use upload_manager::UploadManager;

pub(super) const BULK_CHUNK_BYTES: u32 = 1024 * 1024;
pub(super) const MAX_QUEUED_TRANSFERS: usize = 128;

fn identity_component<T>(value: T, label: &str) -> Result<u64, String>
where
    T: TryInto<u64>,
{
    value
        .try_into()
        .map_err(|_| format!("invalid {label} identity"))
}

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
    #[serde(default)]
    pub known_root_token: String,
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
        known_root_token: command.known_root_token,
        ..Default::default()
    };
    let client = get_client(&clients, &client_id)?;
    // Reads only. A mutation that could be cancelled mid-flight would leave the
    // caller unable to say whether it happened, and this app never lets a
    // remote mutation reach an unknown outcome it could have avoided.
    let cancellable = matches!(
        operation,
        v1::Operation::ListDirectory | v1::Operation::WatchDirectory
    );
    // Claimed here, on the command thread, before anything is dispatched: a
    // caller that abandons its read in the same tick must find something to
    // cancel rather than a slot the worker has not created yet.
    let claim = (cancellable && !request.operation_id.is_empty())
        .then(|| client.claim_file_operation(&request.operation_id))
        .transpose()?;
    let request = v1::Request {
        operation: operation.into(),
        file: Some(request),
        ..Default::default()
    };
    let response =
        tauri::async_runtime::spawn_blocking(move || client.request_file(request, claim))
            .await
            .map_err(|error| format!("file request task failed: {error}"))??;
    let file = response.file.ok_or("host omitted file-service response")?;
    Ok(file_response_json(&file))
}

/// Cancels an in-flight control-lane file request by its renderer operation ID.
///
/// The renderer owns the operation ID from the moment it issues the request, so
/// a collapse, root replacement, or superseded preview can stop bounded remote
/// enumeration that nothing will read — rather than paying for it and throwing
/// the answer away.
/// Async, and off the main thread, for the same reason `file_request` is: this
/// reaches the control writer, which waits on a full queue and on the physical
/// write. On the interaction path it is now issued by every collapse, root
/// swap, and superseded preview, so a synchronous version would put a
/// potentially multi-second stall on exactly the interactions whose budget is
/// "no long task".
#[tauri::command]
pub async fn cancel_file_request(
    client_id: String,
    operation_id: String,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    if operation_id.is_empty() {
        return Err("operation ID is required".into());
    }
    let client = get_client(&clients, &client_id)?;
    tauri::async_runtime::spawn_blocking(move || client.cancel_file(&operation_id))
        .await
        .map_err(|error| error.to_string())?
}

fn operation_from_name(value: &str) -> Result<v1::Operation, String> {
    match value {
        "resolveActiveRoot" => Ok(v1::Operation::ResolveActiveRoot),
        "listDirectory" => Ok(v1::Operation::ListDirectory),
        "watchDirectory" => Ok(v1::Operation::WatchDirectory),
        "unwatchDirectory" => Ok(v1::Operation::UnwatchDirectory),
        "mutate" => Ok(v1::Operation::FileMutation),
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
    use super::scheduler::{BulkBinding, CancelState};
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

    /// A read abandoned before its request reached a worker thread must still
    /// be cancellable: the claim exists from the moment the command thread
    /// takes the operation ID, and a cancellation that beats dispatch refuses
    /// the request rather than letting it run for an answer nobody reads.
    #[test]
    fn cancelling_before_dispatch_refuses_the_request_instead_of_losing_it() {
        let client = Arc::new(TerminalClient::new());
        client.ready.store(true, Ordering::Release);
        let claim = client.claim_file_operation("op-1").unwrap();
        // Claimed twice is a caller bug, and stays one.
        assert!(client.claim_file_operation("op-1").is_err());
        client.cancel_file("op-1").unwrap();
        let refused = client
            .request_file(v1::Request::default(), Some(claim))
            .unwrap_err();
        assert!(refused.starts_with("cancelled"), "got {refused}");
    }

    /// The abort and the request are separate messages across the command
    /// boundary, so the abort really can arrive first. It must still stop the
    /// request rather than let a remote scan run for an answer nobody reads.
    #[test]
    fn cancelling_a_file_read_before_its_request_is_claimed_still_refuses_it() {
        let client = Arc::new(TerminalClient::new());
        client.ready.store(true, Ordering::Release);
        client.cancel_file("racing").unwrap();
        let claim = client.claim_file_operation("racing").unwrap();
        let refused = client
            .request_file(v1::Request::default(), Some(claim))
            .unwrap_err();
        assert!(refused.starts_with("cancelled"), "got {refused}");
    }

    /// Requests that never reach a host must still leave the registry empty:
    /// one retained entry per request is unbounded growth on a long session.
    #[test]
    fn refused_requests_leave_no_operation_behind_on_either_lane() {
        let client = Arc::new(TerminalClient::new());
        for round in 0..32 {
            let id = format!("op-{round}");
            let claim = client.claim_file_operation(&id).unwrap();
            // Not ready, so the request is refused before it is written.
            assert!(
                client
                    .request_file(v1::Request::default(), Some(claim))
                    .is_err()
            );
            assert!(client.request_git(v1::Request::default(), &id).is_err());
        }
        assert_eq!(client.operations.len(), 0);
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
