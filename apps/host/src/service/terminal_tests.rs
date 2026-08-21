use super::*;

fn long_lived_attachment_command() -> std::process::Command {
    let mut command = std::process::Command::new("sh");
    command.args(["-c", "exec sleep 30", "terminal-attachment-startup-fixture"]);
    command
}

fn start_long_lived_attachment(
    event_tx: mpsc::Sender<SequencerControl>,
    resources: Arc<Mutex<PaneResourceStore>>,
    generation: Arc<AtomicU64>,
    output_credit: Arc<OutputCredit>,
    emission_order: Arc<Mutex<()>>,
) -> anyhow::Result<TerminalAttachment> {
    let overflowed = Arc::new(AtomicBool::new(false));
    let clipboard = Arc::new(ClipboardNotificationSender::start(
        event_tx.clone(),
        Arc::clone(&overflowed),
    ));
    TerminalAttachment::start_with_command(
        "$1",
        &["%1".into()],
        AttachmentRuntime {
            event_tx,
            overflowed,
            resources,
            terminal_generation: generation,
            output_credit,
            emission_order,
            topology_trigger: TopologyOutputTrigger::default(),
            clipboard,
        },
        long_lived_attachment_command(),
    )
}

#[test]
fn every_attachment_worker_spawn_failure_reaps_the_child_and_allows_retry() {
    use super::startup::{assert_last_startup_child_reaped, with_worker_spawn_failure};

    for stage in [1, 2] {
        let (events, _receiver) = mpsc::channel(8);
        let result = with_worker_spawn_failure(stage, || {
            start_long_lived_attachment(
                events,
                Arc::new(Mutex::new(PaneResourceStore::with_total_limit(
                    4, 1024, 4096,
                ))),
                Arc::new(AtomicU64::new(0)),
                Arc::new(OutputCredit::negotiated(false)),
                Arc::new(Mutex::new(())),
            )
        });
        assert!(result.is_err(), "worker stage {stage} unexpectedly started");
        assert_last_startup_child_reaped();
    }

    let (events, _receiver) = mpsc::channel(8);
    let mut retry = start_long_lived_attachment(
        events,
        Arc::new(Mutex::new(PaneResourceStore::with_total_limit(
            4, 1024, 4096,
        ))),
        Arc::new(AtomicU64::new(0)),
        Arc::new(OutputCredit::negotiated(false)),
        Arc::new(Mutex::new(())),
    )
    .unwrap();
    retry.stop();
}

/// The per-session detach path: `stop()` and drop one attachment while the
/// shared credit stays open for the rest of the connection. Without the wakeup
/// in `stop()` the waiter parks forever and `Drop`'s `join_workers` hangs the
/// service thread.
#[test]
fn stopping_one_attachment_unparks_its_credit_waiter_before_the_join() {
    let output_credit = Arc::new(OutputCredit::negotiated(true));
    output_credit
        .admit(
            OutputCharge::terminal(OUTPUT_WINDOW_BYTES as usize),
            &AtomicBool::new(false),
        )
        .unwrap()
        .commit();
    let (events, _receiver) = mpsc::channel(8);
    let mut attachment = start_long_lived_attachment(
        events,
        Arc::new(Mutex::new(PaneResourceStore::with_total_limit(
            4, 1024, 4096,
        ))),
        Arc::new(AtomicU64::new(0)),
        Arc::clone(&output_credit),
        Arc::new(Mutex::new(())),
    )
    .unwrap();
    let stopped = Arc::clone(&attachment.stopped);
    let credit = Arc::clone(&output_credit);
    let (sender, receiver) = std_mpsc::channel();
    std::thread::spawn(move || {
        credit.await_window(&stopped);
        sender.send(()).unwrap();
    });
    assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
    attachment.stop();
    drop(attachment);
    receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    // The rest of the connection still holds a live, un-closed window.
    output_credit
        .acknowledge(OutputCharge {
            bytes: 1,
            records: 1,
        })
        .unwrap();
}

