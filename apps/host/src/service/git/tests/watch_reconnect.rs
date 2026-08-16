use super::*;

#[tokio::test]
async fn commit_surfaces_hook_stderr_and_watch_refreshes_external_edits() {
    let fixture = Fixture::new("watch-commit");
    fixture.write("file", b"one");
    fixture.git(&["add", "file"]);
    let hook = fixture.root.join(".git/hooks/pre-commit");
    fs::write(&hook, b"#!/bin/sh\necho hook-blocked >&2\nexit 17\n").unwrap();
    let mut mode = fs::metadata(&hook).unwrap().permissions();
    mode.set_mode(0o755);
    fs::set_permissions(&hook, mode).unwrap();
    let service = Arc::new(GitService::new());
    let status = service.status(&fixture.request()).unwrap();
    let mut commit = fixture.request();
    commit.repository_id = status.repository.unwrap().repository_id;
    commit.expected_status_generation = status.generation;
    commit.connection_epoch = 9;
    commit.commit_message = "blocked".into();
    let result = service
        .commit(commit, 9, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert_ne!(result.exit_code, 0);
    assert!(
        result
            .stderr
            .windows(12)
            .any(|value| value == b"hook-blocked")
    );

    let (sender, mut receiver) = mpsc::channel(4);
    let closed = Arc::new(AtomicBool::new(false));
    let mut watch = fixture.request();
    watch.watch_id = "watch-1".into();
    service.watch(watch, sender, Arc::clone(&closed)).unwrap();
    fixture.write("external", b"changed");
    let event = tokio::time::timeout(Duration::from_secs(3), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        event,
        SequencerControl::OrderedEvent(v1::HostEvent { git: Some(_), .. })
    ));
    service.unwatch("watch-1").unwrap();
    closed.store(true, Ordering::Release);
}

#[tokio::test]
async fn replacing_watch_cancels_old_lease_and_same_porcelain_edit_emits_once() {
    let fixture = Fixture::new("watch-coalescing");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.write("file", b"aaaa\n");
    let service = Arc::new(GitService::new());
    let closed = Arc::new(AtomicBool::new(false));
    let mut watch = fixture.request();
    watch.watch_id = "same-lease".into();
    let (old_sender, mut old_receiver) = mpsc::channel(4);
    service
        .watch(watch.clone(), old_sender, Arc::clone(&closed))
        .unwrap();
    let (new_sender, mut new_receiver) = mpsc::channel(4);
    service
        .watch(watch, new_sender, Arc::clone(&closed))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(450)).await;
    fixture.write("file", b"bbbb\n");
    // Replacement can race an already-issued refresh from the retired lease.
    // Superseded/transient events are not the acceptance boundary: wait for
    // the replacement lease's authoritative status and then prove it emits no
    // duplicate for the same porcelain state.
    let event = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let event = new_receiver
                .recv()
                .await
                .expect("replacement Git watch closed before status");
            if matches!(
                event,
                SequencerControl::OrderedEvent(v1::HostEvent {
                    git: Some(v1::GitEvent {
                        status: Some(_),
                        ..
                    }),
                    ..
                })
            ) {
                break event;
            }
        }
    })
    .await
    .expect("replacement watch omitted authoritative Git status");
    let SequencerControl::OrderedEvent(v1::HostEvent {
        git: Some(v1::GitEvent {
            status: Some(status),
            ..
        }),
        ..
    }) = event
    else {
        panic!("replacement watch omitted authoritative Git status");
    };
    assert!(
        status
            .entries
            .iter()
            .any(|entry| entry.path == b"file" && entry.worktree_status == "M")
    );
    assert!(!matches!(
        tokio::time::timeout(Duration::from_millis(700), new_receiver.recv()).await,
        Ok(Some(_))
    ));
    assert!(!matches!(
        tokio::time::timeout(Duration::from_millis(700), old_receiver.recv()).await,
        Ok(Some(_))
    ));
    service.unwatch("same-lease").unwrap();
    closed.store(true, Ordering::Release);
}

