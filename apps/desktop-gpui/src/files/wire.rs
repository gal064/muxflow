//! The file service's shapes and their decode. Port of `features/files/types.ts`
//! and `wire.ts`, plus the request validation from `api.ts`.
//!
//! Every answer arrives as `desktop-core`'s JSON (`file_response_json`,
//! `file_event_json`), so this is the one place that reads it. u64 values stay
//! decimal strings, as the wire carries them.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::commands::quick_open::{FileMatch, FileSearchResults};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    File,
    Directory,
    Missing,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub name: String,
    pub kind: FileKind,
    pub size_bytes: String,
    pub modified_millis: i64,
    /// The host's version identity for this entry.
    pub generation: String,
    pub executable: bool,
    pub symlink_target: Option<String>,
    pub target_kind: Option<TargetKind>,
    pub expandable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryListing {
    pub root_token: String,
    pub directory: String,
    pub revision: String,
    pub entries: Vec<FileEntry>,
    /// The host rebuilt this listing because its watcher lost events, so what
    /// is cached below it may have missed changes too. Not the same question
    /// as `complete`, which is only about further pages.
    pub recovered_from_overflow: bool,
    pub next_page_token: Option<String>,
    pub complete: bool,
}

/// The pane's root capability. The host derives the token from the server and
/// the path, so one token is one worktree whichever pane resolved it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActiveRoot {
    pub token: String,
    pub pane_id: String,
    pub path: String,
    pub git_worktree: bool,
    pub revision: String,
}

pub fn same_root(left: Option<&ActiveRoot>, right: Option<&ActiveRoot>) -> bool {
    matches!((left, right), (Some(left), Some(right))
        if left.token == right.token && left.path == right.path && left.pane_id == right.pane_id)
}

/// The workspace a file surface shows, without the transport that serves it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileSelection {
    pub host_profile_id: String,
    pub server_identity: String,
    pub session_id: String,
    pub pane_id: String,
}

impl FileSelection {
    /// Stable across reconnects, unlike [`FileScope::key`].
    pub fn key(&self) -> String {
        [
            self.host_profile_id.as_str(),
            &self.server_identity,
            &self.session_id,
            &self.pane_id,
        ]
        .join("\0")
    }
}

/// A selection bound to the live bridge that can serve requests for it. Port of
/// `FileWorkspaceScope`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileScope {
    pub client_id: String,
    pub host_profile_id: String,
    pub server_identity: String,
    pub generation: u64,
    /// The host-issued generation epoch of this exact bridge.
    pub terminal_epoch: u64,
    pub session_id: String,
    pub pane_id: String,
}

impl FileScope {
    pub fn key(&self) -> String {
        [
            self.client_id.as_str(),
            &self.host_profile_id,
            &self.server_identity,
            &self.terminal_epoch.to_string(),
            &self.session_id,
            &self.pane_id,
        ]
        .join("\0")
    }

    /// Downloads outlive pane, root and session selection within one live
    /// connection.
    pub fn transfer_key(&self) -> String {
        [
            self.client_id.as_str(),
            &self.server_identity,
            &self.terminal_epoch.to_string(),
        ]
        .join("\0")
    }
}

/// The bridge publishes its snapshot before it says it is connected; a request
/// scope never exists in that gap. Port of `liveFileScope`.
pub fn live_file_scope(
    connected: bool,
    client_id: Option<&str>,
    terminal_epoch: u64,
    generation: u64,
    selection: Option<&FileSelection>,
) -> Option<FileScope> {
    let client_id = client_id.filter(|id| !id.is_empty())?;
    let selection = selection?;
    (connected && terminal_epoch != 0).then(|| FileScope {
        client_id: client_id.to_owned(),
        host_profile_id: selection.host_profile_id.clone(),
        server_identity: selection.server_identity.clone(),
        generation,
        terminal_epoch,
        session_id: selection.session_id.clone(),
        pane_id: selection.pane_id.clone(),
    })
}

/// The pane a path or a search is resolved against.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneRoute {
    pub session_id: String,
    pub window_id: String,
    pub cwd: String,
}

