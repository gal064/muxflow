use std::{
    collections::HashMap,
    process::{Child, ChildStdin},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tauri::{
    State,
    ipc::{Channel, InvokeResponseBody},
};
#[cfg(test)]
use tmux_agent_protocol::{HOST_CAPABILITIES, PROTOCOL_MAJOR};
use tmux_agent_protocol::{
    envelope,
    v1::{self, envelope::Payload},
    write_frame_sync,
};
use uuid::Uuid;

use tmux_control::{DESKTOP_INPUT_COALESCE_BYTES, MAX_INPUT_REQUEST_BYTES};

mod event_frame;
use event_frame::{TerminalEvent, encode_event};
pub(crate) mod agent;
pub(crate) mod files;
pub(crate) mod git;
pub(crate) mod tmux_action;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const GIT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum ConnectionSpec {
    Local,
    Ssh {
        #[serde(default, rename = "profileId")]
        profile_id: String,
        target: String,
        #[serde(default, rename = "configPath")]
        config_path: Option<String>,
    },
}

impl ConnectionSpec {
    pub fn validate(&self) -> Result<(), String> {
        if let Self::Ssh {
            target,
            config_path,
            profile_id,
        } = self
        {
            validate_profile_id(profile_id)?;
            validate_ssh_target(target)?;
            if config_path
                .as_ref()
                .is_some_and(|path| path.is_empty() || path.bytes().any(|byte| byte == 0))
            {
                return Err("invalid SSH config path".into());
            }
        }
        Ok(())
    }
}

pub(crate) mod profiles;
pub use profiles::ProfileStore;
pub(crate) mod helper;
mod transport;
pub(crate) use transport::close_all_control_masters;
use transport::{
    ControlLane, SshLease, acquire_control_master, ensure_control_master, host_helper_path,
    ssh_profile_control_socket,
};

struct TerminalClient {
    _ssh_lease: Option<SshLease>,
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
    stopped: AtomicBool,
    ready: AtomicBool,
    read_only: AtomicBool,
    next_request_id: AtomicU64,
    pending: Mutex<HashMap<u64, mpsc::Sender<Result<v1::Response, String>>>>,
    git_operations: Mutex<HashMap<String, u64>>,
    input_tx: Mutex<Option<mpsc::SyncSender<ClientInputDispatch>>>,
    input_epoch: AtomicU64,
    terminal_epoch: AtomicU64,
    server_identity: Mutex<String>,
    host_profile_id: Mutex<String>,
}

enum ClientInputDispatch {
    Bytes {
        pane_id: String,
        data: Vec<u8>,
        epoch: u64,
    },
    Barrier(mpsc::SyncSender<Result<(), String>>),
    Stop,
}

struct InitialHostState {
    snapshot: tmux_control::TmuxSnapshot,
    agent_snapshot: Option<v1::AgentSnapshot>,
    accepted_sequence: u64,
    generation: u64,
    buffered_events: Vec<v1::Envelope>,
}

impl TerminalClient {
    fn new(ssh_lease: Option<SshLease>) -> Self {
        Self {
            _ssh_lease: ssh_lease,
            stdin: Mutex::new(None),
            child: Mutex::new(None),
            stopped: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            read_only: AtomicBool::new(false),
            next_request_id: AtomicU64::new(100),
            pending: Mutex::new(HashMap::new()),
            git_operations: Mutex::new(HashMap::new()),
            input_tx: Mutex::new(None),
            input_epoch: AtomicU64::new(0),
            terminal_epoch: AtomicU64::new(0),
            server_identity: Mutex::new(String::new()),
            host_profile_id: Mutex::new(String::new()),
        }
    }

    fn start_input_dispatch(self: &Arc<Self>, client_id: &str) -> Result<(), String> {
        let (sender, receiver) = mpsc::sync_channel(512);
        *self.input_tx.lock().unwrap() = Some(sender);
        let client = Arc::clone(self);
        thread::Builder::new()
            .name(format!("host-input-dispatch-{client_id}"))
            .spawn(move || run_client_input_dispatch(client, receiver))
            .map(|_| ())
            .map_err(|error| format!("failed to start input dispatcher: {error}"))
    }

    /// Queues a keystroke and returns.
    ///
    /// Waiting for the host's ack put a full round trip — an entire RTT over
    /// SSH — on the main thread of every keypress, and the ack carried no
    /// information the caller could act on. Delivery failures now surface where
    /// they belong: backpressure is still refused synchronously here, because
    /// the queue is local and its answer is immediate, while a host-side
    /// rejection arrives as a pane-scoped recovery event on the event stream
    /// and a transport failure tears down the bridge visibly.
    fn enqueue_input(&self, pane_id: String, data: Vec<u8>) -> Result<(), String> {
        if self.stopped.load(Ordering::Acquire)
            || !self.ready.load(Ordering::Acquire)
            || self.read_only.load(Ordering::Acquire)
        {
            return Err(
                "terminal bridge is disconnected, reconciling, or read-only; input was not queued"
                    .into(),
            );
        }
        if data.is_empty() {
            return Ok(());
        }
        self.input_tx
            .lock()
            .unwrap()
            .as_ref()
            .ok_or_else(|| "terminal input dispatcher is unavailable".to_owned())?
            .try_send(ClientInputDispatch::Bytes {
                pane_id,
                data,
                epoch: self.input_epoch.load(Ordering::Acquire),
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    "terminal input queue is full; retry without dropping bytes".to_owned()
                }
                mpsc::TrySendError::Disconnected(_) => {
                    "terminal input dispatcher is disconnected".to_owned()
                }
            })
    }

    fn flush_input(&self) -> Result<(), String> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.input_tx
            .lock()
            .unwrap()
            .as_ref()
            .ok_or_else(|| "terminal input dispatcher is unavailable".to_owned())?
            .send(ClientInputDispatch::Barrier(sender))
            .map_err(|_| "terminal input dispatcher is disconnected".to_owned())?;
        receiver
            .recv_timeout(REQUEST_TIMEOUT)
            .map_err(|_| "terminal input flush timed out".to_owned())?
    }

    fn request(&self, request: v1::Request) -> Result<v1::Response, String> {
        self.request_with_timeout(request, REQUEST_TIMEOUT, None)
    }

    /// Writes a request without registering a waiter for its response.
    ///
    /// Used by the keystroke path, whose acks carry nothing actionable. The
    /// reader drops responses with no waiter, so the host stays free to answer
    /// without either side having to change shape.
    fn dispatch_request(&self, request: v1::Request) -> Result<(), String> {
        if !self.ready.load(Ordering::Acquire) || self.read_only.load(Ordering::Acquire) {
            return Err(
                "host is disconnected, reconciling, or read-only; input was not sent".into(),
            );
        }
        let request_id = self.next_request_id.fetch_add(1, Ordering::AcqRel);
        self.stdin
            .lock()
            .unwrap()
            .as_mut()
            .ok_or_else(|| "host bridge is disconnected".to_owned())
            .and_then(|stdin| {
                write_frame_sync(stdin, &envelope(request_id, 0, Payload::Request(request)))
                    .map_err(|error| error.to_string())
            })
    }

    fn request_git(
        &self,
        request: v1::Request,
        operation_id: &str,
    ) -> Result<v1::Response, String> {
        self.request_with_timeout(request, GIT_REQUEST_TIMEOUT, Some(operation_id.to_owned()))
    }

    fn request_with_timeout(
        &self,
        request: v1::Request,
        timeout: Duration,
        git_operation_id: Option<String>,
    ) -> Result<v1::Response, String> {
        if !self.ready.load(Ordering::Acquire) || self.read_only.load(Ordering::Acquire) {
            // Coded like the host's own refusals, so the frontend can lead with
            // a sentence and keep this behind the disclosure (11.4.4). The
            // uncoded form reached the user verbatim as a full-width red banner
            // that enumerated three internal states and named "mutation".
            return Err(
                "mutation_rejected: host connection is not writable (disconnected, reconciling, or read-only)"
                    .into(),
            );
        }
        let request_id = self.next_request_id.fetch_add(1, Ordering::AcqRel);
        let (sender, receiver) = mpsc::channel();
        self.pending.lock().unwrap().insert(request_id, sender);
        if let Some(operation_id) = &git_operation_id {
            let mut operations = self.git_operations.lock().unwrap();
            if operations.contains_key(operation_id) {
                self.pending.lock().unwrap().remove(&request_id);
                return Err("duplicate Git operation ID".into());
            }
            operations.insert(operation_id.clone(), request_id);
        }
        let write_result = self
            .stdin
            .lock()
            .unwrap()
            .as_mut()
            .ok_or_else(|| "host bridge is disconnected".to_owned())
            .and_then(|stdin| {
                write_frame_sync(stdin, &envelope(request_id, 0, Payload::Request(request)))
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = write_result {
            self.pending.lock().unwrap().remove(&request_id);
            if let Some(operation_id) = &git_operation_id {
                self.git_operations.lock().unwrap().remove(operation_id);
            }
            return Err(error);
        }
        let result = match receiver.recv_timeout(timeout) {
            Ok(Ok(response)) if response.ok => Ok(response),
            Ok(Ok(response)) => Err(format!(
                "{}: {}",
                response.error_code, response.display_message
            )),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                if let Some(stdin) = self.stdin.lock().unwrap().as_mut() {
                    let _ = write_frame_sync(
                        stdin,
                        &envelope(
                            self.next_request_id.fetch_add(1, Ordering::AcqRel),
                            0,
                            Payload::Cancel(v1::Cancel {
                                target_request_id: request_id,
                            }),
                        ),
                    );
                }
                self.pending.lock().unwrap().remove(&request_id);
                Err(
                    "host request timed out; commit outcome is unknown and the request will not be replayed"
                        .into(),
                )
            }
        };
        if let Some(operation_id) = &git_operation_id {
            self.git_operations.lock().unwrap().remove(operation_id);
        }
        result
    }

    fn cancel_git(&self, operation_id: &str) -> Result<(), String> {
        let request_id = self
            .git_operations
            .lock()
            .unwrap()
            .get(operation_id)
            .copied()
            .ok_or_else(|| "unknown or completed Git operation ID".to_owned())?;
        let mut stdin = self.stdin.lock().unwrap();
        let stdin = stdin.as_mut().ok_or("host bridge is disconnected")?;
        write_frame_sync(
            stdin,
            &envelope(
                self.next_request_id.fetch_add(1, Ordering::AcqRel),
                0,
                Payload::Cancel(v1::Cancel {
                    target_request_id: request_id,
                }),
            ),
        )
        .map_err(|error| error.to_string())
    }

    fn fail_pending(&self, message: &str) {
        self.git_operations.lock().unwrap().clear();
        for (_, sender) in self.pending.lock().unwrap().drain() {
            let _ = sender.send(Err(message.into()));
        }
    }
}

