use std::sync::atomic::Ordering;

use serde::Deserialize;
use serde_json::{Value, json};
use tauri::State;
use tmux_agent_protocol::v1;

use super::{TerminalClients, get_client};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommand {
    pub operation: String,
    #[serde(default)]
    pub operation_id: String,
    #[serde(default)]
    pub root: String,
    #[serde(default)]
    pub root_token: String,
    #[serde(default)]
    pub expected_server_identity: String,
    #[serde(default)]
    pub repository_id: String,
    #[serde(default)]
    pub expected_status_generation: String,
    #[serde(default)]
    pub expected_source_generation: String,
    #[serde(default)]
    pub connection_epoch: String,
    #[serde(default)]
    pub path: Vec<u8>,
    #[serde(default)]
    pub original_path: Vec<u8>,
    #[serde(default)]
    pub diff_target: String,
    #[serde(default)]
    pub mutation: String,
    #[serde(default)]
    pub hunk_index: u32,
    #[serde(default)]
    pub confirmation_token: String,
    #[serde(default)]
    pub commit_message: String,
    #[serde(default)]
    pub watch_id: String,
}

/// Async so the host round trip never runs on the WebView's main thread. Git
/// carries a five-minute timeout for hooks, so a blocking command here could
/// freeze the entire UI for five minutes.
#[tauri::command]
pub async fn git_request(
    client_id: String,
    command: GitCommand,
    clients: State<'_, TerminalClients>,
) -> Result<Value, String> {
    if command.operation_id.is_empty() {
        return Err("Git operationId is required".into());
    }
    let operation_id = command.operation_id.clone();
    let operation = operation_from_name(&command.operation)?;
    let client = get_client(&clients, &client_id)?;
    let connection_epoch = parse_optional_u64("connectionEpoch", &command.connection_epoch)?;
    if operation_requires_epoch(operation) {
        let current_epoch = client.terminal_epoch.load(Ordering::Acquire);
        if connection_epoch == 0 || connection_epoch != current_epoch {
            return Err("Git mutation belongs to a stale or missing connection generation".into());
        }
    }
    let current_server = client.server_identity.lock().unwrap().clone();
    if command.expected_server_identity.is_empty()
        || command.expected_server_identity != current_server
    {
        return Err("Git request belongs to a stale or missing server identity".into());
    }
    let request = v1::GitRequest {
        operation_id: command.operation_id,
        root: command.root,
        root_token: command.root_token,
        expected_server_identity: command.expected_server_identity,
        repository_id: command.repository_id,
        expected_status_generation: parse_optional_u64(
            "expectedStatusGeneration",
            &command.expected_status_generation,
        )?,
        expected_source_generation: command.expected_source_generation,
        connection_epoch,
        path: command.path,
        original_path: command.original_path,
        diff_target: diff_target_from_name(&command.diff_target)?.into(),
        mutation: mutation_from_name(&command.mutation)?.into(),
        hunk_index: command.hunk_index,
        confirmation_token: command.confirmation_token,
        commit_message: command.commit_message,
        watch_id: command.watch_id,
        // Deferred diff bodies never travel on the control lane.
        content: None,
    };
    let protocol_request = v1::Request {
        operation: operation.into(),
        git: Some(request),
        ..Default::default()
    };
    let response = tauri::async_runtime::spawn_blocking(move || {
        client.request_git(protocol_request, &operation_id)
    })
    .await
    .map_err(|error| format!("Git request task failed: {error}"))??;
    response
        .git
        .as_ref()
        .map(git_response_json)
        .ok_or_else(|| "host omitted Git response".into())
}

#[tauri::command]
pub fn cancel_git_request(
    client_id: String,
    operation_id: String,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    if operation_id.is_empty() {
        return Err("Git operationId is required".into());
    }
    get_client(&clients, &client_id)?.cancel_git(&operation_id)
}

fn operation_from_name(value: &str) -> Result<v1::Operation, String> {
    match value {
        "status" => Ok(v1::Operation::GitStatus),
        "watch" => Ok(v1::Operation::WatchGit),
        "unwatch" => Ok(v1::Operation::UnwatchGit),
        "diff" => Ok(v1::Operation::GitDiff),
        "prepareDiscard" => Ok(v1::Operation::PrepareGitDiscard),
        "mutate" => Ok(v1::Operation::GitMutation),
        "commit" => Ok(v1::Operation::GitCommit),
        _ => Err(format!("unsupported Git operation {value}")),
    }
}

fn operation_requires_epoch(operation: v1::Operation) -> bool {
    matches!(
        operation,
        v1::Operation::PrepareGitDiscard | v1::Operation::GitMutation | v1::Operation::GitCommit
    )
}

