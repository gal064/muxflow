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

use std::sync::{Arc, OnceLock};

use serde::Deserialize;
use tauri::{
    State,
    ipc::{Channel, InvokeResponseBody},
};
use tmux_agent_protocol::v1;
use uuid::Uuid;

use super::files::bulk_pool::BulkLease;
use super::files::scheduler::{BulkBinding, CancelState};
use super::{ConnectionSpec, ProfileStore, TerminalClient, TerminalClients, get_client};

/// Bytes requested per round trip. Matches the file transfer chunk size, which
/// the bulk framing and host flow control are already sized for.
const GIT_CONTENT_CHUNK: u32 = 1024 * 1024;

/// Diff-body reads running at once, across all connections.
///
/// One is enough — a diff surface has one read in flight and cancels it before
/// starting another — and it bounds this lane to a single bulk bridge beyond
/// the two the transfer engine may hold.
const MAX_CONCURRENT_READS: usize = 1;

/// Frame kinds on the response channel. The renderer's matching decoder is in
/// `apps/desktop/src/features/git/api.ts`.
const FRAME_CHUNK: u8 = 1;
const FRAME_COMPLETE: u8 = 2;
const FRAME_ERROR: u8 = 3;

pub(super) const SIDE_OLD: u8 = 1;
pub(super) const SIDE_NEW: u8 = 2;

fn admission() -> &'static tokio::sync::Semaphore {
    static VALUE: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    VALUE.get_or_init(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_READS))
}

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
    client: Arc<TerminalClient>,
    /// Absent when this connection cannot open a bulk bridge, in which case the
    /// bodies come back over the control lane instead.
    binding: Option<BulkBinding>,
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
    let job = prepare(command, on_event, &profiles, &clients)?;
    let read_id = job.read_id.clone();
    let registration = job
        .client
        .register_git_content_read(&read_id, &job.cancellation)?;
    tauri::async_runtime::spawn(async move {
        let _registration = registration;
        let permit = match admission().acquire().await {
            Ok(permit) => permit,
            Err(_) => {
                emit_error(&job.channel, "Git diff content admission closed");
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
            // The worker panicked, so its channel went with it. There is
            // nothing left to report to, and the registration is released by
            // this task's own guard.
            Err(error) => eprintln!("Git diff content read failed: {error}"),
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
    get_client(&clients, &client_id)?.cancel_git_content_read(&read_id);
    Ok(())
}

fn prepare(
    command: GitDiffContentCommand,
    channel: Channel<InvokeResponseBody>,
    profiles: &ProfileStore,
    clients: &State<'_, TerminalClients>,
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
    let client = get_client(clients, &command.client_id)?;
    // A read-only host refuses a bulk connection outright, so its bodies come
    // back over the control lane instead. They are bounded chunks, and this is
    // the only way a large diff stays viewable there at all.
    let binding = BulkBinding::capture(
        Arc::clone(&client),
        command.expected_server_identity.clone(),
        connection_epoch,
    )
    .ok();
    Ok(GitContentJob {
        read_id: Uuid::new_v4().to_string(),
        connection: profiles.connection_for(&command.profile_id)?,
        client,
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
    match &job.binding {
        Some(binding) => {
            binding.validate()?;
            let deadline = job.cancellation.arm_inactivity_deadline();
            let mut lease =
                BulkLease::acquire(&job.connection, binding, &job.cancellation, &deadline)?;
            let _process_binding = job.cancellation.bind_process(lease.process_id())?;
            let mut protocol = lease.client();
            stream_sides(job, |request| {
                let response = protocol.request_cancellable(request, &job.cancellation, &deadline);
                deadline.touch();
                response
            })
        }
        None => stream_sides(job, |request| {
            job.client
                .request_git_operation(request, &Uuid::new_v4().to_string())
        }),
    }
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

/// Removes a read from its client's registry exactly once.
pub(crate) struct GitContentRegistration {
    client: Arc<TerminalClient>,
    read_id: String,
}

impl GitContentRegistration {
    pub(crate) fn new(client: Arc<TerminalClient>, read_id: String) -> Self {
        Self { client, read_id }
    }
}

impl Drop for GitContentRegistration {
    fn drop(&mut self) {
        self.client.release_git_content_read(&self.read_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
