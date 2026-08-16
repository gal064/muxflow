use super::*;
use crate::connection::TerminalClient;

#[path = "tests/admission.rs"]
mod admission;

fn live_binding(epoch: u64) -> BulkBinding {
    let client = Arc::new(TerminalClient::new(None));
    client.ready.store(true, Ordering::Release);
    client.terminal_epoch.store(epoch, Ordering::Release);
    *client.server_identity.lock().unwrap() = format!("server-{epoch}");
    BulkBinding::capture(client, format!("server-{epoch}"), epoch).unwrap()
}

#[test]
fn timeout_and_stale_causes_preserve_unknown_publication_outcome() {
    for (reason, kind) in [
        (CancelReason::Timeout, TransferFailureKind::Timeout),
        (CancelReason::StaleBinding, TransferFailureKind::StaleScope),
    ] {
        let failure = failure_for_cancel(reason, TransferPhase::Verifying, "lost".into());
        assert_eq!(failure.outcome, TransferOutcome::Unknown);
        assert_eq!(failure.failure_kind, kind);
        assert_eq!(failure.cleanup_status, CleanupStatus::ConnectionClosed);
    }
}

#[test]
fn malformed_peer_replacement_reuses_lane_after_original_is_closed() {
    let _serial = engine_test_lock();
    let active_sockets = Arc::new(AtomicU32::new(0));
    let maximum = Arc::new(AtomicU32::new(0));
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    for index in 0..3 {
        let active_sockets = Arc::clone(&active_sockets);
        let maximum = Arc::clone(&maximum);
        let finished_tx = finished_tx.clone();
        enqueue_transfer(
            format!("malformed-replacement-{index}-{}", uuid::Uuid::new_v4()),
            live_binding(80 + index),
            Arc::new(CancelState::new()),
            || {},
            move || {
                let original = active_sockets.fetch_add(1, Ordering::AcqRel) + 1;
                maximum.fetch_max(original, Ordering::AcqRel);
                std::thread::sleep(std::time::Duration::from_millis(30));
                // A malformed peer is closed/reaped before the ownership
                // recovery connection inherits the same lane.
                active_sockets.fetch_sub(1, Ordering::AcqRel);
                let replacement = active_sockets.fetch_add(1, Ordering::AcqRel) + 1;
                maximum.fetch_max(replacement, Ordering::AcqRel);
                std::thread::sleep(std::time::Duration::from_millis(30));
                active_sockets.fetch_sub(1, Ordering::AcqRel);
                Ok(())
            },
            move |result, _| finished_tx.send(result).unwrap(),
        )
        .unwrap();
    }
    for _ in 0..3 {
        assert!(
            finished_rx
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap()
                .is_ok()
        );
    }
    assert_eq!(active_sockets.load(Ordering::Acquire), 0);
    assert!(maximum.load(Ordering::Acquire) <= 2);
}

#[test]
fn queued_cancellation_removes_the_job_immediately() {
    let mut queue = VecDeque::from(["active-next", "cancel-me", "tail"]);
    assert_eq!(
        take_queued_job(&mut queue, |job| *job == "cancel-me"),
        Some("cancel-me")
    );
    assert_eq!(queue, ["active-next", "tail"]);
}

