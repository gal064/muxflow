//! Reading the diff bodies the control response deliberately withheld.
//!
//! The control connection carries keystrokes, terminal frames and every other
//! Git request. A multi-megabyte diff body in front of those is a visible
//! typing stall, so the control response describes the body and this reads it
//! on the independent bulk connection instead. Nothing is staged on the host
//! between the two requests: the size and digest the control response stated
//! are what the bulk read is checked against.
//!
//! Both sides of one diff are read by one job over one bulk lease. That is one
//! bridge acquisition rather than two, it lets the host serve consecutive
//! chunks from one read, and it means a failure on either side stops the other
//! instead of leaving it streaming to nobody.
//!
//! A connection with no bulk lane never gets here: the host inlines its diff
//! bodies instead, because a reference to a body the client cannot fetch would
//! be worse than the payload it was avoiding.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use serde::Deserialize;
use tauri::{
    State,
    ipc::{Channel, InvokeResponseBody},
};
use tmux_agent_protocol::v1;
use uuid::Uuid;

use super::files::bulk_pool::BulkLease;
use super::files::bulk_protocol::Exchange;
use super::files::scheduler::{BulkBinding, CancelState};
use super::{ConnectionSpec, ProfileStore, TerminalClient, TerminalClients, get_client};

/// Bytes requested per round trip. Matches the file transfer chunk size, which
/// the bulk framing and host flow control are already sized for.
const GIT_CONTENT_CHUNK: u32 = 1024 * 1024;

/// Diff-body reads one connection runs at once.
///
/// One is enough — a diff surface has one read in flight and cancels it before
/// starting another — and it bounds this lane to a single bulk bridge per
/// connection beyond the two the transfer engine may hold. Per connection, so
/// one profile's slow read cannot serialize an unrelated profile behind it.
const MAX_CONCURRENT_READS: usize = 1;

/// Frame kinds on the response channel. The renderer's matching decoder is in
/// `apps/desktop/src/features/git/api.ts`.
const FRAME_CHUNK: u8 = 1;
const FRAME_COMPLETE: u8 = 2;
const FRAME_ERROR: u8 = 3;