#[test]
fn overlapping_watch_refreshes_publish_only_the_newest_snapshot_without_an_error() {
    let fixture = Fixture::new("watch-superseded-publication");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.write("file", b"newest\n");
    let service = GitService::new();
    let request = fixture.request();
    let newest = service.status(&request).unwrap();
    let repository_id = newest.repository.as_ref().unwrap().repository_id.clone();

    // Model an older overlapping refresh completing after the newest ticket.
    // The status service still reports this condition to direct callers.
    {
        let mut states = service.states.lock().unwrap();
        let state = states.get_mut(&repository_id).unwrap();
        state.applied_refresh = state.issued_refresh.saturating_add(2);
    }
    let stable_root = WorktreeRoot::capture(&request.root).unwrap();
    let superseded = service.status_with_repository(
        &request,
        &stable_root.stable_path(),
        newest.repository.clone().unwrap(),
        None,
    );
    assert_eq!(
        superseded.unwrap_err().to_string(),
        "Git status refresh superseded by a newer snapshot"
    );

    let mut source_generation = String::new();
    let mut last_error = String::new();
    let published = super::super::watch::watch_refresh_event(
        &request,
        &mut source_generation,
        &mut last_error,
        Ok(newest),
    )
    .expect("newest status should be published");
    assert!(published.status.is_some());
    assert!(published.error.is_empty());

    let stale_publication = super::super::watch::watch_refresh_event(
        &request,
        &mut source_generation,
        &mut last_error,
        Err(StatusRefreshSuperseded.into()),
    );
    assert!(stale_publication.is_none());
    assert!(last_error.is_empty());
}

#[tokio::test]
async fn watch_bootstrap_is_cancellable_and_holds_events_until_response_activation() {
    let fixture = Fixture::new("watch-bootstrap-order");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    let service = Arc::new(GitService::new());
    let mut request = fixture.request();
    request.watch_id = "bootstrap-order".into();
    let (sender, mut receiver) = mpsc::channel(8);
    let cancellation = Arc::new(AtomicBool::new(false));
    let bootstrap = service
        .watch_cancellable(
            request.clone(),
            sender,
            Arc::new(AtomicBool::new(false)),
            Arc::clone(&cancellation),
        )
        .unwrap();
    fixture.write("file", b"changed before response\n");
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(receiver.try_recv().is_err());
    bootstrap.activate.send(()).unwrap();
    let event = tokio::time::timeout(Duration::from_secs(3), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event, SequencerControl::OrderedEvent(_)));
    service.unwatch("bootstrap-order").unwrap();

    let mut cancelled_request = fixture.request();
    cancelled_request.watch_id = "cancelled-bootstrap".into();
    let cancelled = Arc::new(AtomicBool::new(true));
    let (sender, _) = mpsc::channel(1);
    assert!(
        service
            .watch_cancellable(
                cancelled_request,
                sender,
                Arc::new(AtomicBool::new(false)),
                cancelled,
            )
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert!(service.unwatch("cancelled-bootstrap").is_err());
}

#[tokio::test]
#[ignore = "Phase 14 opt-in 32-consumer measurement fixture"]
async fn phase14_thirty_two_consumers_report_native_watchers_and_status_processes() {
    let fixture = Fixture::new("phase14-32-consumers");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    let service = Arc::new(GitService::new());
    let before = phase14_git_snapshot();
    let closed = Arc::new(AtomicBool::new(false));
    let mut receivers = Vec::new();
    for consumer in 0..32 {
        let mut request = fixture.request();
        request.watch_id = format!("phase14-watch-{consumer}");
        let (sender, receiver) = mpsc::channel(2);
        service.watch(request, sender, Arc::clone(&closed)).unwrap();
        receivers.push(receiver);
    }
    let active = phase14_git_snapshot();
    assert_eq!(
        active.native_watcher_creations - before.native_watcher_creations,
        32
    );
    assert_eq!(active.native_watchers - before.native_watchers, 32);
    assert_eq!(active.subscribers - before.subscribers, 32);
    assert!(active.status_processes - before.status_processes >= 32);
    println!(
        "PHASE14_METRIC {}",
        serde_json::json!({
            "lane": "git32Consumers",
            "consumers": 32,
            "nativeWatcherCreations": active.native_watcher_creations - before.native_watcher_creations,
            "nativeWatchers": active.native_watchers - before.native_watchers,
            "subscriberHighWater": active.subscribers_high_water,
            "gitProcesses": active.git_processes - before.git_processes,
            "statusProcesses": active.status_processes - before.status_processes,
            "diffProcesses": active.diff_processes - before.diff_processes,
            "mutationProcesses": active.mutation_processes - before.mutation_processes,
            "activeProcessHighWater": active.active_processes_high_water,
        })
    );
    for consumer in 0..32 {
        service
            .unwatch(&format!("phase14-watch-{consumer}"))
            .unwrap();
    }
    closed.store(true, Ordering::Release);
    drop(receivers);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while phase14_git_snapshot().native_watchers > before.native_watchers
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let released = phase14_git_snapshot();
    assert_eq!(released.subscribers, before.subscribers);
    assert_eq!(released.native_watchers, before.native_watchers);
}
