use std::sync::Arc;

use serde_json::{Value, json};
use tauri::{
    State,
    ipc::{Channel, InvokeResponseBody},
};
use tmux_agent_protocol::v1;
use uuid::Uuid;

use super::bulk_pool::BulkLease;
use super::bulk_protocol::{Exchange, RequestFailure};
use super::file_stream::run_file_read;
use super::scheduler::{
    BulkBinding, CancelReason, CancelState, QueuedPublication, cancel_transfer,
    enqueue_transfer_with_queued,
};
use super::serialization::metadata_json;
use super::transfer_event::{
    CleanupStatus, TransferEvent, TransferFailure, TransferFailureKind, TransferOutcome,
    TransferState, late_queued_publication_rollback,
};
use super::{BULK_CHUNK_BYTES, parse_optional_u64, parse_required_u64};
use crate::connection::{ConnectionSpec, ProfileStore, TerminalClients, get_client};

/// One editor file-I/O job: what both directions carry, and which one it is.
///
/// This was two structs sharing eight fields, wrapped in a two-variant enum
/// whose entire body was four accessors — each a two-arm match returning a
/// field *both* variants had. The shape says the true thing instead: reading
/// and writing a file differ in three values, not in eleven.
pub(super) struct FileJob {
    pub(super) transfer_id: String,
    pub(super) connection: ConnectionSpec,
    pub(super) root: String,
    pub(super) root_token: String,
    pub(super) path: String,
    pub(super) binding: BulkBinding,
    pub(super) cancellation: Arc<CancelState>,
    pub(super) channel: Channel<InvokeResponseBody>,
    pub(super) kind: FileJobKind,
}

/// The three values a write needs and a read does not.
pub(super) enum FileJobKind {
    Read,
    Write(FileWrite),
}

pub(super) struct FileWrite {
    pub(super) operation_id: String,
    pub(super) file_generation: u64,
    pub(super) content: Arc<[u8]>,
}

