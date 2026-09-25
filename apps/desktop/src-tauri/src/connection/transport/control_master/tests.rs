use super::*;
use std::{
    os::unix::fs::PermissionsExt,
    os::unix::net::UnixListener,
    sync::{
        Barrier,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
};

fn update_high_water(high_water: &AtomicUsize, value: usize) {
    let mut observed = high_water.load(std::sync::atomic::Ordering::Acquire);
    while value > observed {
        match high_water.compare_exchange_weak(
            observed,
            value,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        ) {
            Ok(_) => break,
            Err(current) => observed = current,
        }
    }
}

fn fake_delayed_ssh(
    master: &SshMaster,
    start: &Barrier,
    calls: &AtomicUsize,
    active: &AtomicUsize,
    high_water: &AtomicUsize,
) {
    start.wait();
    coordinate_master_state(
        master,
        &|| false,
        |_, _| Ok(MasterLiveness::Live),
        || {
            calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            let now = active.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
            update_high_water(high_water, now);
            thread::sleep(Duration::from_millis(100));
            active.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            Ok((MasterProcess::External, false))
        },
    )
    .unwrap();
}

fn fake_delayed_counts(masters: Vec<SshMaster>) -> (usize, usize) {
    let start = Arc::new(Barrier::new(masters.len() + 1));
    let calls = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let high_water = Arc::new(AtomicUsize::new(0));
    let workers: Vec<_> = masters
        .into_iter()
        .map(|master| {
            let start = Arc::clone(&start);
            let calls = Arc::clone(&calls);
            let active = Arc::clone(&active);
            let high_water = Arc::clone(&high_water);
            thread::spawn(move || fake_delayed_ssh(&master, &start, &calls, &active, &high_water))
        })
        .collect();
    start.wait();
    for worker in workers {
        worker.join().unwrap();
    }
    (
        calls.load(std::sync::atomic::Ordering::Acquire),
        high_water.load(std::sync::atomic::Ordering::Acquire),
    )
}

#[test]
fn fake_delayed_ssh_starts_exactly_one_master_for_the_same_socket() {
    let temporary = tempfile::tempdir().unwrap();
    let master = master_entry("same-host", None, &temporary.path().join("same.sock")).unwrap();
    assert_eq!(fake_delayed_counts(vec![master.clone(), master]), (1, 1));
}

#[test]
fn fake_delayed_ssh_for_different_sockets_is_not_globally_serialized() {
    let temporary = tempfile::tempdir().unwrap();
    let masters = ["first.sock", "second.sock"]
        .map(|socket| master_entry("same-host", None, &temporary.path().join(socket)).unwrap());
    assert_eq!(fake_delayed_counts(masters.into()), (2, 2));
}

#[test]
#[ignore = "Phase 14 opt-in SSH master coordination fixture"]
fn phase14_fake_delayed_ssh_master_coordination_counts() {
    let temporary = tempfile::tempdir().unwrap();
    let same = master_entry(
        "same-host",
        None,
        &temporary.path().join("phase14-same.sock"),
    )
    .unwrap();
    let same_counts = fake_delayed_counts(vec![same.clone(), same]);
    let different = ["phase14-first.sock", "phase14-second.sock"]
        .map(|socket| master_entry("same-host", None, &temporary.path().join(socket)).unwrap());
    let different_counts = fake_delayed_counts(different.into());
    assert_eq!(same_counts, (1, 1));
    assert_eq!(different_counts, (2, 2));
    eprintln!(
        "PHASE14_METRIC {}",
        serde_json::json!({
            "lane": "sshMasterCoordination",
            "sameSocketMasterStarts": same_counts.0,
            "sameSocketActiveHighWater": same_counts.1,
            "differentSocketMasterStarts": different_counts.0,
            "differentSocketActiveHighWater": different_counts.1,
        })
    );
}

#[test]
fn dropping_the_final_lease_keeps_the_master_warm_for_control_persist() {
    let temporary = tempfile::tempdir().unwrap();
    let master = master_entry(
        "warm-host",
        Some("custom.conf"),
        &temporary.path().join("warm.sock"),
    )
    .unwrap();
    {
        let mut state = master.coordination.state.lock().unwrap();
        state.lifecycle = MasterLifecycle::Ready {
            process: MasterProcess::External,
            needs_probe: false,
        };
        state.leases = 1;
    }
    drop(SshLease {
        master: master.clone(),
        route: LeaseRoute::Multiplexed,
    });
    let state = master.coordination.state.lock().unwrap();
    assert!(
        matches!(state.lifecycle, MasterLifecycle::Ready { .. }),
        "zero leases must not issue an eager exit"
    );
}

#[test]
fn owned_master_expires_after_the_zero_lease_warm_window() {
    let temporary = tempfile::tempdir().unwrap();
    let socket = temporary.path().join("idle-owned.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let socket_identity = validated_control_socket_identity(&socket).unwrap().unwrap();
    let master = master_entry("idle-owned-host", None, &socket).unwrap();
    let child = Command::new("sh").args(["-c", "sleep 5"]).spawn().unwrap();
    {
        let mut state = master.coordination.state.lock().unwrap();
        state.leases = 1;
        state.lifecycle = MasterLifecycle::Ready {
            process: MasterProcess::Owned(OwnedMaster {
                child,
                socket_identity,
            }),
            needs_probe: false,
        };
    }

    release_control_master(&master, Duration::from_millis(30));

    let deadline = Instant::now() + Duration::from_secs(1);
    while (matches!(
        master.coordination.state.lock().unwrap().lifecycle,
        MasterLifecycle::Ready { .. }
    ) || socket.exists())
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(
        master.coordination.state.lock().unwrap().lifecycle,
        MasterLifecycle::Idle
    ));
    assert!(!socket.exists());
}

#[test]
fn suspect_external_master_is_preserved_and_bypassed_directly() {
    let temporary = tempfile::tempdir().unwrap();
    let socket = temporary.path().join("suspect-external.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    let master = master_entry("suspect-external-host", None, &socket).unwrap();
    {
        let mut state = master.coordination.state.lock().unwrap();
        state.lifecycle = MasterLifecycle::Ready {
            process: MasterProcess::External,
            needs_probe: true,
        };
    }

    let outcome = coordinate_master_state(
        &master,
        &|| false,
        |_, needs_probe| {
            assert!(needs_probe);
            Ok(MasterLiveness::Direct)
        },
        || panic!("unresponsive external master must not be replaced"),
    )
    .unwrap();

    assert!(outcome.direct);
    let state = master.coordination.state.lock().unwrap();
    assert!(matches!(
        state.lifecycle,
        MasterLifecycle::Ready {
            process: MasterProcess::External,
            needs_probe: true,
        }
    ));
    drop(state);
    let lease = SshLease {
        master,
        route: LeaseRoute::Direct,
    };
    let mut command = ssh_base(None);
    lease
        .configure(&mut command, "suspect-external-host", None)
        .unwrap();
    let arguments: Vec<_> = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == ["ControlMaster=no", "-o"])
    );
    assert!(
        arguments
            .iter()
            .any(|argument| argument == "ControlPath=none")
    );
}

