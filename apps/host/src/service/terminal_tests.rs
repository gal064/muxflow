use super::*;

/// A capture ledger for the writes a test performs directly. Coalescing is
/// per control client, so a test that owns neither gets its own.
fn capture_ledger() -> Mutex<HashSet<String>> {
    Mutex::new(HashSet::new())
}

fn visibility_permit(
    sender: &mpsc::Sender<SequencerControl>,
) -> mpsc::OwnedPermit<SequencerControl> {
    sender
        .clone()
        .try_reserve_owned()
        .expect("visibility event queue should have capacity")
}

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
    TerminalAttachment::start_with_command(
        "$1",
        &["%1".into()],
        AttachmentRuntime {
            connection_epoch: 0,
            event_tx,
            overflowed,
            resources,
            terminal_generation: generation,
            output_credit,
            emission_order,
            topology_trigger: TopologyOutputTrigger::default(),
        },
        long_lived_attachment_command(),
        true,
    )
}

/// A stand-in control client that keeps every byte the host writes to it.
///
/// The commands *are* the behaviour under test — tmux discards a
/// `refresh-client -C` from a client that does not own `w->latest`, and the only
/// place the difference exists on this side is the command stream — so the
/// fixture is a process that records that stream and stays alive like a real
/// attachment does.
struct RecordedClient {
    path: std::path::PathBuf,
}

impl RecordedClient {
    fn new() -> Self {
        Self {
            path: std::env::temp_dir().join(format!("ade-latest-claim-{}", uuid::Uuid::new_v4())),
        }
    }

    fn command(&self) -> std::process::Command {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "exec cat > \"$0\"", &self.path.to_string_lossy()]);
        command
    }

    fn written(&self) -> String {
        std::fs::read_to_string(&self.path).unwrap_or_default()
    }

    /// Waits for a line the host has already written to reach the recording,
    /// so an assertion about what follows it is not racing the pipe.
    fn wait_for(&self, occurrences: usize, needle: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while self.written().matches(needle).count() < occurrences {
            assert!(
                std::time::Instant::now() < deadline,
                "control client never received {occurrences}x {needle}: {}",
                self.written()
            );
            std::thread::yield_now();
        }
    }

    /// A write the host performs *after* the one under test, so "no
    /// switch-client" is a statement about a stream that has gone past the point
    /// where one would have appeared, not about a stream that has not caught up.
    fn fence(&self, clients: &mut TerminalClients, occurrences: usize) {
        // This fixture records the control stream and answers none of it, so
        // every capture it was ever sent is still "in flight" and the next one
        // would be coalesced away. Nothing here is about coalescing — the
        // fence exists to push the stream past the write under test — so the
        // ledger is emptied first. `a_capture_already_in_flight_is_not_queued_twice`
        // is where the guard itself is proved.
        clients
            .clients
            .get("$1")
            .expect("the recorded client owns %1")
            .capture_in_flight
            .lock()
            .unwrap()
            .clear();
        clients.request_seed("%1").unwrap();
        self.wait_for(occurrences, "__ADE_CAPTURE__");
    }
}

