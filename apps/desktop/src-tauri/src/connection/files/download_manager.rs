use std::{
    fs,
    io::{BufReader, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{State, ipc::Channel};
use tmux_agent_protocol::{PublicationOutcome, v1};
use uuid::Uuid;

use super::bulk_protocol::{BulkProtocolClient, RequestFailure};
use super::local_destination::PreparedDestination;
use super::scheduler::{
    BulkBinding, BulkChild, CancelState, DeadlineGuard, cancel_transfer, enqueue_transfer,
};
use super::transfer_event::{
    CleanupStatus, TransferEvent, TransferFailure, TransferFailureKind, TransferOutcome,
    TransferResult, TransferState,
};
use super::{BULK_CHUNK_BYTES, parse_required_u64};
use crate::connection::transport::spawn_bulk_bridge;
use crate::connection::{ConnectionSpec, ProfileStore, TerminalClients, get_client};

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadCollisionPolicy {
    Fail,
    OverwriteConfirmed,
    Rename,
}

#[derive(Clone)]
struct DownloadJob {
    transfer_id: String,
    connection: ConnectionSpec,
    root: String,
    root_token: String,
    source: String,
    destination: PathBuf,
    folder: bool,
    collision: DownloadCollisionPolicy,
    binding: BulkBinding,
    cancellation: Arc<CancelState>,
    channel: Channel<Value>,
}

#[derive(Clone, Default)]
pub struct DownloadManager;

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn start_download(
    client_id: String,
    profile_id: String,
    expected_server_identity: String,
    connection_epoch: String,
    root: String,
    root_token: String,
    source: String,
    destination: String,
    folder: bool,
    collision: DownloadCollisionPolicy,
    on_event: Channel<Value>,
    profiles: State<'_, ProfileStore>,
    clients: State<'_, TerminalClients>,
    transfers: State<'_, DownloadManager>,
) -> Result<String, String> {
    if root.is_empty() || root_token.is_empty() || source.is_empty() || destination.is_empty() {
        return Err("root snapshot, source, and destination are required".into());
    }
    let connection = profiles.connection_for(&profile_id)?;
    let binding = BulkBinding::capture(
        get_client(&clients, &client_id)?,
        expected_server_identity,
        parse_required_u64("connectionEpoch", &connection_epoch)?,
    )?;
    let transfer_id = Uuid::new_v4().to_string();
    let cancellation = Arc::new(CancelState::new());
    let job = DownloadJob {
        transfer_id: transfer_id.clone(),
        connection,
        root,
        root_token,
        source,
        destination: PathBuf::from(destination),
        folder,
        collision,
        binding,
        cancellation: Arc::clone(&cancellation),
        channel: on_event,
    };
    emit_download_state(&job, TransferState::Queued, json!({}));
    transfers.enqueue(job)?;
    Ok(transfer_id)
}

#[tauri::command]
pub fn cancel_download(
    transfer_id: String,
    transfers: State<'_, DownloadManager>,
) -> Result<(), String> {
    transfers.cancel(&transfer_id)
}

impl DownloadManager {
    fn enqueue(&self, job: DownloadJob) -> Result<(), String> {
        let id = job.transfer_id.clone();
        let binding = job.binding.clone();
        let cancellation = Arc::clone(&job.cancellation);
        let started_job = job.clone();
        let work_job = job.clone();
        enqueue_transfer(
            id,
            binding,
            cancellation,
            move || emit_download_state(&started_job, TransferState::Running, json!({})),
            move || run_download(&work_job),
            move |result, _reason| finish_download_job(&job, result),
        )
    }

    fn cancel(&self, transfer_id: &str) -> Result<(), String> {
        cancel_transfer(transfer_id).map(|_| ())
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn enqueue_acceptance_download(
    connection: ConnectionSpec,
    binding: BulkBinding,
    root: String,
    root_token: String,
    source: String,
    destination: PathBuf,
    channel: Channel<Value>,
) -> Result<String, String> {
    let transfer_id = Uuid::new_v4().to_string();
    let job = DownloadJob {
        transfer_id: transfer_id.clone(),
        connection,
        root,
        root_token,
        source,
        destination,
        folder: false,
        collision: DownloadCollisionPolicy::Fail,
        binding,
        cancellation: Arc::new(CancelState::new()),
        channel,
    };
    emit_download_state(&job, TransferState::Queued, json!({}));
    DownloadManager.enqueue(job)?;
    Ok(transfer_id)
}

fn finish_download_job(job: &DownloadJob, result: TransferResult) {
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
    let value = if state == TransferState::Cancelled {
        event.error(failure.error)
    } else {
        event.failure(failure.failure_kind, failure.error)
    }
    .value();
    let _ = job.channel.send(value);
}

fn emit_download_state(job: &DownloadJob, state: TransferState, extra: Value) {
    let value = TransferEvent::new(&job.transfer_id, &job.binding, state)
        .fields(extra)
        .value();
    let _ = job.channel.send(value);
}

fn run_download(job: &DownloadJob) -> TransferResult {
    job.binding.validate()?;
    let requested_destination = if job.folder
        && job.destination.extension().and_then(|value| value.to_str()) != Some("tar")
    {
        PathBuf::from(format!("{}.tar", job.destination.to_string_lossy()))
    } else {
        job.destination.clone()
    };
    // Resolve and retain the destination directory before the host allocates a
    // transfer. All later create, cleanup and publication operations use this
    // descriptor, so a parent rename/symlink swap cannot redirect them.
    let destination = PreparedDestination::open(&requested_destination, job.collision)?;
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
    let descriptor_response = match protocol.request_classified_cancellable(
        2,
        v1::Request {
            operation: v1::Operation::StartDownload.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: job.transfer_id.clone(),
                root: job.root.clone(),
                root_token: job.root_token.clone(),
                path: job.source.clone(),
                transfer_id: job.transfer_id.clone(),
                folder: job.folder,
                ..Default::default()
            }),
            ..Default::default()
        },
        &job.cancellation,
        &_deadline,
    ) {
        Ok(response) => response,
        Err(RequestFailure::Remote { code, message, .. }) => {
            return Err(TransferFailure::not_published(format!("{code}: {message}")));
        }
        Err(RequestFailure::Transport(error)) => {
            // Close and reap the original lane before starting the bounded
            // recovery helper. This job continues to hold exactly one global
            // lane permit throughout the handoff.
            _deadline.complete();
            drop(reader);
            drop(stdin);
            drop(_process_binding);
            drop(child);
            let cleanup = cancel_download_out_of_band(job)
                .err()
                .map(|cleanup| format!("host transfer cleanup was not confirmed: {cleanup}"));
            return Err(TransferFailure::new(
                TransferOutcome::NotPublished,
                TransferFailureKind::Transfer,
                if cleanup.is_some() {
                    CleanupStatus::ConnectionClosed
                } else {
                    CleanupStatus::Removed
                },
                format!("download start response was lost: {error}"),
                cleanup,
            ));
        }
        Err(RequestFailure::Cancelled) => {
            return Err(TransferFailure::not_published("download start cancelled"));
        }
    };
    let result = (|| {
        let descriptor = descriptor_response
            .file
            .and_then(|file| file.download)
            .ok_or("download preflight omitted descriptor")?;
        destination.ensure_available(descriptor.total_bytes)?;
        stream_download(job, &mut protocol, &descriptor, &destination, &_deadline)
    })();
    if let Err(mut failure) = result {
        // Every post-StartDownload error takes both cleanup paths. On success,
        // stream_download already cancelled the host record before publication.
        let remote_cleanup = protocol.cancel_download(&job.transfer_id, u64::MAX - 1);
        // An unknown transactional outcome may leave the confirmed original
        // inode under the owned partial name. Preserve it for reconciliation;
        // deleting by the pre-commit name would destroy the user's backup.
        let local_cleanup = if failure.outcome == TransferOutcome::Unknown {
            None
        } else {
            destination.cleanup_partial().err()
        };
        match remote_cleanup {
            Ok(()) => failure.merge_cleanup(CleanupStatus::Removed, None),
            Err(error) => failure.merge_cleanup(CleanupStatus::ConnectionClosed, Some(error)),
        }
        if let Some(error) = local_cleanup {
            failure.merge_cleanup(CleanupStatus::Failed, Some(error));
        } else if failure.outcome == TransferOutcome::NotPublished {
            failure.merge_cleanup(CleanupStatus::Removed, None);
        }
        return Err(failure);
    }
    Ok(())
}

fn cancel_download_out_of_band(job: &DownloadJob) -> Result<(), String> {
    job.binding.validate()?;
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
    let result = protocol.cancel_download(&job.transfer_id, 2);
    deadline.touch();
    result
}

fn stream_download(
    job: &DownloadJob,
    protocol: &mut BulkProtocolClient<'_>,
    descriptor: &v1::DownloadDescriptor,
    destination: &PreparedDestination,
    deadline: &DeadlineGuard,
) -> TransferResult {
    let mut output = destination.create_partial()?;
    let started = Instant::now();
    let mut last_progress = Instant::now() - Duration::from_secs(1);
    let mut hasher = blake3::Hasher::new();
    let mut offset = 0_u64;
    let mut request_id = 10_u64;
    loop {
        if job.cancellation.is_cancelled() {
            return Err("download cancelled".into());
        }
        let response = protocol.request_cancellable(
            request_id,
            v1::Request {
                operation: v1::Operation::ReadDownloadChunk.into(),
                file: Some(v1::FileServiceRequest {
                    operation_id: job.transfer_id.clone(),
                    transfer_id: job.transfer_id.clone(),
                    offset,
                    chunk_bytes: BULK_CHUNK_BYTES,
                    ..Default::default()
                }),
                ..Default::default()
            },
            &job.cancellation,
            deadline,
        )?;
        deadline.touch();
        request_id = request_id.saturating_add(1);
        let chunk = response
            .file
            .and_then(|file| file.transfer_chunk)
            .ok_or("download response omitted chunk")?;
        if chunk.offset != offset
            || (descriptor.total_known
                && (!chunk.total_known || chunk.total_bytes != descriptor.total_bytes))
        {
            return Err("download accounting changed during transfer".into());
        }
        if chunk.data.is_empty() && !chunk.eof {
            thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }
        output
            .write_all(&chunk.data)
            .map_err(|error| error.to_string())?;
        hasher.update(&chunk.data);
        offset = offset
            .checked_add(chunk.data.len() as u64)
            .ok_or("download byte counter overflow")?;
        let elapsed = started.elapsed().as_secs_f64().max(0.001);
        let throughput = offset as f64 / elapsed;
        let current_total = if chunk.total_known {
            chunk.total_bytes
        } else {
            descriptor.total_bytes
        };
        let remaining = current_total.saturating_sub(offset);
        let eta = if throughput > 0.0 {
            remaining as f64 / throughput
        } else {
            0.0
        };
        if last_progress.elapsed() >= Duration::from_millis(100) || chunk.eof {
            emit_download_state(
                job,
                TransferState::Running,
                json!({
                    "transferredBytes": offset.to_string(),
                    "totalBytes": current_total.to_string(),
                    "totalKnown": chunk.total_known,
                    "throughputBytesPerSecond": throughput,
                    "etaSeconds": eta,
                }),
            );
            last_progress = Instant::now();
        }
        if chunk.eof {
            if !chunk.total_known || offset != chunk.total_bytes {
                return Err("download final byte count verification failed".into());
            }
            let digest = hasher.finalize().to_hex().to_string();
            if chunk.blake3 != digest {
                return Err("download BLAKE3 verification failed".into());
            }
            if job.cancellation.is_cancelled() {
                return Err("download cancelled before finalize".into());
            }
            break;
        }
    }
    output.sync_all().map_err(|error| error.to_string())?;
    if !descriptor.folder_archive {
        output
            .set_permissions(fs::Permissions::from_mode(descriptor.mode & 0o777))
            .map_err(|error| error.to_string())?;
        output.sync_all().map_err(|error| error.to_string())?;
    }
    // Release the host transfer/archive child while cancellation is still
    // pre-commit. A cleanup failure leaves only the local owned partial, which
    // the caller removes; no local destination has been published yet.
    protocol.cancel_download(&job.transfer_id, request_id)?;
    deadline.touch();
    if job.cancellation.is_cancelled() {
        return Err("download cancelled during finalize".into());
    }
    job.binding.validate()?;
    if job.cancellation.is_cancelled() {
        return Err("download control binding was lost during finalize".into());
    }
    job.cancellation.prepare_finalize()?;
    emit_download_state(job, TransferState::Verifying, json!({}));
    drop(output);
    let publication = destination
        .publish()
        .map_err(download_publication_failure)?;
    let cleanup_status = if publication.cleanup_error.is_some() {
        CleanupStatus::Retained
    } else {
        CleanupStatus::NotNeeded
    };
    let fields = json!({
            "transferId": job.transfer_id,
            "destination": destination.final_path(),
            "artifactKind": if descriptor.folder_archive { "tarArchive" } else { "file" },
            "transferredBytes": offset.to_string(),
            "totalBytes": offset.to_string(),
            "blake3": hasher.finalize().to_hex().to_string(),
            "outcome": TransferOutcome::Published,
            "cleanupStatus": cleanup_status,
            "cleanupError": publication.cleanup_error,
    });
    if let Err(error) = job.binding.validate() {
        let value = TransferEvent::new(&job.transfer_id, &job.binding, TransferState::Failed)
            .outcome(TransferOutcome::Published)
            .failure(
                TransferFailureKind::StaleScope,
                format!("download was verified and published after its captured scope became stale: {error}"),
            )
            .cleanup(cleanup_status, None)
            .fields(fields)
            .value();
        let _ = job.channel.send(value);
    } else {
        let value = TransferEvent::new(&job.transfer_id, &job.binding, TransferState::Completed)
            .outcome(TransferOutcome::Published)
            .cleanup(cleanup_status, None)
            .fields(fields)
            .value();
        let _ = job.channel.send(value);
    }
    Ok(())
}

fn download_publication_failure(error: tmux_agent_protocol::PublishFailure) -> TransferFailure {
    match error.outcome {
        PublicationOutcome::NotPublished => TransferFailure::new(
            TransferOutcome::NotPublished,
            TransferFailureKind::Transfer,
            CleanupStatus::Retained,
            error.message.clone(),
            Some(error.message),
        ),
        PublicationOutcome::Unknown => {
            TransferFailure::unknown(TransferFailureKind::OutcomeUnknown, error.message)
        }
        PublicationOutcome::Published => TransferFailure::new(
            TransferOutcome::Published,
            TransferFailureKind::Cleanup,
            CleanupStatus::Retained,
            error.message.clone(),
            Some(error.message),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collision_policy_uses_stable_frontend_spelling() {
        assert_eq!(
            serde_json::to_string(&DownloadCollisionPolicy::OverwriteConfirmed).unwrap(),
            "\"overwriteConfirmed\""
        );
    }

    #[test]
    fn rollback_cleanup_failure_is_not_downgraded_by_later_missing_partial() {
        let original = "verified replacement quarantine cleanup failed".to_owned();
        let mut failure = download_publication_failure(tmux_agent_protocol::PublishFailure {
            outcome: PublicationOutcome::NotPublished,
            message: original.clone(),
        });
        failure.merge_cleanup(CleanupStatus::Removed, None);
        assert_eq!(failure.cleanup_status, CleanupStatus::Retained);
        assert_eq!(failure.cleanup_error.as_deref(), Some(original.as_str()));
    }
}
