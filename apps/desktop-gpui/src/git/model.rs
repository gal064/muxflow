//! Git's plain values: the host's answers as `desktop-core` hands them over,
//! checked once on the way in, and the decisions the panel and the diff tab
//! make over them. Port of `features/git/types.ts`, the mapping half of
//! `api.ts`, and the pure helpers of `GitSidebar.tsx`, `GitCommitForm.tsx`,
//! `GitDiffSurface.tsx`, `useSharedGitDiff.ts` and `features/shell/model.ts`.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use desktop_core::app_state::{AppTabKind, AppTabRecord, PersistedAppState};
use serde::Deserialize;
use serde_json::Value;

pub use desktop_core::app_state::GitDiffTarget as Target;

/// The host's own bounded diff limit. A body it describes as larger than this
/// would not be one it could serve, so it is never allocated here.
pub const MAX_DEFERRED_BODY_BYTES: u64 = 10 * 1024 * 1024;

pub fn target_name(target: Target) -> &'static str {
    match target {
        Target::Staged => "staged",
        Target::Unstaged => "unstaged",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    None,
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
    Untracked,
    Ignored,
}

impl ChangeKind {
    fn from_wire(value: &str) -> Self {
        match value {
            "modified" => Self::Modified,
            "added" => Self::Added,
            "deleted" => Self::Deleted,
            "renamed" => Self::Renamed,
            "copied" => Self::Copied,
            "typeChanged" => Self::TypeChanged,
            "unmerged" => Self::Unmerged,
            "untracked" => Self::Untracked,
            "ignored" => Self::Ignored,
            _ => Self::None,
        }
    }

    /// The name the row's tooltip and status letter colour use.
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Modified => "modified",
            Self::Added => "added",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
            Self::Copied => "copied",
            Self::TypeChanged => "typeChanged",
            Self::Unmerged => "unmerged",
            Self::Untracked => "untracked",
            Self::Ignored => "ignored",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub id: String,
    pub worktree_root: String,
    pub initial: bool,
    pub detached_head: bool,
    pub head_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusEntry {
    /// Opaque base64 path identity. Never reconstruct this from `display_path`.
    pub path: String,
    pub display_path: String,
    /// The worktree root joined to `path`, for a same-host drag. Absent for a
    /// path that is not UTF-8, so a lossy display path is never dragged as if
    /// it were exact.
    pub absolute_path: Option<String>,
    pub original_path: Option<String>,
    pub display_original_path: Option<String>,
    pub index_kind: ChangeKind,
    pub worktree_kind: ChangeKind,
    pub conflicted: bool,
    pub conflict_code: String,
    pub untracked: bool,
    pub ignored: bool,
    pub submodule: bool,
    pub submodule_state: String,
    pub symlink: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusSnapshot {
    pub repository: Repository,
    pub generation: u64,
    pub source_generation: String,
    pub entries: Vec<StatusEntry>,
    pub authoritative: bool,
    pub oversized: bool,
    pub total_entry_count: u64,
    pub error: Option<String>,
    pub copy_detection_incomplete: bool,
}

impl StatusSnapshot {
    pub fn entry(&self, path: &str) -> Option<&StatusEntry> {
        self.entries.iter().find(|entry| entry.path == path)
    }
}

/// A diff body the control response withheld, to be read over the bulk lane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentRef {
    pub size: u64,
    pub digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diff {
    pub repository: Repository,
    pub target: Target,
    pub path: String,
    pub original_path: Option<String>,
    pub display_path: String,
    pub old_content: Vec<u8>,
    pub new_content: Vec<u8>,
    pub old_ref: Option<ContentRef>,
    pub new_ref: Option<ContentRef>,
    pub source_generation: String,
    pub binary: bool,
    pub too_large: bool,
    pub old_missing: bool,
    pub new_missing: bool,
    pub hunk_count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    NotApplied,
    Applied,
    PartialOrUnknown,
    Unspecified,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub refresh_failed: bool,
    pub refresh_error: String,
    pub outcome: Outcome,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub error: String,
    pub pre_head_oid: String,
    pub post_head_oid: String,
    pub status_omitted: bool,
    /// What a push published to, as `@{upstream}` names it. Pushes only.
    pub push_target: Option<String>,
    pub status: Option<StatusSnapshot>,
}

impl CommandResult {
    pub fn applied(&self) -> bool {
        self.outcome == Outcome::Applied
    }
}

/// A watch's status or error, as the bridge delivers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchEvent {
    pub watch_id: String,
    pub root_token: String,
    pub update: Result<StatusSnapshot, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationKind {
    StageFile,
    UnstageFile,
    DiscardFile,
    StageHunk,
    UnstageHunk,
    DiscardHunk,
}

impl MutationKind {
    pub fn wire(self) -> &'static str {
        match self {
            Self::StageFile => "stageFile",
            Self::UnstageFile => "unstageFile",
            Self::DiscardFile => "discardFile",
            Self::StageHunk => "stageHunk",
            Self::UnstageHunk => "unstageHunk",
            Self::DiscardHunk => "discardHunk",
        }
    }

    pub fn is_discard(self) -> bool {
        matches!(self, Self::DiscardFile | Self::DiscardHunk)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::StageFile | Self::StageHunk => "Staged",
            Self::UnstageFile | Self::UnstageHunk => "Unstaged",
            Self::DiscardFile | Self::DiscardHunk => "Discarded",
        }
    }

