use std::{
    collections::VecDeque,
    ffi::OsStr,
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{State, ipc::Channel};
use tmux_agent_protocol::{PublicationOutcome, v1};
use uuid::Uuid;

use super::bulk_pool::BulkLease;
use super::bulk_protocol::{BulkProtocolClient, Exchange, RequestFailure};
use super::local_destination::{
    DestinationReservations, PreparedDestination, ReservedDestination,
    destination_parent_diagnostic,
};
use super::scheduler::{
    BulkBinding, CancelState, DeadlineGuard, QueuedPublication, cancel_transfer,
    enqueue_transfer_with_queued,
};
use super::transfer_event::{
    CleanupStatus, TransferEvent, TransferFailure, TransferFailureKind, TransferOutcome,
    TransferResult, TransferState, late_queued_publication_rollback,
};
use super::{BULK_CHUNK_BYTES, parse_required_u64};
use crate::connection::{ConnectionSpec, ProfileStore, TerminalClients, get_client};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadCollisionPolicy {
    Fail,
    OverwriteConfirmed,
    Rename,
}

struct DownloadJob {
    transfer_id: String,
    connection: ConnectionSpec,
    root: String,
    root_token: String,
    source: String,
    folder: bool,
    destination: Arc<ReservedDestination>,
    binding: BulkBinding,
    cancellation: Arc<CancelState>,
    channel: Channel<Value>,
    published: Arc<PathRegistry>,
}

/// A bounded, FIFO set of absolute paths published by this session. The oldest
/// entry is the one whose toast and transfer row are furthest gone.
#[derive(Default)]
pub struct PathRegistry {
    paths: Mutex<VecDeque<PathBuf>>,
}

const MAX_REGISTRY_PATHS: usize = 256;

impl PathRegistry {
    pub(super) fn record(&self, path: PathBuf) {
        let Ok(mut paths) = self.paths.lock() else {
            return;
        };
        if paths.iter().any(|candidate| candidate == &path) {
            return;
        }
        while paths.len() >= MAX_REGISTRY_PATHS {
            paths.pop_front();
        }
        paths.push_back(path);
    }

    pub(super) fn contains(&self, path: &Path) -> bool {
        self.paths
            .lock()
            .map(|paths| paths.iter().any(|candidate| candidate == path))
            .unwrap_or(false)
    }
}

#[derive(Clone, Default)]
pub struct DownloadManager {
    /// The local files this session has actually written, and the whole
    /// authority behind `open_download`/`reveal_download`.
    ///
    /// Handing the renderer a command that opens an arbitrary path with the
    /// user's default application would make any string the webview can
    /// produce an execution request. The commands take a path and answer "did
    /// I write this?" instead — so the renderer's reach into the OS opener is
    /// exactly the set of downloads the app just published, and nothing else.
    published: Arc<PathRegistry>,
    /// Destinations with a transfer currently in flight.
    ///
    /// A running download occupies only its `.partial`; the final name stays
    /// free on disk until `publish()`. Without this, starting a second copy of
    /// a large file while the first is still transferring would be offered the
    /// *same* suggested name, the save panel would have nothing to warn about,
    /// and the second publish would silently replace the first — the exact
    /// "three downloads, three files" the flow exists to guarantee.
    ///
    /// What is reserved is the destination the user actually confirmed, held
    /// from the moment the job is queued until it finishes, either way. Two
    /// things follow that reserving the *suggestion* would get wrong:
    /// cancelling the save panel reserves nothing, and typing the same name
    /// into two panels is caught, because the reservation is made where the
    /// name is committed rather than where it is proposed.
    reserved: Arc<DestinationReservations>,
}

impl DownloadManager {
    pub(super) fn published(&self) -> &PathRegistry {
        &self.published
    }
}

/// The path a download will actually be written to.
///
/// A folder download is a tar archive, and the suffix is applied here — at the
/// one boundary that names the destination — so the reservation, the job, and
/// the file on disk cannot disagree about what is being written.
fn archive_destination(destination: PathBuf, folder: bool) -> PathBuf {
    if folder && destination.extension().and_then(|value| value.to_str()) != Some("tar") {
        return PathBuf::from(format!("{}.tar", destination.to_string_lossy()));
    }
    destination
}

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
    diagnostic_attempt_id: String,
    on_event: Channel<Value>,
    profiles: State<'_, ProfileStore>,
    clients: State<'_, TerminalClients>,
    transfers: State<'_, DownloadManager>,
    incidents: State<'_, crate::incidents::IncidentJournal>,
) -> Result<String, String> {
    if root.is_empty() || root_token.is_empty() || source.is_empty() || destination.is_empty() {
        return Err("root snapshot, source, and destination are required".into());
    }
    if !valid_diagnostic_attempt_id(&diagnostic_attempt_id) {
        return Err("invalid download diagnostic attempt ID".into());
    }
    let connection = profiles.connection_for(&profile_id)?;
    let binding = BulkBinding::capture(
        get_client(&clients, &client_id)?,
        expected_server_identity,
        parse_required_u64("connectionEpoch", &connection_epoch)?,
    )?;
    let transfer_id = Uuid::new_v4().to_string();
    let cancellation = Arc::new(CancelState::new());
    let destination = archive_destination(PathBuf::from(destination), folder);
    let reserved = ReservedDestination::reserve_observed(
        &destination,
        collision,
        Arc::clone(&transfers.reserved),
    );
    let parent_open = match &reserved {
        Ok(destination) => destination.parent_open_diagnostic(),
        Err(failure) => failure.parent_open_diagnostic(),
    };
    let destination_diagnostic = destination_parent_diagnostic(
        &destination,
        reserved.as_ref().err().map(|failure| failure.as_str()),
        parent_open,
        reserved
            .as_ref()
            .ok()
            .map(|destination| destination.parent_directory()),
    );
    let mut diagnostic = destination_diagnostic
        .as_object()
        .cloned()
        .unwrap_or_default();
    diagnostic.insert("attemptId".into(), Value::String(diagnostic_attempt_id));
    diagnostic.insert("folder".into(), Value::Bool(folder));
    diagnostic.insert("admitted".into(), Value::Bool(reserved.is_ok()));
    let _ = incidents.record_native(
        if reserved.is_ok() {
            "download.destinationAdmitted"
        } else {
            "download.destinationRejected"
        },
        Value::Object(diagnostic),
    );
    let destination = Arc::new(reserved.map_err(|failure| failure.into_message())?);
    let job = Arc::new(DownloadJob {
        transfer_id: transfer_id.clone(),
        connection,
        root,
        root_token,
        source,
        folder,
        destination,
        binding,
        cancellation: Arc::clone(&cancellation),
        channel: on_event,
        published: Arc::clone(&transfers.published),
    });
    enqueue(job)?;
    Ok(transfer_id)
}

