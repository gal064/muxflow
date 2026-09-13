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
        let (_guard, observed) = lock_topology_generation(&waiter_lock, &waiter_generation).await;
        observed
    });
    tokio::task::yield_now().await;
    generation.store(2, Ordering::Release);
    drop(guard);
    assert_eq!(waiter.await.unwrap(), 2);
}

#[tokio::test]
async fn bulk_handshake_rejects_stale_server_identity_before_admission() {
    let (mut client, server) = UnixStream::pair().unwrap();
    let task = tokio::spawn(serve_with_shutdown(server, None));
    write_frame(
        &mut client,
        &envelope(
            1,
            0,
            Payload::ClientHello(v1::ClientHello {
                bulk_connection: true,
                expected_server_identity: "definitely-stale".into(),
                connection_epoch: 9_007_199_254_740_993,
            }),
        ),
    )
    .await
    .unwrap();
    let frame = read_frame(&mut client).await.unwrap().unwrap();
    let Some(Payload::Error(error)) = frame.payload else {
        panic!("expected identity refusal")
    };
    assert_eq!(error.code, "server_identity_mismatch");
    assert!(error.display_message.contains("expected server identity"));
    assert!(read_frame(&mut client).await.unwrap().is_none());
    assert!(task.await.unwrap().is_err());
}

/// Every connection leaves one line behind saying how it ended, and the
/// ordinary end — a desktop that closed its side — is the baseline the
/// abnormal ones are read against. Pin it here rather than letting a later
/// exit quietly claim it.
#[tokio::test]
async fn a_desktop_that_closes_its_side_ends_the_connection_as_a_client_eof() {
    let (mut client, server) = UnixStream::pair().unwrap();
    let activity = Arc::new(FrameActivity::started_now());
    let served = tokio::spawn({
        let activity = Arc::clone(&activity);
        async move { serve_connection(server, None, &activity).await }
    });
    write_frame(
        &mut client,
        &envelope(
            1,
            0,
            Payload::ClientHello(v1::ClientHello {
                connection_epoch: 5,
                ..Default::default()
            }),
        ),
    )
    .await
    .unwrap();
    let _hello = read_frame(&mut client).await.unwrap().unwrap();
    drop(client);
    let (outcome, reason) = served.await.unwrap();
    outcome.unwrap();
    assert_eq!(reason, ConnectionEndReason::ClientEof);
    // The handshake is a frame in each direction, so neither side is reported
    // as never having spoken, and the client spoke first.
    assert!(activity.since_last_client_frame() >= activity.since_last_host_frame());
    assert!(activity.lifetime() >= activity.since_last_client_frame());
}