#[derive(Clone, Default)]
pub struct FileIoManager;

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn start_file_read(
    client_id: String,
    profile_id: String,
    expected_server_identity: String,
    connection_epoch: String,
    root: String,
    root_token: String,
    path: String,
    on_event: Channel<InvokeResponseBody>,
    profiles: State<'_, ProfileStore>,
    clients: State<'_, TerminalClients>,
    file_io: State<'_, FileIoManager>,
) -> Result<String, String> {
    if root.is_empty() || root_token.is_empty() || path.is_empty() {
        return Err("root snapshot and file path are required".into());
    }
    let binding = BulkBinding::capture(
        get_client(&clients, &client_id)?,
        expected_server_identity,
        parse_required_u64("connectionEpoch", &connection_epoch)?,
    )?;
    let transfer_id = Uuid::new_v4().to_string();
    let cancellation = Arc::new(CancelState::new());
    file_io.enqueue(FileJob {
        transfer_id: transfer_id.clone(),
        connection: profiles.connection_for(&profile_id)?,
        root,
        root_token,
        path,
        binding,
        cancellation,
        channel: on_event,
        kind: FileJobKind::Read,
    })?;
    Ok(transfer_id)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn start_file_write(
    client_id: String,
    profile_id: String,
    expected_server_identity: String,
    connection_epoch: String,
    root: String,
    root_token: String,
    path: String,
    operation_id: String,
    file_generation: String,
    content: Vec<u8>,
    on_event: Channel<InvokeResponseBody>,
    profiles: State<'_, ProfileStore>,
    clients: State<'_, TerminalClients>,
    file_io: State<'_, FileIoManager>,
) -> Result<String, String> {
    if root.is_empty() || root_token.is_empty() || path.is_empty() || operation_id.is_empty() {
        return Err("root snapshot, file path, and operation ID are required".into());
    }
    if content.len() > 10 * 1024 * 1024 {
        return Err("text content exceeds the 10 MiB editor limit".into());
    }
    if content.contains(&0) || std::str::from_utf8(&content).is_err() {
        return Err("editor writes require valid UTF-8 text without NUL bytes".into());
    }
    let binding = BulkBinding::capture(
        get_client(&clients, &client_id)?,
        expected_server_identity,
        parse_required_u64("connectionEpoch", &connection_epoch)?,
    )?;
    let transfer_id = Uuid::new_v4().to_string();
    let cancellation = Arc::new(CancelState::new());
    file_io.enqueue(FileJob {
        transfer_id: transfer_id.clone(),
        connection: profiles.connection_for(&profile_id)?,
        root,
        root_token,
        path,
        binding,
        cancellation,
        channel: on_event,
        kind: FileJobKind::Write(FileWrite {
            operation_id,
            file_generation: parse_optional_u64("fileGeneration", &file_generation)?,
            content: Arc::from(content),
        }),
    })?;
    Ok(transfer_id)
}

#[tauri::command]
pub fn cancel_file_io(
    transfer_id: String,
    file_io: State<'_, FileIoManager>,
) -> Result<(), String> {
    file_io.cancel(&transfer_id)
}

impl FileIoManager {
    fn cancel(&self, transfer_id: &str) -> Result<(), String> {
        cancel_transfer(transfer_id).map(|_| ())
    }

    /// One `Arc`, three closures. The job used to be cloned once per closure —
    /// three deep copies of every string, capability, and channel handle for
    /// three read-only borrows.
    fn enqueue(&self, job: FileJob) -> Result<(), String> {
        let transfer_id = job.transfer_id.clone();
        let cancellation = Arc::clone(&job.cancellation);
        let binding = job.binding.clone();
        let queued = QueuedPublication::raw(
            job.channel.clone(),
            file_json_frame(
                1,
                TransferEvent::new(&job.transfer_id, &job.binding, TransferState::Queued).value(),
            )?,
            file_json_frame(
                4,
                late_queued_publication_rollback(&job.transfer_id, &job.binding),
            )?,
            "could not publish queued file transfer event",
        );
        let job = Arc::new(job);
        let started_job = Arc::clone(&job);
        let work_job = Arc::clone(&job);
        let finished_job = job;
        enqueue_transfer_with_queued(
            transfer_id,
            binding,
            cancellation,
            queued,
            move || emit_file_job_state(&started_job, 1, TransferState::Running),
            move || match &work_job.kind {
                FileJobKind::Read => {
                    run_file_read(&work_job).map_err(TransferFailure::not_published)
                }
                FileJobKind::Write(write) => run_file_write(&work_job, write),
            },
            move |result, _reason| {
                if let Err(failure) = result {
                    let state = if finished_job.cancellation.reason() == CancelReason::User
                        && failure.outcome == TransferOutcome::NotPublished
                    {
                        TransferState::Cancelled
                    } else {
                        TransferState::Failed
                    };
                    emit_file_failure(&finished_job, 4, state, failure);
                }
            },
        )
    }
}

fn emit_file_failure(job: &FileJob, kind: u8, state: TransferState, failure: TransferFailure) {
    let event = TransferEvent::new(&job.transfer_id, &job.binding, state)
        .outcome(failure.outcome)
        .cleanup(failure.cleanup_status, failure.cleanup_error);
    let event = if state == TransferState::Cancelled {
        event.error(failure.error)
    } else {
        event.failure(failure.failure_kind, failure.error)
    };
    emit_file_json(&job.channel, kind, event.value());
}

fn emit_file_job_state(job: &FileJob, kind: u8, state: TransferState) {
    let _ = send_file_json(
        &job.channel,
        kind,
        TransferEvent::new(&job.transfer_id, &job.binding, state).value(),
    );
}

fn run_file_write(job: &FileJob, write: &FileWrite) -> Result<(), TransferFailure> {
    job.binding.validate()?;
    // Not `_deadline`: it is refreshed by every frame and chunk below, so an
    // underscore would have said the opposite of what it does.
    let deadline = job.cancellation.arm_inactivity_deadline();
    let mut lease =
        BulkLease::acquire(&job.connection, &job.binding, &job.cancellation, &deadline)?;
    // A guard: dropping it unbinds. Named for that rather than for being
    // unread.
    let _process_binding_guard = job.cancellation.bind_process(lease.process_id())?;
    let mut protocol = lease.client();
    protocol.request(
        v1::Request {
            operation: v1::Operation::BeginFileWrite.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: write.operation_id.clone(),
                root: job.root.clone(),
                root_token: job.root_token.clone(),
                path: job.path.clone(),
                transfer_id: job.transfer_id.clone(),
                file_generation: write.file_generation,
                total_bytes: write.content.len() as u64,
                ..Default::default()
            }),
            ..Default::default()
        },
        Exchange::live(&job.cancellation, &deadline),
    )?;
    deadline.touch();
    let mut offset = 0_u64;
    for chunk in write.content.chunks(BULK_CHUNK_BYTES as usize) {
        if job.cancellation.is_cancelled() {
            let _ = protocol.cancel_write(&write.operation_id, &job.transfer_id);
            return Err("file write cancelled".into());
        }
        let next = offset
            .checked_add(chunk.len() as u64)
            .ok_or("file write byte counter overflow")?;
        protocol.request(
            v1::Request {
                operation: v1::Operation::WriteFileChunk.into(),
                file: Some(v1::FileServiceRequest {
                    operation_id: write.operation_id.clone(),
                    transfer_id: job.transfer_id.clone(),
                    offset,
                    total_bytes: write.content.len() as u64,
                    content: chunk.to_vec(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Exchange::live(&job.cancellation, &deadline),
        )?;
        deadline.touch();
        offset = next;
        emit_scoped_file_json(
            &job.channel,
            &job.transfer_id,
            &job.binding,
            1,
            TransferState::Running,
            json!({
                "operationId": write.operation_id,
                "transferredBytes": offset.to_string(),
                "totalBytes": write.content.len().to_string(),
            }),
        );
    }
    let digest = blake3::hash(&write.content).to_hex().to_string();
    if job.cancellation.is_cancelled() {
        let _ = protocol.cancel_write(&write.operation_id, &job.transfer_id);
        return Err("file write cancelled before commit".into());
    }
    job.binding.validate()?;
    job.cancellation.prepare_finalize()?;
    emit_scoped_file_json(
        &job.channel,
        &job.transfer_id,
        &job.binding,
        1,
        TransferState::Verifying,
        json!({
            "operationId": write.operation_id,
            "transferredBytes": offset.to_string(),
            "totalBytes": write.content.len().to_string(),
        }),
    );
    let response = protocol
        .request_classified(
            v1::Request {
                operation: v1::Operation::CommitFileWrite.into(),
                file: Some(v1::FileServiceRequest {
                    operation_id: write.operation_id.clone(),
                    transfer_id: job.transfer_id.clone(),
                    blake3: digest.clone(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Exchange::bounded(&deadline),
        )
        .map_err(|error| match error {
            RequestFailure::Remote { code, message, .. } => {
                TransferFailure::not_published(format!("{code}: {message}"))
            }
            RequestFailure::Transport(error) => TransferFailure::unknown(
                match job.cancellation.reason() {
                    CancelReason::StaleBinding => TransferFailureKind::StaleScope,
                    CancelReason::Timeout => TransferFailureKind::Timeout,
                    _ => TransferFailureKind::OutcomeUnknown,
                },
                format!("file write commit response was lost: {error}"),
            ),
            RequestFailure::Cancelled => {
                TransferFailure::not_published("file write commit cancelled before dispatch")
            }
        })?;
    deadline.touch();
    let metadata = response
        .file
        .and_then(|file| file.metadata)
        .ok_or("file write commit omitted metadata")?;
    if metadata.size != write.content.len() as u64 {
        return Err("file write commit byte verification failed".into());
    }
    let stale = job.binding.validate().err();
    if let Some(error) = stale {
        let value = TransferEvent::new(&job.transfer_id, &job.binding, TransferState::Failed)
            .outcome(TransferOutcome::Published)
            .failure(
                TransferFailureKind::StaleScope,
                format!("file write was published after its captured scope became stale: {error}"),
            )
            .cleanup(CleanupStatus::NotNeeded, None)
            .fields(json!({
                "operationId": write.operation_id,
                "transferredBytes": metadata.size.to_string(),
                "totalBytes": metadata.size.to_string(),
                "generation": metadata.generation.to_string(),
                "blake3": digest,
                "metadata": metadata_json(&metadata),
            }))
            .value();
        emit_file_json(&job.channel, 4, value);
        return Ok(());
    }
    emit_scoped_file_json(
        &job.channel,
        &job.transfer_id,
        &job.binding,
        3,
        TransferState::Completed,
        json!({
            "operationId": write.operation_id,
            "outcome": TransferOutcome::Published,
            "cleanupStatus": CleanupStatus::NotNeeded,
            "transferredBytes": metadata.size.to_string(),
            "totalBytes": metadata.size.to_string(),
            "generation": metadata.generation.to_string(),
            "blake3": digest,
            "metadata": metadata_json(&metadata),
        }),
    );
    Ok(())
}

pub(super) fn content_kind_name(kind: v1::FileContentKind) -> &'static str {
    match kind {
        v1::FileContentKind::Text => "text",
        v1::FileContentKind::Binary => "binary",
        v1::FileContentKind::Image => "image",
        v1::FileContentKind::TooLarge => "tooLarge",
        v1::FileContentKind::Unspecified => "unspecified",
    }
}

fn emit_file_json(channel: &Channel<InvokeResponseBody>, kind: u8, value: Value) {
    let _ = send_file_json(channel, kind, value);
}

fn send_file_json(
    channel: &Channel<InvokeResponseBody>,
    kind: u8,
    value: Value,
) -> Result<(), String> {
    let frame = file_json_frame(kind, value)?;
    channel
        .send(InvokeResponseBody::Raw(frame))
        .map_err(|error| format!("could not publish queued file transfer event: {error}"))
}

fn file_json_frame(kind: u8, value: Value) -> Result<Vec<u8>, String> {
    let mut frame = vec![kind];
    frame.extend_from_slice(
        &serde_json::to_vec(&value)
            .map_err(|error| format!("could not serialize file transfer event: {error}"))?,
    );
    Ok(frame)
}

pub(super) fn emit_scoped_file_json(
    channel: &Channel<InvokeResponseBody>,
    transfer_id: &str,
    binding: &BulkBinding,
    kind: u8,
    state: TransferState,
    extra: Value,
) {
    emit_file_json(
        channel,
        kind,
        TransferEvent::new(transfer_id, binding, state)
            .fields(extra)
            .value(),
    );
}

/// Writes one content frame, reporting whether the renderer is still listening.
pub(super) fn emit_file_chunk(
    channel: &Channel<InvokeResponseBody>,
    offset: u64,
    data: &[u8],
) -> bool {
    let mut frame = Vec::with_capacity(9 + data.len());
    frame.push(2);
    frame.extend_from_slice(&offset.to_be_bytes());
    frame.extend_from_slice(data);
    channel.send(InvokeResponseBody::Raw(frame)).is_ok()
}
