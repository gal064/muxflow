use super::*;

#[test]
fn descriptor_bound_worktree_entry_resists_outside_root_parent_symlink_swap() {
    let fixture = Fixture::new("symlink-swap");
    fixture.write("parent/victim", b"inside\n");
    let outside = fixture
        .root
        .parent()
        .unwrap()
        .join(format!("phase5-outside-{}", Uuid::new_v4()));
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("victim"), b"outside\n").unwrap();
    let root = WorktreeRoot::capture(fixture.root.to_str().unwrap()).unwrap();
    let entry = root.entry(b"parent/victim").unwrap();
    fs::rename(
        fixture.root.join("parent"),
        fixture.root.join("held-parent"),
    )
    .unwrap();
    std::os::unix::fs::symlink(&outside, fixture.root.join("parent")).unwrap();
    assert_eq!(entry.read(Some(64)).unwrap().unwrap(), b"inside\n");
    entry.unlink_file().unwrap();
    assert_eq!(fs::read(outside.join("victim")).unwrap(), b"outside\n");
    assert!(!fixture.root.join("held-parent/victim").exists());
    fs::remove_dir_all(outside).unwrap();
}

#[tokio::test]
async fn stable_root_capability_survives_same_path_repository_replacement_and_identity_changes() {
    let fixture = Fixture::new("root-replacement");
    fixture.write("old", b"old\n");
    fixture.git(&["add", "old"]);
    fixture.git(&["commit", "-qm", "old"]);
    let service = GitService::new(Arc::new(AtomicBool::new(false)), 0);
    let request = fixture.request();
    let old_status = service.status(&request, None).await.unwrap();
    let old_repository = old_status
        .repository
        .as_ref()
        .unwrap()
        .repository_id
        .clone();
    let old_metadata = GitMetadataCapability::capture(
        &old_status.repository.as_ref().unwrap().git_dir,
        &old_status.repository.as_ref().unwrap().common_dir,
    )
    .unwrap();
    let capability = WorktreeRoot::capture(fixture.root.to_str().unwrap()).unwrap();
    let stable_root = capability.stable_path();
    let old_head = runner::git_output_cancellable(
        &stable_root,
        &[OsStr::new("rev-parse"), OsStr::new("HEAD")],
        None,
        None,
    )
    .unwrap()
    .output
    .stdout;
    let held = fixture
        .root
        .parent()
        .unwrap()
        .join(format!("phase5-held-repository-{}", Uuid::new_v4()));
    fs::rename(&fixture.root, &held).unwrap();
    fs::create_dir(&fixture.root).unwrap();
    fixture.git(&["init", "-q"]);
    fixture.git(&["config", "user.name", "Replacement"]);
    fixture.git(&["config", "user.email", "replacement@example.test"]);
    fixture.write("new", b"new\n");
    fixture.git(&["add", "new"]);
    fixture.git(&["commit", "-qm", "new"]);

    let still_old = runner::git_output_cancellable(
        &stable_root,
        &[OsStr::new("rev-parse"), OsStr::new("HEAD")],
        None,
        None,
    )
    .unwrap();
    assert_eq!(still_old.stdout, old_head);
    let capabilities = Arc::new(RepositoryCapabilities::for_test(
        old_status.repository.clone().unwrap(),
        old_metadata,
        capability.try_clone().unwrap(),
    ));
    assert!(
        start_repository_watcher(
            &capabilities,
            &request.root,
            &request.root_token,
            Arc::new(measurements::GitObservation::default()),
        )
        .is_err()
    );
    assert!(
        service
            .status(&request, None)
            .await
            .unwrap_err()
            .to_string()
            .contains("root snapshot")
    );
    let replacement = service.status(&fixture.request(), None).await.unwrap();
    assert_ne!(
        replacement.repository.unwrap().repository_id,
        old_repository
    );
    fs::remove_dir_all(held).unwrap();
}

