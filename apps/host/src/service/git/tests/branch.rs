use super::*;

fn service() -> Arc<GitService> {
    Arc::new(GitService::new(Arc::new(AtomicBool::new(false)), 0))
}

/// `base` committed and recorded as `origin/main`, as a clone would have it.
fn with_default_branch(name: &str) -> Fixture {
    let fixture = Fixture::new(name);
    fixture.write("kept", b"one\ntwo\nthree\n");
    fixture.write("gone", b"bye\n");
    fixture.write("edited", b"a\nb\nc\n");
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    fixture
}

async fn identified(service: &GitService, fixture: &Fixture) -> v1::GitRequest {
    let status = service.status(&fixture.request(), None).await.unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.unwrap().repository_id;
    request.expected_status_generation = status.generation;
    request
}

#[tokio::test]
async fn summarizes_committed_uncommitted_and_untracked_changes_against_the_default_branch() {
    let fixture = with_default_branch("branch-summary");
    fixture.write("edited", b"a\nB\nc\nd\n");
    fixture.git(&["rm", "-q", "gone"]);
    fixture.git(&["commit", "-qam", "on the branch"]);
    fixture.write("kept", b"one\nTWO\nthree\n");
    fixture.write("new.txt", b"x\ny");
    fixture.write("blob", b"\0\x01");
    let service = service();
    let request = identified(&service, &fixture).await;

    let summary = service.branch_summary(&request, None).await.unwrap();

    assert_eq!(summary.base_ref, "origin/main");
    assert_eq!(summary.merge_base_oid.len(), 40);
    let files: Vec<_> = summary
        .files
        .iter()
        .map(|file| {
            (
                String::from_utf8(file.path.clone()).unwrap(),
                v1::GitChangeKind::try_from(file.kind).unwrap(),
                file.added,
                file.removed,
                file.binary,
            )
        })
        .collect();
    assert_eq!(
        files,
        [
            ("blob".into(), v1::GitChangeKind::Untracked, 0, 0, true),
            ("edited".into(), v1::GitChangeKind::Modified, 2, 1, false),
            ("gone".into(), v1::GitChangeKind::Deleted, 0, 1, false),
            ("kept".into(), v1::GitChangeKind::Modified, 1, 1, false),
            ("new.txt".into(), v1::GitChangeKind::Untracked, 2, 0, false),
        ]
    );
    assert_eq!(summary.total_file_count, 5);
    assert_eq!((summary.added, summary.removed), (5, 3));
}

/// `(path, kind, added, removed)` for each file a summary lists.
fn listed(summary: &v1::GitBranchSummary) -> Vec<(String, v1::GitChangeKind, u32, u32)> {
    summary
        .files
        .iter()
        .map(|file| {
            (
                String::from_utf8(file.path.clone()).unwrap(),
                v1::GitChangeKind::try_from(file.kind).unwrap(),
                file.added,
                file.removed,
            )
        })
        .collect()
}

#[tokio::test]
async fn summarizes_what_is_not_committed_against_head_beside_the_whole_branch() {
    let fixture = with_default_branch("branch-summary-head");
    fixture.write("edited", b"a\nB\nc\n");
    fixture.git(&["commit", "-qam", "on the branch"]);
    let head = String::from_utf8(fixture.git(&["rev-parse", "HEAD"]).stdout).unwrap();
    // Staged, unstaged and untracked.
    fixture.write("kept", b"one\nTWO\nthree\n");
    fixture.git(&["add", "kept"]);
    fixture.write("edited", b"a\nB\nc\nd\n");
    fixture.write("new.txt", b"x\ny");
    let service = service();
    let mut request = identified(&service, &fixture).await;

    request.summary_base = v1::GitSummaryBase::Head.into();
    let uncommitted = service.branch_summary(&request, None).await.unwrap();
    request.summary_base = v1::GitSummaryBase::Unspecified.into();
    let branch = service.branch_summary(&request, None).await.unwrap();

    assert_eq!(uncommitted.base_ref, "HEAD");
    assert_eq!(uncommitted.merge_base_oid, head.trim());
    assert_eq!(
        listed(&uncommitted),
        [
            ("edited".into(), v1::GitChangeKind::Modified, 1, 0),
            ("kept".into(), v1::GitChangeKind::Modified, 1, 1),
            ("new.txt".into(), v1::GitChangeKind::Untracked, 2, 0),
        ]
    );
    assert_eq!(uncommitted.total_file_count, 3);
    assert_eq!((uncommitted.added, uncommitted.removed), (4, 1));

    // The branch's own commit counts only against the default branch.
    assert_eq!(branch.base_ref, "origin/main");
    assert_eq!(
        listed(&branch),
        [
            ("edited".into(), v1::GitChangeKind::Modified, 2, 1),
            ("kept".into(), v1::GitChangeKind::Modified, 1, 1),
            ("new.txt".into(), v1::GitChangeKind::Untracked, 2, 0),
        ]
    );
    assert_eq!((branch.added, branch.removed), (5, 2));
}