fn run_client_input_dispatch(
    client: Arc<TerminalClient>,
    receiver: mpsc::Receiver<ClientInputDispatch>,
) {
    let mut deferred = None;
    loop {
        let message = match deferred.take() {
            Some(message) => message,
            None => match receiver.recv() {
                Ok(message) => message,
                Err(_) => break,
            },
        };
        match message {
            ClientInputDispatch::Bytes {
                pane_id,
                mut data,
                epoch,
            } => {
                while data.len() < DESKTOP_INPUT_COALESCE_BYTES {
                    match receiver.try_recv() {
                        Ok(ClientInputDispatch::Bytes {
                            pane_id: next_pane,
                            data: next_data,
                            epoch: next_epoch,
                        }) if next_pane == pane_id
                            && next_epoch == epoch
                            && data.len().saturating_add(next_data.len())
                                <= DESKTOP_INPUT_COALESCE_BYTES =>
                        {
                            data.extend_from_slice(&next_data);
                        }
                        Ok(message) => {
                            deferred = Some(message);
                            break;
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => break,
                    }
                }
                // Accepted by an older connection but not written before it
                // ended: drop it permanently rather than poison the new
                // connection's input state with bytes from the old one.
                if input_epoch_is_current(&client, epoch)
                    && client.ready.load(Ordering::Acquire)
                    && !client.read_only.load(Ordering::Acquire)
                {
                    let _ = client.dispatch_request(v1::Request {
                        operation: v1::Operation::TerminalInput.into(),
                        scope: pane_id,
                        data,
                        ..Default::default()
                    });
                }
            }
            ClientInputDispatch::Barrier(sender) => {
                let _ = sender.send(Ok(()));
            }
            ClientInputDispatch::Stop => break,
        }
    }
}

fn mark_input_reconnected(client: &TerminalClient) {
    client.input_epoch.fetch_add(1, Ordering::AcqRel);
}

fn input_epoch_is_current(client: &TerminalClient, epoch: u64) -> bool {
    epoch == client.input_epoch.load(Ordering::Acquire)
}

#[derive(Default)]
pub struct TerminalClients(Mutex<HashMap<String, Arc<TerminalClient>>>);

#[tauri::command]
pub fn start_terminal(
    session_id: String,
    pane_ids: Vec<String>,
    connection: ConnectionSpec,
    on_event: Channel<InvokeResponseBody>,
    clients: State<'_, TerminalClients>,
) -> Result<String, String> {
    connection.validate()?;
    if !session_id.is_empty() {
        validate_tmux_id(&session_id, '$')?;
    }
    for pane_id in &pane_ids {
        validate_tmux_id(pane_id, '%')?;
    }
    let client_id = Uuid::new_v4().to_string();
    let client = Arc::new(TerminalClient::new(acquire_control_master(&connection)?));
    *client.host_profile_id.lock().unwrap() = match &connection {
        ConnectionSpec::Local => "local".into(),
        ConnectionSpec::Ssh { profile_id, .. } => profile_id.clone(),
    };
    client.start_input_dispatch(&client_id)?;
    let worker_id = client_id.clone();
    let worker_client = Arc::clone(&client);
    thread::Builder::new()
        .name(format!("host-bridge-{client_id}"))
        .spawn(move || {
            supervise_bridge(
                worker_id,
                connection,
                session_id,
                pane_ids,
                on_event,
                worker_client,
            )
        })
        .map_err(|error| format!("failed to start host bridge supervisor: {error}"))?;
    clients.0.lock().unwrap().insert(client_id.clone(), client);
    Ok(client_id)
}

#[tauri::command]
pub fn stop_terminal(client_id: String, clients: State<'_, TerminalClients>) -> Result<(), String> {
    if let Some(client) = clients.0.lock().unwrap().remove(&client_id) {
        client.stopped.store(true, Ordering::Release);
        client.ready.store(false, Ordering::Release);
        client.stdin.lock().unwrap().take();
        if let Some(sender) = client.input_tx.lock().unwrap().take() {
            let _ = sender.try_send(ClientInputDispatch::Stop);
        }
        client.fail_pending("host connection stopped");
        if let Some(mut child) = client.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    Ok(())
}

#[tauri::command]
pub fn send_terminal_input(
    client_id: String,
    pane_id: String,
    data: String,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    validate_tmux_id(&pane_id, '%')?;
    let client = get_client(&clients, &client_id)?;
    client.enqueue_input(pane_id, data.into_bytes())
}

/// Binary terminal input, carried as a raw IPC body.
///
/// A `Vec<u8>` argument in a JSON command becomes a JSON array of numbers —
/// roughly four characters of text per byte, stringified and re-parsed on the
/// main thread. The payload is framed the same way the downlink event frames
/// are, so the bytes cross the boundary once, as bytes.
#[tauri::command]
pub fn send_terminal_input_bytes(
    request: tauri::ipc::Request<'_>,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    let tauri::ipc::InvokeBody::Raw(body) = request.body() else {
        return Err("terminal input IPC body must be raw binary".into());
    };
    let (client_id, pane_id, data) = decode_terminal_input_frame(body)?;
    validate_tmux_id(pane_id, '%')?;
    if data.len() > MAX_INPUT_REQUEST_BYTES {
        return Err("terminal input batch exceeds 1 MiB".into());
    }
    let client = get_client(&clients, client_id)?;
    client.enqueue_input(pane_id.to_owned(), data.to_vec())
}

/// `u16` client-id length, client id, `u16` pane-id length, pane id, payload.
fn decode_terminal_input_frame(body: &[u8]) -> Result<(&str, &str, &[u8]), String> {
    let mut offset = 0;
    let client_id = take_length_prefixed(body, &mut offset)?;
    let pane_id = take_length_prefixed(body, &mut offset)?;
    Ok((client_id, pane_id, &body[offset..]))
}

#[tauri::command]
pub fn resize_terminal_client(
    client_id: String,
    columns: u16,
    rows: u16,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    let client = get_client(&clients, &client_id)?;
    client.flush_input()?;
    client.request(v1::Request {
        operation: v1::Operation::ResizeTerminal.into(),
        columns: columns.into(),
        rows: rows.into(),
        ..Default::default()
    })?;
    Ok(())
}

/// Async: a tab switch reveals and hides panes, and doing that on the WebView's
/// main thread meant the new tab could not paint until the host had answered
/// for the old one — a whole RTT of frozen UI per switch over SSH.
///
/// The payload is a raw IPC body rather than a JSON argument object. A hide
/// carries the renderer's serialized screen, up to 4 MiB, and as a JSON array
/// of numbers that is roughly 15 MB of text to stringify and re-parse on the
/// main thread — measured at one to two seconds per switch.
#[tauri::command]
pub async fn set_terminal_visibility(
    request: tauri::ipc::Request<'_>,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    let tauri::ipc::InvokeBody::Raw(body) = request.body() else {
        return Err("terminal visibility IPC body must be raw binary".into());
    };
    let visibility = decode_terminal_visibility_frame(body)?;
    let client = get_client(&clients, visibility.client_id)?;
    let request = terminal_visibility_request(
        visibility.pane_id.to_owned(),
        visibility.visible,
        visibility.serialized_snapshot.to_vec(),
        visibility.terminal_epoch,
        visibility.output_generation,
        client.terminal_epoch.load(Ordering::Acquire),
    )?;
    tauri::async_runtime::spawn_blocking(move || client.request(request))
        .await
        .map_err(|error| format!("terminal visibility task failed: {error}"))??;
    Ok(())
}