/// The fence's whole purpose, pinned at the one point a reveal can still be
/// held up: the ordered event queue. The recovery event must reach the queue
/// before output that has already observed the new visibility, however long
/// the queue makes the reveal wait for a slot.
#[test]
fn stalled_reveal_recovery_is_admitted_before_concurrent_visible_output() {
    let output_credit = Arc::new(OutputCredit::negotiated(true));
    let mut clients =
        TerminalClients::new(Arc::clone(&output_credit), TopologyOutputTrigger::default());
    let pane_id = "%1".to_owned();
    let session_id = "$1".to_owned();
    let generation = Arc::clone(&clients.generation);
    generation.store(1, Ordering::Release);
    {
        let mut resources = clients.resources.lock().unwrap();
        resources.set_visible(&pane_id, true, 0);
        resources
            .hide_with_checkpoint(
                &pane_id,
                vec![b'S'],
                VisibilityCheckpoint {
                    epoch: 1,
                    generation: 0,
                },
                1,
            )
            .unwrap();
    }
    // One slot, already taken: the reveal reaches its ordered send holding the
    // fence and stays there until the queue drains, which is the interleaving
    // this test needs and the only one the fence still has to survive.
    let (events, mut receiver) = mpsc::channel(1);
    events
        .try_send(SequencerControl::OrderedEvent(v1::HostEvent {
            kind: v1::EventKind::TopologyDirty.into(),
            scope: "topology".into(),
            ..Default::default()
        }))
        .unwrap();
    let attachment = start_long_lived_attachment(
        events.clone(),
        Arc::clone(&clients.resources),
        Arc::clone(&clients.generation),
        Arc::clone(&output_credit),
        Arc::clone(&clients.emission_order),
    )
    .unwrap();
    clients.clients.insert(session_id, attachment);

    let resources = Arc::clone(&clients.resources);
    let emission_order = Arc::clone(&clients.emission_order);
    let stopped = Arc::new(AtomicBool::new(false));
    let overflowed = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(Mutex::new(clients));
    let reveal_clients = Arc::clone(&shared);
    let reveal_events = events.clone();
    let reveal_overflowed = Arc::clone(&overflowed);
    let reveal = std::thread::spawn(move || {
        reveal_clients.lock().unwrap().set_visibility(
            "%1",
            VisibilityChange {
                visible: true,
                serialized_snapshot: Vec::new(),
                checkpoint: VisibilityCheckpoint {
                    epoch: 1,
                    generation: 1,
                },
            },
            &reveal_events,
            &reveal_overflowed,
        )
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if resources
            .lock()
            .unwrap()
            .get("%1")
            .is_some_and(|resource| resource.state == StoredResourceState::Visible)
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "reveal never reached its ordered send"
        );
        std::thread::yield_now();
    }

    let output_events = events.clone();
    let output_overflowed = Arc::clone(&overflowed);
    let output_resources = Arc::clone(&resources);
    let output_generation = Arc::clone(&generation);
    let output_credit_clone = Arc::clone(&output_credit);
    let output = std::thread::spawn(move || {
        TestOutputEmission {
            sender: &output_events,
            overflowed: &output_overflowed,
            resources: &output_resources,
            terminal_generation: &output_generation,
            stopped: &stopped,
            output_credit: &output_credit_clone,
            emission_order: &emission_order,
            topology_trigger: &TopologyOutputTrigger::default(),
            read_started: std::time::Instant::now(),
        }
        .record("%1".into(), vec![b'O']);
    });
    // Freeing the slot releases the reveal's send, and only then can the output
    // thread take the fence at all.
    let SequencerControl::OrderedEvent(filler) = receiver.blocking_recv().unwrap() else {
        panic!("queue did not start with the filler event");
    };
    assert_eq!(
        v1::EventKind::try_from(filler.kind).unwrap(),
        v1::EventKind::TopologyDirty
    );

    let SequencerControl::OrderedEvent(recovery) = receiver.blocking_recv().unwrap() else {
        panic!("reveal did not emit ordered recovery");
    };
    let SequencerControl::OrderedEvent(output_event) = receiver.blocking_recv().unwrap() else {
        panic!("visible output was not ordered after recovery");
    };
    reveal.join().unwrap().unwrap();
    output.join().unwrap();
    assert_eq!(
        v1::EventKind::try_from(recovery.kind).unwrap(),
        v1::EventKind::PaneResource
    );
    assert_eq!(
        recovery.pane_resource.unwrap().serialized_snapshot,
        vec![b'S']
    );
    assert_eq!(
        v1::EventKind::try_from(output_event.kind).unwrap(),
        v1::EventKind::TerminalOutput
    );
    assert_eq!(output_event.terminal.unwrap().data, vec![b'O']);
    assert!(
        receiver.try_recv().is_err(),
        "recovery/output was emitted more than once"
    );
    drop(shared);
}

/// Next ordered event, or a named failure — never an unbounded wait, so a
/// regression reports the step it stalled at instead of hanging the suite.
fn ordered_event_within(
    receiver: &mut mpsc::Receiver<SequencerControl>,
    timeout: Duration,
    expectation: &str,
) -> v1::HostEvent {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(message) = receiver.try_recv() {
            let SequencerControl::OrderedEvent(event) = message else {
                panic!("expected an ordered event: {expectation}");
            };
            return event;
        }
        assert!(std::time::Instant::now() < deadline, "{expectation}");
        std::thread::yield_now();
    }
}