pub(super) const SIDE_OLD: u8 = 1;
pub(super) const SIDE_NEW: u8 = 2;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffContentCommand {
    pub client_id: String,
    pub profile_id: String,
    pub expected_server_identity: String,
    pub connection_epoch: String,
    pub root: String,
    pub root_token: String,
    pub repository_id: String,
    #[serde(default)]
    pub path: Vec<u8>,
    #[serde(default)]
    pub original_path: Vec<u8>,
    pub diff_target: String,
    /// The old side, when the control response withheld it.
    #[serde(default)]
    pub old: Option<GitDiffContentSideCommand>,
    /// The new side, when the control response withheld it.
    #[serde(default)]
    pub new: Option<GitDiffContentSideCommand>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffContentSideCommand {
    pub content_digest: String,
    /// Decimal u64 string, as every u64 crosses this bridge.
    pub size: String,
}

/// One side to read, resolved from the command.
struct DeferredSide {
    side: v1::GitDiffContentSide,
    frame_tag: u8,
    digest: String,
    size: u64,
}

/// Everything one diff-body read is bound to.
struct GitContentJob {
    read_id: String,
    connection: ConnectionSpec,
    binding: BulkBinding,
    cancellation: Arc<CancelState>,
    request: v1::GitRequest,
    sides: Vec<DeferredSide>,
    channel: Channel<InvokeResponseBody>,
}

#[tauri::command]
pub async fn read_git_diff_content(
    command: GitDiffContentCommand,
    on_event: Channel<InvokeResponseBody>,
    profiles: State<'_, ProfileStore>,
    clients: State<'_, TerminalClients>,
) -> Result<String, String> {
    // Resolved once: the registry that admits this read and the connection it
    // is bound to must be the same client, not two lookups that a reconnect
    // could land on either side of.
    let client = get_client(&clients, &command.client_id)?;
    let reads = client.git_content_reads();
    let job = prepare(command, on_event, &profiles, &client)?;
    let read_id = job.read_id.clone();
    let registration = reads.register(&read_id, &job.cancellation)?;
    let reads = Arc::clone(&reads);
    // Kept out of the worker so a panicking worker still has somewhere to
    // report to; otherwise the renderer's read would never settle at all.
    let reporter = job.channel.clone();
    tauri::async_runtime::spawn(async move {
        let _registration = registration;
        let permit = match reads.admission.acquire().await {
            Ok(permit) => permit,
            Err(_) => {
                emit_error(&reporter, "Git diff content admission closed");
                return;
            }
        };
        let completed = tokio::task::spawn_blocking(move || {
            let outcome = stream_git_diff_content(&job);
            (job, outcome)
        })
        .await;
        drop(permit);
        match completed {
            Ok((job, Ok(total))) => {
                let mut frame = vec![FRAME_COMPLETE];
                frame.extend_from_slice(&total.to_be_bytes());
                let _ = job.channel.send(InvokeResponseBody::Raw(frame));
            }
            Ok((job, Err(error))) => emit_error(&job.channel, &error),
            Err(error) => emit_error(&reporter, &format!("Git diff content read failed: {error}")),
        }
    });
    Ok(read_id)
}

#[tauri::command]
pub fn cancel_git_diff_content(
    client_id: String,
    read_id: String,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    get_client(&clients, &client_id)?
        .git_content_reads()
        .cancel(&read_id);
    Ok(())
}

fn prepare(
    command: GitDiffContentCommand,
    channel: Channel<InvokeResponseBody>,
    profiles: &ProfileStore,
    client: &Arc<TerminalClient>,
) -> Result<GitContentJob, String> {
    if command.root.is_empty()
        || command.root_token.is_empty()
        || command.repository_id.is_empty()
        || command.path.is_empty()
    {
        return Err("Git diff content requires its repository and path identity".into());
    }
    let connection_epoch: u64 = command
        .connection_epoch
        .parse()
        .map_err(|_| "connectionEpoch must be a decimal u64 string".to_owned())?;
    let sides = [
        (v1::GitDiffContentSide::Old, SIDE_OLD, command.old.as_ref()),
        (v1::GitDiffContentSide::New, SIDE_NEW, command.new.as_ref()),
    ]
    .into_iter()
    .filter_map(|(side, frame_tag, requested)| {
        requested.map(|requested| {
            Ok(DeferredSide {
                side,
                frame_tag,
                digest: non_empty(&requested.content_digest, "a content digest")?,
                size: positive_u64(&requested.size)?,
            })
        })
    })
    .collect::<Result<Vec<_>, String>>()?;
    if sides.is_empty() {
        return Err("Git diff content requires at least one deferred side".into());
    }
    // Every binding failure — read-only, a replaced epoch, a replaced server
    // identity — is a scope this read cannot be performed in.
    let binding = BulkBinding::capture(
        Arc::clone(client),
        command.expected_server_identity.clone(),
        connection_epoch,
    )?;
    Ok(GitContentJob {
        read_id: Uuid::new_v4().to_string(),
        connection: profiles.connection_for(&command.profile_id)?,
        binding,
        cancellation: Arc::new(CancelState::new()),
        request: v1::GitRequest {
            operation_id: String::new(),
            root: command.root,
            root_token: command.root_token,
            expected_server_identity: command.expected_server_identity,
            repository_id: command.repository_id,
            connection_epoch,
            path: command.path,
            original_path: command.original_path,
            diff_target: diff_target(&command.diff_target)?.into(),
            ..Default::default()
        },
        sides,
        channel,
    })
}

fn stream_git_diff_content(job: &GitContentJob) -> Result<u64, String> {
    job.binding.validate()?;
    let deadline = job.cancellation.arm_inactivity_deadline();
    let mut lease =
        BulkLease::acquire(&job.connection, &job.binding, &job.cancellation, &deadline)?;
    let _process_binding = job.cancellation.bind_process(lease.process_id())?;
    let mut protocol = lease.client();
    stream_sides(job, |request| {
        // Cancellable and deadline-fed, and nothing else: a Git diff body comes
        // back in its response, not in the file lane's body frames, so an
        // exchange that accepted one would be accepting a frame this lane can
        // never produce.
        let response = protocol.request(request, Exchange::live(&job.cancellation, &deadline));
        deadline.touch();
        response
    })
}

/// Reads every deferred side in order through one request transport.
fn stream_sides(
    job: &GitContentJob,
    mut request: impl FnMut(v1::Request) -> Result<v1::Response, String>,
) -> Result<u64, String> {
    let mut total: u64 = 0;
    for side in &job.sides {
        let mut delivered: u64 = 0;
        loop {
            if job.cancellation.is_cancelled() {
                return Err("Git diff content read was cancelled".into());
            }
            let mut git = job.request.clone();
            git.operation_id = Uuid::new_v4().to_string();
            git.content = Some(v1::GitDiffContentRequest {
                side: side.side.into(),
                expected_content_digest: side.digest.clone(),
                expected_size: side.size,
                offset: delivered,
                length: GIT_CONTENT_CHUNK,
            });
            let response = request(v1::Request {
                operation: v1::Operation::GitDiffContent.into(),
                git: Some(git),
                ..Default::default()
            })?;
            let chunk = response
                .git
                .and_then(|git| git.content_chunk)
                .ok_or("host omitted the Git diff content chunk")?;
            if chunk.offset != delivered {
                return Err("host returned an out-of-order Git diff content chunk".into());
            }
            if chunk.total_size != side.size {
                return Err("host returned a Git diff body of a different size".into());
            }
            let mut frame = Vec::with_capacity(10 + chunk.data.len());
            frame.push(FRAME_CHUNK);
            frame.push(side.frame_tag);
            frame.extend_from_slice(&chunk.offset.to_be_bytes());
            frame.extend_from_slice(&chunk.data);
            job.channel
                .send(InvokeResponseBody::Raw(frame))
                .map_err(|error| format!("could not publish Git diff content: {error}"))?;
            delivered = delivered.saturating_add(chunk.data.len() as u64);
            if chunk.last {
                break;
            }
            if chunk.data.is_empty() {
                return Err("host stalled the Git diff content stream".into());
            }
        }
        if delivered != side.size {
            return Err("Git diff content ended before the described body".into());
        }
        total = total.saturating_add(delivered);
    }
    Ok(total)
}

fn emit_error(channel: &Channel<InvokeResponseBody>, error: &str) {
    let mut frame = vec![FRAME_ERROR];
    frame.extend_from_slice(error.as_bytes());
    let _ = channel.send(InvokeResponseBody::Raw(frame));
}

fn non_empty(value: &str, label: &str) -> Result<String, String> {
    if value.is_empty() {
        return Err(format!("Git diff content requires {label}"));
    }
    Ok(value.to_owned())
}

fn positive_u64(value: &str) -> Result<u64, String> {
    let parsed: u64 = value
        .parse()
        .map_err(|_| "size must be a decimal u64 string".to_owned())?;
    if parsed == 0 {
        return Err("Git diff content size must be positive".into());
    }
    Ok(parsed)
}

fn diff_target(value: &str) -> Result<v1::GitDiffTarget, String> {
    match value {
        "unstaged" => Ok(v1::GitDiffTarget::Unstaged),
        "staged" => Ok(v1::GitDiffTarget::Staged),
        _ => Err(format!("unsupported Git diff target {value}")),
    }
}

/// The diff-body reads one connection currently owns.
///
/// Registered per connection so replacing that connection cancels them, and
/// bounded because a caller that starts reads it never finishes must not be
/// able to grow this without limit. A diff surface cancels its previous read
/// before starting another, so the bound is on misbehaviour, not on use.
pub(crate) struct GitContentReads {
    entries: Mutex<HashMap<String, Arc<CancelState>>>,
    admission: tokio::sync::Semaphore,
}

impl Default for GitContentReads {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            admission: tokio::sync::Semaphore::new(MAX_CONCURRENT_READS),
        }
    }
}