struct TerminalVisibilityFrame<'a> {
    client_id: &'a str,
    pane_id: &'a str,
    visible: bool,
    terminal_epoch: u64,
    output_generation: u64,
    serialized_snapshot: &'a [u8],
}

/// `u16` client-id length, client id, `u16` pane-id length, pane id, one
/// visibility byte, two big-endian `u64`s, then the snapshot bytes.
fn decode_terminal_visibility_frame(body: &[u8]) -> Result<TerminalVisibilityFrame<'_>, String> {
    const SCALARS: usize = 1 + 8 + 8;
    let mut offset = 0;
    let client_id = take_length_prefixed(body, &mut offset)?;
    let pane_id = take_length_prefixed(body, &mut offset)?;
    let scalars_end = offset
        .checked_add(SCALARS)
        .filter(|end| *end <= body.len())
        .ok_or("terminal visibility frame is truncated")?;
    let visible = match body[offset] {
        0 => false,
        1 => true,
        _ => return Err("terminal visibility flag must be 0 or 1".into()),
    };
    let terminal_epoch = u64::from_be_bytes(
        body[offset + 1..offset + 9]
            .try_into()
            .map_err(|_| "terminal visibility epoch is truncated")?,
    );
    let output_generation = u64::from_be_bytes(
        body[offset + 9..scalars_end]
            .try_into()
            .map_err(|_| "terminal visibility cutoff is truncated")?,
    );
    Ok(TerminalVisibilityFrame {
        client_id,
        pane_id,
        visible,
        terminal_epoch,
        output_generation,
        serialized_snapshot: &body[scalars_end..],
    })
}