/// The production wedge, rebuilt: a full delivery window, a control reader
/// already parked waiting for it, and a visibility transition arriving on the
/// connection's inline dispatch — the very thread that has to read the
/// acknowledgement the window is waiting for. Nothing here ever acknowledges,
/// and the transition must still finish. If either the reader's wait moves back
/// under the emission fence or `set_visibility` waits for credit again, this
/// test hangs exactly the way the daemon did.
#[test]
fn a_full_window_and_a_parked_reader_cannot_wedge_a_visibility_transition() {
    let output_credit = Arc::new(OutputCredit::negotiated(true));
    output_credit
        .admit(
            OutputCharge::terminal(OUTPUT_WINDOW_BYTES as usize),
            &AtomicBool::new(false),
        )
        .unwrap()
        .commit();
    let mut clients =
        TerminalClients::new(Arc::clone(&output_credit), TopologyOutputTrigger::default());
    clients.generation.store(1, Ordering::Release);
    clients.resources.lock().unwrap().ensure("%1", true, 0);
    let (events, mut receiver) = mpsc::channel(8);
    let attachment = start_long_lived_attachment(
        events.clone(),
        Arc::clone(&clients.resources),
        Arc::clone(&clients.generation),
        Arc::clone(&output_credit),
        Arc::clone(&clients.emission_order),
    )
    .unwrap();
    clients.clients.insert("$1".into(), attachment);

    let reader_events = events.clone();
    let reader_overflowed = Arc::new(AtomicBool::new(false));
    let reader_resources = Arc::clone(&clients.resources);
    let reader_generation = Arc::clone(&clients.generation);
    let reader_credit = Arc::clone(&output_credit);
    let emission_order = Arc::clone(&clients.emission_order);
    let reader_emission_order = Arc::clone(&clients.emission_order);
    let reader_stopped = Arc::new(AtomicBool::new(false));
    let parked_reader_stopped = Arc::clone(&reader_stopped);
    let reader = std::thread::spawn(move || {
        TestOutputEmission {
            sender: &reader_events,
            overflowed: &reader_overflowed,
            resources: &reader_resources,
            terminal_generation: &reader_generation,
            stopped: &parked_reader_stopped,
            output_credit: &reader_credit,
            emission_order: &reader_emission_order,
            topology_trigger: &TopologyOutputTrigger::default(),
            read_started: std::time::Instant::now(),
        }
        .record("%1".into(), vec![b'O']);
    });
    // The reader's record is on the queue, so it has released the fence and is
    // now paying for it in `await_window` — where it will stay for the rest of
    // the test, because no acknowledgement is ever sent.
    let output_event = ordered_event_within(
        &mut receiver,
        Duration::from_secs(5),
        "reader never admitted its output on a full window",
    );
    assert_eq!(
        v1::EventKind::try_from(output_event.kind).unwrap(),
        v1::EventKind::TerminalOutput
    );
    // The invariant itself, before anything else depends on it: a reader that
    // is waiting for delivery credit holds no emission fence.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while emission_order.try_lock().is_err() {
        assert!(
            std::time::Instant::now() < deadline,
            "a reader waiting for delivery credit kept the emission fence"
        );
        std::thread::yield_now();
    }

    let (done, finished) = std_mpsc::channel();
    let visibility_events = events.clone();
    std::thread::spawn(move || {
        let result = clients.set_visibility(
            "%1",
            VisibilityChange {
                visible: false,
                serialized_snapshot: vec![b'S'],
                checkpoint: VisibilityCheckpoint {
                    epoch: 1,
                    generation: 1,
                },
            },
            &visibility_events,
            &AtomicBool::new(false),
        );
        done.send(result.is_ok()).unwrap();
        clients
    });
    assert!(
        finished.recv_timeout(Duration::from_secs(5)).unwrap(),
        "visibility transition failed on a full delivery window"
    );
    let recovery = ordered_event_within(
        &mut receiver,
        Duration::from_secs(5),
        "hide did not emit its recovery event",
    );
    assert_eq!(
        v1::EventKind::try_from(recovery.kind).unwrap(),
        v1::EventKind::PaneResource
    );

    reader_stopped.store(true, Ordering::Release);
    output_credit.wake_waiters();
    reader.join().unwrap();
}

#[test]
fn failed_visibility_admission_invalidates_the_speculative_transition() {
    for close_credit in [true, false] {
        let output_credit = Arc::new(OutputCredit::negotiated(close_credit));
        let mut clients =
            TerminalClients::new(Arc::clone(&output_credit), TopologyOutputTrigger::default());
        clients.generation.store(1, Ordering::Release);
        clients.resources.lock().unwrap().ensure("%1", true, 0);
        clients
            .resources
            .lock()
            .unwrap()
            .hide_with_checkpoint(
                "%1",
                vec![b'S'],
                VisibilityCheckpoint {
                    epoch: 1,
                    generation: 0,
                },
                1,
            )
            .unwrap();
        let (events, receiver) = mpsc::channel(1);
        let attachment = start_long_lived_attachment(
            events.clone(),
            Arc::clone(&clients.resources),
            Arc::clone(&clients.generation),
            Arc::clone(&output_credit),
            Arc::clone(&clients.emission_order),
        )
        .unwrap();
        clients.clients.insert("$1".into(), attachment);
        if close_credit {
            output_credit.close();
        } else {
            drop(receiver);
        }

        assert!(
            clients
                .set_visibility(
                    "%1",
                    VisibilityChange {
                        visible: true,
                        serialized_snapshot: Vec::new(),
                        checkpoint: VisibilityCheckpoint {
                            epoch: 1,
                            generation: 1,
                        },
                    },
                    &events,
                    &AtomicBool::new(false),
                )
                .is_err()
        );
        let resources = clients.resources.lock().unwrap();
        let resource = resources.get("%1").unwrap();
        assert_eq!(resource.state, StoredResourceState::Released);
        assert!(resource.requires_seed);
        assert!(resource.serialized_snapshot.is_empty());
        assert!(resource.raw_tail.is_empty());
    }
}

