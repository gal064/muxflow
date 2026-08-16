use std::{
    collections::HashMap,
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, bail};
use tmux_agent_protocol::{
    FrameError, HELPER_VERSION, HOST_CAPABILITIES, PROTOCOL_MAJOR, envelope, read_frame,
    v1::{self, envelope::Payload},
    write_frame,
};
use tokio::{
    net::UnixStream,
    sync::mpsc,
    time::{sleep, timeout},
};

pub(crate) mod snapshot;
use snapshot::{server_identity, snapshot_from_identity};
mod active_root;
pub(crate) mod agents;
mod filesystem;
use filesystem::FileService;
mod git;
use git::GitService;
mod terminal;
use terminal::TerminalClients;
mod tmux_actions;
mod tmux_config;
pub(crate) use tmux_config::{
    apply_recommended_naming as apply_recommended_tmux_naming,
    remove_recommended_naming as remove_recommended_tmux_naming,
};
mod topology;
#[cfg(test)]
use terminal::validate_tmux_id;
use topology::{TopologyActor, TopologySignal};
mod events;
#[cfg(test)]
use events::CONTROL_EVENT_HUB;
pub(crate) use events::broadcast_control_event;
use events::{
    ConnectionTaskGuard, ProtocolSequencer, SequencerControl, register_control_event_sink,
};

/// Depth of the ordered host-event queue.
///
/// Overflow here is not a dropped frame but a connection-wide resync: the
/// desktop tears the bridge down and reseeds every pane. At 128 a burst of
/// terminal output could reach that cliff during ordinary use, and it took a
/// seed with it — a seed dropped by a full queue leaves the pane waiting for
/// bytes that will never come. The depth is chosen against the reader's 64 KiB
/// read size and tmux's own `pause-after` flow control, which bounds how far
/// ahead of a slow consumer the queue can run.
pub(crate) const EVENT_QUEUE: usize = 1024;
pub(crate) const TERMINAL_INPUT_QUEUE: usize = 256;

