use std::sync::Arc;

use serde_json::{Value, json};
use tauri::{
    State,
    ipc::{Channel, InvokeResponseBody},
};
use tmux_agent_protocol::v1;
use uuid::Uuid;

use super::bulk_pool::BulkLease;
use super::bulk_protocol::RequestFailure;
use super::scheduler::{
    BulkBinding, CancelReason, CancelState, DeadlineGuard, QueuedPublication, cancel_transfer,
    enqueue_transfer_with_queued,
};
use super::serialization::metadata_json;
use super::transfer_event::{
    CleanupStatus, TransferEvent, TransferFailure, TransferFailureKind, TransferOutcome,
    TransferState, late_queued_publication_rollback,
};
use super::{BULK_CHUNK_BYTES, parse_optional_u64, parse_required_u64};
use crate::connection::{ConnectionSpec, ProfileStore, TerminalClients, get_client};

#[derive(Clone)]
struct FileReadJob {
    transfer_id: String,
    connection: ConnectionSpec,
    root: String,
    root_token: String,
    path: String,
    binding: BulkBinding,
    cancellation: Arc<CancelState>,
    channel: Channel<InvokeResponseBody>,
}

#[derive(Clone)]
struct FileWriteJob {
    transfer_id: String,
    connection: ConnectionSpec,
    root: String,
    root_token: String,
    path: String,
    operation_id: String,
    file_generation: u64,
    content: Arc<[u8]>,
    binding: BulkBinding,
    cancellation: Arc<CancelState>,
    channel: Channel<InvokeResponseBody>,
}

#[derive(Clone)]
enum FileIoJob {
    Read(FileReadJob),
    Write(FileWriteJob),
}

impl FileIoJob {
    fn transfer_id(&self) -> &str {
        match self {
            Self::Read(job) => &job.transfer_id,
            Self::Write(job) => &job.transfer_id,
        }
    }

    fn cancellation(&self) -> &Arc<CancelState> {
        match self {
            Self::Read(job) => &job.cancellation,
            Self::Write(job) => &job.cancellation,
        }
    }

    fn channel(&self) -> &Channel<InvokeResponseBody> {
        match self {
            Self::Read(job) => &job.channel,
            Self::Write(job) => &job.channel,
        }
    }

    fn binding(&self) -> &BulkBinding {
        match self {
            Self::Read(job) => &job.binding,
            Self::Write(job) => &job.binding,
        }
    }
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
    file_io.enqueue(FileIoJob::Read(FileReadJob {
        transfer_id: transfer_id.clone(),
        connection: profiles.connection_for(&profile_id)?,
        root,
        root_token,
        path,
        binding,
        cancellation,
        channel: on_event,
    }))?;
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
    file_io.enqueue(FileIoJob::Write(FileWriteJob {
        transfer_id: transfer_id.clone(),
        connection: profiles.connection_for(&profile_id)?,
        root,
        root_token,
        path,
        operation_id,
        file_generation: parse_optional_u64("fileGeneration", &file_generation)?,
        content: Arc::from(content),
        binding,
        cancellation,
        channel: on_event,
    }))?;
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