#[test]
fn fresh_server_has_a_vacuous_input_fence_for_create_session_bootstrap() {
    let mut clients = TerminalClients::new(
        Arc::new(OutputCredit::negotiated(false)),
        TopologyOutputTrigger::default(),
    );
    assert!(clients.clients.is_empty());
    assert!(clients.input.is_none());
    clients.flush_input().unwrap();
}

/// The bound is a blast radius, not the fix for P12-U006: the sizes that
/// actually damaged the user's windows (108x298, 108x314) are *inside* it,
/// and what stops those is the desktop no longer deriving the client size
/// from a pane's share of the topology. What this guarantees is that a
/// future computation can only be wrong by a bounded amount, loudly.
#[test]
fn client_resize_refuses_sizes_no_display_has_and_names_them() {
    for (columns, rows) in [(80, 501), (501, 24), (2000, 2000), (80, 1), (1, 24), (0, 0)] {
        let error = check_client_size(columns, rows)
            .expect_err(&format!("{columns}x{rows} must never reach refresh-client"))
            .to_string();
        assert!(
            error.contains(&format!("{columns}x{rows}")),
            "rejection must name the size it refused, got {error}"
        );
        assert!(error.contains("between 2 and 500 cells"), "got {error}");
    }
    for (columns, rows) in [(2, 2), (188, 51), (239, 57), (500, 500)] {
        check_client_size(columns, rows).unwrap();
    }
}

/// tmux's lexer rejects `refresh-client -A %5:continue`, and a rejected
/// resume leaves the pane paused for the rest of the session (P12-U001).
/// The assertion is byte-exact because the quoting *is* the fix.
#[test]
fn resume_command_quotes_the_pause_argument_tmux_lexer_rejects() {
    let sink = Arc::new(Mutex::new(Vec::new()));
    write_capture_request_resuming(&sink, "%5", true).unwrap();
    let written = String::from_utf8(sink.lock().unwrap().clone()).unwrap();
    let lines: Vec<_> = written.lines().collect();
    assert_eq!(lines[0], "display-message -p '__ADE_RESUME__:5'");
    assert_eq!(lines[1], "refresh-client -A '%5:continue'");
    // tmux drops output produced while a pane is paused rather than
    // replaying it, so the capture that shares this lock hold is what
    // actually recovers the screen. The resume alone would leave a hole.
    assert_eq!(lines[2], "display-message -p '__ADE_CAPTURE__:5'");
    assert!(lines[3].starts_with("capture-pane -p -e -J -S -2000 -t %5"));

    let sink = Arc::new(Mutex::new(Vec::new()));
    write_capture_request_resuming(&sink, "%5", false).unwrap();
    let written = String::from_utf8(sink.lock().unwrap().clone()).unwrap();
    assert!(!written.contains("refresh-client"));
    assert!(!written.contains("__ADE_RESUME__"));
}

#[test]
fn membership_changes_are_in_place_and_deterministic() {
    let current = HashSet::from(["%3".into(), "%1".into()]);
    let desired = HashSet::from(["%2".into(), "%3".into()]);
    assert_eq!(
        membership_delta(&current, &desired),
        (vec!["%2".into()], vec!["%1".into()])
    );
    assert_eq!(
        membership_delta(&current, &current),
        (Vec::<String>::new(), Vec::<String>::new()),
        "an exact no-op must not reach the membership marker write"
    );
}

#[test]
fn exact_membership_noop_emits_nothing_and_delta_emits_one_batch() {
    let mut current = HashSet::from(["%1".into()]);
    let (stream_tx, stream_rx) = std_mpsc::channel();
    let mut stdin = Vec::new();

    let unchanged = current.clone();
    assert!(
        apply_membership_update(&mut current, &unchanged, &stream_tx, &mut stdin)
            .unwrap()
            .is_empty()
    );
    assert!(stdin.is_empty());
    assert!(matches!(
        stream_rx.try_recv(),
        Err(std_mpsc::TryRecvError::Empty)
    ));

    let desired = HashSet::from(["%2".into()]);
    assert_eq!(
        apply_membership_update(&mut current, &desired, &stream_tx, &mut stdin).unwrap(),
        vec!["%1".to_owned()]
    );
    match stream_rx.try_recv().unwrap() {
        StreamControl::Membership { pane_ids } => {
            assert_eq!(pane_ids, ["%2"]);
        }
    }
    assert!(matches!(
        stream_rx.try_recv(),
        Err(std_mpsc::TryRecvError::Empty)
    ));
    let written = String::from_utf8(stdin).unwrap();
    assert_eq!(written.matches("__ADE_MEMBERSHIP__").count(), 1);
    assert_eq!(written.matches("__ADE_CAPTURE__").count(), 1);
}

