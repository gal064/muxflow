//! Reading a large Git diff body over the independent bulk connection.
//!
//! The control connection carries keystrokes, terminal frames and every other
//! Git request. A multi-megabyte diff body in front of those is a visible
//! typing stall, so the control response describes the body and this reads it
//! on the bulk lane instead. Nothing is staged on the host between the two:
//! the size and digest the control response stated are what the bulk read is
//! checked against.

use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex, OnceLock},
};

use tauri::{
    State,
    ipc::{Channel, InvokeResponseBody},
};
use tmux_agent_protocol::v1;
use uuid::Uuid;

use super::files::bulk_pool::BulkLease;
use super::files::scheduler::{BulkBinding, CancelState};
use super::{ConnectionSpec, ProfileStore, TerminalClients, get_client};

/// Bytes requested per bulk round trip. Matches the file transfer chunk size,
/// which the bulk framing and host flow control are already sized for.
const GIT_CONTENT_CHUNK: u32 = 1024 * 1024;

/// Diff bodies read at once. The same two-at-a-time bound the transfer engine
/// uses, so a workspace full of diff tabs cannot open a bulk connection each.
const MAX_CONCURRENT_READS: usize = 2;

/// Frame kinds on the response channel.
const FRAME_CHUNK: u8 = 1;
const FRAME_COMPLETE: u8 = 2;
const FRAME_ERROR: u8 = 3;

struct Admission {
    state: Mutex<usize>,
    released: Condvar,
}

impl Admission {
    fn acquire(&self) -> AdmissionGuard<'_> {
        let mut active = self.state.lock().unwrap();
        while *active >= MAX_CONCURRENT_READS {
            active = self.released.wait(active).unwrap();
        }
        *active += 1;
        AdmissionGuard { admission: self }
    }
}

struct AdmissionGuard<'a> {
    admission: &'a Admission,
}

impl Drop for AdmissionGuard<'_> {
    fn drop(&mut self) {
        *self.admission.state.lock().unwrap() -= 1;
        self.admission.released.notify_one();
    }
}

fn admission() -> &'static Admission {
    static VALUE: OnceLock<Admission> = OnceLock::new();
    VALUE.get_or_init(|| Admission {
        state: Mutex::new(0),
        released: Condvar::new(),
    })
}

fn in_flight() -> &'static Mutex<HashMap<String, Arc<CancelState>>> {
    static VALUE: OnceLock<Mutex<HashMap<String, Arc<CancelState>>>> = OnceLock::new();
    VALUE.get_or_init(Default::default)
}