fn take_length_prefixed<'a>(body: &'a [u8], offset: &mut usize) -> Result<&'a str, String> {
    let header_end = offset
        .checked_add(2)
        .filter(|end| *end <= body.len())
        .ok_or("raw IPC frame is truncated")?;
    let length = usize::from(u16::from_be_bytes([body[*offset], body[*offset + 1]]));
    let end = header_end
        .checked_add(length)
        .filter(|end| *end <= body.len())
        .ok_or("raw IPC frame is truncated")?;
    *offset = end;
    std::str::from_utf8(&body[header_end..end])
        .map_err(|_| "raw IPC frame label is not valid UTF-8".to_owned())
}

fn terminal_visibility_request(
    pane_id: String,
    visible: bool,
    serialized_snapshot: Vec<u8>,
    terminal_epoch: u64,
    output_generation: u64,
    current_epoch: u64,
) -> Result<v1::Request, String> {
    validate_tmux_id(&pane_id, '%')?;
    if serialized_snapshot.len() > 4 * 1024 * 1024 {
        return Err("serialized terminal snapshot exceeds 4 MiB".into());
    }
    if terminal_epoch == 0 || terminal_epoch != current_epoch {
        return Err("terminal visibility checkpoint belongs to a stale connection epoch".into());
    }
    Ok(v1::Request {
        operation: v1::Operation::SetTerminalVisibility.into(),
        scope: pane_id,
        data: serialized_snapshot,
        visible,
        terminal_epoch,
        terminal_generation_cutoff: output_generation,
        ..Default::default()
    })
}