#[test]
fn failed_membership_batch_remains_retryable() {
    struct FailFirstFlush {
        bytes: Vec<u8>,
        fail: bool,
    }

    impl Write for FailFirstFlush {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if std::mem::take(&mut self.fail) {
                Err(std::io::Error::other("injected flush failure"))
            } else {
                Ok(())
            }
        }
    }

    let mut current = HashSet::from(["%1".into()]);
    let failed_desired = HashSet::from(["%2".into()]);
    let next_desired = HashSet::from(["%3".into()]);
    let (stream_tx, stream_rx) = std_mpsc::channel();
    let mut stdin = FailFirstFlush {
        bytes: Vec::new(),
        fail: true,
    };

    assert!(
        apply_membership_update(&mut current, &failed_desired, &stream_tx, &mut stdin,).is_err()
    );
    assert_eq!(current, HashSet::from(["%1".into()]));

    let mut stream = StreamState::new(
        &["%1".into()],
        Arc::new(crate::service::terminal::FlowControl::default()),
    );
    stream.apply_control(stream_rx.recv().unwrap());
    assert_eq!(
        stream.pane_states.keys().cloned().collect::<HashSet<_>>(),
        HashSet::from(["%2".into()])
    );
    if let PaneSeedState::Pending { buffered, .. } = stream.pane_states.get_mut("%2").unwrap() {
        buffered.push((7, b"preserve-me".to_vec()));
    }
    stream.apply_control(StreamControl::Membership {
        pane_ids: vec!["%2".into()],
    });
    assert!(matches!(
        stream.pane_states.get("%2"),
        Some(PaneSeedState::Pending { buffered, .. })
            if buffered == &[(7, b"preserve-me".to_vec())]
    ));

    stream.apply_control(stream_rx.recv().unwrap());
    assert_eq!(
        stream.pane_states.keys().cloned().collect::<HashSet<_>>(),
        HashSet::from(["%1".into()]),
        "a failed batch restores the reader's last committed membership"
    );

    assert_eq!(
        apply_membership_update(&mut current, &next_desired, &stream_tx, &mut stdin).unwrap(),
        vec!["%1".to_owned()]
    );
    assert_eq!(current, next_desired);
    stream.apply_control(stream_rx.recv().unwrap());
    assert_eq!(
        stream.pane_states.keys().cloned().collect::<HashSet<_>>(),
        HashSet::from(["%3".into()]),
        "authoritative replacement must prune the failed update's stale pane"
    );
    assert_eq!(
        String::from_utf8(stdin.bytes)
            .unwrap()
            .matches("__ADE_MEMBERSHIP__")
            .count(),
        2,
        "the failed batch must be retried instead of mistaken for a no-op"
    );
}

#[test]
fn failed_membership_registration_rolls_back_speculative_resources() {
    let mut resources = PaneResourceStore::with_total_limit(32, 1024, 4096);
    register_mounted_panes(&mut resources, &["%1".into()], true, 1);
    register_mounted_panes(&mut resources, &["%2".into()], true, 2);

    let speculative = ["%2".to_owned()];
    remove_pane_resources(&mut resources, speculative.iter());
    assert!(resources.get("%1").is_some());
    assert!(resources.get("%2").is_none());

    register_mounted_panes(&mut resources, &["%3".into()], true, 3);
    remove_unmounted_pane_resources(
        &mut resources,
        &HashSet::from(["%1".into()]),
        &HashSet::from(["%3".into()]),
    );
    assert!(resources.get("%1").is_none());
    assert!(resources.get("%2").is_none());
    assert!(resources.get("%3").is_some());
}

#[test]
fn stopped_reader_cannot_recreate_cleaned_up_resources() {
    let resources = Arc::new(Mutex::new(PaneResourceStore::with_total_limit(
        32, 1024, 4096,
    )));
    let stopped = AtomicBool::new(true);

    let disposition = stream::with_active_resources(&resources, &stopped, |resources| {
        resources.record_output("%1", b"stale old-reader output", 7)
    });

    assert_eq!(disposition, None);
    assert!(resources.lock().unwrap().get("%1").is_none());
}