/// A stalled ordered operation used to own the frame reader itself. That made
/// the acknowledgement below unreadable until the operation returned: every
/// request, pane and workspace behind it appeared frozen. The ordered worker
/// may stall; the frame reader must still consume acknowledgements immediately.
#[tokio::test]
async fn stalled_ordered_operation_does_not_block_the_frame_reader() {
    TEST_LAST_TERMINAL_ACK_EPOCH.store(0, Ordering::Release);
    let (mut client, server) = UnixStream::pair().unwrap();
    let task = tokio::spawn(serve_with_shutdown(server, None));
    write_frame(
        &mut client,
        &envelope(
            1,
            0,
            Payload::ClientHello(v1::ClientHello {
                connection_epoch: 77,
                ..Default::default()
            }),
        ),
    )
    .await
    .unwrap();
    let _hello = read_frame(&mut client).await.unwrap().unwrap();
    write_frame(
        &mut client,
        &envelope(
            2,
            0,
            Payload::Request(v1::Request {
                operation: v1::Operation::TestDelay.into(),
                scope: "stall-ordered-lane".into(),
                ..Default::default()
            }),
        ),
    )
    .await
    .unwrap();
    // No terminal record was ever reserved, so acknowledging one is invalid.
    // Processing it proves the reader is alive independently of ordered work.
    write_frame(
        &mut client,
        &envelope(
            0,
            0,
            Payload::TerminalOutputAck(v1::TerminalOutputAck {
                connection_epoch: 77,
                cumulative_bytes: 1,
                cumulative_records: 1,
            }),
        ),
    )
    .await
    .unwrap();

    tokio::time::timeout(Duration::from_millis(500), async {
        while TEST_LAST_TERMINAL_ACK_EPOCH.load(Ordering::Acquire) != 77 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("frame reader waited for the deliberately stalled ordered lane");

    drop(client);
    assert!(task.await.unwrap().is_err());
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
        let baseline = GitService::new(Arc::new(AtomicBool::new(false)), 0)
            .status(&baseline_request, None)
            .await
            .unwrap();
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

/// The incident this guards: a client whose transport died leaves the socket
/// half-closed — request stream ended, read side still open — and with a live
/// Git watch the teardown could never finish, because the watch subscriber's
/// sender clone was only released by the `GitService` drop that runs *after*
/// the writer await. The connection task, its socket, and the remote bridge
/// process then survived their client by days.
#[tokio::test]
async fn half_closed_client_with_live_git_watch_completes_teardown() {
    let root: PathBuf = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("half-close-teardown-{}", uuid::Uuid::new_v4()));
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
    };
    git(&["init", "-q"]);
    git(&["config", "user.name", "Half Close"]);
    git(&["config", "user.email", "half-close@example.test"]);
    let root_text = root.to_string_lossy().into_owned();

    let (mut client, server) = UnixStream::pair().unwrap();
    let task = tokio::spawn(serve_with_shutdown(server, None));
    write_frame(
        &mut client,
        &envelope(
            1,
            0,
            Payload::ClientHello(v1::ClientHello {
                ..Default::default()
            }),
        ),
    )
    .await
    .unwrap();
    let _hello = read_frame(&mut client).await.unwrap().unwrap();
    write_frame(
        &mut client,
        &envelope(
            7,
            0,
            Payload::Request(v1::Request {
                operation: v1::Operation::WatchGit.into(),
                git: Some(v1::GitRequest {
                    watch_id: "half-close-watch".into(),
                    root: root_text.clone(),
                    root_token: filesystem::root_token(&root_text).unwrap(),
                    expected_server_identity: server_identity(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        ),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let frame = read_frame(&mut client).await.unwrap().unwrap();
            if frame.request_id == 7 {
                break;
            }
        }
    })
    .await
    .expect("git watch bootstrap must answer");

    // Half-close: the request stream ends, but the client — like a bridge
    // whose ssh transport died — keeps the read direction open.
    client.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .expect("teardown must complete for a half-closed client")
        .unwrap()
        .unwrap();
    drop(client);
    fs::remove_dir_all(root).unwrap();
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
        pinned: false,
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
        pinned: false,
    });
    assert!(baseline_changed(
        Some(&cached),
        &externally_mutated,
        &cached.1
    ));
    assert!(baseline_changed(Some(&cached), &cached.0, "tmux:restarted"));
}

/// Each section is reported in order, so the coarsest true statement is the
/// one the timing log carries.
#[test]
fn a_refreshed_baseline_names_what_moved() {
    let pane = |width: u16, window_id: &str| tmux_control::Pane {
        id: "%1".into(),
        session_id: "$1".into(),
        window_id: window_id.into(),
        index: 0,
        active: true,
        width,
        height: 24,
        left: 0,
        top: 0,
        current_path: String::new(),
        current_command: "bash".into(),
        pane_pid: 0,
        start_command: String::new(),
    };
    let base = tmux_control::TmuxSnapshot {
        sessions: vec![tmux_control::Session {
            id: "$1".into(),
            name: "one".into(),
            window_count: 1,
            attached_clients: 0,
            order: 0,
            pinned: false,
        }],
        windows: vec![tmux_control::Window {
            id: "@1".into(),
            session_id: "$1".into(),
            index: 0,
            name: "shell".into(),
            active: true,
            layout: "layout".into(),
            zoomed: false,
            pinned: false,
        }],
        panes: vec![pane(80, "@1")],
    };
    let cached = (base.clone(), "tmux:live".to_owned());
    assert_eq!(
        action_topology_diff(None, &base, "tmux:live"),
        "baseline:absent"
    );
    assert_eq!(
        action_topology_diff(Some(&cached), &base, "tmux:restarted"),
        "identity"
    );
    assert_eq!(
        action_topology_diff(Some(&cached), &base, "tmux:live"),
        "none"
    );

    let mut resized = base.clone();
    resized.panes[0].width = 120;
    assert_eq!(
        action_topology_diff(Some(&cached), &resized, "tmux:live"),
        "panes:geometry"
    );

    let mut moved = base.clone();
    moved.panes[0].window_id = "@2".into();
    assert_eq!(
        action_topology_diff(Some(&cached), &moved, "tmux:live"),
        "panes:membership"
    );

    let mut windows = base.clone();
    windows.windows[0].zoomed = true;
    assert_eq!(
        action_topology_diff(Some(&cached), &windows, "tmux:live"),
        "windows"
    );

    let mut sessions = base.clone();
    sessions.sessions[0].window_count = 2;
    assert_eq!(
        action_topology_diff(Some(&cached), &sessions, "tmux:live"),
        "sessions"
    );
}

/// The counterpart to `saturated_process_wide_sink_gets_resync_instead_of_silent_drop`.
///
/// A resync is the right answer for an ordinary event the queue could not take:
/// the desktop rebuilds from scratch and nothing is lost. It is the wrong answer
/// for the events that exist *because* something already needs repairing — a
/// dropped "this pane needs a seed" is how one pane stays frozen while the rest
/// of the connection carries on looking healthy. Those wait for room instead.
#[tokio::test]
async fn a_full_queue_defers_a_recovery_event_rather_than_dropping_it() {
    let (sender, mut receiver) = mpsc::channel(1);
    let overflowed = AtomicBool::new(false);
    let topology = || v1::HostEvent {
        kind: v1::EventKind::TopologyDirty.into(),
        scope: "topology".into(),
        ..Default::default()
    };
    assert!(emit_event(&sender, &overflowed, topology()));

    // Ordinary events keep the old contract exactly: refused, counted, resynced.
    assert!(!emit_event(&sender, &overflowed, topology()));
    assert!(overflowed.load(Ordering::Acquire));
    overflowed.store(false, Ordering::Release);

    for kind in [
        v1::EventKind::TerminalResnapshotRequired,
        v1::EventKind::TerminalFlowStalled,
        v1::EventKind::TerminalFlowPaused,
        v1::EventKind::PaneResource,
    ] {
        assert!(
            emit_event(
                &sender,
                &overflowed,
                v1::HostEvent {
                    kind: kind.into(),
                    scope: "%7".into(),
                    detail: "pane needs a seed".into(),
                    ..Default::default()
                }
            ),
            "{kind:?} was reported as undeliverable"
        );
    }
    assert!(
        !overflowed.load(Ordering::Acquire),
        "a deferred recovery event escalated to a connection-wide resync"
    );

    let mut delivered = Vec::new();
    for _ in 0..5 {
        let message = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await
            .expect("a deferred recovery event never arrived")
            .unwrap();
        let SequencerControl::OrderedEvent(event) = message else {
            panic!("expected an ordered event")
        };
        delivered.push(v1::EventKind::try_from(event.kind).unwrap());
    }
    delivered.sort_by_key(|kind| *kind as i32);
    assert_eq!(
        delivered,
        {
            let mut expected = vec![
                v1::EventKind::TopologyDirty,
                v1::EventKind::TerminalResnapshotRequired,
                v1::EventKind::TerminalFlowStalled,
                v1::EventKind::TerminalFlowPaused,
                v1::EventKind::PaneResource,
            ];
            expected.sort_by_key(|kind| *kind as i32);
            expected
        },
        "a recovery event was lost"
    );
}

#[tokio::test]
async fn a_different_contract_is_closed_before_pipelined_requests_are_dispatched() {
    let (mut client, server) = UnixStream::pair().unwrap();
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(serve_with_shutdown(server, Some(shutdown_tx)));
    let mut hello = envelope(1, 0, Payload::ClientHello(v1::ClientHello::default()));
    hello.protocol_major = PROTOCOL_MAJOR - 1;
    write_frame(&mut client, &hello).await.unwrap();
    write_frame(
        &mut client,
        &envelope(
            2,
            0,
            Payload::Request(v1::Request {
                operation: v1::Operation::ShutdownDaemon.into(),
                ..Default::default()
            }),
        ),
    )
    .await
    .unwrap();
    let response = read_frame(&mut client).await.unwrap().unwrap();
    let Some(Payload::Error(error)) = response.payload else {
        panic!("expected contract refusal");
    };
    assert_eq!(error.code, "protocol_incompatible");
    assert!(task.await.unwrap().is_err());
    assert!(shutdown_rx.try_recv().is_err());
}