#[tauri::command]
pub async fn request_terminal_seed(
    client_id: String,
    pane_id: String,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    let request = terminal_seed_request(pane_id)?;
    let client = get_client(&clients, &client_id)?;
    tauri::async_runtime::spawn_blocking(move || client.request(request))
        .await
        .map_err(|error| format!("terminal seed task failed: {error}"))??;
    Ok(())
}

fn terminal_seed_request(pane_id: String) -> Result<v1::Request, String> {
    validate_tmux_id(&pane_id, '%')?;
    Ok(v1::Request {
        operation: v1::Operation::RequestTerminalSeed.into(),
        scope: pane_id,
        ..Default::default()
    })
}

fn get_client(
    clients: &State<'_, TerminalClients>,
    client_id: &str,
) -> Result<Arc<TerminalClient>, String> {
    clients
        .0
        .lock()
        .unwrap()
        .get(client_id)
        .cloned()
        .ok_or_else(|| "terminal client is no longer attached".into())
}

mod bridge;
use bridge::supervise_bridge;
#[cfg(test)]
use bridge::{
    handshake_allows_snapshot, reconnect_delay_millis, reconnect_jitter, scoped_terminal_recovery,
    terminal_scope, validate_event_sequence,
};

fn snapshot_from_proto(value: v1::Snapshot) -> tmux_control::TmuxSnapshot {
    tmux_control::TmuxSnapshot {
        sessions: value
            .sessions
            .into_iter()
            .map(|item| tmux_control::Session {
                id: item.id,
                name: item.name,
                window_count: item.window_count,
                attached_clients: item.attached_clients,
                order: item.order,
            })
            .collect(),
        windows: value
            .windows
            .into_iter()
            .map(|item| tmux_control::Window {
                id: item.id,
                session_id: item.session_id,
                index: item.index,
                name: item.name,
                active: item.active,
                layout: item.layout,
                zoomed: item.zoomed,
            })
            .collect(),
        panes: value
            .panes
            .into_iter()
            .map(|item| tmux_control::Pane {
                id: item.id,
                session_id: item.session_id,
                window_id: item.window_id,
                index: item.index,
                active: item.active,
                width: item.width.try_into().unwrap_or(u16::MAX),
                height: item.height.try_into().unwrap_or(u16::MAX),
                left: item.left.try_into().unwrap_or(u16::MAX),
                top: item.top.try_into().unwrap_or(u16::MAX),
                current_path: item.current_path,
                current_command: item.current_command,
                pane_pid: 0,
                start_command: String::new(),
            })
            .collect(),
    }
}

fn send_event(channel: &Channel<InvokeResponseBody>, event: TerminalEvent) {
    let _ = channel.send(InvokeResponseBody::Raw(encode_event(event)));
}

fn validate_tmux_id(value: &str, prefix: char) -> Result<(), String> {
    if value.strip_prefix(prefix).is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        Ok(())
    } else {
        Err(format!("invalid tmux identifier: {value}"))
    }
}