#[test]
fn stop_cleanup_cannot_overtake_in_flight_resource_publication() {
    let resources = Arc::new(Mutex::new(PaneResourceStore::with_total_limit(
        32, 1024, 4096,
    )));
    let stopped = Arc::new(AtomicBool::new(false));
    let order = Arc::new(Mutex::new(Vec::new()));
    let (entered_tx, entered_rx) = std_mpsc::sync_channel(0);
    let (release_tx, release_rx) = std_mpsc::sync_channel(0);

    let reader_resources = Arc::clone(&resources);
    let reader_stopped = Arc::clone(&stopped);
    let reader_order = Arc::clone(&order);
    let reader = std::thread::spawn(move || {
        stream::with_active_resources(&reader_resources, &reader_stopped, |resources| {
            resources.record_output("%1", b"in flight", 1);
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            reader_order.lock().unwrap().push("publish");
        })
        .unwrap();
    });
    entered_rx.recv().unwrap();

    let replacement_resources = Arc::clone(&resources);
    let replacement_stopped = Arc::clone(&stopped);
    let replacement_order = Arc::clone(&order);
    let replacement = std::thread::spawn(move || {
        let mut resources = replacement_resources.lock().unwrap();
        replacement_stopped.store(true, Ordering::Release);
        resources.remove("%1");
        replacement_order.lock().unwrap().push("cleanup");
    });

    release_tx.send(()).unwrap();
    reader.join().unwrap();
    replacement.join().unwrap();
    assert_eq!(*order.lock().unwrap(), ["publish", "cleanup"]);

    assert!(
        stream::with_active_resources(&resources, &stopped, |resources| {
            resources.record_output("%1", b"stale", 2)
        })
        .is_none()
    );
    assert!(resources.lock().unwrap().get("%1").is_none());
}

#[test]
fn mounting_one_pane_does_not_reveal_inactive_window_resources() {
    let mut resources = PaneResourceStore::with_total_limit(32, 1024, 4096);
    resources.ensure("%1", false, 1);
    resources.ensure("%2", false, 1);
    register_mounted_panes(&mut resources, &["%1".into()], true, 2);
    assert!(!resources.is_hidden("%1"));
    assert!(resources.is_hidden("%2"));
}

#[test]
fn seed_restores_every_tmux_exposed_terminal_mode() {
    let seed = build_seed(
        "%1",
        vec![b"primary history".to_vec()],
        vec![b"alternate screen".to_vec()],
        &[b"__ADE_META__:%1:4:5:1:1:0:1:0:1:0:0:1:1:0:80:".to_vec()],
    )
    .unwrap();
    for expected in [
        b"\x1b[?1049h".as_slice(),
        b"\x1b[?2004h",
        b"\x1b[?1002h",
        b"\x1b[?1006h",
        b"\x1b[?1004l",
        b"\x1b[?25l",
        b"\x1b[?1h",
        b"\x1b=",
        b"\x1b[?7l",
        b"\x1b[6;5H",
    ] {
        assert!(
            seed.bytes
                .windows(expected.len())
                .any(|window| window == expected),
            "missing mode sequence {expected:?}"
        );
    }
}

#[test]
fn incomplete_or_unknown_capture_metadata_requires_resnapshot() {
    assert!(build_seed("%1", vec![b"screen".to_vec()], vec![], &[]).is_none());
    assert!(
        build_seed(
            "%1",
            vec![],
            vec![],
            &[b"__ADE_META__:%1:0:0:0:0:0:0:0:0:0:0:0:0:unknown:80:".to_vec()]
        )
        .is_none()
    );
    let tmux_33_seed = build_seed(
        "%11",
        vec![],
        vec![],
        &[b"__ADE_META__:%11:0:0:0::0:0:0:0:0:1:0:0:1:80:".to_vec()],
    )
    .expect("tmux 3.3a's unavailable bracket-paste flag should use a safe default");
    assert!(
        tmux_33_seed
            .bytes
            .windows(b"\x1b[?2004l".len())
            .any(|window| window == b"\x1b[?2004l")
    );
    assert!(
        tmux_33_seed
            .diagnostics
            .iter()
            .any(|value| value.contains("bracketed-paste"))
    );
    assert!(
        tmux_33_seed
            .diagnostics
            .iter()
            .any(|value| value.contains("focus-reporting"))
    );
    assert!(
        parse_capture_metadata(b"__ADE_META__:%11:0:0:0:0:0:0:0:0:0:1:0:0:1:80:", "%11").is_some()
    );
    assert!(
        parse_capture_metadata(
            b"__ADE_META__:       %11:0:0:0:0:0:0:0:0:0:1:0:0:1:80:",
            "%11"
        )
        .is_none()
    );
}

#[test]
fn reconnect_marks_every_pane_pending_for_a_fresh_seed() {
    let pane_ids: Vec<_> = (0..33).map(|index| format!("%{index}")).collect();
    let state = StreamState::new(&pane_ids, Arc::new(FlowControl::default()));
    assert_eq!(state.pane_states.len(), 33);
    assert!(
        state
            .pane_states
            .values()
            .all(|state| matches!(state, PaneSeedState::Pending { .. }))
    );
}

