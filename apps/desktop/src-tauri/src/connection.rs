use std::{
    collections::HashMap,
    process::Child,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
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
};
use uuid::Uuid;

use tmux_control::MAX_INPUT_REQUEST_BYTES;

mod event_frame;
use event_frame::{TerminalEvent, encode_event};
mod delivery_window;
use delivery_window::{DeliveryWindow, HostCharge};
pub(crate) mod delivery_ack;
use delivery_ack::flush_delivery_ack;
mod dispatch;
mod operations;
use dispatch::{
    ClientInputDispatch, ClientInputQueue, INPUT_BYTE_BUDGET, INPUT_MESSAGE_BUDGET, ResizeQueue,
    StopSignal, TerminalSize, run_client_input_dispatch, run_client_resize_dispatch,
};
use operations::{Bound, OperationClaim, OperationLane, OperationRegistry};
mod writer;
use writer::ControlWriterHandle;
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
    SshLease, acquire_control_master, acquire_control_master_for_socket, host_helper_path,
    ssh_profile_control_socket,
};

struct TerminalClient {
    bulk_scope: Uuid,
    writer: Mutex<Option<ControlWriterHandle>>,
    child: Mutex<Option<Child>>,
    stop_signal: StopSignal,
    ready: AtomicBool,
    read_only: AtomicBool,
    next_request_id: AtomicU64,
    pending: Mutex<HashMap<u64, mpsc::Sender<Result<v1::Response, String>>>>,
    operations: Arc<OperationRegistry>,
    input_queue: Mutex<ClientInputQueue>,
    resize_queue: ResizeQueue,
    input_epoch: AtomicU64,
    terminal_epoch: AtomicU64,
    server_identity: Mutex<String>,
    host_profile_id: Mutex<String>,
    delivery_window: Arc<Mutex<Option<Arc<DeliveryWindow>>>>,
    delivery_ack_serialization: Mutex<()>,
    pending_delivery_ack: Mutex<Option<(u64, HostCharge)>>,
}

struct InitialHostState {
    snapshot: tmux_control::TmuxSnapshot,
    agent_snapshot: Option<v1::AgentSnapshot>,
    accepted_sequence: u64,
    generation: u64,
    buffered_events: Vec<v1::Envelope>,
}

impl TerminalClient {
    fn new() -> Self {
        Self {
            bulk_scope: Uuid::new_v4(),
            writer: Mutex::new(None),
            child: Mutex::new(None),
            stop_signal: StopSignal::default(),
            ready: AtomicBool::new(false),
            read_only: AtomicBool::new(false),
            next_request_id: AtomicU64::new(100),
            pending: Mutex::new(HashMap::new()),
            operations: Arc::new(OperationRegistry::default()),
            input_queue: Mutex::new(ClientInputQueue::default()),
            resize_queue: ResizeQueue::default(),
            input_epoch: AtomicU64::new(0),
            terminal_epoch: AtomicU64::new(0),
            server_identity: Mutex::new(String::new()),
            host_profile_id: Mutex::new(String::new()),
            delivery_window: Arc::new(Mutex::new(None)),
            delivery_ack_serialization: Mutex::new(()),
            pending_delivery_ack: Mutex::new(None),
        }
    }

    fn start_dispatchers(self: &Arc<Self>, client_id: &str) -> Result<(), String> {
        let (sender, receiver) = mpsc::sync_channel(INPUT_MESSAGE_BUDGET);
        self.input_queue.lock().unwrap().sender = Some(sender);
        let client = Arc::clone(self);
        thread::Builder::new()
            .name(format!("host-input-dispatch-{client_id}"))
            .spawn(move || run_client_input_dispatch(client, receiver))
            .map_err(|error| format!("failed to start input dispatcher: {error}"))?;
        let client = Arc::clone(self);
        thread::Builder::new()
            .name(format!("host-resize-dispatch-{client_id}"))
            .spawn(move || run_client_resize_dispatch(client))
            .map(|_| ())
            .map_err(|error| {
                self.shutdown_transport("host connection startup failed");
                format!("failed to start resize dispatcher: {error}")
            })
    }