#[tokio::test]
async fn a_repository_with_no_commit_yet_has_everything_uncommitted() {
    let fixture = Fixture::new("branch-summary-unborn");
    fixture.write("staged", b"one\n");
    fixture.git(&["add", "staged"]);
    fixture.write("loose", b"a\nb\n");
    let service = service();
    let mut request = identified(&service, &fixture).await;
    request.summary_base = v1::GitSummaryBase::Head.into();

    let summary = service.branch_summary(&request, None).await.unwrap();

    assert_eq!(summary.base_ref, "HEAD");
    assert_eq!(summary.merge_base_oid, "");
    assert_eq!(
        listed(&summary),
        [
            ("loose".into(), v1::GitChangeKind::Untracked, 2, 0),
            ("staged".into(), v1::GitChangeKind::Added, 1, 0),
        ]
    );
    assert_eq!((summary.added, summary.removed), (3, 0));
}

#[tokio::test]
async fn prefers_the_recorded_remote_head_and_says_when_there_is_no_default_branch() {
    let fixture = Fixture::new("branch-summary-none");
    fixture.write("file", b"a\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    let service = service();
    let request = identified(&service, &fixture).await;
    assert_eq!(
        service.branch_summary(&request, None).await.unwrap(),
        v1::GitBranchSummary::default()
    );

    fixture.git(&["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/trunk",
    ]);
    fixture.write("file", b"b\n");
    let summary = service.branch_summary(&request, None).await.unwrap();
    assert_eq!(summary.base_ref, "origin/trunk");
    assert_eq!(summary.files.len(), 1);
}

#[tokio::test]
async fn the_default_branch_diff_reads_against_the_merge_base_and_is_read_only() {
    let fixture = with_default_branch("branch-diff");
    fixture.write("edited", b"a\nB\nc\n");
    fixture.git(&["commit", "-qam", "on the branch"]);
    fixture.write("edited", b"a\nB\nc\nd\n");
    fixture.write("new.txt", b"x\n");
    let service = service();
    let mut request = identified(&service, &fixture).await;
    request.diff_target = v1::GitDiffTarget::Base.into();

    request.path = b"edited".to_vec();
    let edited = service.diff_only(&request, None).await.unwrap();
    let patch = String::from_utf8(edited.patch.clone()).unwrap();
    assert!(patch.contains("-b\n+B\n"), "{patch}");
    assert!(patch.contains("+d\n"), "{patch}");
    assert_eq!(edited.hunk_count, 1);
    assert_eq!(edited.old_content, b"a\nb\nc\n");
    assert_eq!(edited.new_content, b"a\nB\nc\nd\n");

    request.path = b"new.txt".to_vec();
    let untracked = service.diff_only(&request, None).await.unwrap();
    assert!(untracked.old_missing);
    assert!(
        String::from_utf8(untracked.patch.clone())
            .unwrap()
            .contains("+x\n")
    );

    request.path = b"kept".to_vec();
    let unchanged = service.diff_only(&request, None).await.unwrap();
    assert!(unchanged.patch.is_empty());
    assert_eq!(unchanged.hunk_count, 0);

    request.path = b"edited".to_vec();
    request.expected_source_generation = edited.source_generation;
    request.connection_epoch = 3;
    for mutation in [
        v1::GitMutationKind::StageHunk,
        v1::GitMutationKind::StageFile,
    ] {
        request.mutation = mutation.into();
        let refused = service
            .mutate(request.clone(), 3, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap_err();
        assert!(refused.to_string().contains("read only"), "{refused}");
    }
    assert_eq!(
        fs::read(fixture.root.join("edited")).unwrap(),
        b"a\nB\nc\nd\n"
    );
}

/// The desktop draws the hunks of this patch and sends an index back, so the
/// patch it is given is the one a hunk mutation applies.
#[tokio::test]
async fn the_desktop_diff_carries_the_patch_a_hunk_index_names() {
    let fixture = Fixture::new("client-patch");
    let base = (1..=14).map(|line| format!("{line}\n")).collect::<String>();
    fixture.write("file", base.as_bytes());
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.write(
        "file",
        base.replace("1\n", "one\n")
            .replace("14\n", "fourteen\n")
            .as_bytes(),
    );
    let service = service();
    let mut request = identified(&service, &fixture).await;
    request.path = b"file".to_vec();
    request.diff_target = v1::GitDiffTarget::Unstaged.into();

    let diff = service.diff_only(&request, None).await.unwrap();
    assert_eq!(diff.hunk_count, 2);
    assert_eq!(
        diff.patch,
        service.diff_with_patch(&request).await.unwrap().patch
    );

    request.mutation = v1::GitMutationKind::StageHunk.into();
    request.expected_source_generation = diff.source_generation;
    request.hunk_index = 1;
    request.connection_epoch = 5;
    let staged = service
        .mutate(request, 5, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert!(staged.applied);
    let cached = fixture.git(&["diff", "--cached"]);
    let cached = String::from_utf8(cached.stdout).unwrap();
    assert!(cached.contains("+fourteen"), "{cached}");
    assert!(!cached.contains("+one"), "{cached}");
}

#[tokio::test]
async fn a_patch_too_large_for_the_control_lane_is_left_out_but_still_counted() {
    let fixture = Fixture::new("client-patch-large");
    fixture.write("file", b"seed\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    let large = (0..40_000)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    fixture.write("file", large.as_bytes());
    let service = service();
    let mut request = identified(&service, &fixture).await;
    request.path = b"file".to_vec();
    request.diff_target = v1::GitDiffTarget::Unstaged.into();

    let diff = service.diff_only(&request, None).await.unwrap();
    assert!(diff.patch.is_empty());
    assert_eq!(diff.hunk_count, 1);
    assert!(diff.new_content_ref.is_some());
}

#[tokio::test]
async fn lists_local_branches_and_switches_only_when_git_can() {
    let fixture = Fixture::new("branches");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.git(&["branch", "feature"]);
    fixture.git(&["switch", "-q", "feature"]);
    fixture.write("file", b"feature\n");
    fixture.git(&["commit", "-qam", "feature"]);
    fixture.git(&["switch", "-q", "master"]);
    let service = service();
    let request = identified(&service, &fixture).await;

    let list = service.branches(&request, None).await.unwrap();
    let mut names = list.branches.clone();
    names.sort();
    assert_eq!(names, ["feature", "master"]);
    assert_eq!(list.current, "master");

    let mut checkout = request.clone();
    checkout.connection_epoch = 9;
    for refused in ["-f", "missing", "feature~1", "master@{0}"] {
        checkout.branch = refused.into();
        let result = service
            .checkout(checkout.clone(), 9, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert!(!result.applied, "{refused}");
    }

    // A change the switch would overwrite: Git refuses and nothing moves.
    fixture.write("file", b"local\n");
    let status = service.status(&fixture.request(), None).await.unwrap();
    checkout.expected_status_generation = status.generation;
    checkout.branch = "feature".into();
    let refused = service
        .checkout(checkout.clone(), 9, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert_eq!(
        v1::GitCommandOutcome::try_from(refused.outcome).unwrap(),
        v1::GitCommandOutcome::NotApplied
    );
    assert_eq!(fs::read(fixture.root.join("file")).unwrap(), b"local\n");

    fixture.git(&["checkout", "-q", "--", "file"]);
    let status = service.status(&fixture.request(), None).await.unwrap();
    checkout.expected_status_generation = status.generation;
    let switched = service
        .checkout(checkout, 9, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert!(switched.applied);
    assert_eq!(
        switched.status.unwrap().repository.unwrap().head_name,
        "feature"
    );
    assert_eq!(fs::read(fixture.root.join("file")).unwrap(), b"feature\n");
}
