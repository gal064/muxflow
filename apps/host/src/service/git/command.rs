//! Turning a completed Git command plus its post-command refresh into a result
//! the desktop can act on without guessing.

use super::*;

pub(super) struct CommandState {
    head_oid: String,
    index_generation: Option<String>,
    status_generation: u64,
    source_generation: String,
    authoritative: bool,
}

pub(super) fn command_state(root: &str, status: &v1::GitStatusSnapshot) -> CommandState {
    CommandState {
        head_oid: status
            .repository
            .as_ref()
            .map(|repository| repository.head_oid.clone())
            .unwrap_or_default(),
        index_generation: git_index_generation(root).ok(),
        status_generation: status.generation,
        source_generation: status.source_generation.clone(),
        authoritative: status.authoritative && !status.oversized,
    }
}

pub(super) fn truthful_command_result(
    execution: anyhow::Result<GitOutput>,
    pre: CommandState,
    refresh: anyhow::Result<v1::GitStatusSnapshot>,
    root: &str,
    commit: bool,
) -> v1::GitCommandResult {
    let post = refresh
        .as_ref()
        .ok()
        .map(|status| command_state(root, status));
    let post_authoritative = post
        .as_ref()
        .is_some_and(|state| state.authoritative && state.index_generation.is_some());
    let state_unchanged = post.as_ref().is_some_and(|state| {
        pre.authoritative
            && pre.index_generation.is_some()
            && state.head_oid == pre.head_oid
            && state.index_generation == pre.index_generation
            && state.source_generation == pre.source_generation
    });
    let head_advanced = post.as_ref().is_some_and(|state| {
        post_authoritative && !state.head_oid.is_empty() && state.head_oid != pre.head_oid
    });

    let (exit_code, stdout, stderr, stdout_truncated, stderr_truncated, command_error, success) =
        match execution {
            Ok(output) => {
                let success = output.status.success() && output.interrupted.is_none();
                let error = output.interrupted.clone().unwrap_or_else(|| {
                    if success {
                        String::new()
                    } else {
                        String::from_utf8_lossy(&output.stderr).trim().to_owned()
                    }
                });
                (
                    output.status.code().unwrap_or(-1),
                    output.output.stdout,
                    output.output.stderr,
                    output.stdout_truncated,
                    output.stderr_truncated,
                    error,
                    success,
                )
            }
            Err(error) => (
                -1,
                Vec::new(),
                Vec::new(),
                false,
                false,
                error.to_string(),
                false,
            ),
        };
    let outcome = if success || (commit && head_advanced) {
        v1::GitCommandOutcome::Applied
    } else if post_authoritative && state_unchanged {
        v1::GitCommandOutcome::NotApplied
    } else {
        v1::GitCommandOutcome::PartialOrUnknown
    };
    let (status, refresh_failed, refresh_error) = match refresh {
        Ok(status) if status.authoritative && !status.oversized => {
            (Some(status), false, String::new())
        }
        Ok(status) => {
            let error = if status.error.is_empty() {
                "post-command Git status is not authoritative".into()
            } else {
                status.error.clone()
            };
            (Some(status), true, error)
        }
        Err(error) => (None, true, error.to_string()),
    };
    v1::GitCommandResult {
        exit_code,
        stdout,
        stderr,
        status,
        applied: outcome == v1::GitCommandOutcome::Applied,
        refresh_failed,
        refresh_error,
        outcome: outcome.into(),
        stdout_truncated,
        stderr_truncated,
        error: command_error,
        pre_head_oid: pre.head_oid,
        post_head_oid: post
            .as_ref()
            .map(|state| state.head_oid.clone())
            .unwrap_or_default(),
        pre_index_generation: pre.index_generation.unwrap_or_default(),
        post_index_generation: post
            .as_ref()
            .and_then(|state| state.index_generation.clone())
            .unwrap_or_default(),
        post_state_authoritative: post_authoritative,
        pre_status_generation: pre.status_generation,
        post_status_generation: post.as_ref().map_or(0, |state| state.status_generation),
        status_omitted: false,
        // Only a push has one, and only the push executor knows it.
        push_target: String::new(),
    }
}