    /// What is being done to a row, while the host is still deciding whether
    /// it happened.
    pub fn pending_label(self) -> &'static str {
        match self {
            Self::StageFile => "staging…",
            Self::UnstageFile => "unstaging…",
            Self::DiscardFile => "discarding…",
            Self::StageHunk => "staging hunk…",
            Self::UnstageHunk => "unstaging hunk…",
            Self::DiscardHunk => "discarding hunk…",
        }
    }
}

/// One mutation, addressed by the opaque path and the status it was decided
/// against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mutation {
    pub kind: MutationKind,
    pub path: String,
    pub original_path: Option<String>,
    pub target: Target,
    pub expected_status_generation: u64,
    pub expected_source_generation: String,
    pub hunk_index: Option<u32>,
    pub confirmation_token: Option<String>,
}

// What `desktop-core` serializes (`connection/git.rs`). Every field defaults,
// because a missing one is the host's empty value, not a malformed answer.

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct WireRepository {
    repository_id: String,
    worktree_root: String,
    initial: bool,
    detached_head: bool,
    head_name: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct WireEntry {
    path: String,
    display_path: String,
    original_path: String,
    display_original_path: String,
    index_kind: String,
    worktree_kind: String,
    conflicted: bool,
    conflict_code: String,
    untracked: bool,
    ignored: bool,
    submodule: bool,
    submodule_state: String,
    symlink: bool,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct WireStatus {
    repository: Option<WireRepository>,
    generation: String,
    source_generation: String,
    entries: Option<Vec<WireEntry>>,
    authoritative: bool,
    oversized: bool,
    total_entry_count: Option<String>,
    error: String,
    copy_detection_incomplete: bool,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct WireContentRef {
    size: String,
    content_digest: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct WireDiff {
    repository: Option<WireRepository>,
    target: String,
    path: Vec<u8>,
    original_path: Vec<u8>,
    display_path: String,
    old_content: Vec<u8>,
    new_content: Vec<u8>,
    source_generation: String,
    binary: bool,
    too_large: bool,
    old_missing: bool,
    new_missing: bool,
    hunk_count: u32,
    old_content_ref: Option<WireContentRef>,
    new_content_ref: Option<WireContentRef>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct WireCommand {
    exit_code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status: Option<WireStatus>,
    refresh_failed: bool,
    refresh_error: String,
    outcome: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
    error: String,
    pre_head_oid: String,
    post_head_oid: String,
    pre_status_generation: String,
    post_status_generation: String,
    status_omitted: bool,
    push_target: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct WireConfirmation {
    token: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct WireResponse {
    status: Option<WireStatus>,
    diff: Option<WireDiff>,
    confirmation: Option<WireConfirmation>,
    command: Option<WireCommand>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct WireEvent {
    watch_id: String,
    root_token: String,
    status: Option<WireStatus>,
    error: String,
}

pub(super) fn response(value: Value) -> Result<WireResponse, String> {
    serde_json::from_value(value)
        .map_err(|error| format!("Host returned malformed Git response: {error}"))
}

fn decimal_u64(value: &str, label: &str) -> Result<u64, String> {
    let canonical = value == "0" || (!value.starts_with('0') && !value.is_empty());
    match value.parse::<u64>() {
        Ok(parsed) if canonical && value.bytes().all(|byte| byte.is_ascii_digit()) => Ok(parsed),
        _ => Err(format!("{label} is not a decimal u64 string.")),
    }
}

fn optional(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

/// A status path is canonical base64 and nothing else: every consumer
/// round-trips it back to bytes to address a file.
fn require_base64_path(value: &str, label: &str) -> Result<(), String> {
    let decoded = BASE64.decode(value);
    match decoded {
        Ok(bytes) if BASE64.encode(&bytes) == value => Ok(()),
        _ => Err(format!("Host returned malformed Git status: {label}.")),
    }
}

pub fn path_bytes(path: &str) -> Vec<u8> {
    BASE64.decode(path).unwrap_or_default()
}

fn repository(value: WireRepository) -> Repository {
    Repository {
        id: value.repository_id,
        worktree_root: value.worktree_root,
        initial: value.initial,
        detached_head: value.detached_head,
        head_name: optional(value.head_name),
    }
}

pub(super) fn status(value: WireStatus) -> Result<StatusSnapshot, String> {
    let (Some(repo), Some(entries)) = (value.repository, value.entries) else {
        return Err("Host returned malformed Git status.".into());
    };
    if repo.repository_id.is_empty() {
        return Err("Host returned malformed Git status.".into());
    }
    let generation = decimal_u64(&value.generation, "Git status generation")?;
    let total_entry_count = match value.total_entry_count {
        Some(count) => decimal_u64(&count, "Git status total entry count")?,
        None => entries.len() as u64,
    };
    let root = repo.worktree_root.clone();
    let entries = entries
        .into_iter()
        .map(|entry| {
            require_base64_path(&entry.path, "Git status path")?;
            if !entry.original_path.is_empty() {
                require_base64_path(&entry.original_path, "Git original path")?;
            }
            let absolute_path = (!root.is_empty())
                .then(|| String::from_utf8(path_bytes(&entry.path)).ok())
                .flatten()
                .map(|relative| format!("{root}/{relative}"));
            Ok(StatusEntry {
                absolute_path,
                path: entry.path,
                display_path: entry.display_path,
                original_path: optional(entry.original_path),
                display_original_path: optional(entry.display_original_path),
                index_kind: ChangeKind::from_wire(&entry.index_kind),
                worktree_kind: ChangeKind::from_wire(&entry.worktree_kind),
                conflicted: entry.conflicted,
                conflict_code: entry.conflict_code,
                untracked: entry.untracked,
                ignored: entry.ignored,
                submodule: entry.submodule,
                submodule_state: entry.submodule_state,
                symlink: entry.symlink,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(StatusSnapshot {
        repository: repository(repo),
        generation,
        source_generation: value.source_generation,
        entries,
        authoritative: value.authoritative,
        oversized: value.oversized,
        total_entry_count,
        error: optional(value.error),
        copy_detection_incomplete: value.copy_detection_incomplete,
    })
}

pub(super) fn response_status(response: WireResponse) -> Result<StatusSnapshot, String> {
    status(response.status.ok_or("Host omitted Git status.")?)
}

fn content_ref(value: Option<WireContentRef>) -> Result<Option<ContentRef>, String> {
    let Some(value) = value else { return Ok(None) };
    let size = decimal_u64(&value.size, "Git diff content size")?;
    if value.content_digest.is_empty() {
        return Err("Host omitted the Git diff content digest.".into());
    }
    Ok(Some(ContentRef {
        size,
        digest: value.content_digest,
    }))
}

/// What a diff request asked for, which its answer must name back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffRequest {
    pub repository_id: String,
    pub path: String,
    pub original_path: Option<String>,
    pub target: Target,
}

/// The diff and the authoritative status it was read against, in one round
/// trip.
pub(super) fn response_diff(
    response: WireResponse,
    request: &DiffRequest,
) -> Result<(Diff, StatusSnapshot), String> {
    let value = response.diff.ok_or("Host omitted the Git diff.")?;
    let target = match value.target.as_str() {
        "staged" => Target::Staged,
        "unstaged" => Target::Unstaged,
        _ => return Err("Host returned an unknown Git diff target.".into()),
    };
    let diff = Diff {
        repository: repository(value.repository.unwrap_or_default()),
        target,
        path: BASE64.encode(&value.path),
        original_path: (!value.original_path.is_empty())
            .then(|| BASE64.encode(&value.original_path)),
        display_path: value.display_path,
        old_content: value.old_content,
        new_content: value.new_content,
        old_ref: content_ref(value.old_content_ref)?,
        new_ref: content_ref(value.new_content_ref)?,
        source_generation: value.source_generation,
        binary: value.binary,
        too_large: value.too_large,
        old_missing: value.old_missing,
        new_missing: value.new_missing,
        hunk_count: value.hunk_count,
    };
    if diff.repository.id != request.repository_id
        || diff.path != request.path
        || diff.original_path != request.original_path
        || diff.target != request.target
    {
        return Err("Host returned a stale Git diff identity.".into());
    }
    let status = response_status(WireResponse {
        status: response.status,
        ..WireResponse::default()
    })?;
    if status.repository.id != request.repository_id {
        return Err("Host returned a Git diff status for a different repository.".into());
    }
    Ok((diff, status))
}

pub(super) fn response_confirmation(response: WireResponse) -> Result<String, String> {
    response
        .confirmation
        .map(|confirmation| confirmation.token)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "Host omitted the discard confirmation token.".to_owned())
}

fn output_text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|_| "Git returned non-UTF-8 output.".into())
}

pub(super) fn response_command(response: WireResponse) -> Result<CommandResult, String> {
    let value = response
        .command
        .ok_or("Host omitted the Git mutation result.")?;
    for (generation, label) in [
        (&value.pre_status_generation, "Git pre-status generation"),
        (&value.post_status_generation, "Git post-status generation"),
    ] {
        if !generation.is_empty() {
            decimal_u64(generation, label)?;
        }
    }
    Ok(CommandResult {
        exit_code: value.exit_code,
        stdout: output_text(value.stdout),
        stderr: output_text(value.stderr),
        refresh_failed: value.refresh_failed,
        refresh_error: value.refresh_error,
        outcome: match value.outcome.as_str() {
            "notApplied" => Outcome::NotApplied,
            "applied" => Outcome::Applied,
            "partialOrUnknown" => Outcome::PartialOrUnknown,
            _ => Outcome::Unspecified,
        },
        stdout_truncated: value.stdout_truncated,
        stderr_truncated: value.stderr_truncated,
        error: value.error,
        pre_head_oid: value.pre_head_oid,
        post_head_oid: value.post_head_oid,
        status_omitted: value.status_omitted,
        push_target: optional(value.push_target),
        status: value.status.map(status).transpose()?,
    })
}

/// A bridge event. One with no root token addresses nothing; one whose status
/// does not hold up becomes an error for its watch rather than a crash.
pub fn watch_events(payload: &str) -> Vec<WatchEvent> {
    let Ok(event) = serde_json::from_str::<WireEvent>(payload) else {
        return Vec::new();
    };
    if event.root_token.is_empty() {
        return Vec::new();
    }
    let mut events = Vec::new();
    let to = |update| WatchEvent {
        watch_id: event.watch_id.clone(),
        root_token: event.root_token.clone(),
        update,
    };
    if let Some(value) = event.status {
        events.push(to(status(value)));
    }
    if !event.error.is_empty() {
        events.push(to(Err(event.error.clone())));
    }
    events
}

// The repository a panel or tab observes.

/// Everything a repository observation is bound to. Anything narrower would
/// let a reconnected or re-rooted consumer inherit an observation it can no
/// longer address.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GitScope {
    pub client_id: String,
    pub host_profile_id: String,
    pub server_identity: String,
    pub terminal_epoch: u64,
    pub root_path: String,
    pub root_token: String,
}

impl GitScope {
    pub fn key(&self) -> String {
        [
            self.client_id.as_str(),
            &self.host_profile_id,
            &self.server_identity,
            &self.terminal_epoch.to_string(),
            &self.root_token,
            &self.root_path,
        ]
        .join("\0")
    }
}

/// The active pane's root, as the Explorer resolves it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootInfo {
    pub path: String,
    pub token: String,
    pub git_worktree: bool,
}

/// The live link a request can go over: connected, with its client, its
/// native epoch and the tmux server it describes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Live {
    pub connected: bool,
    pub client_id: Option<String>,
    pub terminal_epoch: u64,
    pub server_identity: Option<String>,
    pub host_profile_id: String,
}

/// The Git half of `useWorkspaceDomainController`: a request-capable scope
/// only once the bridge says it is connected (its snapshot arrives first), with
/// every transport identity present.
pub fn git_scope(link: &Live, root: &RootInfo) -> Option<GitScope> {
    let (true, Some(client_id), Some(server_identity)) =
        (link.connected, &link.client_id, &link.server_identity)
    else {
        return None;
    };
    (link.terminal_epoch != 0 && !server_identity.is_empty() && !root.token.is_empty()).then(|| {
        GitScope {
            client_id: client_id.clone(),
            host_profile_id: link.host_profile_id.clone(),
            server_identity: server_identity.clone(),
            terminal_epoch: link.terminal_epoch,
            root_path: root.path.clone(),
            root_token: root.token.clone(),
        }
    })
}

// The Source Control panel.

pub struct Groups<'a> {
    pub conflicts: Vec<&'a StatusEntry>,
    pub staged: Vec<&'a StatusEntry>,
    pub unstaged: Vec<&'a StatusEntry>,
    pub untracked: Vec<&'a StatusEntry>,
}

pub fn group_entries(entries: &[StatusEntry]) -> Groups<'_> {
    let visible = || entries.iter().filter(|entry| !entry.ignored);
    Groups {
        conflicts: visible().filter(|entry| entry.conflicted).collect(),
        staged: visible()
            .filter(|entry| !entry.conflicted && entry.index_kind != ChangeKind::None)
            .collect(),
        unstaged: visible()
            .filter(|entry| {
                !entry.conflicted && !entry.untracked && entry.worktree_kind != ChangeKind::None
            })
            .collect(),
        untracked: visible().filter(|entry| entry.untracked).collect(),
    }
}

pub fn status_code(entry: &StatusEntry, target: Target) -> &'static str {
    if entry.conflicted {
        return "U";
    }
    match kind_for(entry, target) {
        ChangeKind::Modified => "M",
        ChangeKind::Added => "A",
        ChangeKind::Deleted => "D",
        ChangeKind::Renamed => "R",
        ChangeKind::Copied => "C",
        ChangeKind::TypeChanged => "T",
        ChangeKind::Untracked => "U",
        ChangeKind::Ignored => "I",
        ChangeKind::Unmerged => "!",
        ChangeKind::None => "",
    }
}

pub fn kind_for(entry: &StatusEntry, target: Target) -> ChangeKind {
    match target {
        Target::Staged => entry.index_kind,
        Target::Unstaged => entry.worktree_kind,
    }
}

pub fn status_title(entry: &StatusEntry, target: Target) -> String {
    if entry.conflicted {
        format!("Conflict {}", entry.conflict_code)
    } else {
        kind_for(entry, target).name().to_owned()
    }
}

/// The basename in full, and the directory beside it as context.
pub fn split_display_path(display_path: &str) -> (&str, &str) {
    match display_path.rfind('/') {
        Some(slash) => (&display_path[slash + 1..], &display_path[..slash]),
        None => (display_path, ""),
    }
}

/// What the panel's header names the repository by.
pub fn head_label(repository: &Repository) -> &str {
    match &repository.head_name {
        Some(name) => name,
        None if repository.initial => "Initial repository",
        None => "Detached HEAD",
    }
}

/// The notice for a settled row command. A refreshed row already shows
/// success, so this is said only when something needs saying.
pub fn result_message(result: &CommandResult, fallback: &str) -> String {
    let parts: Vec<&str> = [
        result.stdout.trim(),
        result.stderr.trim(),
        result.error.as_str(),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect();
    let command = if parts.is_empty() {
        match result.outcome {
            Outcome::Applied => fallback.to_owned(),
            Outcome::PartialOrUnknown => {
                "Git outcome is partial or unknown; inspect the repository before retrying.".into()
            }
            _ => format!("Git failed with exit code {}.", result.exit_code),
        }
    } else {
        parts.join(" · ")
    };
    if result.refresh_failed {
        let why = if result.status_omitted {
            "Post-command status was omitted to keep the connection responsive"
        } else {
            "Status refresh failed"
        };
        format!("{command} {why}: {}", result.refresh_error)
    } else {
        command
    }
}

/// Whether a settled command says more than its refreshed rows do.
pub fn needs_notice(result: &CommandResult) -> bool {
    !result.applied()
        || result.refresh_failed
        || result.status_omitted
        || !result.stderr.trim().is_empty()
        || !result.error.is_empty()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Commit,
    Push,
}

impl Verb {
    pub fn name(self) -> &'static str {
        match self {
            Self::Commit => "Commit",
            Self::Push => "Push",
        }
    }
}

pub fn pushed_to(result: &CommandResult) -> String {
    format!(
        "Pushed to {}.",
        result.push_target.as_deref().unwrap_or("the upstream")
    )
}

/// The commit form's output block.
pub fn command_details(result: &CommandResult, verb: Verb) -> String {
    let uncertain = format!(
        "Outcome is partial or unknown. HEAD {} → {}. Inspect the repository before retrying.",
        or_question(&result.pre_head_oid),
        or_question(&result.post_head_oid)
    );
    let refresh = format!(
        "{} completed, but {}: {}",
        verb.name(),
        if result.status_omitted {
            "post-command status was omitted to keep the connection responsive"
        } else {
            "status refresh failed"
        },
        result.refresh_error
    );
    let lines: Vec<&str> = [
        result.stdout.as_str(),
        if result.stdout_truncated {
            "[stdout truncated]"
        } else {
            ""
        },
        result.stderr.as_str(),
        if result.stderr_truncated {
            "[stderr truncated]"
        } else {
            ""
        },
        result.error.as_str(),
        if result.outcome == Outcome::PartialOrUnknown {
            &uncertain
        } else {
            ""
        },
        if result.refresh_failed { &refresh } else { "" },
    ]
    .into_iter()
    .filter(|line| !line.is_empty())
    .collect();
    if !lines.is_empty() {
        return lines.join("\n");
    }
    match (result.applied(), verb) {
        (true, Verb::Push) => pushed_to(result),
        (true, Verb::Commit) => "Commit created.".into(),
        _ => format!("Git exited with code {}.", result.exit_code),
    }
}

fn or_question(value: &str) -> &str {
    if value.is_empty() { "?" } else { value }
}

// The diff tab.

/// The one Git diff tab a workspace holds for this repository, path and side.
pub fn find_git_diff_tab<'a>(
    state: &'a PersistedAppState,
    host: &str,
    server: &str,
    session_id: &str,
    repository_id: &str,
    path: &str,
    target: Target,
) -> Option<&'a AppTabRecord> {
    state.app_tabs.iter().find(|tab| {
        tab.host_profile_id == host
            && tab.server_identity == server
            && tab.session_id == session_id
            && tab.kind == AppTabKind::GitDiff
            && tab.git_repository_id.as_deref() == Some(repository_id)
            && tab.git_path.as_deref() == Some(path)
            && tab.git_target == Some(target)
    })
}

/// Where a diff tab opens: the workspace and the repository it belongs to.
pub struct DiffTabPlace<'a> {
    pub host: &'a str,
    pub server: &'a str,
    pub session_id: &'a str,
    pub session_name: &'a str,
    pub root_path: &'a str,
    pub root_token: &'a str,
}