#[test]
fn git_metadata_capability_prevents_same_path_dot_git_retarget_for_stage_and_commit() {
    let fixture = Fixture::new("metadata-replacement");
    fixture.write("base", b"base\n");
    fixture.git(&["add", "base"]);
    fixture.git(&["commit", "-qm", "base"]);
    let root = WorktreeRoot::capture(fixture.root.to_str().unwrap()).unwrap();
    let stable_root = root.stable_path();
    let repository = discover_repository(
        &stable_root,
        fixture.root.to_str().unwrap(),
        root.identity().unwrap(),
        None,
    )
    .unwrap();
    let metadata =
        GitMetadataCapability::capture(&repository.git_dir, &repository.common_dir).unwrap();
    validate_metadata_capability(
        fixture.root.to_str().unwrap(),
        root.identity().unwrap(),
        &repository,
        &metadata,
    )
    .unwrap();
    let old_head = repository.head_oid;
    fixture.write("staged-after-swap", b"old repository only\n");
    fs::rename(fixture.root.join(".git"), fixture.root.join(".git-held")).unwrap();
    fixture.git(&["init", "-q"]);
    fixture.git(&["config", "user.name", "Replacement"]);
    fixture.git(&["config", "user.email", "replacement@example.test"]);

    let _metadata_guard = metadata.install();
    let staged = runner::git_path_cancellable(
        &stable_root,
        &[b"add"],
        b"staged-after-swap",
        &AtomicBool::new(false),
    )
    .unwrap();
    ensure_success(&staged, "descriptor-bound stage").unwrap();
    let committed = runner::git_output_with_deadline(
        &stable_root,
        &[
            OsStr::new("commit"),
            OsStr::new("-m"),
            OsStr::new("descriptor metadata"),
        ],
        None,
        None,
        runner::GIT_COMMIT_DEADLINE,
    )
    .unwrap();
    ensure_success(&committed, "descriptor-bound commit").unwrap();
    let new_old_head = String::from_utf8(
        runner::git_stdout_cancellable(
            &stable_root,
            &[OsStr::new("rev-parse"), OsStr::new("HEAD")],
            None,
        )
        .unwrap(),
    )
    .unwrap();
    assert_ne!(new_old_head.trim(), old_head);
    drop(_metadata_guard);
    assert!(
        !Command::new("git")
            .arg("-C")
            .arg(&fixture.root)
            .args(["rev-parse", "--verify", "HEAD"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        fixture
            .git(&[
                "--git-dir=.git-held",
                "--work-tree=.",
                "show",
                "--quiet",
                "HEAD:staged-after-swap",
            ])
            .status
            .success()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn darwin_git_keeps_cross_parent_metadata_move_paired_with_captured_worktree() {
    let fixture = Fixture::new("metadata-cross-parent-move");
    fixture.write("base", b"base\n");
    fixture.git(&["add", "base"]);
    fixture.git(&["commit", "-qm", "base"]);
    let root = WorktreeRoot::capture(fixture.root.to_str().unwrap()).unwrap();
    let stable_root = root.stable_path();
    let repository = discover_repository(
        &stable_root,
        fixture.root.to_str().unwrap(),
        root.identity().unwrap(),
        None,
    )
    .unwrap();
    let metadata =
        GitMetadataCapability::capture(&repository.git_dir, &repository.common_dir).unwrap();
    validate_metadata_capability(
        fixture.root.to_str().unwrap(),
        root.identity().unwrap(),
        &repository,
        &metadata,
    )
    .unwrap();

    let outside = fixture
        .root
        .parent()
        .unwrap()
        .join(format!("phase5-cross-parent-{}", Uuid::new_v4()));
    fs::create_dir(&outside).unwrap();
    let held_git = outside.join("held-git");
    fs::rename(fixture.root.join(".git"), &held_git).unwrap();
    fixture.write("victim", b"captured worktree\n");
    fs::write(outside.join("victim"), b"outside worktree\n").unwrap();

    let guard = metadata.install();
    let staged =
        runner::git_path_cancellable(&stable_root, &[b"add"], b"victim", &AtomicBool::new(false))
            .unwrap();
    ensure_success(&staged, "cross-parent stage").unwrap();
    let staged_contents = runner::git_stdout_cancellable(
        &stable_root,
        &[OsStr::new("show"), OsStr::new(":victim")],
        None,
    )
    .unwrap();
    assert_eq!(staged_contents, b"captured worktree\n");

    let committed = runner::git_output_with_deadline(
        &stable_root,
        &[
            OsStr::new("commit"),
            OsStr::new("-m"),
            OsStr::new("cross-parent metadata"),
        ],
        None,
        None,
        runner::GIT_COMMIT_DEADLINE,
    )
    .unwrap();
    ensure_success(&committed, "cross-parent commit").unwrap();
    assert_eq!(
        fs::read(outside.join("victim")).unwrap(),
        b"outside worktree\n"
    );

    drop(guard);
    fs::remove_dir_all(outside).unwrap();
}