pub async fn serve_with_shutdown(
    mut stream: UnixStream,
    shutdown: Option<tokio::sync::mpsc::UnboundedSender<()>>,
) -> anyhow::Result<()> {
    let hello = timeout(Duration::from_secs(5), read_frame(&mut stream))
        .await
        .context("client handshake timed out")??
        .context("client disconnected before handshake")?;
    let Some(Payload::ClientHello(client_hello)) = hello.payload else {
        send_handshake_error(
            &mut stream,
            "handshake_required",
            "first frame must be ClientHello",
        )
        .await?;
        bail!("first frame was not ClientHello");
    };

    let host_protocol_major = advertised_protocol_major();
    let protocol_compatible = hello.protocol_major == host_protocol_major;
    let host_helper_version = advertised_helper_version();
    let helper_compatible = client_hello.expected_helper_version.is_empty()
        || client_hello.expected_helper_version == host_helper_version;
    let current_server_identity = server_identity();
    let identity_compatible = !client_hello.bulk_connection
        || (!client_hello.expected_server_identity.is_empty()
            && client_hello.expected_server_identity == current_server_identity);
    let read_only = !protocol_compatible || !helper_compatible || !identity_compatible;
    let incompatibility = if !protocol_compatible {
        format!(
            "protocol major {} is incompatible with host major {host_protocol_major}",
            hello.protocol_major
        )
    } else if !helper_compatible {
        format!(
            "helper {} does not match required {}",
            host_helper_version, client_hello.expected_helper_version
        )
    } else if !identity_compatible {
        format!(
            "bulk connection expected server identity {:?}, but the active server is {:?}",
            client_hello.expected_server_identity, current_server_identity
        )
    } else {
        String::new()
    };
    let mut server_hello = envelope(
        hello.request_id,
        0,
        Payload::ServerHello(v1::ServerHello {
            helper_version: host_helper_version,
            operating_system: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
            tmux_version: daemon_command_version(CommandVersion::Tmux),
            server_identity: current_server_identity,
            capabilities: HOST_CAPABILITIES & client_hello.requested_capabilities,
            read_only,
            incompatibility,
            git_version: daemon_command_version(CommandVersion::Git),
            connection_epoch: client_hello.connection_epoch,
        }),
    );
    server_hello.protocol_major = host_protocol_major;
    write_frame(&mut stream, &server_hello).await?;

    let (mut reader, mut writer) = stream.into_split();
    let (control_tx, mut control_rx) = mpsc::channel::<SequencerControl>(EVENT_QUEUE);
    let mut event_registration =
        (!client_hello.bulk_connection).then(|| register_control_event_sink(control_tx.clone()));
    let closed = Arc::new(AtomicBool::new(false));
    let _connection_task_guard = ConnectionTaskGuard(Arc::clone(&closed));
    let topology_signal = TopologySignal::default();
    let writer_topology_signal = topology_signal.clone();
    let writer_closed = Arc::clone(&closed);
    let writer_task = tokio::spawn(async move {
        let mut sequencer = ProtocolSequencer::default();
        while let Some(message) = control_rx.recv().await {
            if let SequencerControl::TopologyEpochBarrier(completion) = message {
                let _ = completion.send(writer_topology_signal.current_epoch());
                continue;
            }
            writer_topology_signal.observe_event(&message);
            let frame = sequencer.frame(message);
            if write_frame(&mut writer, &frame).await.is_err() {
                break;
            }
        }
        writer_closed.store(true, Ordering::Release);
    });

    let generation = Arc::new(AtomicU64::new(0));
    let overflowed = Arc::new(AtomicBool::new(false));
    let subscribed = Arc::new(AtomicBool::new(false));
    let pending = Arc::new(Mutex::new(HashMap::<u64, Arc<AtomicBool>>::new()));
    let terminal = Arc::new(Mutex::new(TerminalClients::new()));
    let topology_lock = Arc::new(tokio::sync::Mutex::new(()));
    let topology_baseline = Arc::new(Mutex::new(None::<(tmux_control::TmuxSnapshot, String)>));
    let files = Arc::new(FileService::new());
    let git = Arc::new(GitService::new());
    files.spawn_watcher(
        Arc::clone(&closed),
        control_tx.clone(),
        Arc::clone(&overflowed),
    );

    TopologyActor {
        closed: Arc::clone(&closed),
        subscribed: Arc::clone(&subscribed),
        generation: Arc::clone(&generation),
        overflowed: Arc::clone(&overflowed),
        lock: Arc::clone(&topology_lock),
        baseline: Arc::clone(&topology_baseline),
        terminal: Arc::clone(&terminal),
        sender: control_tx.clone(),
        signal: topology_signal.clone(),
    }
    .spawn();

    let mut read_error = None;
    while !closed.load(Ordering::Acquire) {
        let frame = match read_frame(&mut reader).await {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) if is_clean_peer_disconnect(&error) => break,
            Err(error) => {
                read_error = Some(error);
                break;
            }
        };
        if frame.protocol_major != host_protocol_major {
            send_response(
                &control_tx,
                frame.request_id,
                response_error(
                    "protocol_incompatible",
                    "frame protocol major does not match the negotiated connection",
                ),
            )
            .await;
            continue;
        }
        match frame.payload {
            Some(Payload::Cancel(cancel)) => {
                if let Some(token) = pending.lock().unwrap().get(&cancel.target_request_id) {
                    token.store(true, Ordering::Release);
                }
            }
            Some(Payload::Request(request)) => {
                if frame.request_id == 0 {
                    send_response(
                        &control_tx,
                        frame.request_id,
                        response_error("invalid_request_id", "request IDs must be non-zero"),
                    )
                    .await;
                    continue;
                }
                let policy =
                    requests::operation_policy::OperationPolicy::for_raw(request.operation);
                if let Some(error) = policy.admission_error(read_only, client_hello.bulk_connection)
                {
                    send_response(
                        &control_tx,
                        frame.request_id,
                        response_error(error.code(), error.message()),
                    )
                    .await;
                    continue;
                }
                // Daemon shutdown intentionally remains a pre-dispatch exception: this endpoint
                // owns the shutdown sender, and acknowledging it must not register cancellable
                // work that can outlive the connection. Its mutation policy is still documented
                // and exhaustively tested with every other generated operation.
                if policy.handler == requests::operation_policy::Handler::Daemon {
                    if let Some(shutdown) = shutdown.clone() {
                        send_response(&control_tx, frame.request_id, response_ok()).await;
                        tokio::spawn(async move {
                            sleep(Duration::from_millis(100)).await;
                            let _ = shutdown.send(());
                        });
                    } else {
                        send_response(
                            &control_tx,
                            frame.request_id,
                            response_error(
                                "shutdown_unavailable",
                                "this protocol endpoint does not own a daemon",
                            ),
                        )
                        .await;
                    }
                    continue;
                }
                let cancellation = Arc::new(AtomicBool::new(false));
                let registration = {
                    use std::collections::hash_map::Entry;
                    let mut requests = pending.lock().unwrap();
                    if closed.load(Ordering::Acquire) {
                        Err("connection_closed")
                    } else {
                        match requests.entry(frame.request_id) {
                            Entry::Vacant(entry) => {
                                entry.insert(Arc::clone(&cancellation));
                                Ok(())
                            }
                            Entry::Occupied(_) => Err("duplicate_request_id"),
                        }
                    }
                };
                if let Err(code) = registration {
                    let message = if code == "connection_closed" {
                        "connection closed before request registration"
                    } else {
                        "request ID is already in flight"
                    };
                    send_response(&control_tx, frame.request_id, response_error(code, message))
                        .await;
                    continue;
                }
                if closed.load(Ordering::Acquire) {
                    cancellation.store(true, Ordering::Release);
                }
                let detached_work =
                    policy.scheduling == requests::operation_policy::Scheduling::Detached;
                let work = handle_request(
                    frame.request_id,
                    request,
                    policy,
                    Arc::clone(&cancellation),
                    RequestContext {
                        control_tx: control_tx.clone(),
                        event_tx: control_tx.clone(),
                        generation: Arc::clone(&generation),
                        overflowed: Arc::clone(&overflowed),
                        subscribed: Arc::clone(&subscribed),
                        pending: Arc::clone(&pending),
                        terminal: Arc::clone(&terminal),
                        topology_lock: Arc::clone(&topology_lock),
                        topology_baseline: Arc::clone(&topology_baseline),
                        topology_signal: topology_signal.clone(),
                        files: Arc::clone(&files),
                        git: Arc::clone(&git),
                        bulk_connection: client_hello.bulk_connection,
                        connection_epoch: client_hello.connection_epoch,
                        closed: Arc::clone(&closed),
                    },
                );
                if detached_work {
                    // Give the reader one bounded turn to consume an already
                    // buffered Cancel or EOF after synchronous registration.
                    // This closes the request+Cancel scheduling race without
                    // delaying ordinary work perceptibly.
                    tokio::spawn(async move {
                        sleep(Duration::from_millis(1)).await;
                        work.await;
                    });
                } else {
                    work.await;
                }
            }
            _ => {
                send_response(
                    &control_tx,
                    frame.request_id,
                    response_error("invalid_payload", "expected request or cancellation"),
                )
                .await;
            }
        }
    }

    closed.store(true, Ordering::Release);
    for (_, token) in pending.lock().unwrap().drain() {
        token.store(true, Ordering::Release);
    }
    terminal.lock().unwrap().stop();
    drop(event_registration.take());
    drop(control_tx);
    let _ = writer_task.await;
    if let Some(error) = read_error {
        Err(error.into())
    } else {
        Ok(())
    }
}