/// Opens or repoints the diff tab, returning its id. A single click opens a
/// preview, which a pinned open promotes; a preview open never demotes a
/// pinned tab. The caller selects it.
pub fn open_git_diff_tab(
    state: &mut PersistedAppState,
    place: &DiffTabPlace,
    entry: &StatusEntry,
    target: Target,
    status: &StatusSnapshot,
    preview: bool,
    new_id: impl FnOnce() -> String,
) -> String {
    let existing = find_git_diff_tab(
        state,
        place.host,
        place.server,
        place.session_id,
        &status.repository.id,
        &entry.path,
        target,
    )
    .map(|tab| tab.id.clone());
    let (name, _) = split_display_path(&entry.display_path);
    let describe = |tab: &mut AppTabRecord| {
        tab.resource = format!("{}:{}", target_name(target), entry.display_path);
        tab.title = format!("{name} ({})", target_name(target));
        tab.root_path = Some(place.root_path.to_owned());
        tab.root_token = Some(place.root_token.to_owned());
        tab.git_repository_id = Some(status.repository.id.clone());
        tab.git_path = Some(entry.path.clone());
        tab.git_original_path = entry.original_path.clone();
        tab.git_target = Some(target);
        tab.git_status_generation = Some(status.generation.to_string());
        tab.git_source_generation = Some(status.source_generation.clone());
    };
    if let Some(id) = existing {
        let tab = state
            .app_tabs
            .iter_mut()
            .find(|tab| tab.host_profile_id == place.host && tab.id == id)
            .expect("the tab just found");
        describe(tab);
        if !preview {
            tab.preview = None;
        }
        return id;
    }
    let order = state
        .app_tabs
        .iter()
        .filter(|tab| {
            tab.host_profile_id == place.host
                && tab.server_identity == place.server
                && tab.session_id == place.session_id
        })
        .count() as u32;
    let mut tab = AppTabRecord {
        id: new_id(),
        host_profile_id: place.host.to_owned(),
        server_identity: place.server.to_owned(),
        session_id: place.session_id.to_owned(),
        session_name: place.session_name.to_owned(),
        kind: AppTabKind::GitDiff,
        resource: String::new(),
        root_path: None,
        root_token: None,
        preview: preview.then_some(true),
        view_mode: None,
        git_repository_id: None,
        git_path: None,
        git_original_path: None,
        git_target: None,
        git_status_generation: None,
        git_source_generation: None,
        title: String::new(),
        order,
    };
    describe(&mut tab);
    let id = tab.id.clone();
    state.app_tabs.push(tab);
    id
}