    fn shutdown_transport(&self, pending_message: &str) {
        self.stop_signal.stop();
        files::invalidate_bulk_scope(self.bulk_scope, pending_message);
        self.ready.store(false, Ordering::Release);
        self.resize_queue.stop();
        if let Some(window) = self.delivery_window.lock().unwrap().take() {
            window.close();
        }
        self.pending_delivery_ack.lock().unwrap().take();
        if let Some(writer) = self.writer.lock().unwrap().take() {
            writer.close();
        }
        if let Some(sender) = self.input_queue.lock().unwrap().sender.take() {
            let _ = sender.try_send(ClientInputDispatch::Stop);
        }
        self.fail_pending(pending_message);
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn reconnect_transport(&self) {
        files::invalidate_bulk_scope(
            self.bulk_scope,
            "bulk transfer control connection is reconnecting",
        );
        self.ready.store(false, Ordering::Release);
        if let Some(window) = self.delivery_window.lock().unwrap().take() {
            window.close();
        }
        self.pending_delivery_ack.lock().unwrap().take();
        if let Some(writer) = self.writer.lock().unwrap().take() {
            writer.close();
        }
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
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
        if self.stop_signal.is_stopped()
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
        if data.len() > MAX_INPUT_REQUEST_BYTES {
            return Err("terminal input batch exceeds 1 MiB; retry with a smaller batch".into());
        }
        let data_len = data.len();
        let mut queue = self.input_queue.lock().unwrap();
        if queue.messages >= INPUT_MESSAGE_BUDGET
            || queue.bytes.saturating_add(data_len) > INPUT_BYTE_BUDGET
        {
            return Err("terminal input queue budget is full; retry without dropping bytes".into());
        }
        queue.messages += 1;
        queue.bytes += data_len;
        let result = queue
            .sender
            .as_ref()
            .ok_or_else(|| "terminal input dispatcher is unavailable".to_owned())
            .and_then(|sender| {
                sender
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
            });
        if result.is_err() {
            queue.messages -= 1;
            queue.bytes -= data_len;
        }
        result
    }

    fn release_input_budget(&self, messages: usize, bytes: usize) {
        let mut queue = self.input_queue.lock().unwrap();
        queue.messages = queue.messages.saturating_sub(messages);
        queue.bytes = queue.bytes.saturating_sub(bytes);
    }

    fn flush_input(&self) -> Result<(), String> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let dispatcher = self
            .input_queue
            .lock()
            .unwrap()
            .sender
            .clone()
            .ok_or_else(|| "terminal input dispatcher is unavailable".to_owned())?;
        dispatcher
            .send(ClientInputDispatch::Barrier(sender))
            .map_err(|_| "terminal input dispatcher is disconnected".to_owned())?;
        receiver
            .recv_timeout(REQUEST_TIMEOUT)
            .map_err(|_| "terminal input flush timed out".to_owned())?
    }

    fn enqueue_resize(
        &self,
        columns: u16,
        rows: u16,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), String>>, String> {
        if self.stop_signal.is_stopped() {
            return Err("terminal bridge is stopped; resize was not queued".into());
        }
        self.resize_queue.replace(TerminalSize { columns, rows })
    }