/// What the host said, mapped rather than reduced to "something changed".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceEvent {
    RootChanged(ActiveRoot),
    DirectorySnapshot {
        root_token: String,
        listing: DirectoryListing,
    },
    FileChanged {
        root_token: String,
        path: String,
        /// `None` when the host could not describe the entry: a gap to recover.
        entry: Option<FileEntry>,
        /// The file's generation, even when its entry cannot be drawn.
        generation: String,
        /// The request that caused it, so a document knows its own save.
        operation_id: String,
    },
    FileDeleted {
        root_token: String,
        path: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileMutation {
    CreateFile {
        parent: String,
        name: String,
    },
    CreateDirectory {
        parent: String,
        name: String,
    },
    Rename(Relocation),
    Move(Relocation),
    Duplicate(Relocation),
    Delete {
        path: String,
        confirmed_non_empty: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relocation {
    pub path: String,
    pub destination: String,
    pub overwrite: bool,
    pub confirmed_non_empty: bool,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct WireMetadata {
    path: String,
    name: String,
    kind: String,
    size: String,
    /// A JSON number, unlike `size` beside it: an `int64` of milliseconds.
    modified_unix_millis: i64,
    mode: u32,
    symlink_target: String,
    expandable: bool,
    generation: String,
    symlink_target_kind: String,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct WireRoot {
    pane_id: String,
    root: String,
    root_token: String,
    git_worktree: bool,
    server_identity: String,
    topology_generation: String,
    root_generation: String,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct WireDirectory {
    path: String,
    generation: String,
    entries: Vec<WireMetadata>,
    next_page_token: String,
    complete: bool,
    recovered_from_overflow: bool,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct WireSearch {
    matches: Vec<WireMatch>,
    complete: Option<bool>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct WireMatch {
    path: String,
    relative_path: String,
    score: Option<i64>,
}

/// A response or an event; the fields either may carry.
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct WireFile {
    active_root: Option<WireRoot>,
    directory: Option<WireDirectory>,
    metadata: Option<WireMetadata>,
    search: Option<WireSearch>,
    deleted: bool,
    root_token: String,
    operation_id: String,
}

fn parse(value: Value) -> Result<WireFile, String> {
    serde_json::from_value(value)
        .map_err(|error| format!("Host returned a malformed file answer: {error}"))
}

fn map_root(value: WireRoot) -> ActiveRoot {
    ActiveRoot {
        token: value.root_token,
        pane_id: value.pane_id,
        path: value.root,
        git_worktree: value.git_worktree,
        revision: value.root_generation,
    }
}

fn map_entry(value: WireMetadata) -> FileEntry {
    let kind = match value.kind.as_str() {
        "directory" => FileKind::Directory,
        "symlink" => FileKind::Symlink,
        _ => FileKind::File,
    };
    let target_kind = match value.symlink_target_kind.as_str() {
        "file" => Some(TargetKind::File),
        "directory" => Some(TargetKind::Directory),
        "missing" => Some(TargetKind::Missing),
        "other" => Some(TargetKind::Other),
        _ => None,
    };
    FileEntry {
        path: value.path,
        name: value.name,
        kind,
        size_bytes: value.size,
        modified_millis: value.modified_unix_millis,
        generation: value.generation,
        executable: value.mode & 0o111 != 0,
        symlink_target: (!value.symlink_target.is_empty()).then_some(value.symlink_target),
        target_kind,
        expandable: value.expandable,
    }
}

fn map_directory(value: WireDirectory, root_token: &str) -> DirectoryListing {
    DirectoryListing {
        root_token: root_token.to_owned(),
        directory: value.path,
        revision: value.generation,
        entries: value.entries.into_iter().map(map_entry).collect(),
        recovered_from_overflow: value.recovered_from_overflow,
        next_page_token: (!value.next_page_token.is_empty()).then_some(value.next_page_token),
        complete: value.complete,
    }
}

/// A deleted or unreadable path arrives with a default-shaped record, and a
/// row patched from that would be an invented zero-byte file.
fn is_renderable(value: &WireMetadata) -> bool {
    !value.path.is_empty()
        && !value.name.is_empty()
        && !value.kind.is_empty()
        && value.kind != "unspecified"
}

pub fn decode_root(value: Value) -> Result<ActiveRoot, String> {
    let root = parse(value)?
        .active_root
        .ok_or("Host omitted the active root.")?;
    Ok(map_root(root))
}

pub fn decode_listing(value: Value, root_token: &str) -> Result<DirectoryListing, String> {
    let directory = parse(value)?
        .directory
        .ok_or("Host omitted the directory listing.")?;
    Ok(map_directory(directory, root_token))
}

/// The search answer, refused when it names anything outside the directory it
/// was asked about. Port of the check in `searchFiles`.
pub fn decode_search(value: Value, cwd: &str) -> Result<FileSearchResults, String> {
    let malformed = || "Host returned malformed file search results.".to_owned();
    let search = parse(value)?.search.ok_or_else(malformed)?;
    let complete = search.complete.ok_or_else(malformed)?;
    if search.matches.len() > 75 {
        return Err(malformed());
    }
    let prefix = format!("{}/", cwd.trim_end_matches('/'));
    let matches = search
        .matches
        .into_iter()
        .map(|item| {
            let inside = !item.relative_path.is_empty()
                && !item.relative_path.starts_with('/')
                && item
                    .relative_path
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != "..")
                && item.path == format!("{prefix}{}", item.relative_path);
            match (inside, item.score) {
                (true, Some(score)) => Ok(FileMatch {
                    path: item.path,
                    relative_path: item.relative_path,
                    score,
                }),
                _ => Err(
                    "Host returned a file search match outside the captured directory.".to_owned(),
                ),
            }
        })
        .collect::<Result<_, _>>()?;
    Ok(FileSearchResults { matches, complete })
}

/// A terminal path, resolved by the host for one pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalFileResolution {
    pub path: String,
    pub root: ActiveRoot,
    /// The topology generation of the host's final pane revalidation.
    pub topology_generation: u64,
}

pub fn decode_terminal_file(
    value: Value,
    server_identity: &str,
    pane_id: &str,
) -> Result<TerminalFileResolution, String> {
    let wire = parse(value)?;
    let (Some(root), Some(path)) = (
        wire.active_root,
        wire.metadata
            .map(|metadata| metadata.path)
            .filter(|path| !path.is_empty()),
    ) else {
        return Err("Host omitted the resolved terminal file.".into());
    };
    if root.server_identity != server_identity || root.pane_id != pane_id {
        return Err("Host resolved the terminal file for a stale pane scope.".into());
    }
    let topology_generation = decimal(&root.topology_generation)
        .ok_or("Host returned a malformed terminal file topology generation.")?;
    Ok(TerminalFileResolution {
        path,
        root: map_root(root),
        topology_generation,
    })
}

/// One file-service frame (kind 12), mapped into the events it carries. Port of
/// `publishWireEvent`.
pub fn decode_events(payload: &str) -> Result<Vec<WorkspaceEvent>, String> {
    let value: Value = serde_json::from_str(payload)
        .map_err(|_| "file-service payload is not valid UTF-8 JSON".to_owned())?;
    if !value.get("operationId").is_some_and(Value::is_string) {
        return Err("file-service payload is malformed".into());
    }
    let wire = parse(value)?;
    let mut events = Vec::new();
    if let Some(root) = wire.active_root {
        events.push(WorkspaceEvent::RootChanged(map_root(root)));
    }
    let token = wire.root_token;
    if token.is_empty() {
        return Ok(events);
    }
    if let Some(directory) = wire.directory {
        events.push(WorkspaceEvent::DirectorySnapshot {
            listing: map_directory(directory, &token),
            root_token: token.clone(),
        });
    }
    if let Some(metadata) = wire.metadata {
        let path = metadata.path.clone();
        events.push(if wire.deleted {
            WorkspaceEvent::FileDeleted {
                root_token: token,
                path,
            }
        } else {
            WorkspaceEvent::FileChanged {
                root_token: token,
                path,
                generation: metadata.generation.clone(),
                entry: is_renderable(&metadata).then(|| map_entry(metadata)),
                operation_id: wire.operation_id,
            }
        });
    }
    Ok(events)
}

pub fn decimal(value: &str) -> Option<u64> {
    let canonical =
        value == "0" || (!value.starts_with('0') && value.bytes().all(|b| b.is_ascii_digit()));
    if canonical && !value.is_empty() {
        value.parse().ok()
    } else {
        None
    }
}

/// The commands `desktop-core`'s `file_request` takes, as the renderer built
/// them.
pub mod command {
    use super::*;

    fn with_root(root: &ActiveRoot, mut command: Value) -> Value {
        command["root"] = json!(root.path);
        command["rootToken"] = json!(root.token);
        command
    }

    pub fn resolve_root(scope: &FileScope, known_token: Option<&str>, operation_id: &str) -> Value {
        json!({
            "operation": "resolveActiveRoot", "operationId": operation_id, "paneId": scope.pane_id,
            "expectedServerIdentity": scope.server_identity,
            "expectedTopologyGeneration": scope.generation.to_string(),
            "knownRootToken": known_token.unwrap_or_default(),
        })
    }

    pub fn list(
        root: &ActiveRoot,
        path: &str,
        page_token: Option<&str>,
        operation_id: &str,
    ) -> Value {
        with_root(
            root,
            json!({
                "operation": "listDirectory", "operationId": operation_id, "path": path,
                "pageToken": page_token.unwrap_or_default(), "pageSize": 4096,
            }),
        )
    }

    pub fn watch(root: &ActiveRoot, path: &str, watch_id: &str, operation_id: &str) -> Value {
        with_root(
            root,
            json!({
                "operation": "watchDirectory", "operationId": operation_id, "path": path, "watchId": watch_id,
            }),
        )
    }

    pub fn unwatch(watch_id: &str, operation_id: &str) -> Value {
        json!({ "operation": "unwatchDirectory", "operationId": operation_id, "watchId": watch_id })
    }

    pub fn search(
        scope: &FileScope,
        root: &ActiveRoot,
        pane: &PaneRoute,
        query: &str,
        operation_id: &str,
    ) -> Value {
        with_root(
            root,
            json!({
                "operation": "searchFiles", "operationId": operation_id, "paneId": scope.pane_id,
                "expectedServerIdentity": scope.server_identity,
                "expectedTopologyGeneration": scope.generation.to_string(),
                "expectedSessionId": pane.session_id, "expectedWindowId": pane.window_id,
                "expectedCwd": pane.cwd, "searchQuery": query,
            }),
        )
    }

    pub fn mutate(root: &ActiveRoot, mutation: &FileMutation, operation_id: &str) -> Value {
        let base = json!({ "operation": "mutate", "operationId": operation_id });
        let join = |parent: &str, name: &str| format!("{}/{name}", parent.trim_end_matches('/'));
        let mut command = match mutation {
            FileMutation::CreateFile { parent, name } => json!({
                "mutation": "create", "path": join(parent, name), "createDirectory": false,
            }),
            FileMutation::CreateDirectory { parent, name } => json!({
                "mutation": "create", "path": join(parent, name), "createDirectory": true,
            }),
            FileMutation::Delete {
                path,
                confirmed_non_empty,
            } => {
                json!({ "mutation": "delete", "path": path, "nonEmptyConfirmed": confirmed_non_empty })
            }
            FileMutation::Rename(relocation)
            | FileMutation::Move(relocation)
            | FileMutation::Duplicate(relocation) => json!({
                "mutation": match mutation {
                    FileMutation::Rename(_) => "rename",
                    FileMutation::Move(_) => "move",
                    _ => "duplicate",
                },
                "path": relocation.path, "destination": relocation.destination,
                "overwriteConfirmed": relocation.overwrite,
                "nonEmptyConfirmed": relocation.confirmed_non_empty,
            }),
        };
        for (key, value) in base.as_object().into_iter().flatten() {
            command[key] = value.clone();
        }
        with_root(root, command)
    }
}

/// The live pane only while the host's answer still describes the route that
/// asked. Port of `currentTerminalFilePane`: the generation is compared
/// monotonically, because an unrelated topology change on a busy pane advances
/// it without moving this pane.
pub fn current_terminal_file_pane<'a>(
    captured: &tmux_control::Pane,
    live: &'a [tmux_control::Pane],
    requested_generation: u64,
    resolved_generation: u64,
) -> Option<&'a tmux_control::Pane> {
    if resolved_generation < requested_generation {
        return None;
    }
    live.iter().find(|pane| {
        pane.id == captured.id
            && pane.session_id == captured.session_id
            && pane.window_id == captured.window_id
            && pane.current_path == captured.current_path
    })
}