fn is_clean_peer_disconnect(error: &FrameError) -> bool {
    matches!(
        error,
        FrameError::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
            )
    )
}

mod requests;
use requests::{RequestContext, handle_request};

fn reconcile_terminal_clients(
    terminal: &Arc<Mutex<TerminalClients>>,
    snapshot: &tmux_control::TmuxSnapshot,
    event_sender: &mpsc::Sender<SequencerControl>,
    overflowed: &Arc<AtomicBool>,
) {
    let mut terminal = terminal.lock().unwrap();
    reconcile_terminal_clients_locked(&mut terminal, snapshot, event_sender, overflowed);
}

fn reconcile_terminal_clients_if_open(
    closed: &AtomicBool,
    terminal: &Arc<Mutex<TerminalClients>>,
    snapshot: &tmux_control::TmuxSnapshot,
    event_sender: &mpsc::Sender<SequencerControl>,
    overflowed: &Arc<AtomicBool>,
) {
    // Serialize the final connection-close check with attachment creation.
    // Otherwise a topology discovery that started before EOF can attach a new
    // tmux control client after `serve_with_shutdown` has already stopped and
    // cleared the connection's existing clients.
    let mut terminal = terminal.lock().unwrap();
    if closed.load(Ordering::Acquire) {
        return;
    }
    reconcile_terminal_clients_locked(&mut terminal, snapshot, event_sender, overflowed);
}

