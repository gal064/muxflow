use super::*;

#[test]
fn queued_delivery_failure_rolls_back_every_admission_artifact() {
    use super::super::super::{
        download_manager::DownloadCollisionPolicy,
        local_destination::{DestinationReservations, ReservedDestination},
    };

    let _serial = engine_test_lock();
    let id = format!("queued-delivery-failure-{}", uuid::Uuid::new_v4());
    let started = Arc::new(AtomicU32::new(0));
    let worked = Arc::new(AtomicU32::new(0));
    let terminal = Arc::new(AtomicU32::new(0));
    let root = std::env::temp_dir().join(format!("queued-send-lease-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let reservations = Arc::new(DestinationReservations::default());
    let destination = ReservedDestination::reserve(
        &root.join("result.bin"),
        DownloadCollisionPolicy::Fail,
        Arc::clone(&reservations),
    )
    .unwrap();
    let error = enqueue_transfer_with_queued(
        id.clone(),
        live_binding(97),
        Arc::new(CancelState::new()),
        || Err("injected closed event channel".into()),
        {
            let started = Arc::clone(&started);
            move || {
                started.fetch_add(1, Ordering::AcqRel);
            }
        },
        {
            let worked = Arc::clone(&worked);
            move || {
                let _destination = destination;
                worked.fetch_add(1, Ordering::AcqRel);
                Ok(())
            }
        },
        {
            let terminal = Arc::clone(&terminal);
            move |_, _| {
                terminal.fetch_add(1, Ordering::AcqRel);
            }
        },
    )
    .unwrap_err();
    assert!(error.contains("injected closed event channel"));
    assert_eq!(started.load(Ordering::Acquire), 0);
    assert_eq!(worked.load(Ordering::Acquire), 0);
    assert_eq!(terminal.load(Ordering::Acquire), 0);
    assert!(cancel_transfer(&id).is_err());
    assert_eq!(acceptance_engine_counts(), (0, 0));
    assert_eq!(reservations.len(), 0);
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
fn failed_pending_head_retriggers_later_admitted_job() {
    let _serial = engine_test_lock();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let first_id = format!("failed-pending-head-{}", uuid::Uuid::new_v4());
    let first = std::thread::spawn(move || {
        enqueue_transfer_with_queued(
            first_id,
            live_binding(98),
            Arc::new(CancelState::new()),
            move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Err("injected queued failure".into())
            },
            || panic!("rejected head must not run"),
            || panic!("rejected head must not work"),
            |_, _| panic!("rejected head must not terminalize"),
        )
    });
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();

    let (later_tx, later_rx) = std::sync::mpsc::channel();
    enqueue_transfer_with_queued(
        format!("behind-failed-head-{}", uuid::Uuid::new_v4()),
        live_binding(99),
        Arc::new(CancelState::new()),
        || Ok(()),
        || {},
        || Ok(()),
        move |result, _| later_tx.send(result).unwrap(),
    )
    .unwrap();
    assert!(later_rx.try_recv().is_err());
    release_tx.send(()).unwrap();
    assert!(first.join().unwrap().is_err());
    assert!(
        later_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap()
            .is_ok()
    );
    assert_eq!(acceptance_engine_counts(), (0, 0));
}

#[test]
fn provisional_admission_latches_cancellation_but_rolls_back_when_publication_fails() {
    let _serial = engine_test_lock();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let id = format!("cancel-pending-admission-{}", uuid::Uuid::new_v4());
    let enqueue_id = id.clone();
    let enqueue = std::thread::spawn(move || {
        enqueue_transfer_with_queued(
            enqueue_id,
            live_binding(100),
            Arc::new(CancelState::new()),
            move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Err("injected queued publication failure".into())
            },
            || panic!("rejected pending admission must not start"),
            || panic!("rejected pending admission must not work"),
            |_, _| panic!("rejected pending admission must not terminalize"),
        )
    });
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    let (cancelled_tx, cancelled_rx) = std::sync::mpsc::channel();
    let cancel_id = id.clone();
    let cancel =
        std::thread::spawn(move || cancelled_tx.send(cancel_transfer(&cancel_id)).unwrap());
    wait_for_admission_cancellation_latch(&id);
    assert!(cancelled_rx.try_recv().is_err());
    release_tx.send(()).unwrap();
    assert!(enqueue.join().unwrap().is_err());
    assert!(cancelled_rx.recv().unwrap().is_err());
    cancel.join().unwrap();
    assert!(cancel_transfer(&id).is_err());
    assert_eq!(acceptance_engine_counts(), (0, 0));
}

#[test]
fn cancellation_after_queued_is_observable_latches_until_admission_commits() {
    let _serial = engine_test_lock();
    let (published_tx, published_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (terminal_tx, terminal_rx) = std::sync::mpsc::channel();
    let id = format!("cancel-published-admission-{}", uuid::Uuid::new_v4());
    let enqueue_id = id.clone();
    let enqueue = std::thread::spawn(move || {
        enqueue_transfer_with_queued(
            enqueue_id,
            live_binding(100),
            Arc::new(CancelState::new()),
            move || {
                published_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            },
            || panic!("cancelled pending admission must not start"),
            || panic!("cancelled pending admission must not work"),
            move |result, reason| terminal_tx.send((result, reason)).unwrap(),
        )
    });
    published_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    let (cancelled_tx, cancelled_rx) = std::sync::mpsc::channel();
    let cancel_id = id.clone();
    let cancel =
        std::thread::spawn(move || cancelled_tx.send(cancel_transfer(&cancel_id)).unwrap());
    wait_for_admission_cancellation_latch(&id);
    assert!(cancelled_rx.try_recv().is_err());
    assert!(terminal_rx.try_recv().is_err());
    release_tx.send(()).unwrap();
    enqueue.join().unwrap().unwrap();
    assert_eq!(
        cancelled_rx.recv().unwrap().unwrap(),
        CancelResponse {
            disposition: CancelDisposition::CancelRequested,
            phase: TransferPhase::Queued,
        }
    );
    cancel.join().unwrap();
    let (result, reason) = terminal_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    assert!(result.is_err());
    assert_eq!(reason, CancelReason::User);
    assert_eq!(acceptance_engine_counts(), (0, 0));
}

#[test]
fn worker_and_finished_panics_release_lane_and_dispatch_follower() {
    let _serial = engine_test_lock();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (blocker_tx, blocker_rx) = std::sync::mpsc::channel();
    enqueue_transfer(
        format!("panic-lane-blocker-{}", uuid::Uuid::new_v4()),
        live_binding(101),
        Arc::new(CancelState::new()),
        || {},
        {
            let gate = Arc::clone(&gate);
            move || {
                let mut released = gate.0.lock().unwrap();
                while !*released {
                    released = gate.1.wait(released).unwrap();
                }
                Ok(())
            }
        },
        move |result, _| blocker_tx.send(result).unwrap(),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while acceptance_engine_counts().0 != 1 && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }

    let (panic_tx, panic_rx) = std::sync::mpsc::channel();
    enqueue_transfer(
        format!("panicking-worker-{}", uuid::Uuid::new_v4()),
        live_binding(102),
        Arc::new(CancelState::new()),
        || {},
        || panic!("injected worker panic"),
        move |result, _| {
            panic_tx.send(result).unwrap();
            panic!("injected finished callback panic");
        },
    )
    .unwrap();
    let (follower_tx, follower_rx) = std::sync::mpsc::channel();
    enqueue_transfer(
        format!("panic-follower-{}", uuid::Uuid::new_v4()),
        live_binding(103),
        Arc::new(CancelState::new()),
        || {},
        || Ok(()),
        move |result, _| follower_tx.send(result).unwrap(),
    )
    .unwrap();
    assert!(
        panic_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap()
            .is_err()
    );
    assert!(
        follower_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap()
            .is_ok()
    );
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    assert!(
        blocker_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap()
            .is_ok()
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while acceptance_engine_counts() != (0, 0) && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(acceptance_engine_counts(), (0, 0));
}
