use super::*;

#[test]
fn incompatible_or_disconnected_client_rejects_mutation_without_queueing() {
    let client = TerminalClient::new();
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
fn invalid_same_epoch_delivery_ack_fails_closed_instead_of_retrying_the_ledger() {
    let client = TerminalClient::new();
    client.ready.store(true, Ordering::Release);
    let window = DeliveryWindow::new(17);
    window
        .reserve(100, HostCharge::terminal(80))
        .unwrap()
        .commit()
        .unwrap();
    *client.pending_delivery_ack.lock().unwrap() = Some((16, HostCharge::terminal(20)));
    *client.delivery_window.lock().unwrap() = Some(Arc::clone(&window));
    assert!(client.acknowledge_delivery(17, 1, 99).is_err());
    assert!(!client.ready.load(Ordering::Acquire));
    assert!(client.delivery_window.lock().unwrap().is_none());
    assert!(client.pending_delivery_ack.lock().unwrap().is_none());
    assert!(window.reserve(1, HostCharge::default()).is_err());
}

#[test]
fn disconnected_input_is_rejected_and_reconnect_starts_a_fresh_epoch() {
    let client = Arc::new(TerminalClient::new());
    let (sender, receiver) = mpsc::sync_channel(1);
    client.input_queue.lock().unwrap().sender = Some(sender);

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
    let client = Arc::new(TerminalClient::new());
    let (sender, receiver) = mpsc::sync_channel(1);
    client.input_queue.lock().unwrap().sender = Some(sender);
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

#[test]
fn oversized_pane_resource_crosses_native_delivery_and_releases_exact_credit() {
    let epoch = 73;
    let window = DeliveryWindow::new(epoch);
    let shared_window = Arc::new(Mutex::new(Some(Arc::clone(&window))));
    let (sender, receiver) = mpsc::channel();
    let channel = Channel::new(move |body| {
        if let InvokeResponseBody::Raw(frame) = body {
            sender.send(frame).unwrap();
        }
        Ok(())
    });
    let channel = TerminalEventChannel::new(Uuid::new_v4(), channel, shared_window);
    let snapshot_bytes = delivery_window::NATIVE_DELIVERY_WINDOW_BYTES as usize + 1_024;
    let event = TerminalEvent::PaneResource {
        pane_id: "%1".into(),
        state: "hiddenBuffered".into(),
        requires_seed: true,
        recovery_reason: "oversized-recovery".into(),
        generation: 9,
        snapshot_generation: 8,
        tail_through_generation: 9,
        serialized_snapshot: vec![0x5a; snapshot_bytes],
        raw_tail: vec![0xa5; 1_024],
    };
    let frame = event_frame::encode_event_with_sequence(event, 41);
    assert!(frame.len() as u64 > delivery_window::NATIVE_DELIVERY_WINDOW_BYTES);
    let host = HostCharge {
        bytes: (snapshot_bytes + 1_024) as u64,
        records: 1,
    };
    channel.send_charged(frame.clone(), host).unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        frame
    );
    assert_eq!(
        window.acknowledge(epoch, 1, frame.len() as u64).unwrap(),
        Some(host)
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
    let payload: serde_json::Value = serde_json::from_slice(&frame[sequence_offset + 8..]).unwrap();
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
    let client = TerminalClient::new();
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
        terminal_visibility_request("%1".into(), false, b"snapshot".to_vec(), 7, 42, 7).unwrap();
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
fn incompatible_server_hello_enters_read_only_and_quarantines_its_snapshot() {
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

#[test]
fn startup_rollback_releases_both_dispatch_workers() {
    let client = Arc::new(TerminalClient::new());
    client.start_dispatchers("rollback-test").unwrap();
    let weak = Arc::downgrade(&client);

    client.shutdown_transport("startup rolled back");
    drop(client);

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while weak.upgrade().is_some() && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        weak.upgrade().is_none(),
        "dispatcher workers retained client"
    );
}

#[test]
fn full_input_channel_does_not_block_shutdown_while_resize_flush_waits() {
    let client = Arc::new(TerminalClient::new());
    let (dispatcher, receiver) = mpsc::sync_channel(1);
    dispatcher
        .send(ClientInputDispatch::Bytes {
            pane_id: "%1".into(),
            data: vec![1],
            epoch: 0,
        })
        .unwrap();
    client.input_queue.lock().unwrap().sender = Some(dispatcher);
    let _resize = client.enqueue_resize(100, 30).unwrap();
    let flush_client = Arc::clone(&client);
    let flush = thread::spawn(move || flush_client.flush_input());
    thread::sleep(Duration::from_millis(20));

    let started = std::time::Instant::now();
    client.shutdown_transport("shutdown during resize flush");
    assert!(started.elapsed() < Duration::from_millis(200));

    drop(receiver);
    assert!(flush.join().unwrap().is_err());
}

#[test]
fn input_flush_reports_the_first_failed_write() {
    let client = Arc::new(TerminalClient::new());
    let (sender, receiver) = mpsc::sync_channel(8);
    client.input_queue.lock().unwrap().sender = Some(sender.clone());
    mark_input_reconnected(&client);
    client.ready.store(true, Ordering::Release);
    let worker_client = Arc::clone(&client);
    let worker = thread::spawn(move || run_client_input_dispatch(worker_client, receiver));

    client
        .enqueue_input("%1".into(), b"accepted".to_vec())
        .unwrap();
    let error = client.flush_input().unwrap_err();
    assert!(error.contains("host bridge is disconnected"), "{error}");

    sender.send(ClientInputDispatch::Stop).unwrap();
    worker.join().unwrap();
}

#[test]
fn abandoned_input_barrier_cannot_consume_a_failed_write() {
    let client = Arc::new(TerminalClient::new());
    let (sender, receiver) = mpsc::sync_channel(8);
    let worker_client = Arc::clone(&client);
    let worker = thread::spawn(move || run_client_input_dispatch(worker_client, receiver));
    sender
        .send(ClientInputDispatch::Bytes {
            pane_id: "%1".into(),
            data: b"accepted".to_vec(),
            epoch: 0,
        })
        .unwrap();
    let (abandoned_tx, abandoned_rx) = mpsc::sync_channel(1);
    drop(abandoned_rx);
    sender
        .send(ClientInputDispatch::Barrier(abandoned_tx))
        .unwrap();
    let (live_tx, live_rx) = mpsc::sync_channel(1);
    sender.send(ClientInputDispatch::Barrier(live_tx)).unwrap();
    let error = live_rx.recv().unwrap().unwrap_err();
    assert!(error.contains("disconnected connection"), "{error}");

    let (clean_tx, clean_rx) = mpsc::sync_channel(1);
    sender.send(ClientInputDispatch::Barrier(clean_tx)).unwrap();
    assert_eq!(clean_rx.recv().unwrap(), Ok(()));
    sender.send(ClientInputDispatch::Stop).unwrap();
    worker.join().unwrap();
}

#[test]
fn input_flush_reports_bytes_accepted_by_a_replaced_connection() {
    let client = Arc::new(TerminalClient::new());
    let (sender, receiver) = mpsc::sync_channel(8);
    client.input_queue.lock().unwrap().sender = Some(sender.clone());
    client.ready.store(true, Ordering::Release);

    client
        .enqueue_input("%1".into(), b"accepted-before-reconnect".to_vec())
        .unwrap();
    mark_input_reconnected(&client);
    let worker_client = Arc::clone(&client);
    let worker = thread::spawn(move || run_client_input_dispatch(worker_client, receiver));

    let error = client.flush_input().unwrap_err();
    assert!(error.contains("replaced connection"), "{error}");

    sender.send(ClientInputDispatch::Stop).unwrap();
    worker.join().unwrap();
}

/// A helper that cannot serve a single-request file open is refused at the
/// handshake, rather than admitted and found wanting one operation at a time.
#[test]
fn a_helper_missing_the_file_stream_capability_is_refused_at_the_handshake() {
    use tmux_agent_protocol::{CAP_FILE_STREAM, HOST_CAPABILITIES, PROTOCOL_MAJOR};
    let hello = |capabilities: u64| v1::ServerHello {
        capabilities,
        read_only: false,
        ..Default::default()
    };
    assert!(super::bridge::handshake_allows_snapshot(
        PROTOCOL_MAJOR,
        &hello(HOST_CAPABILITIES)
    ));
    assert!(
        !super::bridge::handshake_allows_snapshot(
            PROTOCOL_MAJOR,
            &hello(HOST_CAPABILITIES & !CAP_FILE_STREAM)
        ),
        "an older helper was admitted and would fail every file open"
    );
}