fn valid_diagnostic_attempt_id(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|parsed| {
        parsed.get_version() == Some(uuid::Version::Random) && parsed.to_string() == value
    })
}

/// The name the save panel opens with, chosen so the panel's own "…already
/// exists. Replace?" prompt effectively never appears.
///
/// The user's ask was "download three times, get three files, answer nothing".
/// Renaming *after* the panel would be wrong — clicking Replace is consent, and
/// silently renaming past it would ignore the user — so the uniqueness is
/// applied to the default name instead, before they ever see it.
#[tauri::command]
pub fn suggest_download_destination(
    file_name: String,
    app: tauri::AppHandle,
    transfers: State<'_, DownloadManager>,
) -> Result<String, String> {
    use tauri::Manager;

    let directory = app
        .path()
        .download_dir()
        .map_err(|error| format!("could not resolve the Downloads directory: {error}"))?;
    // A name is taken when it is on disk *or* has a transfer in flight against
    // it — see `DownloadManager::reserved`. Nothing is reserved here: this is a
    // proposal, and a user who cancels the panel must not leave a name burned
    // for the rest of the session.
    let name = super::download_naming::suggest_non_colliding_name(
        &directory,
        OsStr::new(file_name.as_str()),
        |candidate| transfers.reserved.contains_display_path(candidate),
    )?;
    directory
        .join(name)
        .into_os_string()
        .into_string()
        .map_err(|_| "the Downloads directory path is not valid UTF-8".to_owned())
}

#[tauri::command]
pub fn cancel_download(transfer_id: String) -> Result<(), String> {
    cancel_transfer(&transfer_id).map(|_| ())
}

/// A free function, not a method: the job already carries everything the work
/// needs, including its own handle on the published-downloads registry. As a
/// `&self` method that ignored `self` it invited the next reader to reach for
/// the manager's state from a call site that has a throwaway one.
fn enqueue(job: Arc<DownloadJob>) -> Result<(), String> {
    let id = job.transfer_id.clone();
    let binding = job.binding.clone();
    let cancellation = Arc::clone(&job.cancellation);
    let queued = QueuedPublication::json(
        job.channel.clone(),
        TransferEvent::new(&job.transfer_id, &job.binding, TransferState::Queued).value(),
        late_queued_publication_rollback(&job.transfer_id, &job.binding),
        "could not deliver download state",
    );
    let started_job = Arc::clone(&job);
    let work_job = Arc::clone(&job);
    enqueue_transfer_with_queued(
        id,
        binding,
        cancellation,
        queued,
        move || emit_download_state(&started_job, TransferState::Running, json!({})),
        move || run_download(&work_job),
        move |result, _reason| finish_download_job(&job, result),
    )
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
    let reservations = Arc::new(DestinationReservations::default());
    let destination = Arc::new(ReservedDestination::reserve(
        &destination,
        DownloadCollisionPolicy::Fail,
        reservations,
    )?);
    let job = Arc::new(DownloadJob {
        transfer_id: transfer_id.clone(),
        connection,
        root,
        root_token,
        source,
        folder: false,
        destination,
        binding,
        cancellation: Arc::new(CancelState::new()),
        channel,
        // Its own publication registry, dropped with this call: an acceptance download is
        // never handed to the UI, so nothing will ever ask to open it and
        // nothing is competing for its name.
        published: Arc::default(),
    });
    enqueue(job)?;
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
    let _ = send_download_state(job, state, extra);
}

