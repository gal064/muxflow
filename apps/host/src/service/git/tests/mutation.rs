use super::*;

#[tokio::test]
async fn mutations_enforce_status_diff_epoch_and_one_time_discard_guards() {
    let fixture = Fixture::new("mutation");
    fixture.write("file", b"a\nb\nc\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.write("file", b"A\nb\nC\n");
    let service = Arc::new(GitService::new());
    let status = service.status(&fixture.request()).unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.unwrap().repository_id;
    request.expected_status_generation = status.generation;
    request.path = b"file".to_vec();
    request.diff_target = v1::GitDiffTarget::Unstaged.into();
    request.expected_source_generation = service.diff(&request).unwrap().source_generation;
    request.mutation = v1::GitMutationKind::StageHunk.into();
    assert!(
        service
            .mutate(request.clone(), 7, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap_err()
            .to_string()
            .contains("connection")
    );
    request.connection_epoch = 7;
    let staged = service
        .mutate(request.clone(), 7, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert_eq!(staged.exit_code, 0);
    let fresh = staged.status.unwrap();
    request.expected_status_generation = fresh.generation;
    request.mutation = v1::GitMutationKind::DiscardFile.into();
    request.expected_source_generation.clear();
    let confirmation = service.prepare_discard(&request, 7).unwrap();
    request.confirmation_token = confirmation.token;
    let wrong_epoch = service
        .mutate(request.clone(), 8, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap_err();
    assert!(wrong_epoch.to_string().contains("connection"));
    let discarded = service
        .mutate(request.clone(), 7, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    request.expected_status_generation = discarded.status.unwrap().generation;
    assert!(
        service
            .mutate(request, 7, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap_err()
            .to_string()
            .contains("confirmation")
    );
}

#[tokio::test]
async fn complete_hunks_stage_unstage_and_confirmed_discard_match_git_apply() {
    let fixture = Fixture::new("hunks");
    let base = (1..=14)
        .map(|line| format!("line-{line}\n"))
        .collect::<String>();
    fixture.write("file", base.as_bytes());
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    let changed = base
        .replace("line-1\n", "changed-1\n")
        .replace("line-14\n", "changed-14\n");
    fixture.write("file", changed.as_bytes());

    let service = Arc::new(GitService::new());
    let status = service.status(&fixture.request()).unwrap();
    let mut operation = fixture.request();
    operation.repository_id = status.repository.unwrap().repository_id;
    operation.expected_status_generation = status.generation;
    operation.connection_epoch = 17;
    operation.path = b"file".to_vec();
    operation.diff_target = v1::GitDiffTarget::Unstaged.into();
    let unstaged = service.diff(&operation).unwrap();
    assert_eq!(unstaged.hunk_count, 2);
    operation.expected_source_generation = unstaged.source_generation;
    operation.mutation = v1::GitMutationKind::StageHunk.into();
    let staged = service
        .mutate(operation.clone(), 17, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert_eq!(staged.exit_code, 0);

    operation.expected_status_generation = staged.status.unwrap().generation;
    operation.diff_target = v1::GitDiffTarget::Staged.into();
    let staged_diff = service.diff(&operation).unwrap();
    assert_eq!(staged_diff.hunk_count, 1);
    operation.expected_source_generation = staged_diff.source_generation;
    operation.mutation = v1::GitMutationKind::UnstageHunk.into();
    let unstaged_again = service
        .mutate(operation.clone(), 17, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert_eq!(unstaged_again.exit_code, 0);

    operation.expected_status_generation = unstaged_again.status.unwrap().generation;
    operation.diff_target = v1::GitDiffTarget::Unstaged.into();
    let stale_diff = service.diff(&operation).unwrap();
    fixture.write("file", format!("{changed}external-tail\n").as_bytes());
    operation.expected_status_generation = service.status(&operation).unwrap().generation;
    operation.expected_source_generation = stale_diff.source_generation;
    operation.mutation = v1::GitMutationKind::DiscardHunk.into();
    operation.confirmation_token = service.prepare_discard(&operation, 17).unwrap().token;
    let stale = service
        .mutate(operation.clone(), 17, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert_eq!(stale.outcome, v1::GitCommandOutcome::NotApplied as i32);
    assert!(stale.error.contains("source diff"));
    let discard_diff = service.diff(&operation).unwrap();
    operation.expected_source_generation = discard_diff.source_generation;
    operation.confirmation_token = service.prepare_discard(&operation, 17).unwrap().token;
    let discarded = service
        .mutate(operation, 17, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert_eq!(discarded.exit_code, 0);
    let content = fs::read_to_string(fixture.root.join("file")).unwrap();
    assert!(content.starts_with("line-1\n"));
    assert!(content.contains("changed-14\n"));
}

#[tokio::test]
async fn copy_provenance_never_mutates_the_independent_source_path() {
    let fixture = Fixture::new("copy-provenance");
    let base = b"source base\n";
    let source_edit = b"independent unstaged source edit\n";
    fixture.write("source", base);
    fixture.git(&["add", "source"]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.write("copy", base);
    fixture.git(&["add", "copy"]);
    fixture.write("source", source_edit);

    let service = Arc::new(GitService::new());
    let status = service.status(&fixture.request()).unwrap();
    let copy = status
        .entries
        .iter()
        .find(|entry| entry.path == b"copy")
        .unwrap();
    assert_eq!(copy.index_kind, v1::GitChangeKind::Copied as i32);
    assert_eq!(copy.original_path, b"source");
    let mut request = fixture.request();
    request.repository_id = status.repository.unwrap().repository_id;
    request.expected_status_generation = status.generation;
    request.connection_epoch = 71;
    request.path = b"copy".to_vec();
    request.original_path = b"source".to_vec();
    request.mutation = v1::GitMutationKind::UnstageFile.into();
    let _unstaged = service
        .mutate(request.clone(), 71, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert_eq!(fs::read(fixture.root.join("source")).unwrap(), source_edit);
    assert_eq!(fixture.git(&["show", ":source"]).stdout, base);

    fixture.git(&["add", "copy"]);
    let status = service.status(&fixture.request()).unwrap();
    let copy = status
        .entries
        .iter()
        .find(|entry| entry.path == b"copy")
        .unwrap();
    assert_eq!(copy.index_kind, v1::GitChangeKind::Copied as i32);
    request.expected_status_generation = status.generation;
    request.original_path = copy.original_path.clone();
    request.mutation = v1::GitMutationKind::DiscardFile.into();
    request.diff_target = v1::GitDiffTarget::Staged.into();
    request.confirmation_token = service.prepare_discard(&request, 71).unwrap().token;
    let discarded = service
        .mutate(request, 71, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert_eq!(discarded.outcome, v1::GitCommandOutcome::Applied as i32);
    assert_eq!(fs::read(fixture.root.join("source")).unwrap(), source_edit);
    assert_eq!(fixture.git(&["show", ":source"]).stdout, base);
}

#[tokio::test]
async fn rename_diffs_use_target_specific_objects_and_reject_hunk_mutations() {
    let fixture = Fixture::new("rename-diff-fidelity");
    let base = b"rename source\nsecond line\n";
    let modified = b"rename source\nworktree destination edit\n";
    fixture.write("original", base);
    fixture.git(&["add", "original"]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.git(&["mv", "original", "destination"]);

    let service = Arc::new(GitService::new());
    let status = service.status(&fixture.request()).unwrap();
    let renamed = status
        .entries
        .iter()
        .find(|entry| entry.path == b"destination")
        .unwrap();
    assert_eq!(renamed.index_kind, v1::GitChangeKind::Renamed as i32);
    assert_eq!(renamed.original_path, b"original");
    let mut request = fixture.request();
    request.repository_id = status.repository.unwrap().repository_id;
    request.expected_status_generation = status.generation;
    request.connection_epoch = 89;
    request.path = b"destination".to_vec();
    request.original_path = b"original".to_vec();
    request.diff_target = v1::GitDiffTarget::Staged.into();
    let staged = service.diff(&request).unwrap();
    assert_eq!(staged.old_content, base);
    assert_eq!(staged.new_content, base);
    assert_eq!(staged.hunk_count, 0);
    assert!(
        staged
            .patch
            .windows(b"rename from original\n".len())
            .any(|part| part == b"rename from original\n")
    );
    assert!(
        staged
            .patch
            .windows(22)
            .any(|part| part == b"rename to destination\n")
    );

    request.expected_source_generation = staged.source_generation;
    request.mutation = v1::GitMutationKind::UnstageHunk.into();
    let error = service
        .mutate(request.clone(), 89, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("renamed or copied"));

    fixture.write("destination", modified);
    let mixed_status = service.status(&fixture.request()).unwrap();
    request.expected_status_generation = mixed_status.generation;
    request.mutation = v1::GitMutationKind::Unspecified.into();
    let staged_mixed = service.diff(&request).unwrap();
    assert_eq!(staged_mixed.old_content, base);
    assert_eq!(staged_mixed.new_content, base);
    assert_eq!(staged_mixed.hunk_count, 0);

    request.diff_target = v1::GitDiffTarget::Unstaged.into();
    let unstaged = service.diff(&request).unwrap();
    assert_eq!(unstaged.old_content, base);
    assert_eq!(unstaged.new_content, modified);
    assert_eq!(unstaged.hunk_count, 1);
    assert!(!unstaged.patch.windows(8).any(|part| part == b"original"));
    assert!(
        !unstaged
            .patch
            .windows(18)
            .any(|part| part == b"deleted file mode")
    );
}

#[tokio::test]
async fn staged_hunk_discard_preserves_unrelated_unstaged_worktree_changes() {
    let fixture = Fixture::new("mixed-staged-discard");
    let base = (1..=30)
        .map(|line| format!("line-{line}\n"))
        .collect::<String>();
    fixture.write("file", base.as_bytes());
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    let staged_content = base.replace("line-2\n", "staged-two\n");
    fixture.write("file", staged_content.as_bytes());
    fixture.git(&["add", "file"]);
    let mixed_content = staged_content.replace("line-29\n", "unstaged-twenty-nine\n");
    fixture.write("file", mixed_content.as_bytes());

    let service = Arc::new(GitService::new());
    let status = service.status(&fixture.request()).unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.unwrap().repository_id;
    request.expected_status_generation = status.generation;
    request.connection_epoch = 73;
    request.path = b"file".to_vec();
    request.diff_target = v1::GitDiffTarget::Staged.into();
    let diff = service.diff(&request).unwrap();
    assert_eq!(diff.hunk_count, 1);
    request.expected_source_generation = diff.source_generation;
    request.mutation = v1::GitMutationKind::DiscardHunk.into();
    request.confirmation_token = service.prepare_discard(&request, 73).unwrap().token;
    let result = service
        .mutate(request, 73, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert_eq!(result.outcome, v1::GitCommandOutcome::Applied as i32);
    assert!(
        fixture
            .git(&["diff", "--cached", "--quiet"])
            .status
            .success()
    );
    let expected = base.replace("line-29\n", "unstaged-twenty-nine\n");
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        expected.as_bytes()
    );
}

#[tokio::test]
async fn concurrent_repository_mutations_serialize_and_second_observes_stale_generation() {
    let fixture = Fixture::new("serialized");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.write("file", b"changed\n");
    let service = Arc::new(GitService::new());
    let status = service.status(&fixture.request()).unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.unwrap().repository_id;
    request.expected_status_generation = status.generation;
    request.connection_epoch = 23;
    request.path = b"file".to_vec();
    request.mutation = v1::GitMutationKind::StageFile.into();
    let first_service = Arc::clone(&service);
    let second_service = Arc::clone(&service);
    let first_request = request.clone();
    let first = tokio::spawn(async move {
        first_service
            .mutate(first_request, 23, Arc::new(AtomicBool::new(false)))
            .await
    });
    let second = tokio::spawn(async move {
        second_service
            .mutate(request, 23, Arc::new(AtomicBool::new(false)))
            .await
    });
    let results = [first.await.unwrap(), second.await.unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| result
                .as_ref()
                .is_err_and(|error| error.to_string().contains("stale")))
            .count(),
        1
    );
}

#[tokio::test]
async fn raw_leading_dash_newline_non_utf8_path_is_safe_as_a_mutation_argument() {
    let fixture = Fixture::new("raw-mutation");
    let raw = b"-raw\n\xff".to_vec();
    fs::write(
        fixture.root.join(std::ffi::OsString::from_vec(raw.clone())),
        b"raw\n",
    )
    .unwrap();
    let service = Arc::new(GitService::new());
    let status = service.status(&fixture.request()).unwrap();
    assert!(status.entries.iter().any(|entry| entry.path == raw));
    let mut request = fixture.request();
    request.repository_id = status.repository.unwrap().repository_id;
    request.expected_status_generation = status.generation;
    request.connection_epoch = 29;
    request.path = raw.clone();
    request.mutation = v1::GitMutationKind::StageFile.into();
    let result = service
        .mutate(request, 29, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(
        result
            .status
            .unwrap()
            .entries
            .iter()
            .any(|entry| { entry.path == raw && entry.index_status == "A" })
    );
}

#[test]
fn worktree_identity_hunk_selection_and_path_safety_are_covered() {
    for path in [
        &b""[..],
        b".",
        b"./file",
        b"dir//file",
        b"dir/./file",
        b"../escape",
        b"/absolute",
        b"nul\0byte",
    ] {
        assert!(validate_git_path(path).is_err());
    }
    let patch = b"diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-a\n+A\n@@ -8 +8 @@\n-b\n+B\n";
    let selected = super::super::mutation::extract_hunk_patch(patch, 1).unwrap();
    assert!(!selected.windows(3).any(|value| value == b"-a\n"));
    assert!(selected.windows(3).any(|value| value == b"+B\n"));

    let fixture = Fixture::new("matrix");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    let linked = fixture
        .root
        .parent()
        .unwrap()
        .join(format!("phase5-linked-{}", Uuid::new_v4()));
    fixture.git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "linked",
        linked.to_str().unwrap(),
    ]);
    let primary_root = WorktreeRoot::capture(fixture.root.to_str().unwrap()).unwrap();
    let primary = discover_repository(
        &primary_root.stable_path(),
        fixture.root.to_str().unwrap(),
        primary_root.identity().unwrap(),
        None,
    )
    .unwrap();
    let linked_root = WorktreeRoot::capture(linked.to_str().unwrap()).unwrap();
    let worktree = discover_repository(
        &linked_root.stable_path(),
        linked.to_str().unwrap(),
        linked_root.identity().unwrap(),
        None,
    )
    .unwrap();
    assert_eq!(primary.common_dir, worktree.common_dir);
    assert_ne!(primary.repository_id, worktree.repository_id);
    fixture.git(&["worktree", "remove", "--force", linked.to_str().unwrap()]);
}

#[tokio::test]
async fn literal_magic_paths_are_safe_and_directory_or_mismatched_targets_fail_closed() {
    let fixture = Fixture::new("literal-paths");
    let magic = b":(glob)*".to_vec();
    fs::write(
        fixture
            .root
            .join(std::ffi::OsString::from_vec(magic.clone())),
        b"literal\n",
    )
    .unwrap();
    fixture.write("ordinary", b"must remain untracked\n");
    let service = Arc::new(GitService::new());
    let status = service.status(&fixture.request()).unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.as_ref().unwrap().repository_id.clone();
    request.expected_status_generation = status.generation;
    request.connection_epoch = 31;
    request.path = magic.clone();
    request.mutation = v1::GitMutationKind::StageFile.into();
    let staged = service
        .mutate(request.clone(), 31, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert!(
        staged
            .status
            .unwrap()
            .entries
            .iter()
            .any(|entry| { entry.path == magic && entry.index_status == "A" })
    );
    let mut staged_name = fixture.git(&["diff", "--cached", "--name-only"]).stdout;
    assert_eq!(staged_name.pop(), Some(b'\n'));
    assert_eq!(staged_name, magic);

    let status = service.status(&fixture.request()).unwrap();
    request.expected_status_generation = status.generation;
    request.path = b"ordinary".to_vec();
    request.original_path = b"not-current".to_vec();
    assert!(
        service
            .mutate(request, 31, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap_err()
            .to_string()
            .contains("original path")
    );

    fixture.write("directory/file", b"nested\n");
    let root = WorktreeRoot::capture(fixture.root.to_str().unwrap()).unwrap();
    assert!(root.entry(b"directory").unwrap().is_directory().unwrap());
    let synthetic = v1::GitStatusSnapshot {
        entries: vec![v1::GitStatusEntry {
            path: b"directory".to_vec(),
            untracked: true,
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut directory = fixture.request();
    directory.path = b"directory".to_vec();
    assert!(
        validate_current_target(fixture.root.to_str().unwrap(), &directory, &synthetic)
            .unwrap_err()
            .to_string()
            .contains("directory")
    );
}

#[tokio::test]
async fn commit_revalidates_generation_and_cancellation_kills_blocking_hook_group() {
    let _hook_test_guard = COMMIT_HOOK_TEST_LOCK.lock().await;
    let fixture = Fixture::new("commit-cancel");
    fixture.write("file", b"one\n");
    fixture.git(&["add", "file"]);
    let service = Arc::new(GitService::new());
    let status = service.status(&fixture.request()).unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.as_ref().unwrap().repository_id.clone();
    request.expected_status_generation = status.generation;
    request.connection_epoch = 41;
    request.commit_message = "blocked".into();
    fixture.write("external", b"changes authority\n");
    let stale = service
        .commit(request.clone(), 41, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap_err();
    assert!(stale.to_string().contains("stale Git status"));

    request.expected_status_generation = service.status(&fixture.request()).unwrap().generation;
    let hook = fixture.root.join(".git/hooks/pre-commit");
    fs::write(
        &hook,
        b"#!/bin/sh\ntouch .git/pre-commit-started\ntrap '' TERM\nsleep 20\n",
    )
    .unwrap();
    let mut mode = fs::metadata(&hook).unwrap().permissions();
    mode.set_mode(0o755);
    fs::set_permissions(&hook, mode).unwrap();
    let cancellation = Arc::new(AtomicBool::new(false));
    let trigger = Arc::clone(&cancellation);
    let marker = fixture.root.join(".git/pre-commit-started");
    let cancellation_thread = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        trigger.store(true, Ordering::Release);
    });
    let started = std::time::Instant::now();
    let result = service.commit(request, 41, cancellation).await.unwrap();
    cancellation_thread.join().unwrap();
    assert_eq!(result.outcome, v1::GitCommandOutcome::NotApplied as i32);
    assert!(result.error.contains("cancelled"));
    assert!(started.elapsed() < Duration::from_secs(8));
}

#[tokio::test]
async fn normal_hook_longer_than_desktop_default_timeout_completes_successfully() {
    let _hook_test_guard = COMMIT_HOOK_TEST_LOCK.lock().await;
    let fixture = Fixture::new("long-hook");
    fixture.write("file", b"one\n");
    fixture.git(&["add", "file"]);
    let hook = fixture.root.join(".git/hooks/pre-commit");
    fs::write(&hook, b"#!/bin/sh\nsleep 6\nexit 0\n").unwrap();
    let mut mode = fs::metadata(&hook).unwrap().permissions();
    mode.set_mode(0o755);
    fs::set_permissions(&hook, mode).unwrap();
    let service = Arc::new(GitService::new());
    let status = service.status(&fixture.request()).unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.unwrap().repository_id;
    request.expected_status_generation = status.generation;
    request.connection_epoch = 59;
    request.commit_message = "long hook succeeds".into();
    let started = std::time::Instant::now();
    let result = service
        .commit(request, 59, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert!(result.applied);
    assert!(!result.refresh_failed);
    assert!(started.elapsed() >= Duration::from_secs(5));
}

#[tokio::test]
async fn successful_commit_with_oversized_hook_output_is_applied_and_flagged_truncated() {
    let _hook_test_guard = COMMIT_HOOK_TEST_LOCK.lock().await;
    let fixture = Fixture::new("hook-output-overflow");
    fixture.write("file", b"one\n");
    fixture.git(&["add", "file"]);
    let hook = fixture.root.join(".git/hooks/pre-commit");
    fs::write(
        &hook,
        b"#!/bin/sh\ndd if=/dev/zero bs=1048576 count=13 1>&2 2>/dev/null\nexit 0\n",
    )
    .unwrap();
    let mut mode = fs::metadata(&hook).unwrap().permissions();
    mode.set_mode(0o755);
    fs::set_permissions(&hook, mode).unwrap();
    let service = Arc::new(GitService::new());
    let status = service.status(&fixture.request()).unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.unwrap().repository_id;
    request.expected_status_generation = status.generation;
    request.connection_epoch = 79;
    request.commit_message = "bounded output".into();
    let mut result = service
        .commit(request, 79, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert!(result.applied);
    assert_eq!(result.outcome, v1::GitCommandOutcome::Applied as i32);
    assert!(result.stderr_truncated);
    assert!(result.stdout.len() + result.stderr.len() <= MAX_GIT_OUTPUT);
    assert!(result.post_state_authoritative);

    // Exercise the aggregate envelope budget, not merely the per-process
    // output cap: a large post-status plus the retained hook output must keep
    // outcome evidence and leave the control sequencer usable.
    result.status = Some(v1::GitStatusSnapshot {
        repository: Some(v1::GitRepository {
            repository_id: "large-post-status".into(),
            ..Default::default()
        }),
        entries: vec![v1::GitStatusEntry {
            path: vec![b'x'; 6 * 1024 * 1024],
            ..Default::default()
        }],
        authoritative: true,
        ..Default::default()
    });
    let mut sequencer = super::super::super::ProtocolSequencer::default();
    let bounded = sequencer.frame(super::super::super::SequencerControl::Response {
        request_id: 79,
        response: v1::Response {
            ok: true,
            git: Some(v1::GitResponse {
                command: Some(result),
                ..Default::default()
            }),
            ..Default::default()
        },
        snapshot_barrier: false,
    });
    assert!(prost::Message::encoded_len(&bounded) < tmux_agent_protocol::MAX_FRAME_BYTES);
    let Some(tmux_agent_protocol::v1::envelope::Payload::Response(response)) = bounded.payload
    else {
        panic!("expected bounded command response")
    };
    let command = response.git.unwrap().command.unwrap();
    assert!(command.applied);
    assert_eq!(command.outcome, v1::GitCommandOutcome::Applied as i32);
    assert!(command.status.is_none());
    assert!(command.status_omitted);
    assert!(command.refresh_failed);
    assert!(!command.post_head_oid.is_empty());
    let next = sequencer.frame(super::super::super::SequencerControl::Response {
        request_id: 80,
        response: v1::Response {
            ok: true,
            ..Default::default()
        },
        snapshot_barrier: false,
    });
    tmux_agent_protocol::encode_frame(&next).unwrap();
}

#[tokio::test]
async fn cancellation_during_post_commit_hook_reports_applied_authoritative_head() {
    let _hook_test_guard = COMMIT_HOOK_TEST_LOCK.lock().await;
    let fixture = Fixture::new("post-commit-cancel");
    fixture.write("file", b"one\n");
    fixture.git(&["add", "file"]);
    let hook = fixture.root.join(".git/hooks/post-commit");
    fs::write(
        &hook,
        b"#!/bin/sh\ntouch .git/post-commit-started\ntrap '' TERM\nsleep 20\n",
    )
    .unwrap();
    let mut mode = fs::metadata(&hook).unwrap().permissions();
    mode.set_mode(0o755);
    fs::set_permissions(&hook, mode).unwrap();
    let service = Arc::new(GitService::new());
    let status = service.status(&fixture.request()).unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.unwrap().repository_id;
    request.expected_status_generation = status.generation;
    request.connection_epoch = 83;
    request.commit_message = "commit before cancellation".into();
    let cancellation = Arc::new(AtomicBool::new(false));
    let trigger = Arc::clone(&cancellation);
    let marker = fixture.root.join(".git/post-commit-started");
    let cancellation_thread = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        trigger.store(true, Ordering::Release);
    });
    let result = service.commit(request, 83, cancellation).await.unwrap();
    cancellation_thread.join().unwrap();
    assert!(result.applied);
    assert_eq!(result.outcome, v1::GitCommandOutcome::Applied as i32);
    assert_ne!(result.pre_head_oid, result.post_head_oid);
    assert!(result.post_state_authoritative);
    assert!(result.error.contains("cancelled"));
}

#[test]
fn completed_command_retains_outcome_when_refresh_fails() {
    let result = command_result(
        runner::success_output(),
        Err(anyhow::anyhow!("refresh cancelled after apply")),
    );
    assert!(result.applied);
    assert!(result.refresh_failed);
    assert!(result.status.is_none());
    assert!(result.refresh_error.contains("cancelled"));
}

#[tokio::test]
async fn submodule_mutations_are_explicitly_rejected_without_changing_pointer_or_worktree() {
    let child = Fixture::new("submodule-pointer-child");
    child.write("nested", b"one\n");
    child.git(&["add", "nested"]);
    child.git(&["commit", "-qm", "one"]);
    let parent = Fixture::new("submodule-pointer-parent");
    let added = Command::new("git")
        .arg("-C")
        .arg(&parent.root)
        .args([
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            child.root.to_str().unwrap(),
            "module",
        ])
        .output()
        .unwrap();
    assert!(added.status.success());
    parent.git(&["commit", "-qm", "submodule"]);
    let recorded = parent.git(&["rev-parse", "HEAD:module"]).stdout;
    parent.git(&["-C", "module", "config", "user.name", "Phase Five"]);
    parent.git(&[
        "-C",
        "module",
        "config",
        "user.email",
        "phase5@example.test",
    ]);
    fs::write(parent.root.join("module/nested"), b"two\n").unwrap();
    parent.git(&["-C", "module", "commit", "-qam", "two"]);

    let service = Arc::new(GitService::new());
    let status = service.status(&parent.request()).unwrap();
    let mut request = parent.request();
    request.repository_id = status.repository.as_ref().unwrap().repository_id.clone();
    request.expected_status_generation = status.generation;
    request.connection_epoch = 53;
    request.path = b"module".to_vec();
    request.mutation = v1::GitMutationKind::StageFile.into();
    let error = service
        .mutate(request.clone(), 53, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("submodule Git mutations are unsupported")
    );
    assert_eq!(parent.git(&["rev-parse", ":module"]).stdout, recorded);
    assert_ne!(
        parent.git(&["-C", "module", "rev-parse", "HEAD"]).stdout,
        recorded
    );
}