#[test]
fn capture_and_metadata_are_correlated_across_distinct_tmux_command_blocks() {
    let mut state = StreamState::new(&["%1".into()], Arc::new(FlowControl::default()));
    state.expected_capture = Some("%1".into());
    let tag = |number| CommandTag {
        timestamp: 1,
        number,
        flags: 1,
    };
    let CommandBlock::CapturePrimary { pane_id, .. } = state.start_block(tag(2)) else {
        panic!("capture-pane block was not correlated with its marker");
    };
    if let Some(PaneSeedState::Pending {
        buffered,
        buffered_bytes,
        ..
    }) = state.pane_states.get_mut("%1")
    {
        buffered.push((1, b"already captured".to_vec()));
        *buffered_bytes = b"already captured".len();
    }
    state.pending_alternate = Some((pane_id, vec![b"visible screen".to_vec()], 1));
    let PaneSeedState::Pending { buffered, .. } = state.pane_states.get("%1").unwrap() else {
        panic!("pane stopped awaiting its seed");
    };
    assert_eq!(buffered.len(), 1);
    let CommandBlock::CaptureAlternate {
        pane_id,
        visible_lines,
        ..
    } = state.start_block(tag(3))
    else {
        panic!("second capture block was not correlated with the first");
    };
    state.pending_metadata = Some(PendingCaptureMetadata {
        pane_id: pane_id.clone(),
        visible_lines: visible_lines.clone(),
        saved_normal_lines: vec![b"saved normal screen".to_vec()],
        visible_boundary: 1,
    });
    let CommandBlock::CaptureMetadata {
        saved_normal_lines, ..
    } = state.start_block(tag(4))
    else {
        panic!("metadata block was not correlated with both screen captures");
    };
    let seed = build_seed(
        &pane_id,
        visible_lines,
        saved_normal_lines,
        &[b"__ADE_META__:%1:0:0:1:1:0:0:0:0:0:1:0:0:1:80:".to_vec()],
    )
    .expect("metadata should complete seed");
    let position = |needle: &[u8]| {
        seed.bytes
            .windows(needle.len())
            .position(|window| window == needle)
    };
    // The pane is in the alternate screen, so what tmux displays — the
    // first capture — has to land *after* the switch to it, and the saved
    // normal grid before. Painting them the other way round is what made
    // every agent-pane seed come up blank (P12-U003).
    let switch = position(b"\x1b[?1049h").expect("alternate screen switch");
    assert!(position(b"saved normal screen").unwrap() < switch);
    assert!(position(b"visible screen").unwrap() > switch);
}

#[test]
fn capture_boundary_tracks_the_active_screen_without_duplicate_replay() {
    let mut seeder = ScreenSeeder::default();
    seeder.buffer(10, b"in primary capture".to_vec());
    seeder.buffer(11, b"between captures".to_vec());
    seeder.buffer(13, b"after capture".to_vec());
    let replay = seeder.complete(b"seed".to_vec(), 12).replay;
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].sequence, 13);
    assert_eq!(replay[0].bytes, b"after capture");
}

#[test]
fn joined_capture_reconstructs_soft_wrap_at_authoritative_width() {
    let command = capture_command("%1");
    assert!(command.matches("capture-pane -p -e -J").count() == 2);
    assert!(command.contains("#{pane_width}"));
    let logical_line = vec![b'w'; 160];
    let seed = build_seed(
        "%1",
        vec![logical_line.clone()],
        vec![],
        &[b"__ADE_META__:%1:0:0:0:1:0:0:0:0:0:1:0:0:0:80:".to_vec()],
    )
    .unwrap();
    assert!(
        seed.bytes
            .windows(logical_line.len())
            .any(|window| window == logical_line)
    );
    let wrap_enable = seed
        .bytes
        .windows(b"\x1b[?7h".len())
        .position(|window| window == b"\x1b[?7h")
        .unwrap();
    let line = seed
        .bytes
        .windows(logical_line.len())
        .position(|window| window == logical_line)
        .unwrap();
    assert!(wrap_enable < line);
}