/// Reads one connection may have outstanding at once.
const MAX_GIT_CONTENT_READS: usize = 8;

impl GitContentReads {
    fn register(
        self: &Arc<Self>,
        read_id: &str,
        cancellation: &Arc<CancelState>,
    ) -> Result<GitContentRegistration, String> {
        let mut entries = self.entries.lock().unwrap();
        if entries.len() >= MAX_GIT_CONTENT_READS {
            return Err("too many Git diff content reads are in progress".into());
        }
        entries.insert(read_id.to_owned(), Arc::clone(cancellation));
        Ok(GitContentRegistration {
            reads: Arc::clone(self),
            read_id: read_id.to_owned(),
        })
    }

    fn cancel(&self, read_id: &str) {
        let cancellation = self.entries.lock().unwrap().get(read_id).cloned();
        if let Some(cancellation) = cancellation {
            cancellation.cancel();
        }
    }

    /// Connection replacement. Nothing registered here can still be delivered.
    pub(crate) fn cancel_all(&self) {
        for (_, cancellation) in self.entries.lock().unwrap().drain() {
            cancellation.cancel();
        }
    }
}

/// Removes a read from its connection's registry exactly once.
struct GitContentRegistration {
    reads: Arc<GitContentReads>,
    read_id: String,
}

impl Drop for GitContentRegistration {
    fn drop(&mut self) {
        self.reads.entries.lock().unwrap().remove(&self.read_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn ready_client() -> Arc<TerminalClient> {
        let client = Arc::new(TerminalClient::new());
        client.ready.store(true, Ordering::Release);
        client.terminal_epoch.store(7, Ordering::Release);
        *client.server_identity.lock().unwrap() = "server".into();
        client
    }

    fn captured_channel() -> (Channel<InvokeResponseBody>, Arc<Mutex<Vec<Vec<u8>>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&sent);
        let channel = Channel::new(move |body| {
            if let InvokeResponseBody::Raw(bytes) = body {
                sink.lock().unwrap().push(bytes);
            }
            Ok(())
        });
        (channel, sent)
    }

    fn deferred(side: v1::GitDiffContentSide, frame_tag: u8, digest: &str, size: u64) -> DeferredSide {
        DeferredSide {
            side,
            frame_tag,
            digest: digest.to_owned(),
            size,
        }
    }

    fn job(sides: Vec<DeferredSide>) -> (GitContentJob, Arc<Mutex<Vec<Vec<u8>>>>) {
        let (channel, sent) = captured_channel();
        let job = GitContentJob {
            read_id: "read".into(),
            connection: ConnectionSpec::Local,
            binding: BulkBinding::capture(ready_client(), "server".into(), 7).unwrap(),
            cancellation: Arc::new(CancelState::new()),
            request: v1::GitRequest {
                root: "/repo".into(),
                root_token: "token".into(),
                expected_server_identity: "server".into(),
                repository_id: "repo".into(),
                connection_epoch: 7,
                path: b"a.txt".to_vec(),
                diff_target: v1::GitDiffTarget::Unstaged.into(),
                ..Default::default()
            },
            sides,
            channel,
        };
        (job, sent)
    }

    /// Answers one request the way the host does: from the described body,
    /// honouring the requested offset but free to serve fewer bytes than asked.
    fn host_chunk(body: &[u8], content: &v1::GitDiffContentRequest, serve: usize) -> v1::Response {
        let offset = content.offset as usize;
        let end = offset.saturating_add(serve).min(body.len());
        chunk_response(v1::GitDiffContentChunk {
            offset: content.offset,
            data: body[offset.min(body.len())..end].to_vec(),
            last: end >= body.len(),
            total_size: body.len() as u64,
        })
    }

    fn chunk_response(chunk: v1::GitDiffContentChunk) -> v1::Response {
        v1::Response {
            git: Some(v1::GitResponse {
                content_chunk: Some(chunk),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn content_of(request: &v1::Request) -> v1::GitDiffContentRequest {
        request
            .git
            .as_ref()
            .and_then(|git| git.content.as_ref())
            .expect("every request in this stream carries a content descriptor")
            .clone()
    }

    /// Splits a published `FRAME_CHUNK` frame back into tag, offset, and data.
    fn decode_chunk_frame(frame: &[u8]) -> (u8, u64, Vec<u8>) {
        assert_eq!(frame[0], FRAME_CHUNK, "not a chunk frame: {frame:?}");
        (
            frame[1],
            u64::from_be_bytes(frame[2..10].try_into().unwrap()),
            frame[10..].to_vec(),
        )
    }

    fn profiles() -> ProfileStore {
        // A path that does not exist loads the defaults, whose one profile is
        // the "local" identity the valid command below names.
        ProfileStore::load(
            std::env::temp_dir().join(format!("git-content-tests-{}", Uuid::new_v4())),
        )
        .unwrap()
    }

    fn command() -> GitDiffContentCommand {
        GitDiffContentCommand {
            client_id: "client".into(),
            profile_id: "local".into(),
            expected_server_identity: "server".into(),
            connection_epoch: "7".into(),
            root: "/repo".into(),
            root_token: "token".into(),
            repository_id: "repo".into(),
            path: b"a.txt".to_vec(),
            original_path: Vec::new(),
            diff_target: "unstaged".into(),
            old: Some(GitDiffContentSideCommand {
                content_digest: "old-digest".into(),
                size: "5".into(),
            }),
            new: Some(GitDiffContentSideCommand {
                content_digest: "new-digest".into(),
                size: "4".into(),
            }),
        }
    }

    /// The exchange this lane exists for: both withheld sides, each reassembled
    /// from consecutive chunks, published in old-then-new order.
    #[test]
    fn both_sides_are_reassembled_in_order_from_consecutive_chunks() {
        let old_body = b"0123456789";
        let new_body = b"abcd";
        let (job, sent) = job(vec![
            deferred(v1::GitDiffContentSide::Old, SIDE_OLD, "old-digest", old_body.len() as u64),
            deferred(v1::GitDiffContentSide::New, SIDE_NEW, "new-digest", new_body.len() as u64),
        ]);
        let total = stream_sides(&job, |request| {
            assert_eq!(request.operation, i32::from(v1::Operation::GitDiffContent));
            let content = content_of(&request);
            // Each request names the digest and size the diff was classified
            // with, so the host can refuse a body that changed underneath it.
            if content.side == i32::from(v1::GitDiffContentSide::Old) {
                assert_eq!(content.expected_content_digest, "old-digest");
                assert_eq!(content.expected_size, old_body.len() as u64);
                Ok(host_chunk(old_body, &content, 4))
            } else {
                assert_eq!(content.expected_content_digest, "new-digest");
                assert_eq!(content.expected_size, new_body.len() as u64);
                Ok(host_chunk(new_body, &content, 4))
            }
        })
        .unwrap();
        assert_eq!(total, (old_body.len() + new_body.len()) as u64);
        let frames = sent.lock().unwrap();
        let decoded: Vec<_> = frames.iter().map(|frame| decode_chunk_frame(frame)).collect();
        // Every old-side frame precedes every new-side frame.
        let switch = decoded.iter().position(|(tag, _, _)| *tag == SIDE_NEW).unwrap();
        assert!(decoded[..switch].iter().all(|(tag, _, _)| *tag == SIDE_OLD));
        assert!(decoded[switch..].iter().all(|(tag, _, _)| *tag == SIDE_NEW));
        // Offsets are consecutive and the reassembled bytes are the bodies.
        for (side_frames, body) in [(&decoded[..switch], &old_body[..]), (&decoded[switch..], &new_body[..])] {
            let mut reassembled = Vec::new();
            for (_, offset, data) in side_frames {
                assert_eq!(*offset, reassembled.len() as u64);
                reassembled.extend_from_slice(data);
            }
            assert_eq!(reassembled, body);
        }
    }

    /// A repeated or skipped offset is refused instead of silently reassembled
    /// into a body the digest never described.
    #[test]
    fn a_chunk_at_the_wrong_offset_is_refused() {
        let body = b"0123456789";
        for wrong_offset in [0u64, 8] {
            let (job, sent) =
                job(vec![deferred(v1::GitDiffContentSide::Old, SIDE_OLD, "digest", body.len() as u64)]);
            let mut requests = 0;
            let error = stream_sides(&job, |request| {
                requests += 1;
                assert!(requests <= 3, "the stream kept requesting past a corrupt chunk");
                let content = content_of(&request);
                if requests == 1 {
                    return Ok(host_chunk(body, &content, 4));
                }
                // The second chunk repeats offset 0 (a duplicate) or jumps
                // ahead (a gap); the client expects offset 4 either way.
                let mut duplicate = content.clone();
                duplicate.offset = wrong_offset;
                Ok(host_chunk(body, &duplicate, 4))
            })
            .unwrap_err();
            assert!(error.contains("out-of-order"), "unexpected error: {error}");
            // Only the frame that agreed with the stream was published.
            assert_eq!(sent.lock().unwrap().len(), 1);
        }
    }

    /// A host describing a different body size than the control response did
    /// means the diff changed; the read refuses rather than substitutes.
    #[test]
    fn a_chunk_describing_a_different_total_size_is_refused() {
        let (job, _sent) = job(vec![deferred(v1::GitDiffContentSide::Old, SIDE_OLD, "digest", 10)]);
        let error = stream_sides(&job, |_request| {
            Ok(chunk_response(v1::GitDiffContentChunk {
                offset: 0,
                data: b"0123".to_vec(),
                last: false,
                total_size: 11,
            }))
        })
        .unwrap_err();
        assert!(error.contains("different size"), "unexpected error: {error}");
    }

    /// A response that answers the operation but omits the chunk is a protocol
    /// violation, not an empty body.
    #[test]
    fn a_response_without_a_chunk_is_refused() {
        for response in [
            v1::Response::default(),
            v1::Response {
                git: Some(v1::GitResponse::default()),
                ..Default::default()
            },
        ] {
            let (job, sent) = job(vec![deferred(v1::GitDiffContentSide::Old, SIDE_OLD, "digest", 10)]);
            let error = stream_sides(&job, |_request| Ok(response.clone())).unwrap_err();
            assert!(error.contains("omitted"), "unexpected error: {error}");
            assert!(sent.lock().unwrap().is_empty());
        }
    }

    /// An empty non-final chunk would loop forever at the same offset.
    #[test]
    fn an_empty_chunk_that_is_not_last_is_refused_as_a_stall() {
        let (job, _sent) = job(vec![deferred(v1::GitDiffContentSide::Old, SIDE_OLD, "digest", 10)]);
        let mut requests = 0;
        let error = stream_sides(&job, |_request| {
            requests += 1;
            assert!(requests <= 2, "the stream kept re-requesting a stalled offset");
            Ok(chunk_response(v1::GitDiffContentChunk {
                offset: 0,
                data: Vec::new(),
                last: false,
                total_size: 10,
            }))
        })
        .unwrap_err();
        assert!(error.contains("stalled"), "unexpected error: {error}");
    }

    /// `last` before the described byte count is a truncated body; bytes past
    /// it are an oversized one. Neither may settle as a completed read.
    #[test]
    fn a_body_shorter_or_longer_than_described_is_refused() {
        for data in [&b"0123"[..], &b"0123456789AB"[..]] {
            let (job, _sent) = job(vec![deferred(v1::GitDiffContentSide::Old, SIDE_OLD, "digest", 10)]);
            let error = stream_sides(&job, |_request| {
                Ok(chunk_response(v1::GitDiffContentChunk {
                    offset: 0,
                    data: data.to_vec(),
                    last: true,
                    total_size: 10,
                }))
            })
            .unwrap_err();
            assert!(
                error.contains("ended before the described body"),
                "unexpected error: {error}"
            );
        }
    }

    /// Cancellation between chunks stops the stream before the next request.
    #[test]
    fn cancellation_between_chunks_stops_the_read() {
        let body = b"0123456789";
        let (job, sent) = job(vec![deferred(v1::GitDiffContentSide::Old, SIDE_OLD, "digest", body.len() as u64)]);
        let cancellation = Arc::clone(&job.cancellation);
        let error = stream_sides(&job, |request| {
            let content = content_of(&request);
            assert_eq!(content.offset, 0, "a request was issued after cancellation");
            cancellation.cancel();
            Ok(host_chunk(body, &content, 4))
        })
        .unwrap_err();
        assert!(error.contains("cancelled"), "unexpected error: {error}");
        // The chunk already read was published; nothing after it was.
        assert_eq!(sent.lock().unwrap().len(), 1);
    }

    /// The error frame is the tag byte followed by the message itself.
    #[test]
    fn an_error_is_published_as_one_error_frame() {
        let (channel, sent) = captured_channel();
        emit_error(&channel, "boom");
        assert_eq!(
            sent.lock().unwrap().as_slice(),
            [[&[FRAME_ERROR][..], b"boom"].concat()]
        );
    }

    /// A valid command resolves to a job whose sides carry the frame tags and
    /// declared identities the renderer will demultiplex by.
    #[test]
    fn prepare_resolves_both_sides_in_old_then_new_order() {
        let (channel, _sent) = captured_channel();
        let job = prepare(command(), channel, &profiles(), &ready_client()).unwrap();
        assert!(!job.read_id.is_empty());
        assert_eq!(job.request.diff_target, i32::from(v1::GitDiffTarget::Unstaged));
        let described: Vec<_> = job
            .sides
            .iter()
            .map(|side| (side.frame_tag, side.digest.as_str(), side.size))
            .collect();
        assert_eq!(
            described,
            [(SIDE_OLD, "old-digest", 5), (SIDE_NEW, "new-digest", 4)]
        );
    }

    /// Every way a command can fail to describe a performable read.
    #[test]
    fn a_command_missing_its_identity_or_scope_is_refused() {
        type Break = (&'static str, Box<dyn Fn(&mut GitDiffContentCommand)>);
        let cases: Vec<Break> = vec![
            ("an empty path", Box::new(|command| command.path = Vec::new())),
            ("an empty root token", Box::new(|command| command.root_token = String::new())),
            (
                "no deferred side at all",
                Box::new(|command| {
                    command.old = None;
                    command.new = None;
                }),
            ),
            (
                "a non-numeric epoch",
                Box::new(|command| command.connection_epoch = "seven".into()),
            ),
            (
                "a zero-sized side",
                Box::new(|command| command.old.as_mut().unwrap().size = "0".into()),
            ),
            (
                "a side without a digest",
                Box::new(|command| command.new.as_mut().unwrap().content_digest = String::new()),
            ),
            (
                "an unknown diff target",
                Box::new(|command| command.diff_target = "banana".into()),
            ),
            (
                "a profile that does not exist",
                Box::new(|command| command.profile_id = "missing".into()),
            ),
            (
                "a server identity the connection no longer has",
                Box::new(|command| command.expected_server_identity = "other".into()),
            ),
        ];
        for (name, sabotage) in cases {
            let mut broken = command();
            sabotage(&mut broken);
            let (channel, _sent) = captured_channel();
            assert!(
                prepare(broken, channel, &profiles(), &ready_client()).is_err(),
                "{name} was accepted anyway"
            );
        }
    }

    /// The per-connection registry bounds outstanding reads, and a finished
    /// read's registration frees its slot on drop.
    #[test]
    fn the_registry_bounds_reads_and_a_dropped_registration_frees_its_slot() {
        let reads = Arc::new(GitContentReads::default());
        let mut held = Vec::new();
        for index in 0..MAX_GIT_CONTENT_READS {
            held.push(
                reads
                    .register(&format!("read-{index}"), &Arc::new(CancelState::new()))
                    .unwrap(),
            );
        }
        assert!(
            reads
                .register("one-too-many", &Arc::new(CancelState::new()))
                .is_err()
        );
        held.pop();
        reads
            .register("replacement", &Arc::new(CancelState::new()))
            .unwrap();
    }

    /// Cancel reaches exactly the named read; connection replacement sweeps
    /// every read; an unknown id is a no-op.
    #[test]
    fn cancel_reaches_only_the_named_read_and_cancel_all_sweeps_the_rest() {
        let reads = Arc::new(GitContentReads::default());
        let first = Arc::new(CancelState::new());
        let second = Arc::new(CancelState::new());
        let _first_registration = reads.register("first", &first).unwrap();
        let _second_registration = reads.register("second", &second).unwrap();
        reads.cancel("missing");
        assert!(!first.is_cancelled() && !second.is_cancelled());
        reads.cancel("first");
        assert!(first.is_cancelled());
        assert!(!second.is_cancelled());
        reads.cancel_all();
        assert!(second.is_cancelled());
        // The sweep drained the registry, so an old name is registrable again.
        reads
            .register("second", &Arc::new(CancelState::new()))
            .unwrap();
    }

    #[test]
    fn targets_use_the_frontend_spelling() {
        assert_eq!(diff_target("staged").unwrap(), v1::GitDiffTarget::Staged);
        assert_eq!(
            diff_target("unstaged").unwrap(),
            v1::GitDiffTarget::Unstaged
        );
        assert!(diff_target("").is_err());
    }

    #[test]
    fn a_side_without_a_digest_or_a_positive_size_is_refused() {
        assert!(non_empty("", "a content digest").is_err());
        assert!(positive_u64("0").is_err());
        assert!(positive_u64("nine").is_err());
        assert_eq!(positive_u64("18446744073709551615").unwrap(), u64::MAX);
    }
}