    fn wait_for_reconnect(&self, delay: Duration) -> bool {
        self.stop_signal.wait_timeout(delay)
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
        let writer = self
            .writer
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "host bridge is disconnected".to_owned())?;
        writer.write(
            envelope(request_id, 0, Payload::Request(request)),
            Instant::now() + REQUEST_TIMEOUT,
        )
    }

    fn request_git(
        self: &Arc<Self>,
        request: v1::Request,
        operation_id: &str,
    ) -> Result<v1::Response, String> {
        let claim = self.operations.claim(OperationLane::Git, operation_id)?;
        self.request_with_timeout(request, GIT_REQUEST_TIMEOUT, Some(claim))
    }

    /// A control-lane file request the renderer can cancel by operation id.
    ///
    /// Explorer listings are the one control-lane file operation worth
    /// cancelling: a collapsed folder, a replaced root, or a superseded
    /// preview leaves a bounded remote enumeration running that nothing will
    /// ever read, and on the remote link that is the whole cost of the action.
    fn request_file(
        &self,
        request: v1::Request,
        claim: Option<OperationClaim>,
    ) -> Result<v1::Response, String> {
        self.request_with_timeout(request, REQUEST_TIMEOUT, claim)
    }

    fn request_with_timeout(
        &self,
        request: v1::Request,
        timeout: Duration,
        operation: Option<OperationClaim>,
    ) -> Result<v1::Response, String> {
        let deadline = Instant::now() + timeout;
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
        if let Some(claim) = &operation
            && matches!(self.operations.bind(claim, request_id), Bound::Cancelled)
        {
            self.pending.lock().unwrap().remove(&request_id);
            return Err("cancelled: request was cancelled before dispatch".into());
        }
        let write_result = self
            .writer
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "host bridge is disconnected".to_owned())
            .and_then(|writer| {
                writer.write(envelope(request_id, 0, Payload::Request(request)), deadline)
            });
        if let Err(error) = write_result {
            self.pending.lock().unwrap().remove(&request_id);
            if let Some(claim) = &operation {
                self.operations.unbind(claim);
            }
            return Err(error);
        }
        let result = match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(Ok(response)) if response.ok => Ok(response),
            Ok(Ok(response)) => Err(format!(
                "{}: {}",
                response.error_code, response.display_message
            )),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                if let Some(writer) = self.writer.lock().unwrap().clone() {
                    let _ = writer.try_write(
                        envelope(
                            self.next_request_id.fetch_add(1, Ordering::AcqRel),
                            0,
                            Payload::Cancel(v1::Cancel {
                                target_request_id: request_id,
                            }),
                        ),
                        Instant::now() + REQUEST_TIMEOUT,
                    );
                }
                self.pending.lock().unwrap().remove(&request_id);
                Err(
                    "host request timed out; commit outcome is unknown and the request will not be replayed"
                        .into(),
                )
            }
        };
        if let Some(claim) = &operation {
            self.operations.unbind(claim);
        }
        result
    }

    fn cancel_git(&self, operation_id: &str) -> Result<(), String> {
        self.cancel_operation(OperationLane::Git, operation_id)
    }

    fn cancel_file(&self, operation_id: &str) -> Result<(), String> {
        self.cancel_operation(OperationLane::File, operation_id)
    }

    pub(crate) fn claim_file_operation(
        &self,
        operation_id: &str,
    ) -> Result<OperationClaim, String> {
        self.operations.claim(OperationLane::File, operation_id)
    }

    fn cancel_operation(&self, lane: OperationLane, operation_id: &str) -> Result<(), String> {
        let Some(request_id) = self.operations.cancel(lane, operation_id) else {
            // Claimed but not yet dispatched. The tombstone the registry left
            // refuses the request rather than sending it to a host that would
            // never be told to stop.
            return Ok(());
        };
        let writer = self
            .writer
            .lock()
            .unwrap()
            .clone()
            .ok_or("host bridge is disconnected")?;
        writer.write(
            envelope(
                self.next_request_id.fetch_add(1, Ordering::AcqRel),
                0,
                Payload::Cancel(v1::Cancel {
                    target_request_id: request_id,
                }),
            ),
            Instant::now() + REQUEST_TIMEOUT,
        )
    }

    fn fail_pending(&self, message: &str) {
        self.operations.clear();
        for (_, sender) in self.pending.lock().unwrap().drain() {
            let _ = sender.send(Err(message.into()));
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
    measurement_id: String,
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
    let measurement_id =
        Uuid::parse_str(&measurement_id).map_err(|_| "invalid terminal measurement ID")?;
    let client_id = Uuid::new_v4().to_string();
    let client = Arc::new(TerminalClient::new());
    *client.host_profile_id.lock().unwrap() = match &connection {
        ConnectionSpec::Local => "local".into(),
        ConnectionSpec::Ssh { profile_id, .. } => profile_id.clone(),
    };
    client.start_dispatchers(&client_id)?;
    let event_channel = TerminalEventChannel::new(
        measurement_id,
        on_event,
        Arc::clone(&client.delivery_window),
    );
    // Publish local progress before the supervisor can perform DNS, ProxyJump,
    // authentication, or any other network work.
    let _ = event_channel.send(encode_event(TerminalEvent::ConnectionState {
        state: "connecting".into(),
    }));
    let worker_id = client_id.clone();
    let worker_client = Arc::clone(&client);
    let worker_channel = event_channel.clone();
    thread::Builder::new()
        .name(format!("host-bridge-{client_id}"))
        .spawn(move || {
            supervise_bridge(
                worker_id,
                connection,
                session_id,
                pane_ids,
                worker_channel,
                worker_client,
            )
        })
        .map_err(|error| {
            client.shutdown_transport("host bridge supervisor failed to start");
            let _ = event_channel.send(encode_event(TerminalEvent::ConnectionState {
                state: "disconnected".into(),
            }));
            format!("failed to start host bridge supervisor: {error}")
        })?;
    clients.0.lock().unwrap().insert(client_id.clone(), client);
    Ok(client_id)
}

#[tauri::command]
pub fn stop_terminal(client_id: String, clients: State<'_, TerminalClients>) -> Result<(), String> {
    if let Some(client) = clients.0.lock().unwrap().remove(&client_id) {
        client.shutdown_transport("host connection stopped");
        // Pooled bulk bridges are owned by this exact control-client token, so
        // once the connection is gone none of *its* bridges can be handed to
        // anything: closing them here frees their ssh
        // channels and remote helper processes now rather than at the idle
        // timeout. Only this connection's, though — another window can be
        // connected to another host at the same time, and its warm bridges are
        // still reachable.
        files::bulk_pool::close_pooled_bulk_bridges(client.bulk_scope);
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
    if data.len() > MAX_INPUT_REQUEST_BYTES {
        return Err("terminal input batch exceeds 1 MiB; retry with a smaller batch".into());
    }
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

/// Tells the host which session the desktop is showing, so tmux sizes from
/// that one's control client. `useVisibleTerminalSession.ts` owns why this
/// exists and when it is sent.
///
/// Async and spawn_blocking for the same reason `set_terminal_visibility` is:
/// this runs on a workspace switch, and holding the WebView's main thread for
/// an SSH round trip is a visibly frozen switch.
#[tauri::command]
pub async fn select_terminal_session(
    client_id: String,
    session_id: String,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    validate_tmux_id(&session_id, '$')?;
    let client = get_client(&clients, &client_id)?;
    let request = v1::Request {
        operation: v1::Operation::SelectTerminalSession.into(),
        session_id,
        ..Default::default()
    };
    tauri::async_runtime::spawn_blocking(move || client.request(request))
        .await
        .map_err(|error| format!("terminal session selection task failed: {error}"))??;
    Ok(())
}

#[tauri::command]
pub async fn resize_terminal_client(
    client_id: String,
    columns: u16,
    rows: u16,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    let client = get_client(&clients, &client_id)?;
    let receiver = client.enqueue_resize(columns, rows)?;
    tokio::time::timeout(
        REQUEST_TIMEOUT + REQUEST_TIMEOUT + Duration::from_secs(1),
        receiver,
    )
    .await
    .map_err(|_| "terminal resize acknowledgement timed out".to_owned())?
    .map_err(|_| "terminal resize acknowledgement channel closed".to_owned())?
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

#[derive(Clone)]
pub(super) struct TerminalEventChannel {
    measurement: Arc<TerminalMeasurement>,
    channel: Channel<InvokeResponseBody>,
    delivery_window: Arc<Mutex<Option<Arc<DeliveryWindow>>>>,
}

impl TerminalEventChannel {
    fn new(
        measurement_id: Uuid,
        channel: Channel<InvokeResponseBody>,
        delivery_window: Arc<Mutex<Option<Arc<DeliveryWindow>>>>,
    ) -> Self {
        Self {
            measurement: Arc::new(TerminalMeasurement(measurement_id)),
            channel,
            delivery_window,
        }
    }

    fn send(&self, frame: Vec<u8>) -> Result<(), String> {
        self.send_charged(frame, HostCharge::default())
    }

    fn send_charged(&self, frame: Vec<u8>, host: HostCharge) -> Result<(), String> {
        let window = self.delivery_window.lock().unwrap().clone();
        let reservation = window
            .as_ref()
            .map(|window| window.reserve(frame.len(), host))
            .transpose()?;
        crate::perf_log::send_bridge_frame(self.measurement.0, &self.channel, frame)
            .and_then(|()| reservation.map_or(Ok(()), |reservation| reservation.commit()))
    }
}

struct TerminalMeasurement(Uuid);

impl Drop for TerminalMeasurement {
    fn drop(&mut self) {
        crate::perf_log::quiesce_bridge_measurement(self.0);
    }
}

fn send_event(channel: &TerminalEventChannel, event: TerminalEvent) {
    let _ = channel.send(encode_event(event));
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
#[path = "connection/tests.rs"]
mod tests;