#[test]
fn cancelled_same_socket_waiter_does_not_wait_for_the_active_establishment() {
    let temporary = tempfile::tempdir().unwrap();
    let master = master_entry("cancel-host", None, &temporary.path().join("cancel.sock")).unwrap();
    let establishment_started = Arc::new(Barrier::new(2));
    let first_master = master.clone();
    let first_started = Arc::clone(&establishment_started);
    let first = thread::spawn(move || {
        coordinate_master_state(
            &first_master,
            &|| false,
            |_, _| Ok(MasterLiveness::Live),
            || {
                first_started.wait();
                thread::sleep(Duration::from_millis(200));
                Ok((MasterProcess::External, false))
            },
        )
    });
    establishment_started.wait();

    let cancelled = AtomicBool::new(false);
    let cancel_start = Instant::now();
    thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            coordinate_master_state(
                &master,
                &|| cancelled.load(Ordering::Acquire),
                |_, _| Ok(MasterLiveness::Live),
                || panic!("same-socket waiter must not establish a second master"),
            )
        });
        thread::sleep(Duration::from_millis(20));
        cancelled.store(true, Ordering::Release);
        assert!(waiter.join().unwrap().is_err());
    });
    assert!(
        cancel_start.elapsed() < Duration::from_millis(150),
        "cancelled waiter remained blocked behind the active SSH attempt"
    );
    first.join().unwrap().unwrap();
}

