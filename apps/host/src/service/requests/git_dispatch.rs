use super::*;

pub(crate) fn handles(operation: v1::Operation) -> bool {
    matches!(
        operation,
        v1::Operation::GitStatus
            | v1::Operation::WatchGit
            | v1::Operation::UnwatchGit
            | v1::Operation::GitDiff
            | v1::Operation::PrepareGitDiscard
            | v1::Operation::GitMutation
            | v1::Operation::GitCommit
    )
}

pub(super) struct GitDispatchContext<'a> {
    pub(super) control_tx: &'a mpsc::Sender<SequencerControl>,
    pub(super) event_tx: &'a mpsc::Sender<SequencerControl>,
    pub(super) git: &'a Arc<super::super::git::GitService>,
    pub(super) connection_epoch: u64,
    pub(super) closed: &'a Arc<AtomicBool>,
}

pub(super) async fn handle(
    request_id: u64,
    operation: v1::Operation,
    request: v1::Request,
    cancellation: Arc<AtomicBool>,
    context: GitDispatchContext<'_>,
) {
    let Some(git_request) = request.git else {
        send_response(
            context.control_tx,
            request_id,
            response_error("invalid_git_request", "Git request payload is required"),
        )
        .await;
        return;
    };
    let operation_id = git_request.operation_id.clone();
    let mut watch_activation = None;
    let response = match operation {
        v1::Operation::GitStatus | v1::Operation::GitDiff => {
            let service = Arc::clone(context.git);
            let work = git_request.clone();
            let work_cancellation = Arc::clone(&cancellation);
            match tokio::task::spawn_blocking(move || {
                if operation == v1::Operation::GitStatus {
                    service
                        .status_cancellable(&work, Some(&work_cancellation))
                        .map(GitReadResult::Status)
                } else {
                    service
                        .diff_cancellable(&work, Some(&work_cancellation))
                        .map(GitReadResult::Diff)
                }
            })
            .await
            {
                Ok(Ok(GitReadResult::Status(status))) => {
                    git_response(&operation_id, |v| v.status = Some(status))
                }
                Ok(Ok(GitReadResult::Diff(diff))) => {
                    git_response(&operation_id, |v| v.diff = Some(diff))
                }
                Ok(Err(error)) => git_error(&error),
                Err(error) => response_error("git_task_failed", &error.to_string()),
            }
        }
        v1::Operation::WatchGit => {
            let service = Arc::clone(context.git);
            let sender = context.event_tx.clone();
            let closed = Arc::clone(context.closed);
            let work_cancellation = Arc::clone(&cancellation);
            let result = tokio::task::spawn_blocking(move || {
                service.watch_cancellable(git_request, sender, closed, work_cancellation)
            })
            .await;
            match result {
                Ok(Ok(bootstrap)) if !cancellation.load(Ordering::Acquire) => {
                    watch_activation = Some(bootstrap.activate);
                    git_response(&operation_id, |v| v.status = Some(bootstrap.status))
                }
                Ok(Ok(_)) => git_error(&anyhow::anyhow!("Git watch bootstrap cancelled")),
                Ok(Err(error)) => git_error(&error),
                Err(error) => response_error("git_task_failed", &error.to_string()),
            }
        }
        v1::Operation::UnwatchGit => match context.git.unwatch(&git_request.watch_id) {
            Ok(()) => git_response(&operation_id, |_| {}),
            Err(error) => git_error(&error),
        },
        v1::Operation::PrepareGitDiscard => {
            let service = Arc::clone(context.git);
            let work = git_request.clone();
            let epoch = context.connection_epoch;
            let result =
                tokio::task::spawn_blocking(move || service.prepare_discard(&work, epoch)).await;
            match result {
                Ok(Ok(confirmation)) => {
                    git_response(&operation_id, |v| v.confirmation = Some(confirmation))
                }
                Ok(Err(error)) => git_error(&error),
                Err(error) => response_error("git_task_failed", &error.to_string()),
            }
        }
        v1::Operation::GitMutation => match context
            .git
            .mutate(
                git_request,
                context.connection_epoch,
                Arc::clone(&cancellation),
            )
            .await
        {
            Ok(command) => git_response(&operation_id, |v| v.command = Some(command)),
            Err(error) => git_error(&error),
        },
        v1::Operation::GitCommit => match context
            .git
            .commit(
                git_request,
                context.connection_epoch,
                Arc::clone(&cancellation),
            )
            .await
        {
            Ok(command) => git_response(&operation_id, |v| v.command = Some(command)),
            Err(error) => git_error(&error),
        },
        _ => unreachable!(),
    };
    send_response(context.control_tx, request_id, response).await;
    if let Some(activate) = watch_activation {
        // Response and events share the sequencer queue. Activating only after
        // the response is enqueued makes bootstrap ordering deterministic.
        let _ = activate.send(());
    }
}

enum GitReadResult {
    Status(v1::GitStatusSnapshot),
    Diff(v1::GitDiff),
}

fn git_error(error: &anyhow::Error) -> v1::Response {
    let message = error.to_string();
    let code = if message.contains("stale") {
        "stale_git_state"
    } else if message.contains("confirmation") {
        "git_confirmation_required"
    } else if message.contains("cancelled") {
        "cancelled"
    } else if message.contains("timed out") {
        "git_timeout"
    } else if message.contains("not a git repository") {
        "not_git_repository"
    } else {
        "git_rejected"
    };
    response_error(code, &message)
}
