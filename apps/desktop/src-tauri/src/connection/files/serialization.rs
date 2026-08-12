use serde_json::{Value, json};
use tmux_agent_protocol::v1;

pub(crate) fn file_response_json(value: &v1::FileServiceResponse) -> Value {
    json!({
        "operationId": value.operation_id,
        "activeRoot": value.active_root.as_ref().map(active_root_json),
        "directory": value.directory.as_ref().map(directory_json),
        "content": value.content.as_ref().map(content_json),
        "metadata": value.metadata.as_ref().map(metadata_json),
        "deleted": value.deleted,
    })
}

pub(crate) fn file_event_json(value: &v1::FileServiceEvent) -> Value {
    json!({
        "operationId": value.operation_id,
        "activeRoot": value.active_root.as_ref().map(active_root_json),
        "directory": value.directory.as_ref().map(directory_json),
        "metadata": value.metadata.as_ref().map(metadata_json),
        "transferId": value.transfer_id,
        "transferredBytes": value.transferred_bytes.to_string(),
        "totalBytes": value.total_bytes.to_string(),
        "state": value.state,
        "error": value.error,
        "deleted": value.deleted,
        "rootToken": value.root_token,
        "watchId": value.watch_id,
    })
}

fn active_root_json(value: &v1::ActiveRoot) -> Value {
    json!({
        "paneId": value.pane_id,
        "root": value.root,
        "rootToken": value.root_token,
        "gitWorktree": value.git_worktree,
        "serverIdentity": value.server_identity,
        "topologyGeneration": value.topology_generation.to_string(),
        "rootGeneration": value.root_generation.to_string(),
    })
}

fn directory_json(value: &v1::DirectorySnapshot) -> Value {
    json!({
        "watchId": value.watch_id,
        "root": value.root,
        "path": value.path,
        "generation": value.generation.to_string(),
        "entries": value.entries.iter().map(metadata_json).collect::<Vec<_>>(),
        "overflowed": value.overflowed,
        "authoritative": value.authoritative,
        "nextPageToken": value.next_page_token,
        "complete": value.complete,
    })
}

fn content_json(value: &v1::FileContent) -> Value {
    json!({
        "metadata": value.metadata.as_ref().map(metadata_json),
        "kind": match v1::FileContentKind::try_from(value.kind).unwrap_or_default() {
            v1::FileContentKind::Text => "text",
            v1::FileContentKind::Binary => "binary",
            v1::FileContentKind::Image => "image",
            v1::FileContentKind::TooLarge => "tooLarge",
            v1::FileContentKind::Unspecified => "unspecified",
        },
        "content": value.content,
        "generation": value.generation.to_string(),
    })
}

pub(super) fn metadata_json(value: &v1::FileMetadata) -> Value {
    json!({
        "path": value.path,
        "name": value.name,
        "kind": kind_name(value.kind),
        "size": value.size.to_string(),
        "modifiedUnixMillis": value.modified_unix_millis,
        "mode": value.mode,
        "symlink": value.symlink,
        "symlinkTarget": value.symlink_target,
        "expandable": value.expandable,
        "generation": value.generation.to_string(),
        "mime": value.mime,
        "imagePreviewEligible": value.image_preview_eligible,
        "symlinkTargetKind": kind_name(value.symlink_target_kind),
    })
}

fn kind_name(kind: i32) -> &'static str {
    match v1::FileKind::try_from(kind).unwrap_or_default() {
        v1::FileKind::File => "file",
        v1::FileKind::Directory => "directory",
        v1::FileKind::Symlink => "symlink",
        v1::FileKind::Other => "other",
        v1::FileKind::Unspecified => "unspecified",
    }
}