/// The path a diff tab is about, before any diff has arrived to say it. A
/// saved tab carries `target:displayPath` as its resource; anything else is
/// shown whole rather than guessed at.
pub fn tab_display_path(tab: &AppTabRecord) -> &str {
    tab.git_target
        .and_then(|target| {
            tab.resource
                .strip_prefix(target_name(target))
                .and_then(|rest| rest.strip_prefix(':'))
        })
        .unwrap_or(&tab.resource)
}

/// Whether the status still reports the change a tab is displaying.
pub fn entry_still_changed(status: &StatusSnapshot, path: &str, target: Target) -> bool {
    status
        .entry(path)
        .is_some_and(|entry| kind_for(entry, target) != ChangeKind::None)
}

/// What the content area of a diff tab shows, decided once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shown {
    /// Displayable, but not as text.
    Blocked(&'static str),
    Text {
        original: String,
        modified: String,
    },
}

pub fn shown_diff(diff: &Diff) -> Shown {
    if diff.binary {
        return Shown::Blocked("Binary changes cannot be displayed or edited as text.");
    }
    let text = |missing: bool, bytes: &[u8]| {
        if missing {
            Ok(String::new())
        } else {
            String::from_utf8(bytes.to_vec())
        }
    };
    let (Ok(original), Ok(modified)) = (
        text(diff.old_missing, &diff.old_content),
        text(diff.new_missing, &diff.new_content),
    ) else {
        return Shown::Blocked(
            "This diff contains non-UTF-8 content and is shown safely as binary.",
        );
    };
    if diff.too_large {
        return Shown::Blocked(
            "This diff is too large for the editor. File-level Git actions remain available on the Source Control row.",
        );
    }
    Shown::Text { original, modified }
}