    fn enqueue(&self, job: FileIoJob) -> Result<(), String> {
        let transfer_id = job.transfer_id().to_owned();
        let cancellation = Arc::clone(job.cancellation());
        let binding = job.binding().clone();
        let queued = QueuedPublication::raw(
            job.channel().clone(),
            file_json_frame(
                1,
                TransferEvent::new(job.transfer_id(), job.binding(), TransferState::Queued).value(),
            )?,
            file_json_frame(
                4,
                late_queued_publication_rollback(job.transfer_id(), job.binding()),
            )?,
            "could not publish queued file transfer event",
        );
        let started_job = job.clone();
        let work_job = job.clone();
        let finished_job = job.clone();
        enqueue_transfer_with_queued(
            transfer_id,
            binding,
            cancellation,
            queued,
            move || emit_file_job_state(&started_job, 1, TransferState::Running),
            move || match &work_job {
                FileIoJob::Read(job) => run_file_read(job).map_err(TransferFailure::not_published),
                FileIoJob::Write(job) => run_file_write(job),
            },
            move |result, _reason| {
                if let Err(failure) = result {
                    let state = if finished_job.cancellation().reason() == CancelReason::User
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

fn emit_file_failure(job: &FileIoJob, kind: u8, state: TransferState, failure: TransferFailure) {
    let event = TransferEvent::new(job.transfer_id(), job.binding(), state)
        .outcome(failure.outcome)
        .cleanup(failure.cleanup_status, failure.cleanup_error);
    let event = if state == TransferState::Cancelled {
        event.error(failure.error)
    } else {
        event.failure(failure.failure_kind, failure.error)
    };
    emit_file_json(job.channel(), kind, event.value());
}

fn emit_file_job_state(job: &FileIoJob, kind: u8, state: TransferState) {
    let _ = send_file_job_state(job, kind, state);
}

fn send_file_job_state(job: &FileIoJob, kind: u8, state: TransferState) -> Result<(), String> {
    send_file_json(
        job.channel(),
        kind,
        TransferEvent::new(job.transfer_id(), job.binding(), state).value(),
    )
}

/// Opens one file over exactly one bulk request.
///
/// The staircase this replaces asked three different questions — stat, bulk
/// preflight, then one request per mebibyte — so a warm 10 MiB open cost twelve
/// round trips on the remote link and each answer could describe a different
/// version of the file. Here the host classifies and streams from one
/// descriptor, so the cost is one round trip plus transfer time and the
/// metadata, generation, and bytes provably belong together.
fn run_file_read(job: &FileReadJob) -> Result<(), String> {
    job.binding.validate()?;
    let _deadline = job.cancellation.arm_inactivity_deadline();
    let mut lease =
        BulkLease::acquire(&job.connection, &job.binding, &job.cancellation, &_deadline)?;
    let _process_binding = job.cancellation.bind_process(lease.process_id())?;
    let mut protocol = lease.client();
    let mut state = FileReadStream::new(job, &_deadline);
    let response = protocol
        .stream_cancellable(
            v1::Request {
                operation: v1::Operation::OpenFileStream.into(),
                file: Some(v1::FileServiceRequest {
                    operation_id: job.transfer_id.clone(),
                    root: job.root.clone(),
                    root_token: job.root_token.clone(),
                    path: job.path.clone(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            &job.cancellation,
            &_deadline,
            &mut |frame| state.accept(frame),
        )
        .map_err(|error| error.to_string())?;
    _deadline.touch();
    let content = response
        .file
        .and_then(|file| file.content)
        .ok_or("file open response omitted its content classification")?;
    state.finish(&content)
}

/// The renderer-facing side of one `OpenFileStream` exchange.
///
/// It exists so the frame observer stays a state machine with one owner rather
/// than a pile of captured mutable locals: exactly one header, strictly ordered
/// body frames, and a terminal response that has to agree with both.
struct FileReadStream<'a> {
    job: &'a FileReadJob,
    /// Refreshed by every accepted frame. Without it the inactivity watchdog
    /// counts the whole transfer as one silence, so a large open on a slow
    /// remote link is cancelled precisely while it is making steady progress.
    deadline: &'a DeadlineGuard,
    header: Option<v1::FileStreamHeader>,
    offset: u64,
    hasher: blake3::Hasher,
    completed: bool,
}

impl<'a> FileReadStream<'a> {
    fn new(job: &'a FileReadJob, deadline: &'a DeadlineGuard) -> Self {
        Self {
            job,
            deadline,
            header: None,
            offset: 0,
            hasher: blake3::Hasher::new(),
            completed: false,
        }
    }

    fn accept(&mut self, frame: v1::FileStreamFrame) -> Result<(), String> {
        self.deadline.touch();
        if let Some(header) = frame.header {
            if self.header.is_some() {
                return Err("file open stream repeated its header".into());
            }
            let metadata = header
                .metadata
                .clone()
                .ok_or("file open stream header omitted metadata")?;
            let kind = v1::FileContentKind::try_from(header.content_kind).unwrap_or_default();
            emit_scoped_file_json(
                &self.job.channel,
                &self.job.transfer_id,
                &self.job.binding,
                1,
                TransferState::Running,
                json!({
                    "eventKind": "metadata",
                    "metadata": metadata_json(&metadata),
                    "contentKind": content_kind_name(kind),
                    "totalBytes": header.total_bytes.to_string(),
                }),
            );
            self.header = Some(header);
            return Ok(());
        }
        let header = self
            .header
            .as_ref()
            .ok_or("file open stream sent a body before its header")?;
        if !header.content_streaming {
            return Err("file open stream sent a body it declared it would not send".into());
        }
        if self.completed {
            return Err("file open stream continued past its own end".into());
        }
        if frame.offset != self.offset {
            return Err("file open stream chunks arrived out of sequence".into());
        }
        self.hasher.update(&frame.data);
        if !frame.data.is_empty() {
            emit_file_chunk(&self.job.channel, self.offset, &frame.data);
        }
        self.offset = self
            .offset
            .checked_add(frame.data.len() as u64)
            .ok_or("file open byte counter overflow")?;
        if self.offset > header.total_bytes {
            return Err("file open stream exceeded its declared byte count".into());
        }
        emit_scoped_file_json(
            &self.job.channel,
            &self.job.transfer_id,
            &self.job.binding,
            1,
            TransferState::Running,
            json!({
                "transferredBytes": self.offset.to_string(),
                "totalBytes": header.total_bytes.to_string(),
            }),
        );
        if frame.eof {
            if self.offset != header.total_bytes
                || frame.blake3 != self.hasher.finalize().to_hex().to_string()
            {
                return Err("file open byte/BLAKE3 verification failed".into());
            }
            self.completed = true;
        }
        Ok(())
    }

    /// Publishes the terminal renderer event, after checking that the response
    /// describes the same file the header and body did.
    fn finish(self, content: &v1::FileContent) -> Result<(), String> {
        let header = self
            .header
            .ok_or("file open response arrived without a header")?;
        let metadata = header
            .metadata
            .clone()
            .ok_or("file open stream header omitted metadata")?;
        if content.generation != header.generation || content.kind != header.content_kind {
            return Err("file open response disagreed with its own stream header".into());
        }
        if header.content_streaming && !self.completed {
            return Err("file open stream ended before its declared content".into());
        }
        emit_scoped_file_json(
            &self.job.channel,
            &self.job.transfer_id,
            &self.job.binding,
            3,
            TransferState::Completed,
            json!({
                "outcome": TransferOutcome::Published,
                "cleanupStatus": CleanupStatus::NotNeeded,
                "metadataOnly": !header.content_streaming,
                "metadata": metadata_json(&metadata),
                "contentKind": content_kind_name(
                    v1::FileContentKind::try_from(header.content_kind).unwrap_or_default(),
                ),
                "transferredBytes": self.offset.to_string(),
                "totalBytes": header.total_bytes.to_string(),
                "generation": header.generation.to_string(),
            }),
        );
        Ok(())
    }
}

fn run_file_write(job: &FileWriteJob) -> Result<(), TransferFailure> {
    job.binding.validate()?;
    let _deadline = job.cancellation.arm_inactivity_deadline();
    let mut lease =
        BulkLease::acquire(&job.connection, &job.binding, &job.cancellation, &_deadline)?;
    let _process_binding = job.cancellation.bind_process(lease.process_id())?;
    let mut protocol = lease.client();
    protocol.request_cancellable(
        v1::Request {
            operation: v1::Operation::BeginFileWrite.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: job.operation_id.clone(),
                root: job.root.clone(),
                root_token: job.root_token.clone(),
                path: job.path.clone(),
                transfer_id: job.transfer_id.clone(),
                file_generation: job.file_generation,
                total_bytes: job.content.len() as u64,
                ..Default::default()
            }),
            ..Default::default()
        },
        &job.cancellation,
        &_deadline,
    )?;
    _deadline.touch();
    let mut offset = 0_u64;
    for chunk in job.content.chunks(BULK_CHUNK_BYTES as usize) {
        if job.cancellation.is_cancelled() {
            let _ = protocol.cancel_write(&job.operation_id, &job.transfer_id);
            return Err("file write cancelled".into());
        }
        let next = offset
            .checked_add(chunk.len() as u64)
            .ok_or("file write byte counter overflow")?;
        protocol.request_cancellable(
            v1::Request {
                operation: v1::Operation::WriteFileChunk.into(),
                file: Some(v1::FileServiceRequest {
                    operation_id: job.operation_id.clone(),
                    transfer_id: job.transfer_id.clone(),
                    offset,
                    total_bytes: job.content.len() as u64,
                    content: chunk.to_vec(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            &job.cancellation,
            &_deadline,
        )?;
        _deadline.touch();
        offset = next;
        emit_scoped_file_json(
            &job.channel,
            &job.transfer_id,
            &job.binding,
            1,
            TransferState::Running,
            json!({
                "operationId": job.operation_id,
                "transferredBytes": offset.to_string(),
                "totalBytes": job.content.len().to_string(),
            }),
        );
    }
    let digest = blake3::hash(&job.content).to_hex().to_string();
    if job.cancellation.is_cancelled() {
        let _ = protocol.cancel_write(&job.operation_id, &job.transfer_id);
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
            "operationId": job.operation_id,
            "transferredBytes": offset.to_string(),
            "totalBytes": job.content.len().to_string(),
        }),
    );
    let response = protocol
        .request_classified_with_deadline(
            v1::Request {
                operation: v1::Operation::CommitFileWrite.into(),
                file: Some(v1::FileServiceRequest {
                    operation_id: job.operation_id.clone(),
                    transfer_id: job.transfer_id.clone(),
                    blake3: digest.clone(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            &_deadline,
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
    _deadline.touch();
    let metadata = response
        .file
        .and_then(|file| file.metadata)
        .ok_or("file write commit omitted metadata")?;
    if metadata.size != job.content.len() as u64 {
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
                "operationId": job.operation_id,
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
            "operationId": job.operation_id,
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

fn content_kind_name(kind: v1::FileContentKind) -> &'static str {
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

fn emit_scoped_file_json(
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

fn emit_file_chunk(channel: &Channel<InvokeResponseBody>, offset: u64, data: &[u8]) {
    let mut frame = Vec::with_capacity(9 + data.len());
    frame.push(2);
    frame.extend_from_slice(&offset.to_be_bytes());
    frame.extend_from_slice(data);
    let _ = channel.send(InvokeResponseBody::Raw(frame));
}
