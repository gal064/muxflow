use std::{
    fs::{File, OpenOptions},
    io::{BufReader, Read, Seek},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{State, ipc::Channel};
use tmux_agent_protocol::v1;
use uuid::Uuid;

use super::bulk_protocol::{BulkProtocolClient, RequestFailure};
use super::cleanup::CleanupReport;
use super::clipboard_staging::lock_owned_source as lock_owned_clipboard_source;
use super::scheduler::{BulkBinding, BulkChild, CancelState, cancel_transfer, enqueue_transfer};
use super::transfer_event::{
    CleanupStatus, TransferEvent, TransferFailure, TransferFailureKind, TransferOutcome,
    TransferResult, TransferState,
};
use super::{BULK_CHUNK_BYTES, parse_required_u64};
use crate::connection::transport::spawn_bulk_bridge;
use crate::connection::{ConnectionSpec, ProfileStore, TerminalClients, get_client};

const LARGE_UPLOAD_BYTES: u64 = 500 * 1024 * 1024;
const MAX_PNG_BYTES: u64 = 25 * 1024 * 1024;
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UploadCollisionPolicy {
    Fail,
    OverwriteConfirmed,
    Rename,
}

impl UploadCollisionPolicy {
    fn protocol(self) -> v1::CollisionPolicy {
        match self {
            Self::Fail => v1::CollisionPolicy::Fail,
            Self::OverwriteConfirmed => v1::CollisionPolicy::OverwriteConfirmed,
            Self::Rename => v1::CollisionPolicy::Rename,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

/// The deliberately local-only result used by the file picker boundary.  This
/// command must not start a helper, reserve a bulk slot, or touch app caches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectedLocalTerminalPath {
    path: String,
    size_bytes: String,
    name: String,
}

#[tauri::command]
pub fn inspect_local_terminal_paths(
    paths: Vec<String>,
) -> Result<Vec<InspectedLocalTerminalPath>, String> {
    paths
        .into_iter()
        .map(|path| {
            let path_ref = Path::new(&path);
            let (_file, identity) = open_regular_source(path_ref)?;
            let name = path_ref
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("upload source basename is not valid UTF-8")?
                .to_owned();
            Ok(InspectedLocalTerminalPath {
                path,
                size_bytes: identity.size.to_string(),
                name,
            })
        })
        .collect()
}

#[derive(Clone)]
struct UploadJob {
    transfer_id: String,
    connection: ConnectionSpec,
    source_path: PathBuf,
    source_identity: SourceIdentity,
    source_guard: Option<Arc<File>>,
    destination_name: String,
    collision: UploadCollisionPolicy,
    large_upload_confirmed: bool,
    image_png: bool,
    binding: BulkBinding,
    cancellation: Arc<CancelState>,
    channel: Channel<Value>,
}

#[derive(Clone)]
struct UploadPreflightJob {
    transfer_id: String,
    connection: ConnectionSpec,
    source_path: PathBuf,
    source_identity: SourceIdentity,
    destination_name: String,
    collision: UploadCollisionPolicy,
    large_upload_confirmed: bool,
    image_png: bool,
    binding: BulkBinding,
    cancellation: Arc<CancelState>,
    channel: Channel<Value>,
}

#[derive(Clone, Default)]
pub struct UploadManager;

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn start_terminal_upload_preflight(
    client_id: String,
    profile_id: String,
    expected_server_identity: String,
    connection_epoch: String,
    source_path: String,
    destination_name: String,
    collision: UploadCollisionPolicy,
    large_upload_confirmed: bool,
    image_png: bool,
    on_event: Channel<Value>,
    profiles: State<'_, ProfileStore>,
    clients: State<'_, TerminalClients>,
    uploads: State<'_, UploadManager>,
) -> Result<String, String> {
    let (mut source, identity) = open_regular_source(Path::new(&source_path))?;
    let destination_name = upload_basename(Path::new(&source_path), &destination_name)?;
    validate_png_if_requested(&mut source, &identity, image_png)?;
    if identity.size > LARGE_UPLOAD_BYTES && !large_upload_confirmed {
        return Err(
            "large_upload_confirmation_required: preflight requires explicit confirmation".into(),
        );
    }
    let binding = BulkBinding::capture(
        get_client(&clients, &client_id)?,
        expected_server_identity,
        parse_required_u64("connectionEpoch", &connection_epoch)?,
    )?;
    let transfer_id = Uuid::new_v4().to_string();
    let job = UploadPreflightJob {
        transfer_id: transfer_id.clone(),
        connection: profiles.connection_for(&profile_id)?,
        source_path: PathBuf::from(source_path),
        source_identity: identity,
        destination_name,
        collision,
        large_upload_confirmed,
        image_png,
        binding,
        cancellation: Arc::new(CancelState::new()),
        channel: on_event,
    };
    uploads.enqueue_preflight(job)?;
    Ok(transfer_id)
}

#[tauri::command]
pub fn cancel_terminal_upload_preflight(
    preflight_id: String,
    _uploads: State<'_, UploadManager>,
) -> Result<Value, String> {
    cancel_transfer(&preflight_id).map(|response| json!(response))
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn start_terminal_upload(
    client_id: String,
    profile_id: String,
    expected_server_identity: String,
    connection_epoch: String,
    source_path: String,
    destination_name: String,
    collision: UploadCollisionPolicy,
    large_upload_confirmed: bool,
    image_png: bool,
    on_event: Channel<Value>,
    profiles: State<'_, ProfileStore>,
    clients: State<'_, TerminalClients>,
    uploads: State<'_, UploadManager>,
) -> Result<String, String> {
    let source_path = PathBuf::from(source_path);
    let (mut source, source_identity) = open_regular_source(&source_path)?;
    let destination_name = upload_basename(&source_path, &destination_name)?;
    validate_png_if_requested(&mut source, &source_identity, image_png)?;
    let source_guard = lock_owned_clipboard_source(&source_path, source)?;
    if source_identity.size > LARGE_UPLOAD_BYTES && !large_upload_confirmed {
        return Err("large_upload_confirmation_required: uploads above 500 MiB require explicit confirmation".into());
    }
    let binding = BulkBinding::capture(
        get_client(&clients, &client_id)?,
        expected_server_identity,
        parse_required_u64("connectionEpoch", &connection_epoch)?,
    )?;
    let transfer_id = Uuid::new_v4().to_string();
    let cancellation = Arc::new(CancelState::new());
    let job = UploadJob {
        transfer_id: transfer_id.clone(),
        connection: profiles.connection_for(&profile_id)?,
        source_path,
        source_identity,
        source_guard,
        destination_name,
        collision,
        large_upload_confirmed,
        image_png,
        binding,
        cancellation: Arc::clone(&cancellation),
        channel: on_event,
    };
    uploads.enqueue(job)?;
    Ok(transfer_id)
}

#[tauri::command]
pub fn cancel_terminal_upload(
    transfer_id: String,
    uploads: State<'_, UploadManager>,
) -> Result<Value, String> {
    uploads.cancel(&transfer_id)
}

#[tauri::command]
pub fn stage_clipboard_png(request: tauri::ipc::Request<'_>) -> Result<Value, String> {
    let tauri::ipc::InvokeBody::Raw(png_bytes) = request.body() else {
        return Err("clipboard PNG IPC body must be raw binary".into());
    };
    super::clipboard_staging::stage_clipboard_png(png_bytes)
}

impl UploadManager {
    fn enqueue_preflight(&self, job: UploadPreflightJob) -> Result<(), String> {
        let queued = TransferEvent::new(&job.transfer_id, &job.binding, TransferState::Queued)
            .fields(json!({
                "sourcePath": job.source_path,
                "name": job.destination_name,
                "sizeBytes": job.source_identity.size.to_string(),
            }))
            .value();
        let _ = job.channel.send(queued);
        let id = job.transfer_id.clone();
        let binding = job.binding.clone();
        let cancellation = Arc::clone(&job.cancellation);
        let started = job.clone();
        let work = job.clone();
        enqueue_transfer(
            id,
            binding,
            cancellation,
            move || {
                let event = TransferEvent::new(
                    &started.transfer_id,
                    &started.binding,
                    TransferState::Preflighting,
                )
                .fields(json!({
                    "sourcePath": started.source_path,
                    "name": started.destination_name,
                    "sizeBytes": started.source_identity.size.to_string(),
                }))
                .value();
                let _ = started.channel.send(event);
            },
            move || {
                run_upload_preflight(&work).map_err(|error| {
                    let kind = match work.cancellation.reason() {
                        super::scheduler::CancelReason::StaleBinding => {
                            TransferFailureKind::StaleScope
                        }
                        super::scheduler::CancelReason::Timeout => TransferFailureKind::Timeout,
                        _ => TransferFailureKind::Transfer,
                    };
                    TransferFailure::new(
                        TransferOutcome::NotPublished,
                        kind,
                        CleanupStatus::ConnectionClosed,
                        error.clone(),
                        Some(error),
                    )
                })
            },
            move |result, _reason| {
                if let Err(failure) = result {
                    let state = if job.cancellation.reason() == super::scheduler::CancelReason::User
                    {
                        TransferState::Cancelled
                    } else {
                        TransferState::Failed
                    };
                    let event = TransferEvent::new(&job.transfer_id, &job.binding, state)
                        .outcome(failure.outcome)
                        .cleanup(failure.cleanup_status, failure.cleanup_error);
                    let event = if state == TransferState::Cancelled {
                        event.error(failure.error)
                    } else {
                        event.failure(failure.failure_kind, failure.error)
                    }
                    .fields(json!({
                        "sourcePath": job.source_path,
                        "name": job.destination_name,
                        "sizeBytes": job.source_identity.size.to_string(),
                    }))
                    .value();
                    let _ = job.channel.send(event);
                }
            },
        )
    }

    fn enqueue(&self, job: UploadJob) -> Result<(), String> {
        emit(&job, TransferState::Queued, json!({}));
        let id = job.transfer_id.clone();
        let binding = job.binding.clone();
        let cancellation = Arc::clone(&job.cancellation);
        let started_job = job.clone();
        let work_job = job.clone();
        enqueue_transfer(
            id,
            binding,
            cancellation,
            move || emit(&started_job, TransferState::Running, json!({})),
            move || run_upload(&work_job),
            move |result, _reason| finish_upload_job(&job, result),
        )
    }

    fn cancel(&self, transfer_id: &str) -> Result<Value, String> {
        cancel_transfer(transfer_id).map(|response| json!(response))
    }
}

#[cfg(test)]
pub(super) fn enqueue_acceptance_upload(
    connection: ConnectionSpec,
    binding: BulkBinding,
    source_path: PathBuf,
    destination_name: String,
    channel: Channel<Value>,
) -> Result<String, String> {
    let (source, source_identity) = open_regular_source(&source_path)?;
    let transfer_id = Uuid::new_v4().to_string();
    let job = UploadJob {
        transfer_id: transfer_id.clone(),
        connection,
        source_path,
        source_identity,
        source_guard: Some(Arc::new(source)),
        destination_name,
        collision: UploadCollisionPolicy::Fail,
        large_upload_confirmed: true,
        image_png: false,
        binding,
        cancellation: Arc::new(CancelState::new()),
        channel,
    };
    UploadManager.enqueue(job)?;
    Ok(transfer_id)
}

fn run_upload_preflight(job: &UploadPreflightJob) -> Result<(), String> {
    job.binding.validate()?;
    let (_source, current_identity) = open_regular_source(&job.source_path)?;
    if current_identity != job.source_identity {
        return Err("upload source changed while preflight was queued".into());
    }
    let _deadline = job.cancellation.arm_inactivity_deadline();
    let mut child = BulkChild(spawn_bulk_bridge(&job.connection)?);
    let _process_binding = job.cancellation.bind_process(child.0.id())?;
    let mut stdin = child
        .0
        .stdin
        .take()
        .ok_or("bulk bridge stdin unavailable")?;
    let stdout = child
        .0
        .stdout
        .take()
        .ok_or("bulk bridge stdout unavailable")?;
    let mut reader = BufReader::new(stdout);
    let mut protocol = BulkProtocolClient::connect(&mut stdin, &mut reader, &job.binding)?;
    let descriptor = prepare_remote(
        &mut protocol,
        2,
        &job.transfer_id,
        &job.destination_name,
        job.source_identity.size,
        job.collision,
        job.large_upload_confirmed,
        job.image_png,
        &job.cancellation,
        &_deadline,
    )?;
    _deadline.touch();
    let cleanup = protocol.cancel_terminal_upload(&job.transfer_id, 3);
    _deadline.touch();
    let (cleanup_status, cleanup_error) = classify_upload_cleanup(cleanup);
    let mut cleanup = CleanupReport::new(cleanup_status, cleanup_error);
    if !descriptor.cleanup_error.is_empty() {
        cleanup.merge(
            CleanupStatus::Failed,
            Some(descriptor.cleanup_error.clone()),
        );
    }
    let state = if cleanup.status == CleanupStatus::Removed {
        TransferState::Completed
    } else {
        TransferState::Failed
    };
    let mut event = TransferEvent::new(&job.transfer_id, &job.binding, state)
        .outcome(TransferOutcome::NotPublished)
        .cleanup(cleanup.status, cleanup.error.clone())
        .fields(json!({
            "sourcePath": job.source_path,
            "name": descriptor.destination_name,
            "sizeBytes": job.source_identity.size.to_string(),
            "availableBytes": descriptor.available_bytes.to_string(),
            "destination": descriptor.destination_name,
            "collisionRenamed": descriptor.collision_renamed,
            "confirmationRequired": false,
            "remoteChecked": true,
        }));
    if state == TransferState::Failed {
        event = event.failure(
            TransferFailureKind::Cleanup,
            cleanup
                .error
                .unwrap_or_else(|| "remote preflight cleanup failed".into()),
        );
    }
    let _ = job.channel.send(event.value());
    Ok(())
}

fn finish_upload_job(job: &UploadJob, result: TransferResult) {
    let Err(failure) = result else { return };
    let state = if job.cancellation.reason() == super::scheduler::CancelReason::User
        && failure.outcome == TransferOutcome::NotPublished
    {
        TransferState::Cancelled
    } else {
        TransferState::Failed
    };
    let event = TransferEvent::new(&job.transfer_id, &job.binding, state)
        .outcome(failure.outcome)
        .cleanup(failure.cleanup_status, failure.cleanup_error);
    let event = if state == TransferState::Cancelled {
        event.error(failure.error)
    } else {
        event.failure(failure.failure_kind, failure.error)
    };
    let value = event.fields(upload_fields(job, json!({}))).value();
    let _ = job.channel.send(value);
}

fn run_upload(job: &UploadJob) -> TransferResult {
    job.binding.validate()?;
    if job.cancellation.is_cancelled() {
        return Err("upload cancelled before start".into());
    }
    let (mut source, identity) = if let Some(guard) = &job.source_guard {
        let mut source = guard.try_clone().map_err(|error| error.to_string())?;
        source.rewind().map_err(|error| error.to_string())?;
        let identity =
            SourceIdentity::from_metadata(&source.metadata().map_err(|error| error.to_string())?);
        (source, identity)
    } else {
        open_regular_source(&job.source_path)?
    };
    if identity != job.source_identity {
        return Err("upload source changed while queued".into());
    }
    validate_png_if_requested(&mut source, &identity, job.image_png)?;
    let session = {
        let deadline = job.cancellation.arm_inactivity_deadline();
        let mut child = BulkChild(spawn_bulk_bridge(&job.connection)?);
        let _process_binding = job.cancellation.bind_process(child.0.id())?;
        let mut stdin = child
            .0
            .stdin
            .take()
            .ok_or("bulk bridge stdin unavailable")?;
        let stdout = child
            .0
            .stdout
            .take()
            .ok_or("bulk bridge stdout unavailable")?;
        let mut reader = BufReader::new(stdout);
        let mut protocol = BulkProtocolClient::connect(&mut stdin, &mut reader, &job.binding)?;
        prepare_remote(
            &mut protocol,
            2,
            &job.transfer_id,
            &job.destination_name,
            identity.size,
            job.collision,
            job.large_upload_confirmed,
            job.image_png,
            &job.cancellation,
            &deadline,
        )?;
        match stream_upload(job, &mut protocol, &mut source, &deadline) {
            Ok(session) => Ok(session),
            Err(mut failure) => {
                let (status, error) = classify_upload_cleanup(
                    protocol.cancel_terminal_upload(&job.transfer_id, u64::MAX - 1),
                );
                failure.merge_cleanup(status, error);
                Err(failure)
            }
        }
    }?;
    // Reconciliation reuses this job's lane only after the original helper is
    // closed and reaped; a replacement never creates a third connection.
    match session {
        UploadSessionResult::Complete => Ok(()),
        UploadSessionResult::Reconcile {
            offset,
            digest,
            error,
        } => match reconcile_upload_outcome(job, offset, &digest) {
            Ok(result) => emit_verified_upload(job, offset, &digest, result)
                .map_err(TransferFailure::not_published),
            Err(reconcile_error) => Err(TransferFailure::unknown(
                match job.cancellation.reason() {
                    super::scheduler::CancelReason::StaleBinding => TransferFailureKind::StaleScope,
                    super::scheduler::CancelReason::Timeout => TransferFailureKind::Timeout,
                    _ => TransferFailureKind::OutcomeUnknown,
                },
                format!(
                    "upload commit response was lost: {error}; ownership reconciliation failed: {reconcile_error}"
                ),
            )),
        },
    }
}

enum UploadSessionResult {
    Complete,
    Reconcile {
        offset: u64,
        digest: String,
        error: String,
    },
}

fn classify_upload_cleanup(result: Result<String, String>) -> (CleanupStatus, Option<String>) {
    match result {
        Ok(error) if error.is_empty() => (CleanupStatus::Removed, None),
        Ok(error) => (CleanupStatus::Failed, Some(error)),
        Err(error) => (CleanupStatus::ConnectionClosed, Some(error)),
    }
}

fn stream_upload(
    job: &UploadJob,
    protocol: &mut BulkProtocolClient<'_>,
    source: &mut File,
    deadline: &super::scheduler::DeadlineGuard,
) -> Result<UploadSessionResult, TransferFailure> {
    let started = Instant::now();
    let mut last_progress = Instant::now() - Duration::from_secs(1);
    let mut hasher = blake3::Hasher::new();
    let mut offset = 0_u64;
    let mut request_id = 10_u64;
    let mut buffer = vec![0_u8; BULK_CHUNK_BYTES as usize];
    loop {
        job.binding.validate()?;
        if job.cancellation.is_cancelled() {
            return Err("upload cancelled".into());
        }
        let count = source
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        let next = offset
            .checked_add(count as u64)
            .ok_or("upload byte counter overflow")?;
        if next > job.source_identity.size {
            return Err("upload source grew during transfer".into());
        }
        hasher.update(&buffer[..count]);
        let response = protocol.request_cancellable(
            request_id,
            v1::Request {
                operation: v1::Operation::WriteTerminalUploadChunk.into(),
                file: Some(v1::FileServiceRequest {
                    operation_id: job.transfer_id.clone(),
                    transfer_id: job.transfer_id.clone(),
                    offset,
                    total_bytes: job.source_identity.size,
                    content: buffer[..count].to_vec(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            &job.cancellation,
            deadline,
        )?;
        deadline.touch();
        let accepted = response
            .file
            .and_then(|file| file.transfer_chunk)
            .ok_or("upload response omitted byte acknowledgement")?;
        if accepted.offset != next {
            return Err("upload destination acknowledged an unexpected offset".into());
        }
        offset = next;
        request_id = request_id.saturating_add(1);
        let elapsed = started.elapsed().as_secs_f64().max(0.001);
        let throughput = offset as f64 / elapsed;
        let eta = job.source_identity.size.saturating_sub(offset) as f64 / throughput.max(1.0);
        if last_progress.elapsed() >= Duration::from_millis(100)
            || offset == job.source_identity.size
        {
            emit(
                job,
                TransferState::Running,
                json!({
                    "transferredBytes": offset.to_string(),
                    "totalBytes": job.source_identity.size.to_string(),
                    "throughputBytesPerSecond": throughput,
                    "etaSeconds": eta,
                }),
            );
            last_progress = Instant::now();
        }
    }
    if offset != job.source_identity.size {
        return Err("upload source shrank during transfer".into());
    }
    let current =
        SourceIdentity::from_metadata(&source.metadata().map_err(|error| error.to_string())?);
    if current != job.source_identity {
        return Err("upload source changed during transfer".into());
    }
    if job.cancellation.is_cancelled() {
        return Err("upload cancelled before finalize".into());
    }
    job.binding.validate()?;
    job.cancellation.prepare_finalize()?;
    emit(job, TransferState::Verifying, json!({}));
    let digest = hasher.finalize().to_hex().to_string();
    let commit_response = protocol.request_classified_with_deadline(
        request_id,
        v1::Request {
            operation: v1::Operation::CommitTerminalUpload.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: job.transfer_id.clone(),
                transfer_id: job.transfer_id.clone(),
                total_bytes: offset,
                blake3: digest.clone(),
                ..Default::default()
            }),
            ..Default::default()
        },
        deadline,
    );
    // The streaming/commit watchdog may have killed a silent transport. It
    // must not remain armed while a fresh ownership-reconciliation helper is
    // running with its own independent deadline.
    deadline.complete();
    let response = match commit_response {
        Ok(response) => response,
        Err(RequestFailure::Remote {
            code,
            message,
            publication_outcome,
            cleanup_failed,
        }) => {
            return match publication_outcome {
                Some(tmux_agent_protocol::PublicationOutcome::NotPublished) if cleanup_failed => {
                    Err(TransferFailure::new(
                        TransferOutcome::NotPublished,
                        TransferFailureKind::Cleanup,
                        CleanupStatus::Failed,
                        message.clone(),
                        Some(message),
                    ))
                }
                Some(tmux_agent_protocol::PublicationOutcome::NotPublished) => {
                    Err(TransferFailure::not_published(message))
                }
                _ => Ok(UploadSessionResult::Reconcile {
                    offset,
                    digest,
                    error: format!("{code}: {message}"),
                }),
            };
        }
        Err(RequestFailure::Transport(error)) => {
            return Ok(UploadSessionResult::Reconcile {
                offset,
                digest,
                error,
            });
        }
        Err(RequestFailure::Cancelled) => {
            return Err(TransferFailure::not_published(
                "upload commit cancelled before dispatch",
            ));
        }
    };
    deadline.touch();
    let Some(result) = response.file.and_then(|file| file.upload) else {
        return Ok(UploadSessionResult::Reconcile {
            offset,
            digest,
            error: "upload commit returned a malformed success response".into(),
        });
    };
    emit_verified_upload(job, offset, &digest, result)?;
    Ok(UploadSessionResult::Complete)
}

fn reconcile_upload_outcome(
    job: &UploadJob,
    total: u64,
    digest: &str,
) -> Result<v1::UploadDescriptor, String> {
    job.binding.validate()?;
    let deadline = job.cancellation.arm_inactivity_deadline();
    let mut child = BulkChild(spawn_bulk_bridge(&job.connection)?);
    let _process_binding = job.cancellation.bind_authoritative_process(child.0.id())?;
    let mut stdin = child
        .0
        .stdin
        .take()
        .ok_or("bulk bridge stdin unavailable")?;
    let stdout = child
        .0
        .stdout
        .take()
        .ok_or("bulk bridge stdout unavailable")?;
    let mut reader = BufReader::new(stdout);
    let mut protocol = BulkProtocolClient::connect(&mut stdin, &mut reader, &job.binding)?;
    deadline.touch();
    let response = protocol.request_with_deadline(
        2,
        v1::Request {
            operation: v1::Operation::ReconcileTerminalUpload.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: job.transfer_id.clone(),
                transfer_id: job.transfer_id.clone(),
                total_bytes: total,
                blake3: digest.to_owned(),
                ..Default::default()
            }),
            ..Default::default()
        },
        &deadline,
    )?;
    deadline.touch();
    response
        .file
        .and_then(|file| file.upload)
        .ok_or("upload reconciliation omitted verification result".into())
}

fn emit_verified_upload(
    job: &UploadJob,
    offset: u64,
    digest: &str,
    result: v1::UploadDescriptor,
) -> Result<(), String> {
    if !result.verified
        || result.total_bytes != offset
        || result.blake3 != digest
        || result.final_path.is_empty()
    {
        return Err("upload commit verification did not match the local source".into());
    }
    let cleanup_status = match v1::CleanupStatus::try_from(result.cleanup_status)
        .unwrap_or_default()
    {
        v1::CleanupStatus::NotNeeded | v1::CleanupStatus::Unspecified => CleanupStatus::NotNeeded,
        v1::CleanupStatus::Removed => CleanupStatus::Removed,
        v1::CleanupStatus::Retained => CleanupStatus::Retained,
        v1::CleanupStatus::Failed => CleanupStatus::Failed,
        v1::CleanupStatus::ConnectionClosed => CleanupStatus::ConnectionClosed,
    };
    let fields = json!({
            "transferredBytes": offset.to_string(),
            "totalBytes": offset.to_string(),
            "destination": result.final_path,
            "name": result.destination_name,
            "blake3": digest,
            "cleanupStatus": cleanup_status,
            "cleanupError": result.cleanup_error,
    });
    if let Err(error) = job.binding.validate() {
        let value = TransferEvent::new(&job.transfer_id, &job.binding, TransferState::Failed)
            .outcome(TransferOutcome::Published)
            .failure(
                TransferFailureKind::StaleScope,
                format!("upload was verified and published after its captured scope became stale: {error}"),
            )
            .cleanup(cleanup_status, None)
            .fields(upload_fields(job, fields))
            .value();
        let _ = job.channel.send(value);
    } else {
        let value = TransferEvent::new(&job.transfer_id, &job.binding, TransferState::Completed)
            .outcome(TransferOutcome::Published)
            .fields(upload_fields(job, fields))
            .value();
        let _ = job.channel.send(value);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn prepare_remote(
    protocol: &mut BulkProtocolClient<'_>,
    request_id: u64,
    transfer_id: &str,
    destination_name: &str,
    total: u64,
    collision: UploadCollisionPolicy,
    large_upload_confirmed: bool,
    image_png: bool,
    cancellation: &super::scheduler::CancelState,
    deadline: &super::scheduler::DeadlineGuard,
) -> Result<v1::UploadDescriptor, String> {
    protocol
        .request_cancellable(
            request_id,
            v1::Request {
                operation: v1::Operation::PrepareTerminalUpload.into(),
                file: Some(v1::FileServiceRequest {
                    operation_id: transfer_id.into(),
                    transfer_id: transfer_id.into(),
                    destination: destination_name.into(),
                    source_name: destination_name.into(),
                    total_bytes: total,
                    collision_policy: collision.protocol().into(),
                    large_upload_confirmed,
                    image_png,
                    ..Default::default()
                }),
                ..Default::default()
            },
            cancellation,
            deadline,
        )?
        .file
        .and_then(|file| file.upload)
        .ok_or("upload preflight omitted descriptor".into())
}

fn open_regular_source(path: &Path) -> Result<(File, SourceIdentity), String> {
    if !path.is_absolute() {
        return Err("upload source path must be absolute".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| format!("upload source is unavailable or unsafe: {error}"))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("only regular files may be uploaded".into());
    }
    Ok((file, SourceIdentity::from_metadata(&metadata)))
}

impl SourceIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        }
    }
}

fn upload_basename(source: &Path, requested: &str) -> Result<String, String> {
    let name = if requested.is_empty() {
        source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("upload source basename is not valid UTF-8")?
    } else {
        requested
    };
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
        || name.len() > 255
        || name.contains('\0')
    {
        return Err("upload destination must be one non-empty basename".into());
    }
    Ok(name.to_owned())
}

fn validate_png_if_requested(
    source: &mut File,
    identity: &SourceIdentity,
    image_png: bool,
) -> Result<(), String> {
    if !image_png {
        return Ok(());
    }
    if identity.size > MAX_PNG_BYTES {
        return Err("clipboard PNG exceeds the 25 MiB encoded-image limit".into());
    }
    let mut signature = [0_u8; 8];
    source
        .read_exact(&mut signature)
        .map_err(|_| "clipboard image is not a complete PNG".to_string())?;
    if &signature != PNG_SIGNATURE {
        return Err("clipboard image does not have a valid PNG signature".into());
    }
    source
        .rewind()
        .map_err(|error| format!("could not rewind PNG source: {error}"))?;
    Ok(())
}

fn upload_fields(job: &UploadJob, extra: Value) -> Value {
    let mut value = json!({
        "sourcePath": job.source_path,
        "name": job.destination_name,
        "transferredBytes": "0",
        "totalBytes": job.source_identity.size.to_string(),
    });
    if let (Some(target), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
        target.extend(extra.clone());
    }
    value
}

fn emit(job: &UploadJob, state: TransferState, extra: Value) {
    let value = TransferEvent::new(&job.transfer_id, &job.binding, state)
        .fields(upload_fields(job, extra))
        .value();
    let _ = job.channel.send(value);
}

#[cfg(test)]
#[path = "upload_manager/tests.rs"]
mod tests;