impl Drop for RecordedClient {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The recorded control client, attached to `$1` and owning `%1`. The receiver
/// comes back with it because dropping it would stop the reader, and this
/// fixture is about what the writer emits.
fn clients_with_recorded_client(
    recorded: &RecordedClient,
) -> (TerminalClients, mpsc::Receiver<SequencerControl>) {
    let output_credit = Arc::new(OutputCredit::negotiated(false));
    let mut clients =
        TerminalClients::new(Arc::clone(&output_credit), TopologyOutputTrigger::default());
    let (events, receiver) = mpsc::channel(64);
    let overflowed = Arc::new(AtomicBool::new(false));
    let attachment = TerminalAttachment::start_with_command(
        "$1",
        &["%1".into()],
        AttachmentRuntime {
            connection_epoch: 0,
            event_tx: events,
            overflowed,
            resources: Arc::clone(&clients.resources),
            terminal_generation: Arc::clone(&clients.generation),
            output_credit,
            emission_order: Arc::clone(&clients.emission_order),
            topology_trigger: TopologyOutputTrigger::default(),
        },
        recorded.command(),
        true,
    )
    .unwrap();
    clients.clients.insert("$1".into(), attachment);
    // The attachment's own startup capture, so later fences are countable.
    recorded.wait_for(1, "__ADE_CAPTURE__");
    (clients, receiver)
}

/// One snapshot's worth of the gate's only input.
fn session_snapshot(attached_clients: u32) -> tmux_control::TmuxSnapshot {
    tmux_control::TmuxSnapshot {
        sessions: vec![tmux_control::Session {
            id: "$1".into(),
            name: "fixture".into(),
            window_count: 1,
            attached_clients,
            order: 0,
            pinned: false,
        }],
        windows: Vec::new(),
        panes: Vec::new(),
    }
}

/// The bug, in one test: a plain terminal the user typed into owns `w->latest`,
/// so the size this daemon asserts is discarded unless it takes the pointer
/// back. The order is the whole fix — the claim recomputes the windows from the
/// size this client holds, so a claim before the size would resize the user's
/// real windows to tmux's 80x24 default.
#[test]
fn sizing_a_session_a_foreign_client_shares_reclaims_the_size_pointer() {
    let recorded = RecordedClient::new();
    let (mut clients, _events) = clients_with_recorded_client(&recorded);
    // Two attached: this daemon's control client, and somebody else's.
    clients.reconcile(&session_snapshot(2));
    clients.last_size = Some((120, 40));

    clients.select_session("$1").unwrap();
    recorded.wait_for(1, "switch-client");

    let written = recorded.written();
    let size = written.find("refresh-client -C 120,40").unwrap();
    let claim = written.find("switch-client -E -t $1").unwrap();
    assert!(
        size < claim,
        "the claim must follow the size it makes tmux honour: {written}"
    );
    assert_eq!(
        written.matches("switch-client").count(),
        1,
        "one selection must cost exactly one topology reconcile: {written}"
    );
    clients.stop();
}

/// The common case, which must stay free. Every claim emits a
/// `%session-changed` on this daemon's own control stream and so costs a
/// topology reconcile; a session nobody else is attached to has no pointer to
/// reclaim.
#[test]
fn sizing_a_session_nobody_else_shares_never_claims() {
    let recorded = RecordedClient::new();
    let (mut clients, _events) = clients_with_recorded_client(&recorded);
    // The one attached client is this daemon's own.
    clients.reconcile(&session_snapshot(1));
    clients.last_size = Some((120, 40));

    clients.select_session("$1").unwrap();
    recorded.fence(&mut clients, 2);
    assert!(
        !recorded.written().contains("switch-client"),
        "an unshared session paid for a reconcile it did not need: {}",
        recorded.written()
    );

    clients.resize(100, 30).unwrap();
    recorded.fence(&mut clients, 3);
    let written = recorded.written();
    assert!(written.contains("refresh-client -C 100,30"), "{written}");
    assert!(
        !written.contains("switch-client"),
        "a resize on an unshared session claimed anyway: {written}"
    );
    clients.stop();
}

/// The hazard that would be worse than the bug: `switch-client` recomputes the
/// windows from the size the claiming client holds, and a control client that
/// has never been sent a `refresh-client -C` holds tmux's 80x24 default. Sizing
/// a session the desktop has not given a size to must therefore claim nothing,
/// however many foreign clients are attached.
#[test]
fn a_client_that_was_never_sized_is_never_used_to_claim() {
    let recorded = RecordedClient::new();
    let (mut clients, _events) = clients_with_recorded_client(&recorded);
    clients.reconcile(&session_snapshot(3));
    assert!(clients.last_size.is_none());

    clients.select_session("$1").unwrap();
    recorded.fence(&mut clients, 2);
    let written = recorded.written();
    assert!(
        !written.contains("refresh-client -C"),
        "nothing asserted a size, so nothing may have claimed on one: {written}"
    );
    assert!(
        !written.contains("switch-client"),
        "an unsized client claimed the size pointer: {written}"
    );

    // The refusal lives in the operation, not in its call sites: a future
    // caller that reaches it out of order still cannot shrink the user's
    // windows to 80x24.
    let attachment = clients.clients.get_mut("$1").unwrap();
    assert!(!attachment.claim_latest("$1").unwrap());
    recorded.fence(&mut clients, 3);
    assert!(
        !recorded.written().contains("switch-client"),
        "claim_latest wrote on an unsized client: {}",
        recorded.written()
    );
    clients.stop();
}

/// The resize path carries the same gate as selection. A desktop resize is a
/// size the user's own terminal never asked for, so it is exactly when the
/// pointer has to move — and exactly when a session nobody shares must still
/// pay nothing.
#[test]
fn resizing_a_session_a_foreign_client_shares_reclaims_the_size_pointer() {
    let recorded = RecordedClient::new();
    let (mut clients, _events) = clients_with_recorded_client(&recorded);
    clients.reconcile(&session_snapshot(2));
    clients.last_size = Some((120, 40));
    clients.select_session("$1").unwrap();
    recorded.wait_for(1, "switch-client");

    clients.resize(100, 30).unwrap();
    recorded.wait_for(2, "switch-client");
    let written = recorded.written();
    let size = written.find("refresh-client -C 100,30").unwrap();
    let claim = written.rfind("switch-client -E -t $1").unwrap();
    assert!(
        size < claim,
        "the resize claimed before it sized: {written}"
    );
    assert_eq!(clients.last_size, Some((100, 30)));
    clients.stop();
}

/// The gate is arithmetic on tmux's own count, and this daemon's input sidecar
/// is one of the clients tmux counts. Mistaking it for a foreign one would claim
/// on every sizing of the session it happens to be attached to — the churn the
/// gate exists to avoid.
#[test]
fn the_input_sidecar_is_not_counted_as_a_foreign_client() {
    let recorded = RecordedClient::new();
    let (mut clients, _events) = clients_with_recorded_client(&recorded);
    // Two attached, and both of them are this daemon's: the control client and
    // the input sidecar on the same session.
    clients.reconcile(&session_snapshot(2));
    clients.input_session = Some("$1".into());
    clients.last_size = Some((120, 40));
    assert!(!clients.foreign_client_shares("$1"));

    clients.select_session("$1").unwrap();
    recorded.fence(&mut clients, 2);
    assert!(
        !recorded.written().contains("switch-client"),
        "the daemon's own sidecar was mistaken for a user's terminal: {}",
        recorded.written()
    );

    // The third client is the one that can own `w->latest`.
    clients.reconcile(&session_snapshot(3));
    assert!(clients.foreign_client_shares("$1"));
    assert!(
        !clients.foreign_client_shares("$2"),
        "a session the last snapshot never carried must not gate on a guess"
    );
    clients.stop();
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

/// A saturated ordered queue must delay the visibility transition without
/// holding the terminal mutex or emission fence. Once capacity is reserved,
/// the recovery event must still precede output that observes visibility.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_reveal_recovery_is_admitted_before_concurrent_visible_output() {
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
                VisibilityCheckpoint {
                    epoch: 1,
                    generation: 0,
                },
                1,
            )
            .unwrap();
    }
    // One slot, already taken: the reveal must wait for a reservation before it
    // is allowed to take the terminal mutex and make the pane visible.
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
    let reveal = tokio::spawn(async move {
        let permit = reveal_events.clone().reserve_owned().await.unwrap();
        tokio::task::spawn_blocking(move || {
            reveal_clients.lock().unwrap().set_visibility(
                "%1",
                VisibilityChange {
                    visible: true,
                    renderer_holds_snapshot: true,
                    checkpoint: VisibilityCheckpoint {
                        epoch: 1,
                        generation: 0,
                    },
                },
                permit,
                &reveal_events,
                &reveal_overflowed,
            )
        })
        .await
        .unwrap()
    });

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        resources.lock().unwrap().get("%1").unwrap().state,
        StoredResourceState::HiddenBuffered,
        "visibility changed before its recovery event had reserved capacity"
    );
    assert!(
        shared.try_lock().is_ok(),
        "waiting for sequencer capacity held the terminal mutex"
    );

    let SequencerControl::OrderedEvent(filler) = receiver.recv().await.unwrap() else {
        panic!("queue did not start with the filler event");
    };
    assert_eq!(
        v1::EventKind::try_from(filler.kind).unwrap(),
        v1::EventKind::TopologyDirty
    );

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
            "reveal never committed after reserving sequencer capacity"
        );
        tokio::task::yield_now().await;
    }

    let output_events = events.clone();
    let output_overflowed = Arc::clone(&overflowed);
    let output_resources = Arc::clone(&resources);
    let output_generation = Arc::clone(&generation);
    let output_credit_clone = Arc::clone(&output_credit);
    let output = std::thread::spawn(move || {
        TestOutputEmission {
            connection_epoch: 0,
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
    let SequencerControl::OrderedEvent(recovery) = receiver.recv().await.unwrap() else {
        panic!("reveal did not emit ordered recovery");
    };
    let SequencerControl::OrderedEvent(output_event) = receiver.recv().await.unwrap() else {
        panic!("visible output was not ordered after recovery");
    };
    reveal.await.unwrap().unwrap();
    output.join().unwrap();
    assert_eq!(
        v1::EventKind::try_from(recovery.kind).unwrap(),
        v1::EventKind::PaneResource
    );
    // The renderer's own screen is the recovery base; what crosses is the
    // host's verification of it and the output since.
    assert!(recovery.pane_resource.unwrap().resume_from_renderer);
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
            connection_epoch: 0,
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
                renderer_holds_snapshot: true,
                checkpoint: VisibilityCheckpoint {
                    epoch: 1,
                    generation: 1,
                },
            },
            visibility_permit(&visibility_events),
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

        let permit = events.clone().try_reserve_owned();
        if !close_credit {
            assert!(permit.is_err());
            let resources = clients.resources.lock().unwrap();
            let resource = resources.get("%1").unwrap();
            assert_eq!(resource.state, StoredResourceState::HiddenBuffered);
            continue;
        }
        assert!(
            clients
                .set_visibility(
                    "%1",
                    VisibilityChange {
                        visible: true,
                        renderer_holds_snapshot: true,
                        checkpoint: VisibilityCheckpoint {
                            epoch: 1,
                            generation: 0,
                        },
                    },
                    permit.unwrap(),
                    &events,
                    &AtomicBool::new(false),
                )
                .is_err()
        );
        let resources = clients.resources.lock().unwrap();
        let resource = resources.get("%1").unwrap();
        assert_eq!(resource.state, StoredResourceState::Released);
        assert!(resource.requires_seed);
        assert!(resource.raw_tail.is_empty());
    }
}