fn diff_target_from_name(value: &str) -> Result<v1::GitDiffTarget, String> {
    match value {
        "" => Ok(v1::GitDiffTarget::Unspecified),
        "unstaged" => Ok(v1::GitDiffTarget::Unstaged),
        "staged" => Ok(v1::GitDiffTarget::Staged),
        _ => Err(format!("unsupported Git diff target {value}")),
    }
}

fn mutation_from_name(value: &str) -> Result<v1::GitMutationKind, String> {
    match value {
        "" => Ok(v1::GitMutationKind::Unspecified),
        "stageFile" => Ok(v1::GitMutationKind::StageFile),
        "unstageFile" => Ok(v1::GitMutationKind::UnstageFile),
        "discardFile" => Ok(v1::GitMutationKind::DiscardFile),
        "stageHunk" => Ok(v1::GitMutationKind::StageHunk),
        "unstageHunk" => Ok(v1::GitMutationKind::UnstageHunk),
        "discardHunk" => Ok(v1::GitMutationKind::DiscardHunk),
        _ => Err(format!("unsupported Git mutation {value}")),
    }
}

fn parse_optional_u64(label: &str, value: &str) -> Result<u64, String> {
    if value.is_empty() {
        Ok(0)
    } else {
        value
            .parse()
            .map_err(|_| format!("{label} must be a decimal u64 string"))
    }
}

pub(crate) fn git_response_json(value: &v1::GitResponse) -> Value {
    json!({
        "operationId": value.operation_id,
        "status": value.status.as_ref().map(status_json),
        "diff": value.diff.as_ref().map(diff_json),
        "confirmation": value.confirmation.as_ref().map(|item| json!({ "token": item.token, "expiresUnixMillis": item.expires_unix_millis.to_string() })),
        "command": value.command.as_ref().map(command_json),
    })
}

pub(crate) fn git_event_json(value: &v1::GitEvent) -> Value {
    json!({ "watchId": value.watch_id, "rootToken": value.root_token, "status": value.status.as_ref().map(status_json), "error": value.error })
}

fn repository_json(value: &v1::GitRepository) -> Value {
    json!({ "repositoryId": value.repository_id, "worktreeRoot": value.worktree_root,
        "gitDir": value.git_dir, "commonDir": value.common_dir, "initial": value.initial,
        "detachedHead": value.detached_head, "headName": value.head_name, "headOid": value.head_oid })
}

fn status_json(value: &v1::GitStatusSnapshot) -> Value {
    json!({ "repository": value.repository.as_ref().map(repository_json), "generation": value.generation.to_string(),
        "sourceGeneration": value.source_generation, "entries": value.entries.iter().map(status_entry_json).collect::<Vec<_>>(),
        "authoritative": value.authoritative, "oversized": value.oversized,
        "totalEntryCount": value.total_entry_count.to_string(), "error": value.error,
        "copyDetectionIncomplete": value.copy_detection_incomplete })
}

fn status_entry_json(value: &v1::GitStatusEntry) -> Value {
    json!({ "path": value.path, "displayPath": value.display_path, "originalPath": value.original_path,
        "displayOriginalPath": value.display_original_path, "indexKind": change_kind_name(value.index_kind),
        "worktreeKind": change_kind_name(value.worktree_kind), "indexStatus": value.index_status,
        "worktreeStatus": value.worktree_status, "headMode": value.head_mode, "indexMode": value.index_mode,
        "worktreeMode": value.worktree_mode, "headOid": value.head_oid, "indexOid": value.index_oid,
        "untracked": value.untracked, "ignored": value.ignored, "conflicted": value.conflicted,
        "conflictCode": value.conflict_code, "submodule": value.submodule, "submoduleState": value.submodule_state,
        "symlink": value.symlink, "binary": value.binary, "renameScore": value.rename_score })
}

fn diff_json(value: &v1::GitDiff) -> Value {
    json!({ "repository": value.repository.as_ref().map(repository_json), "target": diff_target_name(value.target),
        "path": value.path, "originalPath": value.original_path, "displayPath": value.display_path,
        // The patch is deliberately not serialized: only hunk mutation needs
        // it, and the host re-derives it there in process.
        "oldContent": value.old_content, "newContent": value.new_content,
        "sourceGeneration": value.source_generation, "binary": value.binary, "tooLarge": value.too_large,
        "oldMissing": value.old_missing, "newMissing": value.new_missing, "hunkCount": value.hunk_count,
        "oldContentRef": value.old_content_ref.as_ref().map(content_ref_json),
        "newContentRef": value.new_content_ref.as_ref().map(content_ref_json) })
}

