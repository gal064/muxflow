use std::{
    collections::HashMap,
    process::Command,
    sync::{
        Arc, Mutex,
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

pub(crate) const EVENT_QUEUE: usize = 128;
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
            tmux_version: command_version("tmux", "-V"),
            server_identity: current_server_identity,
            capabilities: HOST_CAPABILITIES & client_hello.requested_capabilities,
            read_only,
            incompatibility,
            git_version: command_version("git", "--version"),
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
        signal: topology_signal,
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
                if v1::Operation::try_from(request.operation).unwrap_or_default()
                    == v1::Operation::ShutdownDaemon
                {
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
                let operation = v1::Operation::try_from(request.operation).unwrap_or_default();
                let detached_work = requests::file_ops::runs_off_control_loop(operation)
                    || requests::git_dispatch::handles(operation);
                let work = handle_request(
                    frame.request_id,
                    request,
                    read_only,
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
    terminal.reconcile(snapshot);
    for session in &snapshot.sessions {
        let pane_ids: Vec<_> = snapshot
            .panes
            .iter()
            .filter(|pane| pane.session_id == session.id)
            .map(|pane| pane.id.clone())
            .collect();
        if pane_ids.is_empty() {
            continue;
        }
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
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};
    use tokio::io::AsyncWriteExt as _;

    #[test]
    fn rejects_tmux_command_injection() {
        assert!(validate_tmux_id("%2", '%').is_ok());
        assert!(validate_tmux_id("%2; kill-server", '%').is_err());
    }

    #[test]
    fn peer_transport_reset_is_a_clean_disconnect_but_malformed_frames_are_not() {
        let reset = FrameError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionReset));
        assert!(is_clean_peer_disconnect(&reset));
        assert!(!is_clean_peer_disconnect(&FrameError::TooLarge(
            tmux_agent_protocol::MAX_FRAME_BYTES + 1,
        )));
    }

    #[test]
    fn snapshot_scopes_are_explicitly_bounded() {
        for scope in ["", "full", "topology"] {
            assert!(matches!(scope, "" | "full" | "topology"));
        }
        assert!(!matches!("files", "" | "full" | "topology"));
    }

    #[test]
    fn snapshot_conversion_preserves_server_instance_and_bytesafe_fields() {
        let snapshot =
            snapshot_from_identity(tmux_control::TmuxSnapshot::default(), 4, "tmux:test".into());
        assert_eq!(snapshot.generation, 4);
        assert!(!snapshot.server_identity.is_empty());
    }

    #[test]
    fn sequencer_fifo_snapshot_barrier_includes_every_prior_event() {
        let mut sequencer = ProtocolSequencer::default();
        let first = sequencer.frame(SequencerControl::OrderedEvent(v1::HostEvent {
            kind: v1::EventKind::TopologyDirty.into(),
            ..Default::default()
        }));
        assert_eq!(first.sequence, 1);
        let barrier = sequencer.frame(SequencerControl::Response {
            request_id: 7,
            response: response_snapshot(v1::Snapshot::default()),
            snapshot_barrier: true,
        });
        let Some(Payload::Response(response)) = barrier.payload else {
            panic!("expected response");
        };
        assert_eq!(response.accepted_sequence, 1);
        let after = sequencer.frame(SequencerControl::OrderedEvent(v1::HostEvent::default()));
        assert_eq!(after.sequence, 2);
    }

    #[tokio::test]
    async fn mutation_generation_is_observed_only_after_topology_lock() {
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        let generation = Arc::new(AtomicU64::new(1));
        let guard = lock.lock().await;
        let waiter_lock = Arc::clone(&lock);
        let waiter_generation = Arc::clone(&generation);
        let waiter = tokio::spawn(async move {
            let (_guard, observed) =
                lock_topology_generation(&waiter_lock, &waiter_generation).await;
            observed
        });
        tokio::task::yield_now().await;
        generation.store(2, Ordering::Release);
        drop(guard);
        assert_eq!(waiter.await.unwrap(), 2);
    }

    #[tokio::test]
    async fn bulk_handshake_rejects_stale_server_identity_and_echoes_epoch() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(serve_with_shutdown(server, None));
        write_frame(
            &mut client,
            &envelope(
                1,
                0,
                Payload::ClientHello(v1::ClientHello {
                    desktop_version: "test".into(),
                    requested_capabilities: HOST_CAPABILITIES,
                    expected_helper_version: HELPER_VERSION.into(),
                    bulk_connection: true,
                    expected_server_identity: "definitely-stale".into(),
                    connection_epoch: 9_007_199_254_740_993,
                }),
            ),
        )
        .await
        .unwrap();
        let frame = read_frame(&mut client).await.unwrap().unwrap();
        let Some(Payload::ServerHello(hello)) = frame.payload else {
            panic!("expected hello")
        };
        assert!(hello.read_only);
        assert!(hello.incompatibility.contains("expected server identity"));
        assert_eq!(hello.connection_epoch, 9_007_199_254_740_993);
        drop(client);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn buffered_cancel_or_eof_before_first_poll_cannot_stage_a_file() {
        async fn exercise(eof: bool) {
            let root: PathBuf = std::env::current_dir()
                .unwrap()
                .join("tmp")
                .join(format!("phase5-cancel-race-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&root).unwrap();
            let git = |arguments: &[&str]| {
                let output = Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args(arguments)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                output
            };
            git(&["init", "-q"]);
            git(&["config", "user.name", "Cancel Race"]);
            git(&["config", "user.email", "cancel@example.test"]);
            fs::write(root.join("file"), b"must remain untracked\n").unwrap();
            let root_text = root.to_string_lossy().into_owned();
            let epoch = 9_001;
            let baseline_request = v1::GitRequest {
                root: root_text.clone(),
                root_token: filesystem::root_token(&root_text).unwrap(),
                expected_server_identity: server_identity(),
                ..Default::default()
            };
            let baseline = GitService::new().status(&baseline_request).unwrap();
            let mutation = v1::Request {
                operation: v1::Operation::GitMutation.into(),
                git: Some(v1::GitRequest {
                    operation_id: "cancel-race".into(),
                    repository_id: baseline.repository.unwrap().repository_id,
                    expected_status_generation: baseline.generation,
                    connection_epoch: epoch,
                    path: b"file".to_vec(),
                    mutation: v1::GitMutationKind::StageFile.into(),
                    ..baseline_request
                }),
                ..Default::default()
            };

            let (mut client, server) = UnixStream::pair().unwrap();
            let task = tokio::spawn(serve_with_shutdown(server, None));
            write_frame(
                &mut client,
                &envelope(
                    1,
                    0,
                    Payload::ClientHello(v1::ClientHello {
                        desktop_version: "cancel-race".into(),
                        requested_capabilities: HOST_CAPABILITIES,
                        expected_helper_version: HELPER_VERSION.into(),
                        connection_epoch: epoch,
                        ..Default::default()
                    }),
                ),
            )
            .await
            .unwrap();
            let _hello = read_frame(&mut client).await.unwrap().unwrap();
            let mut buffered =
                tmux_agent_protocol::encode_frame(&envelope(44, 0, Payload::Request(mutation)))
                    .unwrap();
            if !eof {
                buffered.extend_from_slice(
                    &tmux_agent_protocol::encode_frame(&envelope(
                        45,
                        0,
                        Payload::Cancel(v1::Cancel {
                            target_request_id: 44,
                        }),
                    ))
                    .unwrap(),
                );
            }
            client.write_all(&buffered).await.unwrap();
            if eof {
                client.shutdown().await.unwrap();
            } else {
                tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        let response = read_frame(&mut client).await.unwrap().unwrap();
                        if response.request_id == 44 {
                            break;
                        }
                    }
                })
                .await
                .unwrap();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(git(&["diff", "--cached", "--quiet"]).status.success());
            drop(client);
            task.await.unwrap().unwrap();
            fs::remove_dir_all(root).unwrap();
        }

        exercise(false).await;
        exercise(true).await;
    }

    #[tokio::test]
    async fn process_wide_file_events_preserve_bulk_commit_order_and_generation() {
        let (sender, mut receiver) = mpsc::channel(8);
        let _registration = register_control_event_sink(sender);
        for generation in [41_u64, 42] {
            broadcast_control_event(v1::HostEvent {
                kind: v1::EventKind::FileChanged.into(),
                scope: "note.md".into(),
                file: Some(v1::FileServiceEvent {
                    operation_id: format!("save-{generation}"),
                    metadata: Some(v1::FileMetadata {
                        path: "note.md".into(),
                        generation,
                        ..Default::default()
                    }),
                    state: "committed".into(),
                    ..Default::default()
                }),
                ..Default::default()
            });
        }
        let mut observed = Vec::new();
        for _ in 0..2 {
            let SequencerControl::OrderedEvent(event) = receiver.recv().await.unwrap() else {
                panic!("expected ordered file event")
            };
            let file = event.file.unwrap();
            observed.push((file.operation_id, file.metadata.unwrap().generation));
        }
        assert_eq!(observed, [("save-41".into(), 41), ("save-42".into(), 42)]);
    }

    #[tokio::test]
    async fn saturated_process_wide_sink_gets_resync_instead_of_silent_drop() {
        let (sender, mut receiver) = mpsc::channel(1);
        let _registration = register_control_event_sink(sender);
        broadcast_control_event(v1::HostEvent {
            kind: v1::EventKind::FileChanged.into(),
            scope: "first".into(),
            ..Default::default()
        });
        broadcast_control_event(v1::HostEvent {
            kind: v1::EventKind::FileChanged.into(),
            scope: "dropped".into(),
            ..Default::default()
        });
        assert!(
            matches!(receiver.recv().await, Some(SequencerControl::OrderedEvent(event)) if event.scope == "first")
        );
        let recovered = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap();
        assert!(
            matches!(recovered, Some(SequencerControl::InjectGap(event)) if event.kind == v1::EventKind::ResyncRequired as i32)
        );
    }

    #[test]
    fn control_event_registration_deregisters_explicitly_on_drop() {
        let (sender, _receiver) = mpsc::channel(1);
        let registration = register_control_event_sink(sender);
        let id = registration.id;
        assert!(
            CONTROL_EVENT_HUB
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .iter()
                .any(|sink| sink.id == id)
        );
        drop(registration);
        assert!(
            !CONTROL_EVENT_HUB
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .iter()
                .any(|sink| sink.id == id)
        );
    }

    #[test]
    fn external_mutation_before_poll_invalidates_cached_action_baseline() {
        let cached = (tmux_control::TmuxSnapshot::default(), "tmux:one".into());
        assert!(!baseline_changed(Some(&cached), &cached.0, &cached.1));
        let mut attachment_only = cached.0.clone();
        attachment_only.sessions.push(tmux_control::Session {
            id: "$1".into(),
            name: "stable".into(),
            window_count: 1,
            attached_clients: 2,
            order: 0,
        });
        let mut cached_with_session = cached.clone();
        cached_with_session.0.sessions.push(tmux_control::Session {
            attached_clients: 0,
            ..attachment_only.sessions[0].clone()
        });
        assert!(!baseline_changed(
            Some(&cached_with_session),
            &attachment_only,
            &cached.1,
        ));
        let mut externally_mutated = cached.0.clone();
        externally_mutated.sessions.push(tmux_control::Session {
            id: "$9".into(),
            name: "external".into(),
            window_count: 1,
            attached_clients: 0,
            order: 0,
        });
        assert!(baseline_changed(
            Some(&cached),
            &externally_mutated,
            &cached.1
        ));
        assert!(baseline_changed(Some(&cached), &cached.0, "tmux:restarted"));
    }
}