/// A unified patch holding the whole file as one hunk, which Kit's `Diff`
/// draws: today's diff shows every line, not only the changed regions. The
/// patch's file name is only for highlighting, so an unusual name falls back
/// to a plain one rather than a header the parser could misread.
pub fn whole_file_patch(display_path: &str, original: &str, modified: &str) -> String {
    let (name, _) = split_display_path(display_path);
    let safe = !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    let name = if safe { name } else { "file" };
    // Lines end at `\n` only, as the patch parser reads them: `similar`'s own
    // line split would also break at a lone `\r` and produce a patch whose
    // hunk counts the parser rejects.
    fn lines(text: &str) -> Vec<&str> {
        match text.strip_suffix('\n').unwrap_or(text) {
            "" if text.is_empty() => Vec::new(),
            body => body.split('\n').collect(),
        }
    }
    let (old, new) = (lines(original), lines(modified));
    let radius = old.len().max(new.len()) + 1;
    // Bounded, as Monaco's diff computation was: past the deadline the diff
    // is coarser but still correct.
    let diff = similar::TextDiff::configure()
        .timeout(std::time::Duration::from_millis(500))
        .diff_slices(&old, &new);
    let patch = diff
        .unified_diff()
        .context_radius(radius)
        .header(&format!("a/{name}"), &format!("b/{name}"))
        .to_string();
    if !patch.is_empty() || original.is_empty() {
        return patch;
    }
    // No textual change (a mode change, say): the file is still shown.
    let count = original.split_inclusive('\n').count();
    let mut patch = format!("--- a/{name}\n+++ b/{name}\n@@ -1,{count} +1,{count} @@\n");
    for line in original.split_inclusive('\n') {
        patch.push(' ');
        patch.push_str(line);
    }
    if !original.ends_with('\n') {
        patch.push_str("\n\\ No newline at end of file\n");
    }
    patch
}

