use super::*;

pub(super) struct GitDispatchContext<'a> {
    pub(super) control_tx: &'a mpsc::Sender<SequencerControl>,
    pub(super) event_tx: &'a mpsc::Sender<SequencerControl>,
    pub(super) git: &'a Arc<super::super::git::GitService>,
    pub(super) connection_epoch: u64,
    /// Whether this connection can open the independent bulk lane at all. A
    /// read-only host cannot, so its diff bodies must be inlined.
    pub(super) bulk_available: bool,
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
        v1::Operation::GitStatus => match context
            .git
            .status(&git_request, Some(Arc::clone(&cancellation)))
            .await
        {
            Ok(status) => git_response(&operation_id, |v| v.status = Some(status)),
            Err(error) => git_error(&error),
        },
        // The diff response carries the status it was read against, so the
        // desktop no longer pays a status round trip before every diff.
        v1::Operation::GitDiff => match context
            .git
            .diff(
                &git_request,
                context.bulk_available,
                Some(Arc::clone(&cancellation)),
            )
            .await
        {
            Ok((diff, status)) => git_response(&operation_id, |v| {
                v.diff = Some(diff);
                v.status = Some(status);
            }),
            Err(error) => git_error(&error),
        },
        v1::Operation::GitDiffContent => match context
            .git
            .diff_content(&git_request, Some(Arc::clone(&cancellation)))
            .await
        {
            Ok(chunk) => git_response(&operation_id, |v| v.content_chunk = Some(chunk)),
            Err(error) => git_error(&error),
        },
        v1::Operation::WatchGit => {
            match context
                .git
                .watch(
                    git_request,
                    context.event_tx.clone(),
                    Arc::clone(&cancellation),
                )
                .await
            {
                Ok(bootstrap) if !cancellation.load(Ordering::Acquire) => {
                    watch_activation = Some(bootstrap.activate);
                    git_response(&operation_id, |v| v.status = Some(bootstrap.status))
                }
                Ok(_) => git_error(&anyhow::anyhow!("Git watch bootstrap cancelled")),
                Err(error) => git_error(&error),
            }
        }
        v1::Operation::UnwatchGit => match context.git.unwatch(&git_request.watch_id) {
            Ok(()) => git_response(&operation_id, |_| {}),
            Err(error) => git_error(&error),
        },
        v1::Operation::PrepareGitDiscard => match context
            .git
            .prepare_discard(&git_request, context.connection_epoch)
            .await
        {
            Ok(confirmation) => {
                git_response(&operation_id, |v| v.confirmation = Some(confirmation))
            }
            Err(error) => git_error(&error),
        },
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
        activate.activate();
    }
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