/// The ledger records a capture tmux received, not one this process wrote.
///
/// It is read to *suppress* seeds, so an entry standing for a capture that
/// never left this process silences the request that would replace it, and the
/// pane waits for a photograph nobody is taking. Recording after the flush
/// risks one redundant photograph; recording before it risks none at all.
#[test]
fn a_capture_whose_flush_failed_is_not_recorded_as_in_flight() {
    struct UnflushableStdin;
    impl Write for UnflushableStdin {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            Ok(buffer.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("tmux control stdin is gone"))
        }
    }

    let stdin = Arc::new(Mutex::new(UnflushableStdin));
    let ledger = capture_ledger();
    assert!(write_capture_request_resuming(&stdin, &ledger, "%1", false).is_err());
    assert!(
        ledger.lock().unwrap().is_empty(),
        "a capture tmux never received would coalesce away the seed that replaces it"
    );
}

/// A pane's tail crosses the wire once, on the reveal that draws it.
///
/// The hide is an acknowledgement, not a delivery: the renderer is on its way
/// out of that workspace and ignores a `hiddenBuffered` echo, so bytes sent
/// here are paid for ahead of the switch the user is waiting for and then paid
/// for again when the reveal hands back the same tail. The store keeps them —
/// that is the point — and only the answer is empty.
#[test]
fn a_hide_answers_with_no_bytes_and_the_reveal_carries_the_whole_tail() {
    let output_credit = Arc::new(OutputCredit::negotiated(true));
    let mut clients =
        TerminalClients::new(Arc::clone(&output_credit), TopologyOutputTrigger::default());
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
    clients.generation.store(2, Ordering::Release);
    {
        let mut resources = clients.resources.lock().unwrap();
        resources.set_visible("%1", true, 1);
        resources.record_output("%1", b"printed-while-visible", 2);
    }
    let checkpoint = VisibilityCheckpoint {
        epoch: 1,
        generation: 1,
    };

    clients
        .set_visibility(
            "%1",
            VisibilityChange {
                visible: false,
                renderer_holds_snapshot: true,
                checkpoint,
            },
            visibility_permit(&events),
            &events,
            &AtomicBool::new(false),
        )
        .unwrap();

    let hide = ordered_event_within(
        &mut receiver,
        Duration::from_secs(5),
        "hide did not emit its recovery event",
    );
    let hidden = hide.pane_resource.as_ref().unwrap();
    assert!(
        hidden.raw_tail.is_empty(),
        "the hide answer carried {} bytes",
        hidden.raw_tail.len()
    );
    assert_eq!(hide.terminal_delivery_bytes, 0);
    assert_eq!(
        clients
            .resources
            .lock()
            .unwrap()
            .get("%1")
            .unwrap()
            .raw_tail,
        b"printed-while-visible",
        "the host stopped holding the tail it must hand back on the reveal"
    );

    clients
        .set_visibility(
            "%1",
            VisibilityChange {
                visible: true,
                renderer_holds_snapshot: true,
                checkpoint,
            },
            visibility_permit(&events),
            &events,
            &AtomicBool::new(false),
        )
        .unwrap();
    let reveal = ordered_event_within(
        &mut receiver,
        Duration::from_secs(5),
        "reveal did not emit its recovery event",
    );
    let revealed = reveal.pane_resource.as_ref().unwrap();
    assert!(revealed.resume_from_renderer);
    assert_eq!(revealed.raw_tail, b"printed-while-visible");
    assert_eq!(
        reveal.terminal_delivery_bytes as usize,
        revealed.raw_tail.len()
    );
    clients.stop();
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

#[test]
fn control_client_reports_the_terminal_palette_before_its_first_capture() {
    let recorded = RecordedClient::new();
    let (mut clients, _events) = clients_with_recorded_client(&recorded);
    let written = recorded.written();
    let foreground = written
        .find("refresh-client -r '%1:\x1b]10;rgb:ffff/ffff/ffff\x1b\\'")
        .unwrap();
    let background = written
        .find("refresh-client -r '%1:\x1b]11;rgb:2828/2c2c/3434\x1b\\'")
        .unwrap();
    let capture = written.find("__ADE_CAPTURE__").unwrap();
    assert!(
        foreground < background && background < capture,
        "{written:?}"
    );
    clients.stop();
}

#[test]
fn color_reports_are_capability_gated_for_tmux_33() {
    assert!(tmux_supports_control_color_reports(
        b"refresh-client (refresh) [-cDlLRSU] [-A pane:state] [-B name:what:format] [-C XxY] [-f flags] [-r pane:report] [-t target-client] [adjustment]\n"
    ));
    assert!(!tmux_supports_control_color_reports(
        b"refresh-client (refresh) [-cDlLRSU] [-A pane:state] [-B name:what:format] [-C XxY] [-f flags] [-t target-client] [adjustment]\n"
    ));
}

#[test]
fn control_client_palette_matches_terminal_theme_tokens() {
    let tokens = include_str!("../../../desktop/src/tokens.css");
    let token = |name: &str| {
        tokens
            .lines()
            .find_map(|line| {
                let (candidate, value) = line.trim().strip_suffix(';')?.split_once(':')?;
                (candidate == name).then_some(value.trim())
            })
            .unwrap_or_else(|| panic!("missing terminal theme token {name}"))
    };
    let osc_rgb = |hex: &str| {
        let hex = hex
            .strip_prefix('#')
            .unwrap_or_else(|| panic!("terminal theme token is not hexadecimal: {hex}"));
        assert_eq!(hex.len(), 6, "terminal theme token must be 24-bit RGB");
        format!("{0}{0}/{1}{1}/{2}{2}", &hex[0..2], &hex[2..4], &hex[4..6])
    };

    assert_eq!(TERMINAL_FOREGROUND_OSC_RGB, osc_rgb(token("--term-fg")));
    assert_eq!(TERMINAL_BACKGROUND_OSC_RGB, osc_rgb(token("--term-bg")));
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
    write_capture_request_resuming(&sink, &capture_ledger(), "%5", true).unwrap();
    let written = String::from_utf8(sink.lock().unwrap().clone()).unwrap();
    let lines: Vec<_> = written.lines().collect();
    assert_eq!(lines[0], "display-message -p '__ADE_RESUME__:5'");
    assert_eq!(lines[1], "refresh-client -A '%5:continue'");
    // tmux drops output produced while a pane is paused rather than
    // replaying it, so the capture that shares this lock hold is what
    // actually recovers the screen. The resume alone would leave a hole.
    assert_eq!(lines[2], "display-message -p '__ADE_CAPTURE__:5'");
    assert!(lines[3].starts_with("capture-pane -p -e -J -t %5"));

    let sink = Arc::new(Mutex::new(Vec::new()));
    write_capture_request_resuming(&sink, &capture_ledger(), "%5", false).unwrap();
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
        apply_membership_update(
            &mut current,
            &unchanged,
            &stream_tx,
            &mut stdin,
            &capture_ledger(),
            true
        )
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
        apply_membership_update(
            &mut current,
            &desired,
            &stream_tx,
            &mut stdin,
            &capture_ledger(),
            true
        )
        .unwrap(),
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
fn malformed_membership_id_has_no_partial_effects() {
    let mut current = HashSet::from(["%1".into()]);
    let desired = HashSet::from(["%2:'; display-message -p __INJECTED__; #".into()]);
    let (stream_tx, stream_rx) = std_mpsc::channel();
    let mut stdin = Vec::new();

    assert!(
        apply_membership_update(
            &mut current,
            &desired,
            &stream_tx,
            &mut stdin,
            &capture_ledger(),
            true
        )
        .is_err()
    );
    assert_eq!(current, HashSet::from(["%1".into()]));
    assert!(stdin.is_empty());
    assert!(matches!(
        stream_rx.try_recv(),
        Err(std_mpsc::TryRecvError::Empty)
    ));
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
        apply_membership_update(
            &mut current,
            &failed_desired,
            &stream_tx,
            &mut stdin,
            &capture_ledger(),
            true,
        )
        .is_err()
    );
    assert_eq!(current, HashSet::from(["%1".into()]));

    let mut stream = StreamState::new(
        &["%1".into()],
        Arc::new(crate::service::terminal::FlowControl::default()),
    );
    stream.apply_control(stream_rx.recv().unwrap(), &capture_ledger());
    assert_eq!(
        stream.pane_states.keys().cloned().collect::<HashSet<_>>(),
        HashSet::from(["%2".into()])
    );
    if let PaneSeedState::Pending { buffered, .. } = stream.pane_states.get_mut("%2").unwrap() {
        buffered.push((7, b"preserve-me".to_vec()));
    }
    stream.apply_control(
        StreamControl::Membership {
            pane_ids: vec!["%2".into()],
        },
        &capture_ledger(),
    );
    assert!(matches!(
        stream.pane_states.get("%2"),
        Some(PaneSeedState::Pending { buffered, .. })
            if buffered == &[(7, b"preserve-me".to_vec())]
    ));

    stream.apply_control(stream_rx.recv().unwrap(), &capture_ledger());
    assert_eq!(
        stream.pane_states.keys().cloned().collect::<HashSet<_>>(),
        HashSet::from(["%1".into()]),
        "a failed batch restores the reader's last committed membership"
    );

    assert_eq!(
        apply_membership_update(
            &mut current,
            &next_desired,
            &stream_tx,
            &mut stdin,
            &capture_ledger(),
            true,
        )
        .unwrap(),
        vec!["%1".to_owned()]
    );
    assert_eq!(current, next_desired);
    stream.apply_control(stream_rx.recv().unwrap(), &capture_ledger());
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

/// A seed is a photograph of the screen, not of the scrollback.
///
/// The capture that produces it asks tmux for the displayed grid alone — a
/// 200x50 screen is ~10 KB where `-S -2000` was ~191 KB, and that difference
/// sits on the wire ahead of the answer to the switch the user is waiting for.
/// Everything else about the seed is unchanged, which is what the mode
/// assertions below are for: dropping the history must not cost a single one
/// of the terminal modes tmux exposes.
#[test]
fn a_screen_seed_carries_no_scrollback_and_still_restores_every_mode() {
    let capture = capture_command("%1");
    assert!(
        !capture.contains("-S "),
        "the seed capture must ask for no history range: {capture}"
    );
    assert!(capture.starts_with("capture-pane -p -e -J -t %1 ;"));
    assert!(capture.contains("capture-pane -p -e -N -t %1"));
    // The alternate-screen views and metadata leg are the rest of the seed;
    // none may accidentally grow a history range.
    assert!(capture.contains("capture-pane -p -e -J -a -q -t %1"));
    assert!(capture.contains("capture-pane -p -e -N -a -q -t %1"));
    assert!(capture.contains("__ADE_META__"));

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
    // A screen capture of a partly filled pane is mostly blank lines, so the
    // repaint ends wherever the last line left the cursor — several rows above
    // where tmux says it is. Placement is therefore absolute and last, and
    // nothing is painted after it that could move it.
    assert!(
        seed.bytes.ends_with(b"\x1b[6;5H"),
        "the cursor must be placed absolutely, as the final act of the seed"
    );
}

/// Five independent callers ask for one pane's screen on a single workspace
/// switch — membership, the reveal, the renderer's own request, the reseed
/// loop, the desktop's watchdog — and before this each one cost another whole
/// screen on the wire ahead of the switch's answer. A capture already written
/// and not yet answered *is* the seed the second caller wants.
#[test]
fn a_capture_already_in_flight_is_not_queued_twice() {
    let recorded = RecordedClient::new();
    let (mut clients, _events) = clients_with_recorded_client(&recorded);
    {
        // This fixture's child holds the recording open and its control stream
        // shut, so the reader reaches EOF at once and empties the ledger on its
        // way out — a client that cannot answer must not hold a pane out of the
        // seed that replaces it. The capture in flight is therefore stated here
        // rather than inherited from the attachment's own startup.
        let attachment = clients.clients.get("$1").expect("the fixture owns %1");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !attachment.stopped.load(Ordering::Acquire) {
            assert!(
                std::time::Instant::now() < deadline,
                "reader never finished"
            );
            std::thread::yield_now();
        }
        attachment
            .capture_in_flight
            .lock()
            .unwrap()
            .insert("%1".to_owned());
    }
    // Neither of these may reach tmux: the capture already written is the seed
    // they are asking for.
    clients.request_seed("%1").unwrap();
    clients.request_seed("%1").unwrap();
    // The fence empties the ledger and asks again, so a capture *does* reach
    // the pipe: if either request above had been written it would be ahead of
    // this one in the same stream, and the count would be three.
    recorded.fence(&mut clients, 2);
    assert_eq!(recorded.written().matches("__ADE_CAPTURE__").count(), 2);
}

/// The scrollback the screen-only seed no longer sends is not lost — it is in
/// tmux, and this is how it is fetched: one command that asks for the history
/// range alone, answering a question the user asked rather than joining the
/// output stream.
#[test]
fn a_history_request_captures_only_the_scrollback_range() {
    let command = capture_history_command("%1", 2000, 0);
    assert!(command.contains("__ADE_HISTORY__:2000"));
    // `-E -1` stops at the line above the screen: the history and the seed
    // meet exactly once, with no row in both and none missing between them.
    assert!(command.contains("-S -2000 -E -1"));
    // No `__ADE_META__` leg, because this is not a screen: nothing in the answer
    // may be mistaken for a seed the reader has to store.
    assert!(!command.contains("__ADE_META__:"));
    // No `-J`, unlike the screen capture. Joined lines make the answer's line
    // count say nothing about how many rows it covers, and every other number
    // in this protocol — `-S`/`-E`, the renderer's skip, `history_size` — is a
    // row. One physical row per line is what lets them be compared at all. The
    // reflow and copy that `-J` used to buy are bought instead by the way the
    // desktop composes the page: a row that fills the grid is written without a
    // line break, and xterm wraps it itself.
    assert!(!command.contains(" -J "), "{command}");
    // And the size probe stays, for the reason that outlived the join: tmux
    // clamps a range running past the top of its history and answers one
    // entirely above it with a single row, so the rows themselves cannot say
    // the top was reached. Targeted, because `#{history_size}` is pane-scoped,
    // and last, because the leading marker has to stay untargeted so that it
    // always succeeds.
    assert!(
        command.ends_with("; display-message -p -t %1 '__ADE_HISTORY_META__:#{history_size}'"),
        "{command}"
    );
}

/// A pane that printed after it was seeded is not handed those rows twice.
///
/// tmux measures both bounds from the *current* display, so everything that
/// scrolled off since the seed is above it — rows the renderer already has in
/// its own scrollback. The renderer counts them and says so, and the range
/// starts above them: `-S -(skip+lines) -E -(skip+1)`, which is contiguous with
/// what the renderer is holding and overlaps none of it.
#[test]
fn a_history_request_starts_above_the_scrollback_the_renderer_already_holds() {
    let command = capture_history_command("%1", 2000, 40);
    assert!(command.contains("-S -2040 -E -41"));
    // Still the range the user asked for, not a shorter one.
    assert!(command.contains("__ADE_HISTORY__:2000"));

    // A skip no buffer could justify is clamped rather than obeyed. tmux does
    // not answer an out-of-range range with nothing — it clamps both bounds to
    // the top of the history and answers with the single row there — so the
    // answer's own size says nothing about whether the top was reached. What
    // stops the pane asking again is `history_size`, and the renderer's mirror
    // of this clamp; the clamp here only keeps the numbers finite.
    let clamped = capture_history_command("%1", 10, u32::MAX);
    assert!(clamped.contains("-S -10010 -E -10001"), "{clamped}");
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
    state.pending_visible_cells = Some((pane_id, vec![b"visible screen".to_vec()], 1));
    let PaneSeedState::Pending { buffered, .. } = state.pane_states.get("%1").unwrap() else {
        panic!("pane stopped awaiting its seed");
    };
    assert_eq!(buffered.len(), 1);
    let CommandBlock::CaptureVisibleCells {
        pane_id,
        visible_lines,
        ..
    } = state.start_block(tag(3))
    else {
        panic!("visible-cell capture was not correlated with the logical capture");
    };
    state.pending_alternate = Some(PendingAlternateCapture {
        pane_id: pane_id.clone(),
        visible_lines: visible_lines.clone(),
        visible_cell_lines: vec![b"visible cell screen".to_vec()],
        visible_boundary: 1,
    });
    let CommandBlock::CaptureAlternate {
        visible_cell_lines, ..
    } = state.start_block(tag(4))
    else {
        panic!("saved-normal capture was not correlated with the visible captures");
    };
    state.pending_saved_normal_cells = Some(PendingSavedNormalCells {
        pane_id: pane_id.clone(),
        visible_lines: visible_lines.clone(),
        visible_cell_lines: visible_cell_lines.clone(),
        saved_normal_lines: vec![b"saved normal screen".to_vec()],
        visible_boundary: 1,
    });
    let CommandBlock::CaptureSavedNormalCells {
        saved_normal_lines, ..
    } = state.start_block(tag(5))
    else {
        panic!("saved-normal cell capture was not correlated with the logical capture");
    };
    state.pending_metadata = Some(PendingCaptureMetadata {
        pane_id: pane_id.clone(),
        visible_lines: visible_lines.clone(),
        visible_cell_lines,
        saved_normal_lines: saved_normal_lines.clone(),
        saved_normal_cell_lines: vec![b"saved normal cell screen".to_vec()],
        visible_boundary: 1,
    });
    let CommandBlock::CaptureMetadata {
        saved_normal_lines, ..
    } = state.start_block(tag(6))
    else {
        panic!("metadata block was not correlated with every screen capture");
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
    assert!(command.matches("capture-pane -p -e -N").count() == 2);
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

#[test]
fn screen_seed_preserves_a_styled_erase_through_the_end_of_the_row() {
    let background = b"\x1b[48;2;65;69;76m";
    let mut logical_row = background.to_vec();
    logical_row.extend_from_slice(b"Proposed Plan");
    let mut captured_cell_row = logical_row.clone();
    captured_cell_row.extend_from_slice(&[b' '; 67]);

    let seed = build_seed_with_cell_captures(
        "%1",
        vec![logical_row],
        vec![captured_cell_row],
        vec![],
        vec![],
        &[b"__ADE_META__:%1:0:0:0:1:0:0:0:0:0:1:0:0:1:80:".to_vec()],
    )
    .unwrap();
    let expected = [background.as_slice(), b"\x1b[1;14H\x1b[K".as_slice()].concat();
    assert!(
        seed.bytes
            .windows(expected.len())
            .any(|window| window == expected),
        "the plan background must be repainted through the right edge"
    );
    assert!(
        !seed.bytes.windows(67).any(|window| window == [b' '; 67]),
        "preserving trailing cells must not turn every capture into a full-width payload"
    );
}

#[test]
fn screen_seed_preserves_each_differently_styled_trailing_blank_span() {
    let red = b"\x1b[41m";
    let blue = b"\x1b[44m";
    let mut captured_cell_row = red.to_vec();
    captured_cell_row.push(b'A');
    captured_cell_row.extend_from_slice(&[b' '; 9]);
    captured_cell_row.extend_from_slice(blue);
    captured_cell_row.extend_from_slice(&[b' '; 10]);

    let seed = build_seed_with_cell_captures(
        "%1",
        vec![[red.as_slice(), b"A"].concat()],
        vec![captured_cell_row],
        vec![],
        vec![],
        &[b"__ADE_META__:%1:0:0:0:1:0:0:0:0:0:1:0:0:1:20:".to_vec()],
    )
    .unwrap();
    let expected = [
        red.as_slice(),
        b"\x1b[1;2H\x1b[9X".as_slice(),
        blue.as_slice(),
        b"\x1b[1;11H\x1b[K".as_slice(),
    ]
    .concat();
    assert!(
        seed.bytes
            .windows(expected.len())
            .any(|window| window == expected),
        "each trailing blank span must retain its own background"
    );
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
        // The hide the reveal below cannot be matched against: it is answered
        // with a seed, and that seed is the one the pane ends up owed.
        resources
            .hide_with_checkpoint(
                "%1",
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
                renderer_holds_snapshot: false,
                checkpoint: VisibilityCheckpoint {
                    epoch: 1,
                    generation: 1,
                },
            },
            visibility_permit(&events),
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
        store.append("%2", &[b'x'; 64], 1);
        assert!(store.take_degradations().is_empty());
    }
    let (events, mut receiver) = mpsc::channel(8);
    let generation = AtomicU64::new(1);
    let stopped = AtomicBool::new(false);
    let overflowed = AtomicBool::new(false);
    let output_credit = OutputCredit::negotiated(false);
    let emission_order = Mutex::new(());
    TestOutputEmission {
        connection_epoch: 0,
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