/// Diff-body frames, matching `desktop-core`'s `connection/git_content.rs`.
const FRAME_CHUNK: u8 = 1;
const FRAME_COMPLETE: u8 = 2;
const SIDE_OLD: u8 = 1;
const SIDE_NEW: u8 = 2;

/// The withheld bodies of one diff, assembled from the bulk lane's frames.
pub struct DeferredBodies {
    sides: Vec<(u8, Vec<u8>, usize)>,
}

impl DeferredBodies {
    pub fn new(old: Option<&ContentRef>, new: Option<&ContentRef>) -> Result<Self, String> {
        let mut sides = Vec::new();
        for (tag, side) in [(SIDE_OLD, old), (SIDE_NEW, new)] {
            let Some(side) = side else { continue };
            if side.size == 0 || side.size > MAX_DEFERRED_BODY_BYTES {
                return Err(
                    "Host described a Git diff body outside the bounded diff limit.".into(),
                );
            }
            sides.push((tag, vec![0; side.size as usize], 0));
        }
        Ok(Self { sides })
    }

    /// Takes one frame. `Ok(true)` once both bodies are complete.
    pub fn accept(&mut self, frame: &[u8]) -> Result<bool, String> {
        match frame.first() {
            None => Err("Host emitted an empty Git diff content frame.".into()),
            Some(&FRAME_CHUNK) => {
                if frame.len() < 10 {
                    return Err("Host emitted an invalid Git diff content chunk.".into());
                }
                let Some((_, body, received)) =
                    self.sides.iter_mut().find(|(tag, ..)| *tag == frame[1])
                else {
                    return Err(
                        "Host emitted a Git diff content chunk for a side that was not requested."
                            .into(),
                    );
                };
                let offset = u64::from_be_bytes(frame[2..10].try_into().expect("eight bytes"));
                let data = &frame[10..];
                if offset != *received as u64 || *received + data.len() > body.len() {
                    return Err("Host emitted an out-of-order Git diff content chunk.".into());
                }
                body[*received..*received + data.len()].copy_from_slice(data);
                *received += data.len();
                Ok(false)
            }
            Some(&FRAME_COMPLETE) => {
                if self
                    .sides
                    .iter()
                    .any(|(_, body, received)| *received != body.len())
                {
                    return Err("Git diff content ended before the described bodies.".into());
                }
                Ok(true)
            }
            Some(_) => {
                let message = String::from_utf8_lossy(&frame[1..]).into_owned();
                Err(if message.is_empty() {
                    "Git diff content read failed.".into()
                } else {
                    message
                })
            }
        }
    }

    /// Fills the diff's withheld sides.
    pub fn fill(self, diff: &mut Diff) {
        for (tag, body, _) in self.sides {
            if tag == SIDE_OLD {
                diff.old_content = body;
            } else {
                diff.new_content = body;
            }
        }
    }
}
