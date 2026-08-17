//! One editor open, from its single bulk request to the renderer event that
//! publishes it.
//!
//! Split out of `editor_manager.rs`, which held two structurally different
//! protocols for the same resource — this exchange and the chunked write
//! staircase — in one file, along with both Tauri commands and 200 lines of
//! tests for only one of them.

use serde_json::json;
use tmux_agent_protocol::v1;

use super::bulk_pool::BulkLease;
use super::bulk_protocol::Exchange;
use super::editor_manager::{FileJob, content_kind_name, emit_file_chunk, emit_scoped_file_json};
use super::scheduler::DeadlineGuard;
use super::serialization::metadata_json;
use super::transfer_event::{CleanupStatus, TransferOutcome, TransferState};

/// Opens one file over exactly one bulk request.
///
/// The staircase this replaces asked three different questions — stat, bulk
/// preflight, then one request per mebibyte — so a warm 10 MiB open cost twelve
/// round trips on the remote link and each answer could describe a different
/// version of the file. Here the host classifies and streams from one
/// descriptor, so the cost is one round trip plus transfer time and the
/// metadata, generation, and bytes provably belong together.
pub(super) fn run_file_read(job: &FileJob) -> Result<(), String> {
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
    let mut state = FileReadStream::new(job, &deadline);
    let response = protocol
        .request_classified(
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
            Exchange {
                cancellation: Some(&job.cancellation),
                deadline: Some(&deadline),
                on_frame: Some(&mut |frame| state.accept(frame)),
            },
        )
        .map_err(|error| error.to_string())?;
    deadline.touch();
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
    job: &'a FileJob,
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
    fn new(job: &'a FileJob, deadline: &'a DeadlineGuard) -> Self {
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
        // Re-checked per frame, as the write path does per chunk: a read whose
        // connection scope was replaced mid-transfer must stop rather than run
        // to completion and publish content for a scope nobody is showing.
        self.job.binding.validate()?;
        // The host stamps this on the header and on every body frame. Checked
        // rather than ignored: a field the sender writes and the receiver never
        // looks at is worse than no field, because it reads as a binding that
        // holds. Scoping is genuinely by `request_id` on a lease-exclusive
        // bridge, so this can only ever fail on a host that is confused about
        // which open it is answering — which is exactly when the desktop should
        // refuse the bytes rather than publish them.
        if frame.operation_id != self.job.transfer_id {
            return Err("file open stream frame named another operation".into());
        }
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
        // The renderer's channel going away means the rest of this body has no
        // reader. Failing here ends the exchange — and, because the caller
        // treats an error as a cancellation, stops the remote work with it —
        // instead of streaming a whole file into a channel nobody owns and
        // then reporting success.
        if !frame.data.is_empty() && !emit_file_chunk(&self.job.channel, self.offset, &frame.data) {
            return Err("file open channel closed before its content".into());
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
                // The leaf's generation — the identity every other producer of
                // an on-screen fact reports: directory listings, precise watch
                // events, and the write path all describe the name the user
                // opened. `header.generation` describes the descriptor the host
                // actually read, which for a symlink is its target, and exists
                // only for the content cross-check above. Publishing it here
                // made a symlinked file disagree with its own listing row on
                // every open, costing a second full remote read per event.
                "generation": metadata.generation.to_string(),
            }),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::editor_manager::FileJobKind;
    use super::super::scheduler::{BulkBinding, CancelState};
    use super::*;
    use crate::connection::{ConnectionSpec, TerminalClient};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use tauri::ipc::Channel;

    fn job() -> FileJob {
        let client = Arc::new(TerminalClient::new());
        client.ready.store(true, Ordering::Release);
        client.terminal_epoch.store(7, Ordering::Release);
        *client.server_identity.lock().unwrap() = "server".into();
        FileJob {
            transfer_id: "read".into(),
            connection: ConnectionSpec::Local,
            root: "/repo".into(),
            root_token: "token".into(),
            path: "/repo/a.txt".into(),
            binding: BulkBinding::capture(client, "server".into(), 7).unwrap(),
            cancellation: Arc::new(CancelState::new()),
            channel: Channel::new(|_| Ok(())),
            kind: FileJobKind::Read,
        }
    }

    fn header(bytes: &[u8], streaming: bool) -> v1::FileStreamHeader {
        v1::FileStreamHeader {
            metadata: Some(v1::FileMetadata {
                path: "/repo/a.txt".into(),
                generation: 5,
                ..Default::default()
            }),
            content_kind: v1::FileContentKind::Text.into(),
            generation: 5,
            total_bytes: bytes.len() as u64,
            content_streaming: streaming,
        }
    }

    fn body(offset: u64, data: &[u8], eof: bool, digest: &str) -> v1::FileStreamFrame {
        v1::FileStreamFrame {
            operation_id: "read".into(),
            header: None,
            offset,
            data: data.to_vec(),
            eof,
            blake3: digest.to_owned(),
        }
    }

    fn content(generation: u64, kind: v1::FileContentKind) -> v1::FileContent {
        v1::FileContent {
            metadata: None,
            kind: kind.into(),
            content: Vec::new(),
            generation,
        }
    }

    /// The exchange this whole operation exists to make one round trip.
    #[test]
    fn a_header_a_body_and_an_agreeing_response_are_accepted() {
        let job = job();
        let deadline = job.cancellation.arm_inactivity_deadline();
        let mut stream = FileReadStream::new(&job, &deadline);
        let bytes = b"hello";
        let digest = blake3::hash(bytes).to_hex().to_string();
        stream
            .accept(v1::FileStreamFrame {
                operation_id: "read".into(),
                header: Some(header(bytes, true)),
                ..Default::default()
            })
            .unwrap();
        stream.accept(body(0, bytes, true, &digest)).unwrap();
        stream
            .finish(&content(5, v1::FileContentKind::Text))
            .unwrap();
    }

    /// Every refusal in the state machine, each named by what it protects.
    ///
    /// The whole point of the type is that content, identity, and byte count
    /// provably belong together; without these the desktop would publish a
    /// file assembled from frames that never agreed with each other.
    #[test]
    fn a_stream_that_disagrees_with_itself_is_refused_rather_than_published() {
        let bytes = b"hello";
        let digest = blake3::hash(bytes).to_hex().to_string();
        /// One way a stream can contradict itself, and its name.
        type Contradiction = (
            &'static str,
            Box<dyn Fn(&mut FileReadStream<'_>) -> Result<(), String>>,
        );
        let cases: Vec<Contradiction> = vec![
            (
                "a body with no header",
                Box::new(move |stream| stream.accept(body(0, b"hello", true, ""))),
            ),
            (
                "a second header",
                Box::new(|stream| {
                    stream.accept(v1::FileStreamFrame {
                        operation_id: "read".into(),
                        header: Some(header(b"hello", true)),
                        ..Default::default()
                    })?;
                    stream.accept(v1::FileStreamFrame {
                        operation_id: "read".into(),
                        header: Some(header(b"hello", true)),
                        ..Default::default()
                    })
                }),
            ),
            (
                "a body frame out of sequence",
                Box::new(|stream| {
                    stream.accept(v1::FileStreamFrame {
                        operation_id: "read".into(),
                        header: Some(header(b"hello", true)),
                        ..Default::default()
                    })?;
                    stream.accept(body(8, b"lo", false, ""))
                }),
            ),
            (
                "more bytes than the header declared",
                Box::new(|stream| {
                    stream.accept(v1::FileStreamFrame {
                        operation_id: "read".into(),
                        header: Some(header(b"hi", true)),
                        ..Default::default()
                    })?;
                    stream.accept(body(0, b"far too many", false, ""))
                }),
            ),
            (
                "a digest that does not describe the body",
                Box::new(|stream| {
                    stream.accept(v1::FileStreamFrame {
                        operation_id: "read".into(),
                        header: Some(header(b"hello", true)),
                        ..Default::default()
                    })?;
                    stream.accept(body(0, b"hello", true, "0000"))
                }),
            ),
            (
                "a body cut short of its declared length",
                Box::new(move |stream| {
                    stream.accept(v1::FileStreamFrame {
                        operation_id: "read".into(),
                        header: Some(header(b"hello", true)),
                        ..Default::default()
                    })?;
                    stream.accept(body(0, b"hel", false, ""))?;
                    let taken =
                        std::mem::replace(stream, FileReadStream::new(stream.job, stream.deadline));
                    taken.finish(&content(5, v1::FileContentKind::Text))
                }),
            ),
        ];
        for (name, run) in cases {
            let job = job();
            let deadline = job.cancellation.arm_inactivity_deadline();
            let mut stream = FileReadStream::new(&job, &deadline);
            assert!(run(&mut stream).is_err(), "{name} was published anyway");
        }

        // And a terminal response that describes a different file than the
        // header did, which is the cross-check the two generations exist for.
        for wrong in [
            content(6, v1::FileContentKind::Text),
            content(5, v1::FileContentKind::Binary),
        ] {
            let job = job();
            let deadline = job.cancellation.arm_inactivity_deadline();
            let mut stream = FileReadStream::new(&job, &deadline);
            stream
                .accept(v1::FileStreamFrame {
                    operation_id: "read".into(),
                    header: Some(header(bytes, true)),
                    ..Default::default()
                })
                .unwrap();
            stream.accept(body(0, bytes, true, &digest)).unwrap();
            assert!(
                stream.finish(&wrong).is_err(),
                "a response describing another file was published"
            );
        }
    }

    /// A frame that names another operation is refused, not published.
    #[test]
    fn a_frame_belonging_to_another_open_is_refused() {
        let job = job();
        let deadline = job.cancellation.arm_inactivity_deadline();
        let mut stream = FileReadStream::new(&job, &deadline);
        assert!(
            stream
                .accept(v1::FileStreamFrame {
                    operation_id: "some-other-open".into(),
                    header: Some(header(b"hello", true)),
                    ..Default::default()
                })
                .is_err(),
            "a header for another operation was accepted"
        );
    }

    /// A classification-only open owes no body at all.
    #[test]
    fn a_metadata_only_open_finishes_without_content() {
        let job = job();
        let deadline = job.cancellation.arm_inactivity_deadline();
        let mut stream = FileReadStream::new(&job, &deadline);
        stream
            .accept(v1::FileStreamFrame {
                operation_id: "read".into(),
                header: Some(header(&[], false)),
                ..Default::default()
            })
            .unwrap();
        stream
            .finish(&content(5, v1::FileContentKind::Text))
            .unwrap();
    }
}