fn reconcile_terminal_clients_locked(
    terminal: &mut TerminalClients,
    snapshot: &tmux_control::TmuxSnapshot,
    event_sender: &mpsc::Sender<SequencerControl>,
    overflowed: &Arc<AtomicBool>,
) {
    terminal.reconcile(snapshot);
    let mut panes_by_session: HashMap<&str, Vec<String>> = HashMap::new();
    for pane in &snapshot.panes {
        panes_by_session
            .entry(&pane.session_id)
            .or_default()
            .push(pane.id.clone());
    }
    for session in &snapshot.sessions {
        let Some(pane_ids) = panes_by_session.remove(session.id.as_str()) else {
            continue;
        };
        if let Err(error) = terminal.attach(
            &session.id,
            &pane_ids,
            false,
            event_sender.clone(),
            Arc::clone(overflowed),
        ) {
            emit_event(
                event_sender,
                overflowed,
                v1::HostEvent {
                    kind: v1::EventKind::TerminalExit.into(),
                    scope: session.id.clone(),
                    detail: format!("session control client failed: {error}"),
                    ..Default::default()
                },
            );
        }
    }
}

async fn lock_topology_generation<'a>(
    topology_lock: &'a tokio::sync::Mutex<()>,
    generation: &AtomicU64,
) -> (tokio::sync::MutexGuard<'a, ()>, u64) {
    let guard = topology_lock.lock().await;
    let generation = generation.load(Ordering::Acquire);
    (guard, generation)
}

fn baseline_changed(
    cached: Option<&(tmux_control::TmuxSnapshot, String)>,
    fresh: &tmux_control::TmuxSnapshot,
    fresh_identity: &str,
) -> bool {
    match cached {
        None => true,
        Some((snapshot, identity)) => {
            identity.as_str() != fresh_identity || !same_action_topology(snapshot, fresh)
        }
    }
}

fn same_action_topology(
    cached: &tmux_control::TmuxSnapshot,
    fresh: &tmux_control::TmuxSnapshot,
) -> bool {
    let cached = normalize_action_topology(cached.clone());
    let fresh = normalize_action_topology(fresh.clone());
    // Compare the normalized structural projection canonically so discovery
    // record ordering and every guarded field remain deterministic.
    let cached_json =
        serde_json::to_vec(&cached).expect("tmux snapshot serialization is infallible");
    let fresh_json = serde_json::to_vec(&fresh).expect("tmux snapshot serialization is infallible");
    cached_json == fresh_json
}

fn normalize_action_topology(
    mut snapshot: tmux_control::TmuxSnapshot,
) -> tmux_control::TmuxSnapshot {
    // Control-client attachment counts and shell-driven automatic titles can
    // change asynchronously without invalidating an ID/layout operation.
    // Structural membership, indices, selection, layout, zoom and geometry
    // remain guarded and catch mutation-before-poll races.
    for session in &mut snapshot.sessions {
        session.attached_clients = 0;
        session.name.clear();
    }
    for window in &mut snapshot.windows {
        window.name.clear();
    }
    for pane in &mut snapshot.panes {
        pane.current_path.clear();
        pane.current_command.clear();
    }
    snapshot
}