fn send_download_state(
    job: &DownloadJob,
    state: TransferState,
    extra: Value,
) -> Result<(), String> {
    let value = TransferEvent::new(&job.transfer_id, &job.binding, state)
        .fields(extra)
        .value();
    job.channel
        .send(value)
        .map_err(|error| format!("could not deliver download state: {error}"))
}

fn run_download(job: &DownloadJob) -> TransferResult {
    job.binding.validate()?;
    // Admission already resolved and retained the destination directory. All
    // create, cleanup and publication operations use this descriptor, so a
    // parent rename/symlink swap cannot redirect them.
    let destination = PreparedDestination::prepare(Arc::clone(&job.destination))?;
    let _deadline = job.cancellation.arm_inactivity_deadline();
    let mut lease =
        BulkLease::acquire(&job.connection, &job.binding, &job.cancellation, &_deadline)?;
    let _process_binding = job.cancellation.bind_process(lease.process_id())?;
    let mut protocol = lease.client();
    let descriptor_response = match protocol.request_classified(
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
        Exchange::live(&job.cancellation, &_deadline),
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
            drop(_process_binding);
            // Dropping the lease closes the bridge: a transport failure is
            // exactly the case `bulk_pool` refuses to return to the pool.
            drop(lease);
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
        let remote_cleanup = protocol.cancel_download(&job.transfer_id);
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
    let mut lease =
        BulkLease::acquire(&job.connection, &job.binding, &job.cancellation, &deadline)?;
    let _process_binding = job.cancellation.bind_process(lease.process_id())?;
    let mut protocol = lease.client();
    let result = protocol.cancel_download(&job.transfer_id);
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
    loop {
        if job.cancellation.is_cancelled() {
            return Err("download cancelled".into());
        }
        let response = protocol.request(
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
            Exchange::live(&job.cancellation, deadline),
        )?;
        deadline.touch();
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
    protocol.cancel_download(&job.transfer_id)?;
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
    // Recorded whenever the local file exists under its final name, and from
    // the *final* path rather than the requested one — a `Rename` policy may
    // have moved it. This is what later authorizes Open / Show in Finder, so
    // it has to follow the backend's own verdict: a publication that reports
    // `Published` and *then* fails still left a file behind, and the UI offers
    // to open exactly that case.
    let publication = match destination.publish() {
        Ok(publication) => {
            job.published.record(destination.final_path().to_path_buf());
            publication
        }
        Err(error) => {
            let failure = download_publication_failure(error);
            if failure.outcome == TransferOutcome::Published {
                job.published.record(destination.final_path().to_path_buf());
            }
            return Err(failure);
        }
    };
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
    fn diagnostic_attempt_ids_accept_only_canonical_v4_uuids() {
        assert!(valid_diagnostic_attempt_id(
            "8c627ead-fa11-4c04-9109-23cabefa16c1"
        ));
        assert!(!valid_diagnostic_attempt_id(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
        assert!(!valid_diagnostic_attempt_id(
            "8C627EAD-FA11-4C04-9109-23CABEFA16C1"
        ));
        assert!(!valid_diagnostic_attempt_id(
            "8c627eadfa114c04910923cabefa16c1"
        ));
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

#[cfg(test)]
mod registry_tests {
    use super::*;

    #[test]
    fn the_registry_is_bounded_and_evicts_oldest_first() {
        let registry = PathRegistry::default();
        for index in 0..MAX_REGISTRY_PATHS + 10 {
            registry.record(PathBuf::from(format!("/downloads/file-{index}")));
        }
        assert!(!registry.contains(Path::new("/downloads/file-0")));
        assert!(registry.contains(Path::new("/downloads/file-265")));
    }

    #[test]
    fn a_folder_download_is_named_as_the_tar_it_actually_writes() {
        // One rule, applied where the destination is named — so the
        // reservation, the job and the file on disk cannot disagree.
        let named = |path: &str, folder: bool| {
            archive_destination(PathBuf::from(path), folder)
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(named("/d/archive", true), "/d/archive.tar");
        assert_eq!(named("/d/archive.tar", true), "/d/archive.tar");
        assert_eq!(named("/d/report.pdf", false), "/d/report.pdf");
        // A file download is never renamed, even without an extension.
        assert_eq!(named("/d/report", false), "/d/report");
    }
}