/// The pane that froze forever, in one test.
///
/// A reveal emits its recovery event and *then* asks tmux for the seed the
/// event says is coming. When that request fails — the ordinary case being a
/// session control client that has not re-attached yet — the desktop is already
/// waiting for a screen, and nothing used to ask again. The debt is recorded
/// instead, and the attachment that replaces the broken one settles it.
#[test]
fn a_reveal_whose_seed_request_fails_owes_the_pane_a_seed_and_settles_it_later() {
    let output_credit = Arc::new(OutputCredit::negotiated(false));
    let mut clients =
        TerminalClients::new(Arc::clone(&output_credit), TopologyOutputTrigger::default());
    clients.generation.store(1, Ordering::Release);
    {
        let mut resources = clients.resources.lock().unwrap();
        resources.ensure("%1", true, 0);
        // An omitted renderer snapshot releases the resource, so the reveal
        // below is the one that owes the pane an authoritative seed.
        resources
            .hide_with_checkpoint(
                "%1",
                Vec::new(),
                VisibilityCheckpoint {
                    epoch: 1,
                    generation: 0,
                },
                1,
            )
            .unwrap();
    }
    let (events, _receiver) = mpsc::channel(64);
    let broken = start_long_lived_attachment(
        events.clone(),
        Arc::clone(&clients.resources),
        Arc::clone(&clients.generation),
        Arc::clone(&output_credit),
        Arc::clone(&clients.emission_order),
    )
    .unwrap();
    // The state a reconnect leaves behind: an attachment record that still
    // claims the pane, and a tmux process that is gone, so every write to its
    // stdin fails. Killing the child is what produces that; the flag is put
    // back afterwards because the reader raises it on its way out and this test
    // is about the seed request failing, not about a deliberate teardown.
    let stopped = Arc::clone(&broken.stopped);
    {
        let mut child = broken.child.lock().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !stopped.load(Ordering::Acquire) {
        assert!(
            std::time::Instant::now() < deadline,
            "the reader never observed its control client dying"
        );
        std::thread::yield_now();
    }
    stopped.store(false, Ordering::Release);
    clients.clients.insert("$1".into(), broken);

    let error = clients
        .set_visibility(
            "%1",
            VisibilityChange {
                visible: true,
                serialized_snapshot: Vec::new(),
                checkpoint: VisibilityCheckpoint {
                    epoch: 1,
                    generation: 1,
                },
            },
            &events,
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(
        clients.owed_seeds.contains("%1"),
        "a failed seed request left nothing owed: {error}"
    );

    // The replacement client is what the debt was waiting for.
    clients.clients.remove("$1").unwrap().stop();
    let healthy = start_long_lived_attachment(
        events.clone(),
        Arc::clone(&clients.resources),
        Arc::clone(&clients.generation),
        Arc::clone(&output_credit),
        Arc::clone(&clients.emission_order),
    )
    .unwrap();
    clients.clients.insert("$1".into(), healthy);
    clients.settle_owed_seeds();
    assert!(
        clients.owed_seeds.is_empty(),
        "the seed the host owed was never re-requested"
    );
    clients.stop();
}

/// An explicit seed request is the desktop saying it is drawing this pane.
///
/// A resource left released — by an eviction the desktop was never told about —
/// makes `stream.rs` store the completed capture and emit nothing, so the
/// request produces no seed however many times it is repeated.
#[test]
fn an_explicit_seed_request_makes_a_released_pane_emission_eligible_again() {
    let output_credit = Arc::new(OutputCredit::negotiated(false));
    let mut clients =
        TerminalClients::new(Arc::clone(&output_credit), TopologyOutputTrigger::default());
    clients
        .resources
        .lock()
        .unwrap()
        .require_seed("%1", "an eviction nobody was told about");
    assert!(clients.resources.lock().unwrap().is_hidden("%1"));
    let (events, _receiver) = mpsc::channel(8);
    let attachment = start_long_lived_attachment(
        events.clone(),
        Arc::clone(&clients.resources),
        Arc::clone(&clients.generation),
        Arc::clone(&output_credit),
        Arc::clone(&clients.emission_order),
    )
    .unwrap();
    clients.clients.insert("$1".into(), attachment);

    clients
        .request_seed_for_render("%1", &events, &AtomicBool::new(false))
        .unwrap();
    let resources = clients.resources.lock().unwrap();
    assert!(
        !resources.is_hidden("%1"),
        "the seed this pane asked for would have been suppressed"
    );
    assert_eq!(
        resources.get("%1").unwrap().state,
        StoredResourceState::Visible
    );
    drop(resources);
    clients.stop();
}

/// An eviction is a decision about a pane nobody asked about, and it used to be
/// made in silence: the pane kept rendering, its recovery material was gone,
/// and neither side ever said so.
#[test]
fn an_evicted_pane_is_reported_to_the_desktop_as_requiring_a_seed() {
    let resources = Arc::new(Mutex::new(PaneResourceStore::with_total_limit(4, 64, 64)));
    {
        let mut store = resources.lock().unwrap();
        store.set_visible("%1", true, 0);
        store.ensure("%2", false, 0);
        store.snapshot("%2", vec![b'x'; 64], 1);
        assert!(store.take_degradations().is_empty());
    }
    let (events, mut receiver) = mpsc::channel(8);
    let generation = AtomicU64::new(1);
    let stopped = AtomicBool::new(false);
    let overflowed = AtomicBool::new(false);
    let output_credit = OutputCredit::negotiated(false);
    let emission_order = Mutex::new(());
    TestOutputEmission {
        sender: &events,
        overflowed: &overflowed,
        resources: &resources,
        terminal_generation: &generation,
        stopped: &stopped,
        output_credit: &output_credit,
        emission_order: &emission_order,
        topology_trigger: &TopologyOutputTrigger::default(),
        read_started: std::time::Instant::now(),
    }
    .record("%1".into(), vec![b'o'; 64]);

    let SequencerControl::OrderedEvent(degraded) = receiver.try_recv().unwrap() else {
        panic!("the eviction was not reported at all")
    };
    assert_eq!(
        v1::EventKind::try_from(degraded.kind).unwrap(),
        v1::EventKind::PaneResource
    );
    let resource = degraded.pane_resource.unwrap();
    assert_eq!(resource.pane_id, "%2");
    assert!(resource.requires_seed);
    assert!(!resource.recovery_reason.is_empty());
    // The pane that produced the output is still delivered, in order, after it.
    let SequencerControl::OrderedEvent(output) = receiver.try_recv().unwrap() else {
        panic!("visible output was not delivered")
    };
    assert_eq!(output.terminal.unwrap().pane_id, "%1");
}