/// Everything one deferred body read is bound to.
struct GitContentJob {
    read_id: String,
    connection: ConnectionSpec,
    binding: BulkBinding,
    cancellation: Arc<CancelState>,
    request: v1::GitRequest,
    side: v1::GitDiffContentSide,
    digest: String,
    size: u64,
    channel: Channel<InvokeResponseBody>,
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn read_git_diff_content(
    client_id: String,
    profile_id: String,
    expected_server_identity: String,
    connection_epoch: String,
    root: String,
    root_token: String,
    repository_id: String,
    path: Vec<u8>,
    original_path: Vec<u8>,
    diff_target: String,
    side: String,
    content_digest: String,
    size: String,
    on_event: Channel<InvokeResponseBody>,
    profiles: State<'_, ProfileStore>,
    clients: State<'_, TerminalClients>,
) -> Result<String, String> {
    if root.is_empty() || root_token.is_empty() || repository_id.is_empty() || path.is_empty() {
        return Err("Git diff content requires its repository and path identity".into());
    }
    if content_digest.is_empty() {
        return Err("Git diff content requires the digest it was classified with".into());
    }
    let size: u64 = size
        .parse()
        .map_err(|_| "size must be a decimal u64 string".to_owned())?;
    if size == 0 {
        return Err("Git diff content size must be positive".into());
    }
    let connection_epoch: u64 = connection_epoch
        .parse()
        .map_err(|_| "connectionEpoch must be a decimal u64 string".to_owned())?;
    let binding = BulkBinding::capture(
        get_client(&clients, &client_id)?,
        expected_server_identity.clone(),
        connection_epoch,
    )?;
    let job = GitContentJob {
        read_id: Uuid::new_v4().to_string(),
        connection: profiles.connection_for(&profile_id)?,
        binding,
        cancellation: Arc::new(CancelState::new()),
        request: v1::GitRequest {
            operation_id: String::new(),
            root,
            root_token,
            expected_server_identity,
            repository_id,
            connection_epoch,
            path,
            original_path,
            diff_target: diff_target_from_name(&diff_target)?.into(),
            ..Default::default()
        },
        side: side_from_name(&side)?,
        digest: content_digest,
        size,
        channel: on_event,
    };
    let read_id = job.read_id.clone();
    in_flight()
        .lock()
        .unwrap()
        .insert(read_id.clone(), Arc::clone(&job.cancellation));
    std::thread::spawn(move || {
        let _admitted = admission().acquire();
        let outcome = stream_git_diff_content(&job);
        in_flight().lock().unwrap().remove(&job.read_id);
        match outcome {
            Ok(total) => {
                let mut frame = vec![FRAME_COMPLETE];
                frame.extend_from_slice(&total.to_be_bytes());
                let _ = job.channel.send(InvokeResponseBody::Raw(frame));
            }
            Err(error) => {
                let mut frame = vec![FRAME_ERROR];
                frame.extend_from_slice(error.as_bytes());
                let _ = job.channel.send(InvokeResponseBody::Raw(frame));
            }
        }
    });
    Ok(read_id)
}

#[tauri::command]
pub fn cancel_git_diff_content(read_id: String) -> Result<(), String> {
    if let Some(cancellation) = in_flight().lock().unwrap().get(&read_id) {
        cancellation.cancel();
    }
    Ok(())
}

fn stream_git_diff_content(job: &GitContentJob) -> Result<u64, String> {
    job.binding.validate()?;
    let deadline = job.cancellation.arm_inactivity_deadline();
    let mut lease =
        BulkLease::acquire(&job.connection, &job.binding, &job.cancellation, &deadline)?;
    let _process_binding = job.cancellation.bind_process(lease.process_id())?;
    let mut protocol = lease.client();
    let mut delivered: u64 = 0;
    loop {
        if job.cancellation.is_cancelled() {
            return Err("Git diff content read was cancelled".into());
        }
        let mut request = job.request.clone();
        request.operation_id = Uuid::new_v4().to_string();
        request.content = Some(v1::GitDiffContentRequest {
            side: job.side.into(),
            expected_content_digest: job.digest.clone(),
            expected_size: job.size,
            offset: delivered,
            length: GIT_CONTENT_CHUNK,
        });
        let response = protocol.request_cancellable(
            v1::Request {
                operation: v1::Operation::GitDiffContent.into(),
                git: Some(request),
                ..Default::default()
            },
            &job.cancellation,
            &deadline,
        )?;
        deadline.touch();
        let chunk = response
            .git
            .and_then(|git| git.content_chunk)
            .ok_or("host omitted the Git diff content chunk")?;
        if chunk.offset != delivered {
            return Err("host returned an out-of-order Git diff content chunk".into());
        }
        if chunk.total_size != job.size {
            return Err("host returned a Git diff body of a different size".into());
        }
        let mut frame = Vec::with_capacity(9 + chunk.data.len());
        frame.push(FRAME_CHUNK);
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
    if delivered != job.size {
        return Err("Git diff content ended before the described body".into());
    }
    Ok(delivered)
}

fn side_from_name(value: &str) -> Result<v1::GitDiffContentSide, String> {
    match value {
        "old" => Ok(v1::GitDiffContentSide::Old),
        "new" => Ok(v1::GitDiffContentSide::New),
        _ => Err(format!("unsupported Git diff content side {value}")),
    }
}

fn diff_target_from_name(value: &str) -> Result<v1::GitDiffTarget, String> {
    match value {
        "unstaged" => Ok(v1::GitDiffTarget::Unstaged),
        "staged" => Ok(v1::GitDiffTarget::Staged),
        _ => Err(format!("unsupported Git diff target {value}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_reads_are_bounded_and_release_in_order() {
        let admission = Admission {
            state: Mutex::new(0),
            released: Condvar::new(),
        };
        let first = admission.acquire();
        let second = admission.acquire();
        assert_eq!(*admission.state.lock().unwrap(), MAX_CONCURRENT_READS);
        drop(second);
        assert_eq!(*admission.state.lock().unwrap(), MAX_CONCURRENT_READS - 1);
        drop(first);
        assert_eq!(*admission.state.lock().unwrap(), 0);
    }

    #[test]
    fn sides_and_targets_use_the_frontend_spelling() {
        assert_eq!(side_from_name("old").unwrap(), v1::GitDiffContentSide::Old);
        assert_eq!(side_from_name("new").unwrap(), v1::GitDiffContentSide::New);
        assert!(side_from_name("both").is_err());
        assert_eq!(
            diff_target_from_name("staged").unwrap(),
            v1::GitDiffTarget::Staged
        );
        assert!(diff_target_from_name("").is_err());
    }
}