fn validate_ssh_target(target: &str) -> Result<(), String> {
    if target.is_empty()
        || target.starts_with('-')
        || target.chars().any(char::is_whitespace)
        || target.bytes().any(|byte| byte == 0)
    {
        Err("invalid SSH target".into())
    } else {
        Ok(())
    }
}

fn validate_profile_id(value: &str) -> Result<(), String> {
    if !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Ok(())
    } else {
        Err("profile/client identifier contains unsafe characters".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_or_disconnected_client_rejects_mutation_without_queueing() {
        let client = TerminalClient::new(None);
        let error = client
            .request(v1::Request {
                operation: v1::Operation::TerminalInput.into(),
                ..Default::default()
            })
            .unwrap_err();
        // Coded, so the frontend leads with a sentence and keeps the internal
        // state list behind a disclosure rather than printing it as a banner.
        assert!(error.starts_with("mutation_rejected: "), "{error}");
        assert!(error.contains("not writable"));
        assert!(client.pending.lock().unwrap().is_empty());

        client.ready.store(true, Ordering::Release);
        client.read_only.store(true, Ordering::Release);
        let error = client.request(v1::Request::default()).unwrap_err();
        assert!(error.contains("read-only"));
        assert!(client.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn sequence_gap_is_detected_before_event_application() {
        assert!(validate_event_sequence(2, 3).is_ok());
        let error = validate_event_sequence(2, 4).unwrap_err();
        assert_eq!(error, "sequence gap: expected 3, received 4");
    }

    #[test]
    fn reconnect_jitter_is_bounded_and_changes_by_attempt() {
        let first = reconnect_jitter("client-a", 1);
        let second = reconnect_jitter("client-a", 2);
        assert!(first <= 150);
        assert!(second <= 150);
        assert_ne!(first, second);
    }

    #[test]
    fn reconnect_backoff_stays_quick_for_a_blip_and_tops_out_at_a_minute() {
        // A blip must not be punished: the first retries are sub-second.
        assert!(reconnect_delay_millis("client-a", 1) < 600);
        assert!(reconnect_delay_millis("client-a", 2) < 1_000);
        // A machine that is away all afternoon must not retry ten times a
        // minute forever, and the ceiling must hold for every later attempt
        // rather than overflowing back to something short.
        for attempt in 9..64 {
            let delay = reconnect_delay_millis("client-a", attempt);
            assert!(
                (60_000..=60_150).contains(&delay),
                "attempt {attempt} slept {delay} ms"
            );
        }
    }

    #[test]
    fn disconnected_input_is_rejected_and_reconnect_starts_a_fresh_epoch() {
        let client = Arc::new(TerminalClient::new(None));
        let (sender, receiver) = mpsc::sync_channel(1);
        *client.input_tx.lock().unwrap() = Some(sender);

        assert!(
            client
                .enqueue_input("%1".into(), b"offline".to_vec())
                .is_err()
        );
        assert!(
            receiver.try_recv().is_err(),
            "offline input must not be queued"
        );

        mark_input_reconnected(&client);
        client.ready.store(true, Ordering::Release);
        assert_eq!(client.input_epoch.load(Ordering::Acquire), 1);
        // Queueing is the whole of the caller's obligation now: the keystroke
        // path never waits for the host, so this must return before anything
        // drains the queue.
        assert_eq!(
            client.enqueue_input("%1".into(), b"connected".to_vec()),
            Ok(())
        );
        let ClientInputDispatch::Bytes { epoch, data, .. } = receiver.recv().unwrap() else {
            panic!("expected terminal bytes");
        };
        assert_eq!(epoch, 1);
        assert_eq!(data, b"connected");

        mark_input_reconnected(&client);
        assert!(!input_epoch_is_current(&client, epoch));
    }

    #[test]
    fn raw_visibility_frame_carries_its_scalars_and_snapshot_without_a_json_number_array() {
        let mut frame = Vec::new();
        frame.extend_from_slice(&(6_u16).to_be_bytes());
        frame.extend_from_slice(b"client");
        frame.extend_from_slice(&(2_u16).to_be_bytes());
        frame.extend_from_slice(b"%3");
        frame.push(0);
        frame.extend_from_slice(&7_u64.to_be_bytes());
        frame.extend_from_slice(&42_u64.to_be_bytes());
        frame.extend_from_slice(b"screen");
        let decoded = decode_terminal_visibility_frame(&frame).unwrap();
        assert_eq!(decoded.client_id, "client");
        assert_eq!(decoded.pane_id, "%3");
        assert!(!decoded.visible);
        assert_eq!(decoded.terminal_epoch, 7);
        assert_eq!(decoded.output_generation, 42);
        assert_eq!(decoded.serialized_snapshot, b"screen");

        // A truncated or malformed frame is refused rather than read past.
        assert!(decode_terminal_visibility_frame(&frame[..frame.len() - 20]).is_err());
        let mut invalid_flag = frame.clone();
        invalid_flag[12] = 2;
        assert!(decode_terminal_visibility_frame(&invalid_flag).is_err());
    }

    #[test]
    fn raw_terminal_input_frame_round_trips_without_a_json_number_array() {
        let mut frame = Vec::new();
        frame.extend_from_slice(&(6_u16).to_be_bytes());
        frame.extend_from_slice(b"client");
        frame.extend_from_slice(&(2_u16).to_be_bytes());
        frame.extend_from_slice(b"%7");
        frame.extend_from_slice(&[0x00, 0x1b, 0xff]);
        assert_eq!(
            decode_terminal_input_frame(&frame).unwrap(),
            ("client", "%7", [0x00, 0x1b, 0xff].as_slice())
        );
        assert!(decode_terminal_input_frame(&frame[..5]).is_err());
        assert!(decode_terminal_input_frame(&[]).is_err());
    }

    #[test]
    fn queue_backpressure_is_still_refused_synchronously_without_dropping_bytes() {
        let client = Arc::new(TerminalClient::new(None));
        let (sender, receiver) = mpsc::sync_channel(1);
        *client.input_tx.lock().unwrap() = Some(sender);
        mark_input_reconnected(&client);
        client.ready.store(true, Ordering::Release);

        assert_eq!(client.enqueue_input("%1".into(), b"first".to_vec()), Ok(()));
        let error = client
            .enqueue_input("%1".into(), b"second".to_vec())
            .unwrap_err();
        assert!(error.contains("retry without dropping bytes"));
        let ClientInputDispatch::Bytes { data, .. } = receiver.recv().unwrap() else {
            panic!("expected terminal bytes");
        };
        assert_eq!(
            data, b"first",
            "the refused request must not displace the queued one"
        );
    }

    #[test]
    fn terminal_binary_frames_prefix_big_endian_generation() {
        for (event, expected_kind) in [
            (
                TerminalEvent::Seed {
                    pane_id: "%12".into(),
                    generation: 0x0102_0304_0506_0708,
                    data: vec![0, 0xff, b'x'],
                },
                1,
            ),
            (
                TerminalEvent::Output {
                    pane_id: "%12".into(),
                    generation: 0x0102_0304_0506_0708,
                    data: vec![0, 0xff, b'x'],
                },
                2,
            ),
        ] {
            let frame = event_frame::encode_event_with_sequence(event, 42);
            assert_eq!(frame[0], expected_kind);
            assert_eq!(&frame[1..3], &3_u16.to_be_bytes());
            assert_eq!(&frame[3..6], b"%12");
            assert_eq!(&frame[6..14], &42_u64.to_be_bytes());
            assert_eq!(&frame[14..22], &0x0102_0304_0506_0708_u64.to_be_bytes());
            assert_eq!(&frame[22..], &[0, 0xff, b'x']);
        }
    }

    #[test]
    fn terminal_epoch_frame_resets_same_server_generation_watermarks() {
        let frame = encode_event(TerminalEvent::GenerationEpoch {
            epoch: 0x0102_0304_0506_0708,
        });
        assert_eq!(frame[0], 10);
        assert_eq!(&frame[1..3], &8_u16.to_be_bytes());
        assert_eq!(&frame[3..11], b"terminal");
        assert_eq!(&frame[11..19], &0_u64.to_be_bytes());
        assert_eq!(&frame[19..], &0x0102_0304_0506_0708_u64.to_be_bytes());
    }

    #[test]
    fn pane_resource_frame_is_compact_and_sequence_atomic() {
        let frame = event_frame::encode_event_with_sequence(
            TerminalEvent::PaneResource {
                pane_id: "%1".into(),
                state: "hiddenBuffered".into(),
                requires_seed: true,
                recovery_reason: "overflow".into(),
                generation: 9,
                snapshot_generation: 7,
                tail_through_generation: 9,
                serialized_snapshot: vec![1, 2],
                raw_tail: vec![3, 4, 5],
            },
            77,
        );
        assert_eq!(frame[0], 9);
        assert_eq!(&frame[3..5], b"%1");
        assert_eq!(&frame[5..13], &77_u64.to_be_bytes());
        assert_eq!(frame[13], 2);
        assert_eq!(frame[14], 1);
        assert_eq!(&frame[23..31], &7_u64.to_be_bytes());
        assert_eq!(&frame[31..39], &9_u64.to_be_bytes());
        assert!(
            frame.len() < 96,
            "binary resource framing regressed to JSON arrays"
        );
    }

    fn assert_snapshot_frame_sequence(frame: &[u8], expected: u64) {
        assert_eq!(frame[0], 7);
        let label_len = usize::from(u16::from_be_bytes([frame[1], frame[2]]));
        assert_eq!(&frame[3..3 + label_len], b"snapshot");
        let sequence_offset = 3 + label_len;
        assert_eq!(
            &frame[sequence_offset..sequence_offset + 8],
            &expected.to_be_bytes()
        );
        let payload: serde_json::Value =
            serde_json::from_slice(&frame[sequence_offset + 8..]).unwrap();
        assert_eq!(payload["sequence"].as_u64(), Some(expected));
    }

    #[test]
    fn fresh_reconnect_snapshot_frame_uses_accepted_sequence_atomically() {
        let frame = encode_event(TerminalEvent::Snapshot {
            snapshot: tmux_control::TmuxSnapshot::default(),
            sequence: 41,
            generation: 3,
            server_identity: "local:test".into(),
            authoritative: true,
        });
        assert_snapshot_frame_sequence(&frame, 41);
    }

    #[test]
    fn resync_snapshot_frame_cannot_diverge_from_payload_sequence() {
        let frame = event_frame::encode_event_with_sequence(
            TerminalEvent::Snapshot {
                snapshot: tmux_control::TmuxSnapshot::default(),
                sequence: 97,
                generation: 8,
                server_identity: "ssh:test".into(),
                authoritative: true,
            },
            0,
        );
        assert_snapshot_frame_sequence(&frame, 97);
    }

    #[test]
    fn pane_scoped_recovery_does_not_disconnect_sibling_sessions() {
        let client = TerminalClient::new(None);
        client.ready.store(true, Ordering::Release);
        assert_eq!(scoped_terminal_recovery("%12").as_deref(), Some("%12"));
        assert!(scoped_terminal_recovery("terminal").is_none());
        assert!(client.ready.load(Ordering::Acquire));
    }

    #[test]
    fn terminal_seed_command_builds_a_scoped_validated_request() {
        let request = terminal_seed_request("%12".into()).unwrap();
        assert_eq!(
            v1::Operation::try_from(request.operation).unwrap(),
            v1::Operation::RequestTerminalSeed
        );
        assert_eq!(request.scope, "%12");
        assert!(terminal_seed_request("%12; kill-server".into()).is_err());
    }

    #[test]
    fn visibility_handoff_rejects_stale_epoch_and_preserves_cutoff() {
        assert!(terminal_visibility_request("%1".into(), false, Vec::new(), 6, 10, 7).is_err());
        let request =
            terminal_visibility_request("%1".into(), false, b"snapshot".to_vec(), 7, 42, 7)
                .unwrap();
        assert_eq!(request.terminal_epoch, 7);
        assert_eq!(request.terminal_generation_cutoff, 42);
        assert_eq!(request.data, b"snapshot");
    }

    #[test]
    fn terminal_scope_uses_authoritative_snapshot_for_initial_and_stale_requests() {
        let snapshot = tmux_control::TmuxSnapshot {
            sessions: vec![tmux_control::Session {
                id: "$1".into(),
                name: "work".into(),
                window_count: 1,
                attached_clients: 0,
                order: 0,
            }],
            windows: vec![
                tmux_control::Window {
                    id: "@1".into(),
                    session_id: "$1".into(),
                    index: 0,
                    name: "active".into(),
                    active: true,
                    layout: String::new(),
                    zoomed: false,
                },
                tmux_control::Window {
                    id: "@2".into(),
                    session_id: "$1".into(),
                    index: 1,
                    name: "hidden".into(),
                    active: false,
                    layout: String::new(),
                    zoomed: false,
                },
            ],
            panes: [("%2", "@1"), ("%3", "@2")]
                .into_iter()
                .map(|(id, window_id)| tmux_control::Pane {
                    id: id.into(),
                    session_id: "$1".into(),
                    window_id: window_id.into(),
                    index: 0,
                    active: true,
                    width: 80,
                    height: 24,
                    left: 0,
                    top: 0,
                    current_path: "/tmp".into(),
                    current_command: "bash".into(),
                    pane_pid: 0,
                    start_command: String::new(),
                })
                .collect(),
        };
        assert_eq!(
            terminal_scope(&snapshot, "", &[]),
            ("$1".into(), vec!["%2".into()])
        );
        assert_eq!(
            terminal_scope(&snapshot, "$99", &["%99".into()]),
            ("$1".into(), Vec::<String>::new())
        );
    }

    #[test]
    fn incompatible_server_hello_enters_read_only_without_requesting_a_snapshot() {
        let compatible = v1::ServerHello {
            read_only: false,
            capabilities: HOST_CAPABILITIES,
            ..Default::default()
        };
        let read_only = v1::ServerHello {
            read_only: true,
            ..Default::default()
        };
        assert!(handshake_allows_snapshot(PROTOCOL_MAJOR, &compatible));
        assert!(!handshake_allows_snapshot(PROTOCOL_MAJOR + 1, &compatible));
        assert!(!handshake_allows_snapshot(PROTOCOL_MAJOR, &read_only));
        let missing_capability = v1::ServerHello {
            capabilities: 0,
            ..compatible
        };
        assert!(!handshake_allows_snapshot(
            PROTOCOL_MAJOR,
            &missing_capability
        ));
    }
}