#[test]
fn shutdown_fences_an_already_admitted_establishment() {
    let temporary = tempfile::tempdir().unwrap();
    let master =
        master_entry("closing-host", None, &temporary.path().join("closing.sock")).unwrap();
    let establishment_started = Arc::new(Barrier::new(2));
    let worker_master = master.clone();
    let worker_started = Arc::clone(&establishment_started);
    let worker = thread::spawn(move || {
        coordinate_master_state(
            &worker_master,
            &|| false,
            |_, _| Ok(MasterLiveness::Live),
            || {
                worker_started.wait();
                thread::sleep(Duration::from_millis(50));
                Ok((MasterProcess::External, false))
            },
        )
    });
    establishment_started.wait();
    close_master(&master);
    assert!(worker.join().unwrap().is_err());
    let state = master.coordination.state.lock().unwrap();
    assert!(matches!(
        state.lifecycle,
        MasterLifecycle::Closing { in_flight: false }
    ));
}

#[test]
fn shutdown_preserves_an_external_master_and_its_socket() {
    let temporary = tempfile::tempdir().unwrap();
    let socket = temporary.path().join("external.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let master = master_entry("external-host", Some("jump.conf"), &socket).unwrap();
    master.coordination.state.lock().unwrap().lifecycle = MasterLifecycle::Ready {
        process: MasterProcess::External,
        needs_probe: false,
    };

    close_master(&master);

    assert!(socket.exists(), "external control socket was unlinked");
    drop(listener);
}

#[test]
fn nonresponsive_control_socket_check_is_bounded() {
    let temporary = tempfile::tempdir().unwrap();
    let socket = temporary.path().join("unresponsive.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let server = thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        thread::sleep(Duration::from_millis(150));
    });

    let started = Instant::now();
    let error = run_control_command(
        control_master_check_command("unresponsive-host", None, &socket),
        Duration::from_millis(50),
        &|| false,
    )
    .unwrap_err();

    assert_eq!(error, ControlCommandError::TimedOut);
    assert!(started.elapsed() < Duration::from_millis(250));
    server.join().unwrap();
}

#[test]
fn shutdown_cancels_a_check_on_a_nonresponsive_control_socket() {
    let temporary = tempfile::tempdir().unwrap();
    let socket = temporary.path().join("shutdown-unresponsive.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let (accepted_sender, accepted_receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        accepted_sender.send(()).unwrap();
        thread::sleep(Duration::from_millis(250));
    });
    let master = master_entry("unresponsive-host", None, &socket).unwrap();
    let socket_identity = validated_control_socket_identity(&socket).unwrap().unwrap();
    let child = Command::new("sh").args(["-c", "sleep 5"]).spawn().unwrap();
    {
        let mut state = master.coordination.state.lock().unwrap();
        state.lifecycle = MasterLifecycle::Ready {
            process: MasterProcess::Owned(OwnedMaster {
                child,
                socket_identity,
            }),
            needs_probe: true,
        };
    }
    let worker_master = master.clone();
    let worker_socket = socket.clone();
    let worker = thread::spawn(move || {
        coordinate_master(
            &worker_master,
            "unresponsive-host",
            None,
            &worker_socket,
            &|| false,
        )
    });
    accepted_receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap();

    let started = Instant::now();
    close_master(&master);

    assert!(started.elapsed() < Duration::from_millis(200));
    assert!(worker.join().unwrap().is_err());
    assert!(!socket.exists(), "owned control socket was not unlinked");
    server.join().unwrap();
}