/// A body the control lane deliberately withheld. The renderer reads it back
/// over the bulk lane using exactly these two facts.
fn content_ref_json(value: &v1::GitDiffContentRef) -> Value {
    json!({ "size": value.size.to_string(), "contentDigest": value.content_digest })
}

fn command_json(value: &v1::GitCommandResult) -> Value {
    json!({ "exitCode": value.exit_code, "stdout": value.stdout, "stderr": value.stderr,
        "status": value.status.as_ref().map(status_json), "applied": value.applied,
        "refreshFailed": value.refresh_failed, "refreshError": value.refresh_error,
        "outcome": command_outcome_name(value.outcome), "stdoutTruncated": value.stdout_truncated,
        "stderrTruncated": value.stderr_truncated, "error": value.error,
        "preHeadOid": value.pre_head_oid, "postHeadOid": value.post_head_oid,
        "preIndexGeneration": value.pre_index_generation, "postIndexGeneration": value.post_index_generation,
        "postStateAuthoritative": value.post_state_authoritative,
        "preStatusGeneration": value.pre_status_generation.to_string(),
        "postStatusGeneration": value.post_status_generation.to_string(),
        "statusOmitted": value.status_omitted })
}

fn command_outcome_name(value: i32) -> &'static str {
    match v1::GitCommandOutcome::try_from(value).unwrap_or_default() {
        v1::GitCommandOutcome::NotApplied => "notApplied",
        v1::GitCommandOutcome::Applied => "applied",
        v1::GitCommandOutcome::PartialOrUnknown => "partialOrUnknown",
        _ => "unspecified",
    }
}

fn change_kind_name(value: i32) -> &'static str {
    match v1::GitChangeKind::try_from(value).unwrap_or_default() {
        v1::GitChangeKind::Modified => "modified",
        v1::GitChangeKind::Added => "added",
        v1::GitChangeKind::Deleted => "deleted",
        v1::GitChangeKind::Renamed => "renamed",
        v1::GitChangeKind::Copied => "copied",
        v1::GitChangeKind::TypeChanged => "typeChanged",
        v1::GitChangeKind::Unmerged => "unmerged",
        v1::GitChangeKind::Untracked => "untracked",
        v1::GitChangeKind::Ignored => "ignored",
        _ => "unspecified",
    }
}
fn diff_target_name(value: i32) -> &'static str {
    match v1::GitDiffTarget::try_from(value).unwrap_or_default() {
        v1::GitDiffTarget::Staged => "staged",
        v1::GitDiffTarget::Unstaged => "unstaged",
        _ => "unspecified",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_uses_decimal_strings_for_all_git_u64_values() {
        let value = git_response_json(&v1::GitResponse {
            status: Some(v1::GitStatusSnapshot {
                generation: u64::MAX,
                copy_detection_incomplete: true,
                ..Default::default()
            }),
            confirmation: Some(v1::GitConfirmation {
                expires_unix_millis: u64::MAX,
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(value["status"]["generation"], u64::MAX.to_string());
        assert_eq!(value["status"]["copyDetectionIncomplete"], true);
        assert_eq!(
            value["confirmation"]["expiresUnixMillis"],
            u64::MAX.to_string()
        );
    }

    #[test]
    fn json_preserves_applied_command_when_refresh_failed() {
        let value = git_response_json(&v1::GitResponse {
            command: Some(v1::GitCommandResult {
                exit_code: 0,
                applied: true,
                refresh_failed: true,
                refresh_error: "cancelled after apply".into(),
                outcome: v1::GitCommandOutcome::PartialOrUnknown.into(),
                stdout_truncated: true,
                error: "outcome requires authority refresh".into(),
                pre_status_generation: u64::MAX - 1,
                post_status_generation: u64::MAX,
                status_omitted: true,
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(value["command"]["applied"], true);
        assert_eq!(value["command"]["refreshFailed"], true);
        assert_eq!(value["command"]["status"], Value::Null);
        assert_eq!(value["command"]["refreshError"], "cancelled after apply");
        assert_eq!(value["command"]["outcome"], "partialOrUnknown");
        assert_eq!(value["command"]["stdoutTruncated"], true);
        assert_eq!(
            value["command"]["preStatusGeneration"],
            (u64::MAX - 1).to_string()
        );
        assert_eq!(
            value["command"]["postStatusGeneration"],
            u64::MAX.to_string()
        );
        assert_eq!(value["command"]["statusOmitted"], true);
    }
}
