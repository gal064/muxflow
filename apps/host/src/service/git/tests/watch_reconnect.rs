use super::*;

/// Waits for the next Git status event a watch lease produced.
async fn next_status(
    receiver: &mut mpsc::Receiver<SequencerControl>,
) -> Option<v1::GitStatusSnapshot> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let event = receiver.recv().await?;
            if let SequencerControl::OrderedEvent(v1::HostEvent {
                git:
                    Some(v1::GitEvent {
                        status: Some(status),
                        ..
                    }),
                ..
            }) = event
            {
                return Some(status);
            }
        }
    })
    .await
    .ok()
    .flatten()
}

/// Waits for a service's watchers and subscriptions to fall back to zero.
async fn wait_for_release(service: &GitService) -> measurements::GitObservationCounts {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let current = service.observation();
        if (current.native_watchers == 0 && current.subscribers == 0)
            || std::time::Instant::now() >= deadline
        {
            return current;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

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
    let closed = Arc::new(AtomicBool::new(false));
    let service = Arc::new(GitService::new(Arc::clone(&closed), 0));
    let status = service.status(&fixture.request(), None).await.unwrap();
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
    let mut watch = fixture.request();
    watch.watch_id = "watch-1".into();
    service.watch_activated(watch, sender).await.unwrap();
    fixture.write("external", b"changed");
    assert!(next_status(&mut receiver).await.is_some());
    service.unwatch("watch-1").unwrap();
    closed.store(true, Ordering::Release);
}

#[tokio::test]
async fn replacing_watch_retires_the_old_lease_and_one_edit_emits_once() {
    let fixture = Fixture::new("watch-coalescing");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.write("file", b"aaaa\n");
    let closed = Arc::new(AtomicBool::new(false));
    let service = Arc::new(GitService::new(Arc::clone(&closed), 0));
    let mut watch = fixture.request();
    watch.watch_id = "same-lease".into();
    let (old_sender, mut old_receiver) = mpsc::channel(4);
    service
        .watch_activated(watch.clone(), old_sender)
        .await
        .unwrap();
    let (new_sender, mut new_receiver) = mpsc::channel(4);
    service.watch_activated(watch, new_sender).await.unwrap();
    tokio::time::sleep(Duration::from_millis(450)).await;
    fixture.write("file", b"bbbb\n");
    let status = next_status(&mut new_receiver)
        .await
        .expect("replacement watch omitted authoritative Git status");
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
    // The retired lease's sender was replaced in the shared subscriber table,
    // so it receives nothing after replacement.
    assert!(!matches!(
        tokio::time::timeout(Duration::from_millis(700), old_receiver.recv()).await,
        Ok(Some(_))
    ));
    service.unwatch("same-lease").unwrap();
    closed.store(true, Ordering::Release);
}

#[tokio::test]
async fn concurrent_consumers_share_one_status_pipeline_and_one_native_watcher() {
    let fixture = Fixture::new("watch-multiplexing");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    let closed = Arc::new(AtomicBool::new(false));
    let service = Arc::new(GitService::new(Arc::clone(&closed), 0));

    let mut receivers = Vec::new();
    let mut first = fixture.request();
    first.watch_id = "shared-0".into();
    let (sender, receiver) = mpsc::channel(4);
    service.watch_activated(first, sender).await.unwrap();
    receivers.push(receiver);
    // The repository fixture's own setup writes are still settling; let the
    // shared watcher drain them before measuring what the other seven cost.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let settled = service.observation();

    for consumer in 1..8 {
        let mut request = fixture.request();
        request.watch_id = format!("shared-{consumer}");
        let (sender, receiver) = mpsc::channel(4);
        service.watch_activated(request, sender).await.unwrap();
        receivers.push(receiver);
    }
    let subscribed = service.observation();
    assert_eq!(
        subscribed.native_watcher_creations, 1,
        "eight consumers must share one native watcher"
    );
    assert_eq!(
        subscribed.watch_registrations, 1,
        "worktree, git dir and common dir collapse to one recursive registration"
    );
    assert_eq!(subscribed.subscribers, 8);
    assert_eq!(
        subscribed.status_pipelines, settled.status_pipelines,
        "seven further consumers reuse the shared snapshot"
    );

    // An explicitly requested status is the user's only recovery from a
    // filesystem watcher that missed something, so it always reads afresh —
    // exactly once, not once per consumer.
    service.status(&fixture.request(), None).await.unwrap();
    assert_eq!(
        service.observation().status_pipelines,
        settled.status_pipelines + 1
    );

    for consumer in 0..8 {
        service.unwatch(&format!("shared-{consumer}")).unwrap();
    }
    closed.store(true, Ordering::Release);
    drop(receivers);
    let released = wait_for_release(&service).await;
    assert_eq!(released.subscribers, 0);
    assert_eq!(released.native_watchers, 0);
}

#[tokio::test]
async fn a_mutation_publishes_exactly_one_post_command_status_refresh() {
    let fixture = Fixture::new("watch-mutation-reconciliation");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.write("file", b"changed\n");
    let closed = Arc::new(AtomicBool::new(false));
    let service = Arc::new(GitService::new(Arc::clone(&closed), 0));
    let (sender, mut receiver) = mpsc::channel(8);
    let mut watch = fixture.request();
    watch.watch_id = "mutation-watch".into();
    let bootstrap = service.watch_activated(watch, sender).await.unwrap();
    // Let the shared watcher settle so the bootstrap's own filesystem noise
    // cannot be mistaken for the mutation's refresh.
    tokio::time::sleep(Duration::from_millis(500)).await;
    while receiver.try_recv().is_ok() {}

    let mut request = fixture.request();
    request.repository_id = bootstrap.repository.clone().unwrap().repository_id;
    request.expected_status_generation = service
        .status(&fixture.request(), None)
        .await
        .unwrap()
        .generation;
    request.path = b"file".to_vec();
    request.diff_target = v1::GitDiffTarget::Unstaged.into();
    request.mutation = v1::GitMutationKind::StageFile.into();
    request.connection_epoch = 5;
    let before = service.observation();
    let result = service
        .mutate(request, 5, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert_eq!(result.outcome, v1::GitCommandOutcome::Applied as i32);
    let carried = result
        .status
        .expect("mutation carries authoritative status");
    assert!(
        carried
            .entries
            .iter()
            .any(|entry| entry.path == b"file" && entry.index_status == "M")
    );

    let published = next_status(&mut receiver)
        .await
        .expect("subscribers receive the post-command status");
    assert_eq!(published.source_generation, carried.source_generation);
    // One authoritative pre-command check plus exactly one post-command
    // refresh, with the watcher's own wake-up coalescing onto the latter.
    tokio::time::sleep(Duration::from_millis(900)).await;
    let after = service.observation();
    let pipelines = after.status_pipelines - before.status_pipelines;
    assert!(
        (2..=3).contains(&pipelines),
        "a mutation runs one pre-command and one post-command pipeline; a third \
         is possible only when a filesystem signal lands after the post-command \
         refresh already started, and was {pipelines}"
    );
    assert!(!matches!(
        tokio::time::timeout(Duration::from_millis(200), receiver.recv()).await,
        Ok(Some(_))
    ));

    service.unwatch("mutation-watch").unwrap();
    closed.store(true, Ordering::Release);
}

#[tokio::test]
async fn watch_bootstrap_is_cancellable_and_holds_events_until_response_activation() {
    let fixture = Fixture::new("watch-bootstrap-order");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    let closed = Arc::new(AtomicBool::new(false));
    let service = Arc::new(GitService::new(Arc::clone(&closed), 0));
    let mut request = fixture.request();
    request.watch_id = "bootstrap-order".into();
    let (sender, mut receiver) = mpsc::channel(8);
    let bootstrap = service
        .watch(request.clone(), sender, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    fixture.write("file", b"changed before response\n");
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(receiver.try_recv().is_err());
    bootstrap.activate.activate();
    assert!(next_status(&mut receiver).await.is_some());
    service.unwatch("bootstrap-order").unwrap();

    let mut cancelled_request = fixture.request();
    cancelled_request.watch_id = "cancelled-bootstrap".into();
    let (sender, _) = mpsc::channel(1);
    assert!(
        service
            .watch(cancelled_request, sender, Arc::new(AtomicBool::new(true)))
            .await
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert!(service.unwatch("cancelled-bootstrap").is_err());
    closed.store(true, Ordering::Release);
}

#[tokio::test]
async fn dropping_an_unactivated_bootstrap_leaves_no_subscriber_or_watcher() {
    let fixture = Fixture::new("watch-abandoned-bootstrap");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    let closed = Arc::new(AtomicBool::new(false));
    let service = Arc::new(GitService::new(Arc::clone(&closed), 0));
    let mut request = fixture.request();
    request.watch_id = "abandoned".into();
    let (sender, _receiver) = mpsc::channel(2);
    let bootstrap = service
        .watch(request, sender, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    drop(bootstrap);
    let released = wait_for_release(&service).await;
    assert_eq!(released.subscribers, 0);
    assert_eq!(released.native_watchers, 0);
    closed.store(true, Ordering::Release);
}

#[tokio::test]
async fn an_untouched_watched_repository_runs_no_further_status_pipelines() {
    let fixture = Fixture::new("watch-idle");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    let closed = Arc::new(AtomicBool::new(false));
    let service = Arc::new(GitService::new(Arc::clone(&closed), 0));
    let (sender, mut receiver) = mpsc::channel(8);
    let mut watch = fixture.request();
    watch.watch_id = "idle-watch".into();
    service.watch_activated(watch, sender).await.unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    let settled = service.observation();
    // Reading a repository opens its files, and inotify reports opens. If that
    // counted as a change, this refresh would schedule the next one forever.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        service.observation().status_pipelines,
        settled.status_pipelines,
        "an idle repository must not keep re-running Git status"
    );
    assert!(!matches!(
        tokio::time::timeout(Duration::from_millis(100), receiver.recv()).await,
        Ok(Some(_))
    ));
    service.unwatch("idle-watch").unwrap();
    closed.store(true, Ordering::Release);
}

#[tokio::test]
async fn a_directory_created_after_the_watch_is_observed() {
    let fixture = Fixture::new("watch-new-directory");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    let closed = Arc::new(AtomicBool::new(false));
    let service = Arc::new(GitService::new(Arc::clone(&closed), 0));
    let (sender, mut receiver) = mpsc::channel(8);
    let mut watch = fixture.request();
    watch.watch_id = "new-directory".into();
    service.watch_activated(watch, sender).await.unwrap();

    // The watch is registered against a path naming the worktree descriptor.
    // If that descriptor were released once registration returned, the path
    // would name a closed — and possibly reused — file descriptor, and the
    // watch on this new directory could never be added. An empty directory is
    // not a Git change, so nothing is published for it; the file written inside
    // it afterwards is only seen if the directory itself is watched.
    fs::create_dir(fixture.root.join("added")).unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    fixture.write("added/file", b"new\n");
    let status = next_status(&mut receiver).await.expect("a status refresh");
    assert!(
        status
            .entries
            .iter()
            .any(|entry| entry.path == b"added/file"),
        "the file created in a new directory must appear in status"
    );
    service.unwatch("new-directory").unwrap();
    closed.store(true, Ordering::Release);
}

#[tokio::test]
async fn a_diff_reads_status_once_on_an_unobserved_repository() {
    let fixture = Fixture::new("diff-one-pipeline");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.write("file", b"changed\n");
    let closed = Arc::new(AtomicBool::new(false));
    let service = Arc::new(GitService::new(Arc::clone(&closed), 0));
    let status = service.status(&fixture.request(), None).await.unwrap();
    let before = service.observation().status_pipelines;
    let mut request = fixture.request();
    request.repository_id = status.repository.unwrap().repository_id;
    request.path = b"file".to_vec();
    request.diff_target = v1::GitDiffTarget::Unstaged.into();
    let (_diff, landed) = service.diff(&request, None).await.unwrap();
    assert!(landed.authoritative);
    // Nothing invalidated the repository during the read, so the status the
    // diff was read against is the status it landed on. Reading it again would
    // be a second full pipeline that proves nothing.
    assert_eq!(
        service.observation().status_pipelines - before,
        1,
        "one diff must cost one status pipeline"
    );
    closed.store(true, Ordering::Release);
}

#[tokio::test]
#[ignore = "Phase 14 opt-in 32-consumer measurement fixture"]
async fn phase14_thirty_two_consumers_report_native_watchers_and_status_processes() {
    let fixture = Fixture::new("phase14-32-consumers");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    let closed = Arc::new(AtomicBool::new(false));
    let service = Arc::new(GitService::new(Arc::clone(&closed), 0));
    let before = measurements::phase14_git_process_snapshot();
    let mut receivers = Vec::new();
    for consumer in 0..32 {
        let mut request = fixture.request();
        request.watch_id = format!("phase14-watch-{consumer}");
        let (sender, receiver) = mpsc::channel(2);
        service.watch_activated(request, sender).await.unwrap();
        receivers.push(receiver);
    }
    let observation = service.observation();
    let active = measurements::phase14_git_process_snapshot();
    let watcher_creations = observation.native_watcher_creations;
    let native_watchers = observation.native_watchers;
    let watch_registrations = observation.watch_registrations;
    let status_pipelines = observation.status_pipelines;
    assert_eq!(watcher_creations, 1);
    assert_eq!(native_watchers, 1);
    assert_eq!(watch_registrations, 1);
    assert_eq!(status_pipelines, 1);
    assert_eq!(observation.discoveries, 1);
    assert_eq!(observation.subscribers, 32);
    let git_processes = active.git_processes - before.git_processes;
    let status_processes = active.status_processes - before.status_processes;
    let diff_processes = active.diff_processes - before.diff_processes;
    let mutation_processes = active.mutation_processes - before.mutation_processes;
    assert_eq!(status_processes, 1);
    // One batched `rev-parse`, one `status`, and the two `diff --numstat`
    // classifications the visible binary badge still depends on.
    assert_eq!(git_processes, 4);
    assert_eq!(diff_processes, 2);
    assert_eq!(mutation_processes, 0);
    println!(
        "PHASE14_METRIC {}",
        serde_json::json!({
            "lane": "git32Consumers",
            "consumers": 32,
            "discoveries": observation.discoveries,
            "nativeWatcherCreations": watcher_creations,
            "nativeWatchers": native_watchers,
            "watchRegistrations": watch_registrations,
            "statusPipelines": status_pipelines,
            "subscriberHighWater": observation.subscribers_high_water,
            "gitProcesses": git_processes,
            "statusProcesses": status_processes,
            "diffProcesses": diff_processes,
            "mutationProcesses": mutation_processes,
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
    let released = wait_for_release(&service).await;
    assert_eq!(released.subscribers, 0);
    assert_eq!(released.native_watchers, 0);
}

#[tokio::test]
#[ignore = "Phase 14 opt-in Git process accounting fixture"]
async fn phase14_warm_diff_and_mutation_process_counts() {
    let fixture = Fixture::new("phase14-git-processes");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.write("file", b"changed\n");
    let closed = Arc::new(AtomicBool::new(false));
    let service = Arc::new(GitService::new(Arc::clone(&closed), 0));
    let (sender, mut receiver) = mpsc::channel(8);
    let mut watch = fixture.request();
    watch.watch_id = "phase14-process-watch".into();
    let bootstrap = service.watch_activated(watch, sender).await.unwrap();
    let repository_id = bootstrap.repository.clone().unwrap().repository_id;
    tokio::time::sleep(Duration::from_millis(500)).await;
    while receiver.try_recv().is_ok() {}

    // A matching diff, opened while the repository is already observed.
    let before_diff = measurements::phase14_git_process_snapshot();
    let observation_before_diff = service.observation();
    let mut request = fixture.request();
    request.repository_id = repository_id.clone();
    request.path = b"file".to_vec();
    request.diff_target = v1::GitDiffTarget::Unstaged.into();
    let (diff, carried) = service.diff(&request, None).await.unwrap();
    assert!(carried.authoritative, "the diff response carries status");
    assert_eq!(diff.new_content, b"changed\n");
    let after_diff = measurements::phase14_git_process_snapshot();
    let diff_git_processes = after_diff.git_processes - before_diff.git_processes;
    let diff_status_processes = after_diff.status_processes - before_diff.status_processes;
    let diff_pipelines =
        service.observation().status_pipelines - observation_before_diff.status_pipelines;

    // A mutation, with the shared watcher live.
    request.expected_status_generation = carried.generation;
    request.mutation = v1::GitMutationKind::StageFile.into();
    request.connection_epoch = 3;
    let before_mutation = measurements::phase14_git_process_snapshot();
    let observation_before_mutation = service.observation();
    let result = service
        .mutate(request, 3, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert_eq!(result.outcome, v1::GitCommandOutcome::Applied as i32);
    assert!(result.status.is_some(), "the mutation carries status");
    tokio::time::sleep(Duration::from_millis(900)).await;
    let after_mutation = measurements::phase14_git_process_snapshot();
    let mutation_observation = service.observation();

    println!(
        "PHASE14_METRIC {}",
        serde_json::json!({
            "lane": "gitProcessAccounting",
            "warmDiffGitProcesses": diff_git_processes,
            "warmDiffStatusProcesses": diff_status_processes,
            "warmDiffStatusPipelines": diff_pipelines,
            "mutationGitProcesses": after_mutation.git_processes - before_mutation.git_processes,
            "mutationStatusProcesses": after_mutation.status_processes - before_mutation.status_processes,
            "mutationStatusPipelines": mutation_observation.status_pipelines - observation_before_mutation.status_pipelines,
            "totalDiscoveries": mutation_observation.discoveries,
            "nativeWatcherCreations": mutation_observation.native_watcher_creations,
        })
    );
    // The warm diff reuses the observed status: no discovery, no status.
    assert_eq!(diff_status_processes, 0);
    assert_eq!(diff_pipelines, 0);
    assert_eq!(mutation_observation.discoveries, 1);
    // One authoritative pre-command status and one post-command status. The
    // watcher's own wake-up for the mutation's writes coalesces onto the latter.
    assert!(
        (2..=3).contains(&(after_mutation.status_processes - before_mutation.status_processes)),
        "a mutation must not burst status subprocesses"
    );

    service.unwatch("phase14-process-watch").unwrap();
    closed.store(true, Ordering::Release);
}

#[tokio::test]
async fn a_broken_native_watcher_is_retired_and_re_established() {
    let fixture = Fixture::new("watch-recovery");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    let closed = Arc::new(AtomicBool::new(false));
    let service = Arc::new(GitService::new(Arc::clone(&closed), 0));
    let (sender, mut receiver) = mpsc::channel(8);
    let mut watch = fixture.request();
    watch.watch_id = "recovering".into();
    service.watch_activated(watch, sender).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let settled = service.observation();
    assert_eq!(settled.native_watcher_creations, 1);

    // The platform reports the watch broken. It must be retired and replaced,
    // not silently kept as the reason a stale snapshot is still trusted.
    service.fail_native_watcher_for_test();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while service.observation().native_watcher_creations < 2 && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        service.observation().native_watcher_creations >= 2,
        "a broken watcher must be retired and re-established"
    );

    while receiver.try_recv().is_ok() {}
    fixture.write("file", b"changed after failure\n");
    let status = next_status(&mut receiver)
        .await
        .expect("a re-established watcher still reports changes");
    assert!(
        status
            .entries
            .iter()
            .any(|entry| entry.path == b"file" && entry.worktree_status == "M")
    );
    service.unwatch("recovering").unwrap();
    closed.store(true, Ordering::Release);
}

#[tokio::test]
async fn a_recovered_refresh_is_published_even_when_the_repository_did_not_change() {
    let fixture = Fixture::new("watch-error-recovery");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    let closed = Arc::new(AtomicBool::new(false));
    let service = Arc::new(GitService::new(Arc::clone(&closed), 0));
    let (sender, mut receiver) = mpsc::channel(8);
    let mut watch = fixture.request();
    watch.watch_id = "error-recovery".into();
    let bootstrap = service.watch_activated(watch, sender).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    while receiver.try_recv().is_ok() {}

    let coordinator = service.coordinator_for_test(&fixture.request()).unwrap();
    coordinator
        .publish_error("transient failure".into(), coordinator.next_publication())
        .await;
    let error = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        error,
        SequencerControl::OrderedEvent(v1::HostEvent {
            git: Some(v1::GitEvent { status: None, .. }),
            ..
        })
    ));

    // The very same snapshot, republished. Recovering from an error is a
    // transition even when the repository state is byte-identical.
    coordinator
        .publish_status(&Arc::new(bootstrap.clone()), coordinator.next_publication())
        .await;
    let recovered = next_status(&mut receiver)
        .await
        .expect("recovery must reach subscribers");
    assert_eq!(recovered.source_generation, bootstrap.source_generation);
    service.unwatch("error-recovery").unwrap();
    closed.store(true, Ordering::Release);
}