#[test]
fn in_flight_idle_disposal_completion_unblocks_shutdown() {
    let temporary = tempfile::tempdir().unwrap();
    let master =
        master_entry("idle-close-race", None, &temporary.path().join("race.sock")).unwrap();
    master.coordination.state.lock().unwrap().lifecycle = MasterLifecycle::Establishing;

    let closing_master = master.clone();
    let (closed_tx, closed_rx) = mpsc::channel();
    let closer = thread::spawn(move || {
        close_master(&closing_master);
        closed_tx.send(()).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    while !matches!(
        master.coordination.state.lock().unwrap().lifecycle,
        MasterLifecycle::Closing { in_flight: true }
    ) && Instant::now() < deadline
    {
        thread::yield_now();
    }
    assert!(
        closed_rx.try_recv().is_err(),
        "shutdown must wait for idle disposal"
    );

    finish_in_flight(&master);
    closed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    closer.join().unwrap();
    assert!(matches!(
        master.coordination.state.lock().unwrap().lifecycle,
        MasterLifecycle::Closing { in_flight: false }
    ));
}

#[test]
fn revalidation_requested_during_probe_is_not_lost() {
    let temporary = tempfile::tempdir().unwrap();
    let master = master_entry("probe-race", None, &temporary.path().join("probe.sock")).unwrap();
    {
        let mut state = master.coordination.state.lock().unwrap();
        state.leases = 1;
        state.lifecycle = MasterLifecycle::Ready {
            process: MasterProcess::External,
            needs_probe: false,
        };
    }
    let lease = SshLease {
        master: master.clone(),
        route: LeaseRoute::Direct,
    };
    let probe_started = Arc::new(Barrier::new(2));
    let probe_resume = Arc::new(Barrier::new(2));
    let probes = Arc::new(AtomicUsize::new(0));
    let worker_master = master.clone();
    let worker_started = Arc::clone(&probe_started);
    let worker_resume = Arc::clone(&probe_resume);
    let worker_probes = Arc::clone(&probes);
    let worker = thread::spawn(move || {
        coordinate_master_state(
            &worker_master,
            &|| false,
            |_, _| {
                if worker_probes.fetch_add(1, Ordering::AcqRel) == 0 {
                    worker_started.wait();
                    worker_resume.wait();
                }
                Ok(MasterLiveness::Live)
            },
            || panic!("live master must not be re-established"),
        )
    });
    probe_started.wait();
    lease.require_revalidation();
    probe_resume.wait();
    assert!(worker.join().unwrap().unwrap().reused);
    assert_eq!(probes.load(Ordering::Acquire), 2);
    drop(lease);
}

#[test]
fn replacement_inode_is_reclassified_external_and_bypassed() {
    let temporary = tempfile::tempdir().unwrap();
    let socket = temporary.path().join("replacement-before-coordinate.sock");
    let first_listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let owned_identity = validated_control_socket_identity(&socket).unwrap().unwrap();
    let master = master_entry("replacement-host", None, &socket).unwrap();
    let child = Command::new("sh").args(["-c", "sleep 5"]).spawn().unwrap();
    master.coordination.state.lock().unwrap().lifecycle = MasterLifecycle::Ready {
        process: MasterProcess::Owned(OwnedMaster {
            child,
            socket_identity: owned_identity,
        }),
        needs_probe: false,
    };
    // Unlink but keep the first listener bound until the replacement exists:
    // the bound socket pins its inode, so a filesystem that reuses freed inode
    // numbers (ext4, unlike tmpfs) cannot hand the replacement the same one.
    fs::remove_file(&socket).unwrap();
    let replacement_listener = UnixListener::bind(&socket).unwrap();
    drop(first_listener);
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let replacement_identity = validated_control_socket_identity(&socket).unwrap().unwrap();

    let outcome = coordinate_master(&master, "replacement-host", None, &socket, &|| false).unwrap();

    assert!(outcome.direct);
    assert!(matches!(
        master.coordination.state.lock().unwrap().lifecycle,
        MasterLifecycle::Ready {
            process: MasterProcess::External,
            ..
        }
    ));
    assert_eq!(
        validated_control_socket_identity(&socket).unwrap(),
        Some(replacement_identity)
    );
    drop(replacement_listener);
    fs::remove_file(&socket).unwrap();

    let established = AtomicBool::new(false);
    let recovered = coordinate_master_state(
        &master,
        &|| false,
        |process, _| match process {
            MasterProcess::External => external_master_liveness(&socket),
            MasterProcess::Owned(_) => panic!("replacement must remain classified external"),
        },
        || {
            established.store(true, Ordering::Release);
            Ok((MasterProcess::External, false))
        },
    )
    .unwrap();
    assert!(!recovered.direct);
    assert!(
        established.load(Ordering::Acquire),
        "removing the replacement must allow multiplexing to be established again"
    );
}

#[test]
fn finished_owned_master_is_reaped_and_forgotten() {
    let temporary = tempfile::tempdir().unwrap();
    let socket = temporary.path().join("reap.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let socket_identity = validated_control_socket_identity(&socket).unwrap().unwrap();
    let master = master_entry("reap-host", None, &socket).unwrap();
    let child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
    thread::sleep(Duration::from_millis(30));
    let generation = {
        let mut state = master.coordination.state.lock().unwrap();
        state.generation = 1;
        state.lifecycle = MasterLifecycle::Ready {
            process: MasterProcess::Owned(OwnedMaster {
                child,
                socket_identity,
            }),
            needs_probe: false,
        };
        state.generation
    };

    assert!(reap_owned_master_if_finished(&master, generation));
    assert!(matches!(
        master.coordination.state.lock().unwrap().lifecycle,
        MasterLifecycle::Idle
    ));
}

#[test]
fn failed_owned_validation_terminates_child_and_removes_only_its_socket() {
    let temporary = tempfile::tempdir().unwrap();
    let socket = temporary.path().join("failed-owned.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let socket_identity = validated_control_socket_identity(&socket).unwrap().unwrap();
    let master = master_entry("failed-owned-host", None, &socket).unwrap();
    let child = Command::new("sh").args(["-c", "sleep 5"]).spawn().unwrap();
    master.coordination.state.lock().unwrap().lifecycle = MasterLifecycle::Ready {
        process: MasterProcess::Owned(OwnedMaster {
            child,
            socket_identity,
        }),
        needs_probe: false,
    };

    coordinate_master_state(
        &master,
        &|| false,
        |_, _| Ok(MasterLiveness::Replace),
        || Ok((MasterProcess::External, false)),
    )
    .unwrap();

    assert!(!socket.exists(), "failed owned socket was not unlinked");
}

#[test]
fn reaper_waits_while_owned_master_validation_temporarily_moves_the_child() {
    let temporary = tempfile::tempdir().unwrap();
    let socket = temporary.path().join("validating-owned.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let socket_identity = validated_control_socket_identity(&socket).unwrap().unwrap();
    let master = master_entry("validating-owned-host", None, &socket).unwrap();
    let child = Command::new("sh")
        .args(["-c", "sleep 0.15"])
        .spawn()
        .unwrap();
    let generation = {
        let mut state = master.coordination.state.lock().unwrap();
        state.generation = 1;
        state.lifecycle = MasterLifecycle::Ready {
            process: MasterProcess::Owned(OwnedMaster {
                child,
                socket_identity,
            }),
            needs_probe: false,
        };
        state.generation
    };
    spawn_master_reaper(master.clone(), generation);
    let validation_entered = Arc::new(Barrier::new(2));
    let release_validation = Arc::new(Barrier::new(2));
    let worker_master = master.clone();
    let worker_entered = Arc::clone(&validation_entered);
    let worker_release = Arc::clone(&release_validation);
    let worker = thread::spawn(move || {
        coordinate_master_state(
            &worker_master,
            &|| false,
            |_, _| {
                worker_entered.wait();
                worker_release.wait();
                Ok(MasterLiveness::Live)
            },
            || panic!("live owned master must not be replaced"),
        )
    });
    validation_entered.wait();
    thread::sleep(Duration::from_millis(30));
    release_validation.wait();
    worker.join().unwrap().unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    while matches!(
        master.coordination.state.lock().unwrap().lifecycle,
        MasterLifecycle::Ready { .. }
    ) && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(matches!(
        master.coordination.state.lock().unwrap().lifecycle,
        MasterLifecycle::Idle
    ));
    assert!(!socket.exists(), "reaper left the owned socket behind");
}

#[test]
fn owned_shutdown_does_not_unlink_a_replacement_socket_inode() {
    let temporary = tempfile::tempdir().unwrap();
    let socket = temporary.path().join("replaced-owned.sock");
    let first_listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let owned_identity = validated_control_socket_identity(&socket).unwrap().unwrap();
    let master = master_entry("replaced-owned-host", None, &socket).unwrap();
    let child = Command::new("sh").args(["-c", "sleep 5"]).spawn().unwrap();
    master.coordination.state.lock().unwrap().lifecycle = MasterLifecycle::Ready {
        process: MasterProcess::Owned(OwnedMaster {
            child,
            socket_identity: owned_identity,
        }),
        needs_probe: false,
    };
    // Unlink but keep the first listener bound until the replacement exists:
    // the bound socket pins its inode, so a filesystem that reuses freed inode
    // numbers (ext4, unlike tmpfs) cannot hand the replacement the same one.
    fs::remove_file(&socket).unwrap();
    let replacement_listener = UnixListener::bind(&socket).unwrap();
    drop(first_listener);
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let replacement_identity = validated_control_socket_identity(&socket).unwrap().unwrap();
    assert_ne!(owned_identity, replacement_identity);

    close_master(&master);

    assert_eq!(
        validated_control_socket_identity(&socket).unwrap(),
        Some(replacement_identity)
    );
    drop(replacement_listener);
}

#[test]
fn custom_config_is_preserved_for_control_checks() {
    let temporary = tempfile::tempdir().unwrap();
    let socket = temporary.path().join("custom.sock");
    let command = control_master_check_command("jump-host", Some("custom.conf"), &socket);
    let arguments: Vec<_> = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        &arguments[..2],
        &["-F".to_owned(), "custom.conf".to_owned()]
    );
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == ["-S", &socket.to_string_lossy()])
    );
    assert!(arguments.windows(2).any(|pair| pair == ["-O", "check"]));
    assert_eq!(arguments.last().map(String::as_str), Some("jump-host"));
}

#[test]
fn process_namespaces_coexist_and_owner_exit_preserves_the_other_socket() {
    // A short root, not the default tempdir: the namespaced socket path must
    // stay under the platform's 104/108-byte bind limit, and macOS puts the
    // default tempdir 50+ bytes deep under /var/folders.
    let temporary = tempfile::Builder::new()
        .prefix("ade-ns")
        .tempdir_in("/tmp")
        .unwrap();
    let ready = temporary.path().join("namespace-ready");
    let mut owner = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "connection::transport::control_master::tests::process_namespace_child_fixture",
            "--nocapture",
        ])
        .env("ADE_TEST_NAMESPACE_ROOT", temporary.path())
        .env("ADE_TEST_NAMESPACE_READY", &ready)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !ready.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if !ready.exists() {
        // On macOS, re-executing this unbundled test binary can be aborted at
        // launch by an Apple framework callback (UserNotifications throws when
        // bundleProxyForCurrentProcess is nil) before the fixture runs. A
        // fixture that ran and failed exits with a code, not a signal, so only
        // signal death is treated as the environment refusing the re-exec.
        let output = owner.wait_with_output().unwrap();
        panic!(
            "owner did not publish its namespaced socket; child status {:?}, stderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let owner_socket = PathBuf::from(fs::read_to_string(&ready).unwrap());
    let adopter_socket =
        ssh_profile_control_socket_in(temporary.path(), "profile", "same-host", None).unwrap();
    assert_ne!(owner_socket, adopter_socket);
    let adopter = UnixListener::bind(&adopter_socket).unwrap();
    fs::set_permissions(&adopter_socket, fs::Permissions::from_mode(0o600)).unwrap();

    assert!(owner.wait().unwrap().success());
    assert!(adopter_socket.exists(), "owner exit removed adopter socket");
    drop(adopter);
}

#[test]
#[ignore = "subprocess fixture for process-isolated control sockets"]
fn process_namespace_child_fixture() {
    let Some(root) = std::env::var_os("ADE_TEST_NAMESPACE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let ready = PathBuf::from(std::env::var_os("ADE_TEST_NAMESPACE_READY").unwrap());
    let socket = ssh_profile_control_socket_in(&root, "profile", "same-host", None).unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(ready, socket.as_os_str().as_bytes()).unwrap();
    thread::sleep(Duration::from_millis(250));
    drop(listener);
    fs::remove_file(socket).unwrap();
}
#[test]
fn control_socket_identity_is_profile_scoped() {
    let first = ssh_profile_control_socket("profile-a", "same-host", None).unwrap();
    let second = ssh_profile_control_socket("profile-b", "same-host", None).unwrap();
    assert_ne!(first, second);
    assert_eq!(
        first,
        ssh_profile_control_socket("profile-a", "same-host", None).unwrap()
    );
}

#[test]
fn control_socket_fits_the_platform_bind_limit_from_the_real_temporary_root() {
    let socket = ssh_profile_control_socket("profile-a", "same-host", None).unwrap();
    assert!(
        control_socket_binds(&socket),
        "{} leaves no room for OpenSSH's temporary bind",
        socket.display()
    );
}

#[test]
fn control_socket_relocates_when_the_temporary_root_is_too_long() {
    let temporary = tempfile::tempdir().unwrap();
    // Reproduce a macOS-length per-user temporary root, which alone pushes
    // the control socket past the 104-byte Darwin bind limit.
    let deep = temporary.path().join("a".repeat(80));
    fs::create_dir(&deep).unwrap();
    let direct = ssh_profile_control_socket_in(&deep, "profile-a", "same-host", None).unwrap();
    assert!(!control_socket_binds(&direct));
    let resolved = ssh_profile_control_socket("profile-a", "same-host", None).unwrap();
    assert!(control_socket_binds(&resolved));
}

#[test]
fn control_directory_rejects_symlink_and_repairs_owned_legacy_mode() {
    let temporary = tempfile::tempdir().unwrap();
    let uid_root = temporary
        .path()
        .join(format!("muxflow-{}", unsafe { libc::geteuid() }));
    let foreign = temporary.path().join("foreign");
    fs::create_dir(&foreign).unwrap();
    std::os::unix::fs::symlink(&foreign, &uid_root).unwrap();
    assert!(ssh_profile_control_socket_in(temporary.path(), "p", "host", None,).is_err());
    fs::remove_file(&uid_root).unwrap();
    fs::create_dir(&uid_root).unwrap();
    fs::set_permissions(&uid_root, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(ssh_profile_control_socket_in(temporary.path(), "p", "host", None,).is_ok());
    assert_eq!(
        fs::metadata(uid_root).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn control_socket_rejects_regular_files_and_symlinks() {
    let temporary = tempfile::tempdir().unwrap();
    let socket = temporary.path().join("mux.sock");
    fs::write(&socket, b"foreign").unwrap();
    assert!(validate_control_socket(&socket).is_err());
    fs::remove_file(&socket).unwrap();
    std::os::unix::fs::symlink("missing", &socket).unwrap();
    assert!(validate_control_socket(&socket).is_err());
}