async fn reconcile_internal_tmux_change(
    topology_lock: &Arc<tokio::sync::Mutex<()>>,
    topology_baseline: &Arc<Mutex<Option<(tmux_control::TmuxSnapshot, String)>>>,
    generation: &Arc<AtomicU64>,
    terminal: &Arc<Mutex<TerminalClients>>,
    event_tx: &mpsc::Sender<SequencerControl>,
    overflowed: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let _guard = topology_lock.lock().await;
    let (snapshot, identity) = tokio::task::spawn_blocking(tmux_actions::discover_before_action)
        .await
        .map_err(|error| anyhow::anyhow!("topology reconciliation task failed: {error}"))??;
    let changed = baseline_changed(
        topology_baseline.lock().unwrap().as_ref(),
        &snapshot,
        &identity,
    );
    if changed {
        let next_generation = generation.fetch_add(1, Ordering::AcqRel) + 1;
        *topology_baseline.lock().unwrap() = Some((snapshot.clone(), identity.clone()));
        reconcile_terminal_clients(terminal, &snapshot, event_tx, overflowed);
        event_tx
            .send(SequencerControl::OrderedEvent(v1::HostEvent {
                kind: v1::EventKind::TopologySnapshot.into(),
                scope: "topology".into(),
                snapshot: Some(snapshot_from_identity(snapshot, next_generation, identity)),
                detail: "app control-client topology reconciled".into(),
                ..Default::default()
            }))
            .await
            .map_err(|_| anyhow::anyhow!("topology event sequencer stopped"))?;
    }
    Ok(())
}

fn emit_event(
    sender: &mpsc::Sender<SequencerControl>,
    overflowed: &AtomicBool,
    event: v1::HostEvent,
) {
    match sender.try_send(SequencerControl::OrderedEvent(event)) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            crate::diagnostics::record_event_queue_overflow();
            overflowed.store(true, Ordering::Release);
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            overflowed.store(true, Ordering::Release);
        }
    }
}

async fn send_response(
    sender: &mpsc::Sender<SequencerControl>,
    request_id: u64,
    response: v1::Response,
) {
    let _ = sender
        .send(SequencerControl::Response {
            request_id,
            response,
            snapshot_barrier: false,
        })
        .await;
}

async fn send_snapshot_response(
    sender: &mpsc::Sender<SequencerControl>,
    request_id: u64,
    response: v1::Response,
) {
    let _ = sender
        .send(SequencerControl::Response {
            request_id,
            response,
            snapshot_barrier: true,
        })
        .await;
}

fn response_ok() -> v1::Response {
    v1::Response {
        ok: true,
        ..Default::default()
    }
}

fn response_error(code: &str, message: &str) -> v1::Response {
    v1::Response {
        ok: false,
        error_code: code.into(),
        display_message: message.into(),
        ..Default::default()
    }
}

fn response_snapshot(snapshot: v1::Snapshot) -> v1::Response {
    v1::Response {
        ok: true,
        snapshot: Some(snapshot),
        ..Default::default()
    }
}

async fn send_handshake_error(
    stream: &mut UnixStream,
    code: &str,
    message: &str,
) -> anyhow::Result<()> {
    write_frame(
        stream,
        &envelope(
            0,
            0,
            Payload::Error(v1::Error {
                code: code.into(),
                display_message: message.into(),
                retryable: false,
            }),
        ),
    )
    .await?;
    Ok(())
}

fn command_version(program: &str, argument: &str) -> String {
    Command::new(program)
        .arg(argument)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default()
}

enum CommandVersion {
    Tmux,
    Git,
}

fn daemon_command_version(version: CommandVersion) -> String {
    static TMUX: OnceLock<String> = OnceLock::new();
    static GIT: OnceLock<String> = OnceLock::new();
    match version {
        CommandVersion::Tmux => TMUX.get_or_init(|| command_version("tmux", "-V")).clone(),
        CommandVersion::Git => GIT
            .get_or_init(|| command_version("git", "--version"))
            .clone(),
    }
}

fn testing_enabled() -> bool {
    std::env::var_os("ADE_PHASE1_TESTING").is_some()
}

fn advertised_protocol_major() -> u32 {
    if testing_enabled()
        && let Ok(value) = std::env::var("ADE_PHASE1_TEST_PROTOCOL_MAJOR")
        && let Ok(value) = value.parse()
    {
        return value;
    }
    PROTOCOL_MAJOR
}

fn advertised_helper_version() -> String {
    if testing_enabled()
        && let Ok(value) = std::env::var("ADE_PHASE1_TEST_HELPER_VERSION")
        && !value.is_empty()
    {
        return value;
    }
    HELPER_VERSION.into()
}

#[cfg(test)]
#[path = "service/tests.rs"]
mod tests;
