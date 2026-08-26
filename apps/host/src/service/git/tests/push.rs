use super::*;

/// A remote to push to, owned by a fixture so its `Drop` removes it.
///
/// A bare repository created beside the worktree would survive a failing test
/// and leave the next run pushing into someone else's history.
fn bare_remote(owner: &Fixture, name: &str) -> PathBuf {
    let path = owner.root.join(name);
    run_git(
        &owner.root,
        &["init", "--bare", "-q", "-b", "master"],
        Some(&path),
    );
    path
}

fn git_at(root: &Path, args: &[&str]) -> Output {
    run_git(root, args, None)
}

fn run_git(cwd: &Path, args: &[&str], trailing: Option<&Path>) -> Output {
    let mut command = Command::new("git");
    command.arg("-C").arg(cwd).args(args);
    if let Some(value) = trailing {
        command.arg(value);
    }
    let output = command.env("GIT_TERMINAL_PROMPT", "0").output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// One repository with a commit already published to `origin/master`.
fn published(name: &str, remote: &Path) -> Fixture {
    let fixture = Fixture::new(name);
    fixture.write("file", b"one\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    fixture.git(&["push", "-q", "-u", "origin", "master"]);
    fixture
}

async fn request_for(service: &GitService, fixture: &Fixture, epoch: u64) -> v1::GitRequest {
    let status = service.status(&fixture.request(), None).await.unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.unwrap().repository_id;
    request.expected_status_generation = status.generation;
    request.connection_epoch = epoch;
    request
}

#[tokio::test]
async fn push_publishes_the_upstream_branch_and_names_what_it_published_to() {
    let owner = Fixture::new("push-remote-applied");
    let remote = bare_remote(&owner, "origin.git");
    let fixture = published("push-applied", &remote);
    fixture.write("file", b"two\n");
    fixture.git(&["add", "file"]);

    let service = Arc::new(GitService::new(Arc::new(AtomicBool::new(false)), 0));
    let mut request = request_for(&service, &fixture, 101).await;
    request.commit_message = "second".into();
    let committed = service
        .commit(request.clone(), 101, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert!(committed.applied);
    request.expected_status_generation = committed.status.unwrap().generation;

    let pushed = service
        .push(request, 101, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert_eq!(pushed.outcome, v1::GitCommandOutcome::Applied as i32);
    assert!(pushed.applied);
    assert_eq!(pushed.push_target, "origin/master");
    assert_eq!(
        git_at(&remote, &["rev-parse", "master"]).stdout,
        fixture.git(&["rev-parse", "HEAD"]).stdout
    );
}

#[tokio::test]
async fn push_without_an_upstream_refuses_instead_of_creating_one() {
    let fixture = Fixture::new("push-no-upstream");
    fixture.write("file", b"one\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);

    let service = Arc::new(GitService::new(Arc::new(AtomicBool::new(false)), 0));
    let request = request_for(&service, &fixture, 103).await;
    let error = service
        .push(request, 103, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("no upstream"), "{error}");
    // Nothing was configured on the way out.
    assert!(
        !Command::new("git")
            .arg("-C")
            .arg(&fixture.root)
            .args(["remote"])
            .output()
            .unwrap()
            .stdout
            .iter()
            .any(|byte| !byte.is_ascii_whitespace())
    );
}

#[tokio::test]
async fn a_remote_that_refuses_the_push_is_reported_as_a_named_rejection() {
    let owner = Fixture::new("push-remote-rejected");
    let remote = bare_remote(&owner, "origin.git");
    let fixture = published("push-rejected", &remote);

    // Someone else advances the remote branch, so this push is no longer a
    // fast-forward and the remote refuses it.
    let elsewhere = owner.root.join("elsewhere");
    run_git(
        &owner.root,
        &["clone", "-q", remote.to_str().unwrap()],
        Some(&elsewhere),
    );
    git_at(&elsewhere, &["config", "user.name", "Someone Else"]);
    git_at(&elsewhere, &["config", "user.email", "else@example.test"]);
    fs::write(elsewhere.join("other"), b"other\n").unwrap();
    git_at(&elsewhere, &["add", "other"]);
    git_at(&elsewhere, &["commit", "-qm", "other"]);
    git_at(&elsewhere, &["push", "-q", "origin", "master"]);
    let remote_head = git_at(&remote, &["rev-parse", "master"]).stdout;

    fixture.write("file", b"mine\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "mine"]);

    let service = Arc::new(GitService::new(Arc::new(AtomicBool::new(false)), 0));
    let request = request_for(&service, &fixture, 107).await;
    // A refused push is an error, not a result: only an error carries the code
    // the desktop turns into "pull or rebase first".
    let refused = service
        .push(request, 107, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains("rejected"), "{refused}");
    assert_eq!(
        git_at(&remote, &["rev-parse", "master"]).stdout,
        remote_head
    );
}

#[tokio::test]
async fn push_revalidates_the_status_generation_it_was_asked_for() {
    let owner = Fixture::new("push-remote-stale");
    let remote = bare_remote(&owner, "origin.git");
    let fixture = published("push-stale", &remote);

    let service = Arc::new(GitService::new(Arc::new(AtomicBool::new(false)), 0));
    let mut request = request_for(&service, &fixture, 109).await;
    request.expected_status_generation += 1;
    let error = service
        .push(request, 109, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("stale"), "{error}");
}

/// The porcelain report, not the exit code, is what "the remote accepted it"
/// is read from: a clean exit that named no ref is an unknown outcome.
#[test]
fn push_verdict_is_read_from_the_porcelain_ref_report() {
    let accepted = classify_push(&porcelain(
        b"To /tmp/origin\n\trefs/heads/master:refs/heads/master\t0123..4567\nDone\n",
    ));
    assert_eq!(accepted, PushVerdict::Accepted);
    let rejected = classify_push(&porcelain(b"To /tmp/origin\n!\trefs/heads/master:refs/heads/master\t[rejected] (non-fast-forward)\nDone\n"));
    assert_eq!(rejected, PushVerdict::Rejected);
    assert_eq!(
        classify_push(&porcelain(b"To /tmp/origin\n")),
        PushVerdict::Unknown
    );

    let mut result = v1::GitCommandResult {
        exit_code: 0,
        applied: true,
        outcome: v1::GitCommandOutcome::Applied.into(),
        ..Default::default()
    };
    apply_push_verdict(&mut result, Some(PushVerdict::Unknown));
    assert_eq!(
        result.outcome,
        v1::GitCommandOutcome::PartialOrUnknown as i32
    );
    assert!(!result.applied);
    assert!(!result.error.is_empty());
}

fn porcelain(stdout: &[u8]) -> GitOutput {
    use std::os::unix::process::ExitStatusExt;
    std::process::Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: stdout.to_vec(),
        stderr: Vec::new(),
    }
    .into()
}