#[test]
fn injected_queue_full_has_no_event_cancellation_or_owned_guard() {
    use super::super::{
        download_manager::DownloadCollisionPolicy,
        local_destination::{DestinationReservations, ReservedDestination},
    };

    struct AdmissionGuard(Arc<AtomicU32>);
    impl Drop for AdmissionGuard {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    let _serial = engine_test_lock();
    let id = format!("injected-full-{}", uuid::Uuid::new_v4());
    let queued = Arc::new(AtomicU32::new(0));
    let started = Arc::new(AtomicU32::new(0));
    let terminal = Arc::new(AtomicU32::new(0));
    let released = Arc::new(AtomicU32::new(0));
    let guard = AdmissionGuard(Arc::clone(&released));
    let root = std::env::temp_dir().join(format!("full-lease-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let reservations = Arc::new(DestinationReservations::default());
    let destination = ReservedDestination::reserve(
        &root.join("result.bin"),
        DownloadCollisionPolicy::Fail,
        Arc::clone(&reservations),
    )
    .unwrap();
    inject_queue_full_once();
    let error = enqueue_transfer_with_queued(
        id.clone(),
        live_binding(91),
        Arc::new(CancelState::new()),
        {
            let queued = Arc::clone(&queued);
            move || {
                queued.fetch_add(1, Ordering::AcqRel);
                Ok(())
            }
        },
        {
            let started = Arc::clone(&started);
            move || {
                started.fetch_add(1, Ordering::AcqRel);
            }
        },
        move || {
            let _guard = guard;
            let _destination = destination;
            Ok(())
        },
        {
            let terminal = Arc::clone(&terminal);
            move |_, _| {
                terminal.fetch_add(1, Ordering::AcqRel);
            }
        },
    )
    .unwrap_err();
    assert!(error.contains("queue is full"));
    assert_eq!(queued.load(Ordering::Acquire), 0);
    assert_eq!(started.load(Ordering::Acquire), 0);
    assert_eq!(terminal.load(Ordering::Acquire), 0);
    assert_eq!(released.load(Ordering::Acquire), 1);
    assert!(cancel_transfer(&id).is_err());
    assert_eq!(acceptance_engine_counts(), (0, 0));
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    let replacement = ReservedDestination::reserve(
        &root.join("result.bin"),
        DownloadCollisionPolicy::Fail,
        reservations,
    )
    .unwrap();
    drop(replacement);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn admitted_queued_transition_precedes_running_and_terminal() {
    let _serial = engine_test_lock();
    let events = Arc::new(Mutex::new(Vec::new()));
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    enqueue_transfer_with_queued(
        format!("ordered-admission-{}", uuid::Uuid::new_v4()),
        live_binding(92),
        Arc::new(CancelState::new()),
        {
            let events = Arc::clone(&events);
            move || {
                events.lock().unwrap().push("queued");
                Ok(())
            }
        },
        {
            let events = Arc::clone(&events);
            move || events.lock().unwrap().push("running")
        },
        || Ok(()),
        {
            let events = Arc::clone(&events);
            move |result, _| {
                events.lock().unwrap().push("terminal");
                finished_tx.send(result).unwrap();
            }
        },
    )
    .unwrap();
    assert!(
        finished_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap()
            .is_ok()
    );
    assert_eq!(*events.lock().unwrap(), ["queued", "running", "terminal"]);
}

#[test]
fn worker_spawn_failure_rolls_back_and_terminalizes_exactly_once() {
    let _serial = engine_test_lock();
    let id = format!("spawn-failure-{}", uuid::Uuid::new_v4());
    let started = Arc::new(AtomicU32::new(0));
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    inject_worker_spawn_failure_once();
    enqueue_transfer_with_queued(
        id.clone(),
        live_binding(93),
        Arc::new(CancelState::new()),
        || Ok(()),
        {
            let started = Arc::clone(&started);
            move || {
                started.fetch_add(1, Ordering::AcqRel);
            }
        },
        || panic!("work must not run when its worker cannot be spawned"),
        move |result, reason| finished_tx.send((result, reason)).unwrap(),
    )
    .unwrap();
    let (result, reason) = finished_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    let failure = result.unwrap_err();
    assert!(failure.error.contains("injected worker spawn failure"));
    assert_eq!(failure.outcome, TransferOutcome::NotPublished);
    assert_eq!(failure.cleanup_status, CleanupStatus::NotNeeded);
    assert_eq!(reason, CancelReason::None);
    assert_eq!(started.load(Ordering::Acquire), 0);
    assert!(finished_rx.try_recv().is_err());
    assert!(cancel_transfer(&id).is_err());
    assert_eq!(acceptance_engine_counts(), (0, 0));
}

#[test]
fn queued_cancellation_releases_owned_destination_lease() {
    use super::super::{
        download_manager::DownloadCollisionPolicy,
        local_destination::{DestinationReservations, ReservedDestination},
    };

    let _serial = engine_test_lock();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (blocker_tx, blocker_rx) = std::sync::mpsc::channel();
    for index in 0..2 {
        let gate = Arc::clone(&gate);
        let blocker_tx = blocker_tx.clone();
        enqueue_transfer(
            format!("lease-blocker-{index}-{}", uuid::Uuid::new_v4()),
            live_binding(94 + index),
            Arc::new(CancelState::new()),
            || {},
            move || {
                let mut released = gate.0.lock().unwrap();
                while !*released {
                    released = gate.1.wait(released).unwrap();
                }
                Ok(())
            },
            move |result, _| blocker_tx.send(result).unwrap(),
        )
        .unwrap();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while acceptance_engine_counts().0 != 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(acceptance_engine_counts(), (2, 0));

    let root = std::env::temp_dir().join(format!("cancelled-lease-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let reservations = Arc::new(DestinationReservations::default());
    let destination = ReservedDestination::reserve(
        &root.join("result.bin"),
        DownloadCollisionPolicy::Fail,
        Arc::clone(&reservations),
    )
    .unwrap();
    let id = format!("cancelled-lease-job-{}", uuid::Uuid::new_v4());
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    enqueue_transfer(
        id.clone(),
        live_binding(96),
        Arc::new(CancelState::new()),
        || {},
        move || {
            let _destination = destination;
            panic!("queued destination owner must not run after cancellation")
        },
        move |result, _| finished_tx.send(result).unwrap(),
    )
    .unwrap();
    assert_eq!(cancel_transfer(&id).unwrap().phase, TransferPhase::Queued);
    assert!(
        finished_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap()
            .is_err()
    );
    ReservedDestination::reserve(
        &root.join("result.bin"),
        DownloadCollisionPolicy::Fail,
        Arc::clone(&reservations),
    )
    .unwrap();

    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    for _ in 0..2 {
        assert!(
            blocker_rx
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap()
                .is_ok()
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn canonical_engine_limits_all_bulk_jobs_to_two_and_stales_queued_binding() {
    let _serial = engine_test_lock();
    let gate = Arc::new((Mutex::new((0_usize, 0_usize, false)), Condvar::new()));
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    for index in 0..3 {
        let gate = Arc::clone(&gate);
        let finished_tx = finished_tx.clone();
        enqueue_transfer(
            format!("engine-limit-{}-{index}", uuid::Uuid::new_v4()),
            live_binding(100 + index),
            Arc::new(CancelState::new()),
            || {},
            move || {
                let (lock, changed) = &*gate;
                let mut state = lock.lock().unwrap();
                state.0 += 1;
                state.1 = state.1.max(state.0);
                changed.notify_all();
                while !state.2 {
                    state = changed.wait(state).unwrap();
                }
                state.0 -= 1;
                Ok(())
            },
            move |result, _| finished_tx.send(result).unwrap(),
        )
        .unwrap();
    }
    let (lock, changed) = &*gate;
    let mut state = lock.lock().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while state.0 != 2 && std::time::Instant::now() < deadline {
        state = changed
            .wait_timeout(state, std::time::Duration::from_millis(20))
            .unwrap()
            .0;
    }
    assert_eq!(state.0, 2);
    assert_eq!(state.1, 2);
    state.2 = true;
    changed.notify_all();
    drop(state);
    for _ in 0..3 {
        assert!(
            finished_rx
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap()
                .is_ok()
        );
    }

    let blocker_gate = Arc::new((Mutex::new(false), Condvar::new()));
    let mut blocker_receivers = Vec::new();
    for index in 0..2 {
        let gate = Arc::clone(&blocker_gate);
        let (tx, rx) = std::sync::mpsc::channel();
        blocker_receivers.push(rx);
        enqueue_transfer(
            format!("engine-blocker-{}-{index}", uuid::Uuid::new_v4()),
            live_binding(200 + index),
            Arc::new(CancelState::new()),
            || {},
            move || {
                let (lock, changed) = &*gate;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = changed.wait(released).unwrap();
                }
                Ok(())
            },
            move |_, _| tx.send(()).unwrap(),
        )
        .unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    let stale_client = Arc::new(TerminalClient::new(None));
    stale_client.ready.store(true, Ordering::Release);
    stale_client.terminal_epoch.store(300, Ordering::Release);
    *stale_client.server_identity.lock().unwrap() = "server-300".into();
    let stale_binding =
        BulkBinding::capture(stale_client.clone(), "server-300".into(), 300).unwrap();
    let (stale_tx, stale_rx) = std::sync::mpsc::channel();
    enqueue_transfer(
        format!("engine-stale-{}", uuid::Uuid::new_v4()),
        stale_binding,
        Arc::new(CancelState::new()),
        || panic!("stale queued work must never start"),
        || Ok(()),
        move |result, reason| stale_tx.send((result, reason)).unwrap(),
    )
    .unwrap();
    *stale_client.server_identity.lock().unwrap() = "replacement".into();
    let (result, reason) = stale_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    let failure = result.unwrap_err();
    assert!(failure.error.contains("server identity"));
    assert_eq!(failure.outcome, TransferOutcome::NotPublished);
    assert_eq!(failure.failure_kind, TransferFailureKind::StaleScope);
    assert_eq!(reason, CancelReason::StaleBinding);
    *blocker_gate.0.lock().unwrap() = true;
    blocker_gate.1.notify_all();
    for receiver in blocker_receivers {
        receiver
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
    }
}

#[test]
#[ignore = "Phase 14 opt-in full admission queue fixture"]
fn phase14_full_queue_reports_admission_and_exact_terminal_outcomes() {
    let _serial = engine_test_lock();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    for index in 0..2 {
        let gate = Arc::clone(&gate);
        let finished_tx = finished_tx.clone();
        enqueue_transfer(
            format!("phase14-active-{index}"),
            live_binding(10_000 + index),
            Arc::new(CancelState::new()),
            || {},
            move || {
                let (lock, changed) = &*gate;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = changed.wait(released).unwrap();
                }
                Ok(())
            },
            move |result, _| finished_tx.send(result).unwrap(),
        )
        .unwrap();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while acceptance_engine_counts().0 != 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(acceptance_engine_counts(), (2, 0));

    let mut queued_ids = Vec::new();
    for index in 0..MAX_QUEUED_TRANSFERS {
        let id = format!("phase14-queued-{index}");
        queued_ids.push(id.clone());
        let finished_tx = finished_tx.clone();
        enqueue_transfer(
            id,
            live_binding(20_000 + index as u64),
            Arc::new(CancelState::new()),
            || panic!("queued fixture work must be cancelled before release"),
            || Ok(()),
            move |result, _| finished_tx.send(result).unwrap(),
        )
        .unwrap();
    }
    assert_eq!(acceptance_engine_counts(), (2, MAX_QUEUED_TRANSFERS));
    let rejected = enqueue_transfer(
        "phase14-overflow".into(),
        live_binding(30_000),
        Arc::new(CancelState::new()),
        || {},
        || Ok(()),
        |_, _| {},
    )
    .unwrap_err();
    assert!(rejected.contains("queue is full"));

    for id in &queued_ids {
        assert_eq!(cancel_transfer(id).unwrap().phase, TransferPhase::Queued);
    }
    assert_eq!(acceptance_engine_counts(), (2, 0));
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    let mut outcomes = Vec::new();
    for _ in 0..(MAX_QUEUED_TRANSFERS + 2) {
        outcomes.push(
            finished_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("every accepted transfer must terminalize"),
        );
    }
    assert_eq!(outcomes.len(), MAX_QUEUED_TRANSFERS + 2);
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 2);
    assert_eq!(
        outcomes.iter().filter(|result| result.is_err()).count(),
        MAX_QUEUED_TRANSFERS
    );
    println!(
        "PHASE14_METRIC {}",
        serde_json::json!({
            "lane": "transferAdmission",
            "activeHighWater": 2,
            "queuedHighWater": MAX_QUEUED_TRANSFERS,
            "accepted": MAX_QUEUED_TRANSFERS + 2,
            "rejected": 1,
            "terminalOutcomes": outcomes.len(),
            "outcomeParity": outcomes.len() == MAX_QUEUED_TRANSFERS + 2,
        })
    );
}

#[test]
fn active_epoch_loss_kills_worker_process_and_reports_stale() {
    let _serial = engine_test_lock();
    let client = Arc::new(TerminalClient::new(None));
    client.ready.store(true, Ordering::Release);
    client.terminal_epoch.store(401, Ordering::Release);
    *client.server_identity.lock().unwrap() = "server-401".into();
    let binding = BulkBinding::capture(client.clone(), "server-401".into(), 401).unwrap();
    let cancellation = Arc::new(CancelState::new());
    let worker_cancellation = Arc::clone(&cancellation);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    enqueue_transfer(
        format!("active-stale-{}", uuid::Uuid::new_v4()),
        binding,
        cancellation,
        || {},
        move || {
            let mut child = std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap();
            let _binding = match worker_cancellation.bind_process(child.id()) {
                Ok(binding) => binding,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error.into());
                }
            };
            started_tx.send(child.id()).unwrap();
            let status = child.wait().map_err(|error| error.to_string())?;
            if worker_cancellation.is_cancelled() {
                Err("worker stopped after binding loss".into())
            } else if status.success() {
                Ok(())
            } else {
                Err(format!("worker exited unexpectedly: {status}").into())
            }
        },
        move |result, reason| finished_tx.send((result, reason)).unwrap(),
    )
    .unwrap();
    started_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    client.terminal_epoch.store(402, Ordering::Release);
    let (result, reason) = finished_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    assert!(result.is_err());
    assert_eq!(reason, CancelReason::StaleBinding);
}

#[test]
fn finalizing_cancel_records_request_without_killing_authoritative_helper() {
    let cancellation = CancelState::new();
    let mut child = std::process::Command::new("sleep")
        .arg("0.1")
        .spawn()
        .unwrap();
    let _binding = cancellation.bind_process(child.id()).unwrap();
    cancellation.prepare_finalize().unwrap();
    cancellation.cancel();
    let status = child.wait().unwrap();
    assert!(status.success());
    assert_eq!(cancellation.reason(), CancelReason::User);
    assert_eq!(cancellation.phase(), TransferPhase::Verifying);
}

#[test]
fn finalizing_cancel_waits_for_authoritative_outcome_then_releases_worker() {
    let _serial = engine_test_lock();
    let transfer_id = format!("finalize-barrier-{}", uuid::Uuid::new_v4());
    let cancellation = Arc::new(CancelState::new());
    let worker_cancellation = Arc::clone(&cancellation);
    let (verifying_tx, verifying_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    enqueue_transfer(
        transfer_id.clone(),
        live_binding(501),
        cancellation,
        || {},
        move || {
            worker_cancellation.prepare_finalize()?;
            verifying_tx.send(()).unwrap();
            release_rx.recv().map_err(|error| error.to_string())?;
            Ok(())
        },
        move |result, reason| finished_tx.send((result, reason)).unwrap(),
    )
    .unwrap();
    verifying_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    assert_eq!(
        cancel_transfer(&transfer_id).unwrap(),
        CancelResponse {
            disposition: CancelDisposition::AwaitingAuthoritativeOutcome,
            phase: TransferPhase::Verifying,
        }
    );
    assert!(finished_rx.try_recv().is_err());
    release_tx.send(()).unwrap();
    let (result, reason) = finished_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    assert!(result.is_ok());
    assert_eq!(reason, CancelReason::User);
    let (released_tx, released_rx) = std::sync::mpsc::channel();
    enqueue_transfer(
        format!("after-silent-{}", uuid::Uuid::new_v4()),
        live_binding(502),
        Arc::new(CancelState::new()),
        || {},
        || Ok(()),
        move |result, _| released_tx.send(result).unwrap(),
    )
    .unwrap();
    assert!(
        released_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap()
            .is_ok()
    );
}

#[test]
fn silent_commit_and_silent_reconciliation_are_bounded_and_release_worker() {
    use std::io::Write as _;
    use std::process::Stdio;

    fn silent_peer() -> std::process::Child {
        std::process::Command::new("sh")
            .arg("-c")
            .arg("dd bs=1 count=1 of=/dev/null 2>/dev/null; exec sleep 30")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap()
    }

    let _serial = engine_test_lock();
    let transfer_id = format!("silent-authority-{}", uuid::Uuid::new_v4());
    let binding = live_binding(601);
    let event_binding = binding.clone();
    let cancellation = Arc::new(CancelState::new());
    let worker_cancellation = Arc::clone(&cancellation);
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    enqueue_transfer(
        transfer_id.clone(),
        binding,
        cancellation,
        || {},
        move || {
            let mut commit = BulkChild(silent_peer());
            let mut commit_input = commit.0.stdin.take().unwrap();
            let commit_binding = worker_cancellation.bind_process(commit.0.id())?;
            worker_cancellation.prepare_finalize()?;
            let commit_deadline =
                worker_cancellation.arm_deadline(std::time::Duration::from_millis(100));
            commit_input.write_all(b"c").unwrap();
            let commit_status = commit.0.wait().map_err(|error| error.to_string())?;
            commit_deadline.complete();
            drop(commit_binding);
            if commit_status.success() {
                return Err("silent commit peer was not terminated by its deadline".into());
            }

            let mut reconciliation = BulkChild(silent_peer());
            let mut reconciliation_input = reconciliation.0.stdin.take().unwrap();
            let reconciliation_binding =
                worker_cancellation.bind_authoritative_process(reconciliation.0.id())?;
            let reconciliation_deadline =
                worker_cancellation.arm_deadline(std::time::Duration::from_millis(100));
            reconciliation_input.write_all(b"r").unwrap();
            let reconciliation_status =
                reconciliation.0.wait().map_err(|error| error.to_string())?;
            reconciliation_deadline.complete();
            drop(reconciliation_binding);
            if reconciliation_status.success() {
                return Err("silent reconciliation peer was not terminated by its deadline".into());
            }
            Err(TransferFailure::unknown(
                TransferFailureKind::Timeout,
                "commit and ownership reconciliation timed out; owned artifacts retained",
            ))
        },
        move |result, reason| finished_tx.send((result, reason)).unwrap(),
    )
    .unwrap();

    let (result, reason) = finished_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    let error = result.unwrap_err();
    assert_eq!(error.outcome, TransferOutcome::Unknown);
    assert_eq!(error.failure_kind, TransferFailureKind::Timeout);
    assert_eq!(reason, CancelReason::Timeout);
    let event = crate::connection::files::transfer_event::TransferEvent::new(
        &transfer_id,
        &event_binding,
        crate::connection::files::transfer_event::TransferState::Failed,
    )
    .outcome(crate::connection::files::transfer_event::TransferOutcome::Unknown)
    .failure(
        crate::connection::files::transfer_event::TransferFailureKind::OutcomeUnknown,
        error.error.clone(),
    )
    .cleanup(
        crate::connection::files::transfer_event::CleanupStatus::Retained,
        error.cleanup_error,
    )
    .value();
    assert_eq!(event["outcome"], "unknown");
    assert_eq!(event["failureKind"], "outcomeUnknown");
    assert_eq!(event["cleanupStatus"], "retained");
    assert!(event.get("destination").is_none());

    let (released_tx, released_rx) = std::sync::mpsc::channel();
    enqueue_transfer(
        format!("after-silent-authority-{}", uuid::Uuid::new_v4()),
        live_binding(602),
        Arc::new(CancelState::new()),
        || {},
        || Ok(()),
        move |result, _| released_tx.send(result).unwrap(),
    )
    .unwrap();
    assert!(
        released_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap()
            .is_ok()
    );
}

#[test]
fn stale_scope_during_verifying_kills_old_transport_without_reconciliation() {
    use std::io::Write as _;
    use std::process::Stdio;

    let _serial = engine_test_lock();
    let client = Arc::new(TerminalClient::new(None));
    client.ready.store(true, Ordering::Release);
    client.terminal_epoch.store(701, Ordering::Release);
    *client.server_identity.lock().unwrap() = "server-701".into();
    let binding = BulkBinding::capture(client.clone(), "server-701".into(), 701).unwrap();
    let work_binding = binding.clone();
    let cancellation = Arc::new(CancelState::new());
    let worker_cancellation = Arc::clone(&cancellation);
    let (verifying_tx, verifying_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    enqueue_transfer(
        format!("stale-verifying-{}", uuid::Uuid::new_v4()),
        binding,
        cancellation,
        || {},
        move || {
            let mut child = BulkChild(
                std::process::Command::new("sh")
                    .arg("-c")
                    .arg("dd bs=1 count=1 of=/dev/null 2>/dev/null; exec sleep 30")
                    .stdin(Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
            let mut input = child.0.stdin.take().unwrap();
            let _process = worker_cancellation.bind_process(child.0.id())?;
            worker_cancellation.prepare_finalize()?;
            input.write_all(b"c").unwrap();
            verifying_tx.send(()).unwrap();
            let status = child.0.wait().map_err(|error| error.to_string())?;
            assert!(!status.success());
            assert!(work_binding.validate().is_err());
            Err(TransferFailure::unknown(
                TransferFailureKind::StaleScope,
                "captured scope became stale while verifying",
            ))
        },
        move |result, reason| finished_tx.send((result, reason)).unwrap(),
    )
    .unwrap();
    verifying_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    client.terminal_epoch.store(702, Ordering::Release);
    *client.server_identity.lock().unwrap() = "server-702".into();
    let (result, reason) = finished_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    let failure = result.unwrap_err();
    assert_eq!(failure.outcome, TransferOutcome::Unknown);
    assert_eq!(failure.failure_kind, TransferFailureKind::StaleScope);
    assert_eq!(reason, CancelReason::StaleBinding);
    assert_eq!(client.terminal_epoch.load(Ordering::Acquire), 702);
    assert_eq!(*client.server_identity.lock().unwrap(), "server-702");
}
