use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
    path::{Component, Path},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::bail;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tmux_agent_protocol::v1;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::{SequencerControl, snapshot::server_identity};

const MAX_GIT_OUTPUT: usize = 12 * 1024 * 1024;
const MAX_DIFF_CONTENT: usize = 10 * 1024 * 1024;
const MAX_GIT_STATUS_ENCODED: usize = 10 * 1024 * 1024;
const MAX_GIT_COMMAND_ENVELOPE: usize = 15 * 1024 * 1024;
const MAX_GIT_DIAGNOSTIC: usize = 4 * 1024;
const CONFIRMATION_TTL: Duration = Duration::from_secs(120);
const MAX_CONFIRMATIONS: usize = 1024;

mod diff;
use diff::read_diff;
mod mutation;
use mutation::{discard_file, mutate_hunk, unstage_file};
#[cfg(test)]
mod measurements;
#[cfg(test)]
use measurements::phase14_git_snapshot;
#[cfg(test)]
use measurements::{
    phase14_git_process_started, phase14_git_subscribers, phase14_git_watcher_created,
    phase14_git_watcher_dropped,
};
mod path;
use path::WorktreeRoot;
mod parser;
use parser::parse_porcelain_v2_z;
mod runner;
#[cfg(test)]
use runner::command_result;
use runner::{
    GIT_COMMIT_DEADLINE, GitMetadataCapability, GitOutput, ensure_success, git_index_generation,
    git_output_with_deadline, git_path_cancellable,
};
mod status;
use status::{discover_repository, read_status_cancellable, validate_metadata_capability};
mod watch;
use watch::GitWatch;
#[cfg(test)]
use watch::start_repository_watcher;

static REPOSITORY_LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

#[derive(Clone)]
struct ConfirmationBinding {
    repository_id: String,
    root: String,
    root_token: String,
    server_identity: String,
    connection_epoch: u64,
    mutation: i32,
    path: Vec<u8>,
    original_path: Vec<u8>,
    diff_target: i32,
    hunk_index: u32,
    status_generation: u64,
    source_generation: String,
    expires: SystemTime,
}

#[derive(Default)]
struct RepositoryState {
    generation: u64,
    fingerprint: String,
    issued_refresh: u64,
    applied_refresh: u64,
}

#[derive(Debug)]
struct StatusRefreshSuperseded;

impl std::fmt::Display for StatusRefreshSuperseded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Git status refresh superseded by a newer snapshot")
    }
}

impl std::error::Error for StatusRefreshSuperseded {}

pub(super) struct GitService {
    states: Mutex<HashMap<String, RepositoryState>>,
    confirmations: Mutex<HashMap<String, ConfirmationBinding>>,
    watches: Mutex<HashMap<String, GitWatch>>,
    next_generation: AtomicU64,
}

impl GitService {
    pub(super) fn new() -> Self {
        Self {
            states: Mutex::new(HashMap::new()),
            confirmations: Mutex::new(HashMap::new()),
            watches: Mutex::new(HashMap::new()),
            next_generation: AtomicU64::new(0),
        }
    }

    #[cfg(test)]
    pub(super) fn status(&self, request: &v1::GitRequest) -> anyhow::Result<v1::GitStatusSnapshot> {
        self.status_cancellable(request, None)
    }

    pub(super) fn status_cancellable(
        &self,
        request: &v1::GitRequest,
        cancellation: Option<&AtomicBool>,
    ) -> anyhow::Result<v1::GitStatusSnapshot> {
        let root = capture_request_root(request)?;
        self.status_with_root(request, &root, cancellation)
    }

    fn status_with_root(
        &self,
        request: &v1::GitRequest,
        root: &WorktreeRoot,
        cancellation: Option<&AtomicBool>,
    ) -> anyhow::Result<v1::GitStatusSnapshot> {
        let stable_root = root.stable_path();
        let repository =
            discover_repository(&stable_root, &request.root, root.identity()?, cancellation)?;
        validate_repository_id(request, &repository)?;
        let metadata = GitMetadataCapability::capture(&repository.git_dir, &repository.common_dir)?;
        validate_metadata_capability(&request.root, root.identity()?, &repository, &metadata)?;
        let _metadata_guard = metadata.install();
        self.status_with_repository(request, &stable_root, repository, cancellation)
    }

    fn status_with_repository(
        &self,
        _request: &v1::GitRequest,
        root: &str,
        repository: v1::GitRepository,
        cancellation: Option<&AtomicBool>,
    ) -> anyhow::Result<v1::GitStatusSnapshot> {
        let repository_id = repository.repository_id.clone();
        let refresh = {
            let mut states = self.states.lock().unwrap();
            let state = states.entry(repository_id.clone()).or_default();
            state.issued_refresh = state.issued_refresh.saturating_add(1);
            state.issued_refresh
        };
        let mut snapshot = read_status_cancellable(root, repository, cancellation)?;
        let fingerprint = status_fingerprint(&snapshot);
        let mut states = self.states.lock().unwrap();
        let state = states.entry(repository_id).or_default();
        if refresh < state.applied_refresh {
            return Err(StatusRefreshSuperseded.into());
        }
        state.applied_refresh = refresh;
        if state.fingerprint != fingerprint {
            state.fingerprint = fingerprint.clone();
            state.generation = self.next_generation.fetch_add(1, Ordering::AcqRel) + 1;
        }
        snapshot.generation = state.generation;
        snapshot.source_generation = fingerprint;
        Ok(snapshot)
    }

    #[cfg(test)]
    pub(super) fn diff(&self, request: &v1::GitRequest) -> anyhow::Result<v1::GitDiff> {
        self.diff_cancellable(request, None)
    }

    pub(super) fn diff_cancellable(
        &self,
        request: &v1::GitRequest,
        cancellation: Option<&AtomicBool>,
    ) -> anyhow::Result<v1::GitDiff> {
        if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            bail!("Git diff cancelled");
        }
        let root = capture_request_root(request)?;
        let stable_root = root.stable_path();
        let repository =
            discover_repository(&stable_root, &request.root, root.identity()?, cancellation)?;
        validate_repository_id_required(request, &repository)?;
        let metadata = GitMetadataCapability::capture(&repository.git_dir, &repository.common_dir)?;
        validate_metadata_capability(&request.root, root.identity()?, &repository, &metadata)?;
        let _metadata_guard = metadata.install();
        let current =
            self.status_with_repository(request, &stable_root, repository.clone(), cancellation)?;
        validate_status_generation(request, &current)?;
        let diff = read_diff(&stable_root, repository, request, cancellation)?;
        let after = self.status_with_repository(
            request,
            &stable_root,
            current.repository.clone().unwrap(),
            cancellation,
        )?;
        if after.generation != current.generation {
            bail!("stale Git status generation during diff");
        }
        if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            bail!("Git diff cancelled");
        }
        Ok(diff)
    }

    pub(super) fn prepare_discard(
        &self,
        request: &v1::GitRequest,
        connection_epoch: u64,
    ) -> anyhow::Result<v1::GitConfirmation> {
        let root = capture_request_root(request)?;
        let stable_root = root.stable_path();
        let repository = discover_repository(&stable_root, &request.root, root.identity()?, None)?;
        validate_repository_id_required(request, &repository)?;
        ensure_connection_epoch(request, connection_epoch)?;
        let mutation = v1::GitMutationKind::try_from(request.mutation).unwrap_or_default();
        if !matches!(
            mutation,
            v1::GitMutationKind::DiscardFile | v1::GitMutationKind::DiscardHunk
        ) {
            bail!("confirmation tokens are issued only for discard operations");
        }
        if request.expected_status_generation == 0 {
            bail!("expected status generation is required for discard");
        }
        let token = Uuid::new_v4().to_string();
        let expires = SystemTime::now() + CONFIRMATION_TTL;
        let mut confirmations = self.confirmations.lock().unwrap();
        confirmations.retain(|_, binding| binding.expires >= SystemTime::now());
        if confirmations.len() >= MAX_CONFIRMATIONS {
            bail!("too many pending discard confirmations");
        }
        confirmations.insert(
            token.clone(),
            ConfirmationBinding {
                repository_id: repository.repository_id,
                root: request.root.clone(),
                root_token: request.root_token.clone(),
                server_identity: request.expected_server_identity.clone(),
                connection_epoch,
                mutation: request.mutation,
                path: request.path.clone(),
                original_path: request.original_path.clone(),
                diff_target: request.diff_target,
                hunk_index: request.hunk_index,
                status_generation: request.expected_status_generation,
                source_generation: request.expected_source_generation.clone(),
                expires,
            },
        );
        Ok(v1::GitConfirmation {
            token,
            expires_unix_millis: expires
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
        })
    }

    pub(super) async fn mutate(
        self: &Arc<Self>,
        request: v1::GitRequest,
        connection_epoch: u64,
        cancellation: Arc<AtomicBool>,
    ) -> anyhow::Result<v1::GitCommandResult> {
        let preflight = request.clone();
        let preflight_cancellation = Arc::clone(&cancellation);
        let (root, repository, metadata) = tokio::task::spawn_blocking(move || {
            let root = capture_request_root(&preflight)?;
            ensure_connection_epoch(&preflight, connection_epoch)?;
            let repository = discover_repository(
                &root.stable_path(),
                &preflight.root,
                root.identity()?,
                Some(&preflight_cancellation),
            )?;
            validate_repository_id_required(&preflight, &repository)?;
            let metadata =
                GitMetadataCapability::capture(&repository.git_dir, &repository.common_dir)?;
            validate_metadata_capability(
                &preflight.root,
                root.identity()?,
                &repository,
                &metadata,
            )?;
            Ok::<_, anyhow::Error>((root, repository, metadata))
        })
        .await
        .map_err(|error| anyhow::anyhow!("Git mutation preflight task failed: {error}"))??;
        let guard = repository_lock(&repository.repository_id)
            .lock_owned()
            .await;
        if cancellation.load(Ordering::Acquire) {
            bail!("cancelled before mutation");
        }
        let service = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            service.mutate_locked(
                &request,
                connection_epoch,
                &cancellation,
                &root,
                repository,
                metadata,
            )
        })
        .await
        .map_err(|error| anyhow::anyhow!("Git mutation task failed: {error}"))?
    }

    fn mutate_locked(
        &self,
        request: &v1::GitRequest,
        connection_epoch: u64,
        cancellation: &AtomicBool,
        root: &WorktreeRoot,
        repository: v1::GitRepository,
        metadata: GitMetadataCapability,
    ) -> anyhow::Result<v1::GitCommandResult> {
        if cancellation.load(Ordering::Acquire) {
            bail!("cancelled before mutation");
        }
        let stable_root = root.stable_path();
        let _metadata_guard = metadata.install();
        let current = self.status_with_repository(
            request,
            &stable_root,
            repository.clone(),
            Some(cancellation),
        )?;
        validate_status_generation(request, &current)?;
        let mutation = v1::GitMutationKind::try_from(request.mutation).unwrap_or_default();
        validate_mutation_target(request, mutation)?;
        let target_entry = validate_current_target(&stable_root, request, &current)?;
        if target_entry.submodule {
            bail!("submodule Git mutations are unsupported");
        }
        if matches!(
            mutation,
            v1::GitMutationKind::StageHunk
                | v1::GitMutationKind::UnstageHunk
                | v1::GitMutationKind::DiscardHunk
        ) && (!target_entry.original_path.is_empty()
            || matches!(
                v1::GitChangeKind::try_from(target_entry.index_kind).unwrap_or_default(),
                v1::GitChangeKind::Renamed | v1::GitChangeKind::Copied
            ))
        {
            bail!("complete-hunk operations for renamed or copied paths are unsupported");
        }
        // Copy provenance is not a second mutation target. Only a rename
        // transfers ownership from original_path to path.
        let mut mutation_request = request.clone();
        if target_entry.index_kind != v1::GitChangeKind::Renamed as i32 {
            mutation_request.original_path.clear();
        }
        if matches!(
            mutation,
            v1::GitMutationKind::DiscardFile | v1::GitMutationKind::DiscardHunk
        ) {
            self.consume_confirmation(request, connection_epoch)?;
        }
        let pre_state = command_state(&stable_root, &current);
        let execution = (|| match mutation {
            v1::GitMutationKind::StageFile => {
                git_path_cancellable(&stable_root, &[b"add"], &request.path, cancellation)
            }
            v1::GitMutationKind::UnstageFile => {
                unstage_file(&stable_root, &repository, &mutation_request, cancellation)
            }
            v1::GitMutationKind::DiscardFile => {
                discard_file(&stable_root, &repository, &mutation_request, cancellation)
            }
            v1::GitMutationKind::StageHunk
            | v1::GitMutationKind::UnstageHunk
            | v1::GitMutationKind::DiscardHunk => mutate_hunk(
                &stable_root,
                &repository,
                &mutation_request,
                mutation,
                cancellation,
            ),
            v1::GitMutationKind::Unspecified => bail!("Git mutation kind is required"),
        })();
        // Once a mutation command starts, transport cancellation must not
        // suppress the authority probe that tells the client what happened.
        let refresh = self.status_with_repository(request, &stable_root, repository, None);
        Ok(truthful_command_result(
            execution,
            pre_state,
            refresh,
            &stable_root,
            false,
        ))
    }

    pub(super) async fn commit(
        self: &Arc<Self>,
        request: v1::GitRequest,
        connection_epoch: u64,
        cancellation: Arc<AtomicBool>,
    ) -> anyhow::Result<v1::GitCommandResult> {
        if request.commit_message.trim().is_empty() {
            bail!("commit message must not be empty");
        }
        let preflight = request.clone();
        let preflight_cancellation = Arc::clone(&cancellation);
        let (root, repository, metadata) = tokio::task::spawn_blocking(move || {
            let root = capture_request_root(&preflight)?;
            ensure_connection_epoch(&preflight, connection_epoch)?;
            let repository = discover_repository(
                &root.stable_path(),
                &preflight.root,
                root.identity()?,
                Some(&preflight_cancellation),
            )?;
            validate_repository_id_required(&preflight, &repository)?;
            let metadata =
                GitMetadataCapability::capture(&repository.git_dir, &repository.common_dir)?;
            validate_metadata_capability(
                &preflight.root,
                root.identity()?,
                &repository,
                &metadata,
            )?;
            Ok::<_, anyhow::Error>((root, repository, metadata))
        })
        .await
        .map_err(|error| anyhow::anyhow!("Git commit preflight task failed: {error}"))??;
        let guard = repository_lock(&repository.repository_id)
            .lock_owned()
            .await;
        let service = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            if cancellation.load(Ordering::Acquire) {
                bail!("cancelled before commit");
            }
            let stable_root = root.stable_path();
            let _metadata_guard = metadata.install();
            let current = service.status_with_repository(
                &request,
                &stable_root,
                repository.clone(),
                Some(&cancellation),
            )?;
            validate_status_generation(&request, &current)?;
            let pre_state = command_state(&stable_root, &current);
            let execution = git_output_with_deadline(
                &stable_root,
                &[
                    OsStr::new("commit"),
                    OsStr::new("-m"),
                    OsStr::new(&request.commit_message),
                ],
                None,
                Some(&cancellation),
                GIT_COMMIT_DEADLINE,
            );
            let refresh = service.status_with_repository(&request, &stable_root, repository, None);
            Ok(truthful_command_result(
                execution,
                pre_state,
                refresh,
                &stable_root,
                true,
            ))
        })
        .await
        .map_err(|error| anyhow::anyhow!("Git commit task failed: {error}"))?
    }

    fn consume_confirmation(&self, request: &v1::GitRequest, epoch: u64) -> anyhow::Result<()> {
        if request.confirmation_token.is_empty() {
            bail!("discard confirmation token is required");
        }
        let mut confirmations = self.confirmations.lock().unwrap();
        confirmations.retain(|_, binding| binding.expires >= SystemTime::now());
        let binding = confirmations
            .remove(&request.confirmation_token)
            .ok_or_else(|| {
                anyhow::anyhow!("discard confirmation token is invalid or already used")
            })?;
        if binding.expires < SystemTime::now() {
            bail!("discard confirmation token expired");
        }
        if binding.repository_id != request.repository_id
            || binding.root != request.root
            || binding.root_token != request.root_token
            || binding.server_identity != request.expected_server_identity
            || binding.connection_epoch != epoch
            || binding.mutation != request.mutation
            || binding.path != request.path
            || binding.original_path != request.original_path
            || binding.diff_target != request.diff_target
            || binding.hunk_index != request.hunk_index
            || binding.status_generation != request.expected_status_generation
            || binding.source_generation != request.expected_source_generation
        {
            bail!("discard confirmation token does not match the requested mutation");
        }
        Ok(())
    }
}

struct CommandState {
    head_oid: String,
    index_generation: Option<String>,
    status_generation: u64,
    source_generation: String,
    authoritative: bool,
}

fn command_state(root: &str, status: &v1::GitStatusSnapshot) -> CommandState {
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

fn truthful_command_result(
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
    }
}

fn repository_lock(id: &str) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = REPOSITORY_LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap();
    locks.retain(|_, lock| Arc::strong_count(lock) > 1);
    Arc::clone(
        locks
            .entry(id.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
    )
}

fn capture_request_root(request: &v1::GitRequest) -> anyhow::Result<WorktreeRoot> {
    if request.expected_server_identity.is_empty()
        || request.expected_server_identity != server_identity()
    {
        bail!("stale or missing tmux server identity");
    }
    let root = WorktreeRoot::capture(&request.root)?;
    root.validate_token(&request.root, &request.root_token)?;
    Ok(root)
}

fn ensure_connection_epoch(request: &v1::GitRequest, epoch: u64) -> anyhow::Result<()> {
    if epoch == 0 || request.connection_epoch == 0 || request.connection_epoch != epoch {
        bail!("stale or missing connection generation");
    }
    Ok(())
}

fn validate_repository_id(
    request: &v1::GitRequest,
    repository: &v1::GitRepository,
) -> anyhow::Result<()> {
    if !request.repository_id.is_empty() && request.repository_id != repository.repository_id {
        bail!("stale repository identity");
    }
    Ok(())
}

fn validate_repository_id_required(
    request: &v1::GitRequest,
    repository: &v1::GitRepository,
) -> anyhow::Result<()> {
    if request.repository_id.is_empty() {
        bail!("repository identity is required");
    }
    validate_repository_id(request, repository)
}

fn validate_status_generation(
    request: &v1::GitRequest,
    status: &v1::GitStatusSnapshot,
) -> anyhow::Result<()> {
    if !status.authoritative || status.oversized {
        bail!("Git status is not authoritative");
    }
    if request.expected_status_generation == 0
        || request.expected_status_generation != status.generation
    {
        bail!("stale Git status generation");
    }
    Ok(())
}

fn validate_mutation_target(
    request: &v1::GitRequest,
    mutation: v1::GitMutationKind,
) -> anyhow::Result<()> {
    let target = v1::GitDiffTarget::try_from(request.diff_target).unwrap_or_default();
    match mutation {
        v1::GitMutationKind::StageHunk if target != v1::GitDiffTarget::Unstaged => {
            bail!("staging a hunk requires an unstaged source diff")
        }
        v1::GitMutationKind::UnstageHunk if target != v1::GitDiffTarget::Staged => {
            bail!("unstaging a hunk requires a staged source diff")
        }
        v1::GitMutationKind::DiscardHunk if target == v1::GitDiffTarget::Unspecified => {
            bail!("discarding a hunk requires a source diff target")
        }
        _ => Ok(()),
    }
}

fn validate_git_path(path: &[u8]) -> anyhow::Result<()> {
    if path.is_empty() || path.contains(&0) {
        bail!("Git path is empty or contains NUL");
    }
    let value = Path::new(OsStr::from_bytes(path));
    if value.is_absolute() || path == b"." {
        bail!("Git path escapes repository root");
    }
    let components: Vec<_> = value.components().collect();
    if components.is_empty()
        || components
            .iter()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        bail!("Git path contains an unsafe or ambiguous component");
    }
    // Reject spellings Git would normalize differently. Status paths are
    // canonical byte strings, so exact reconstruction loses no valid target.
    let mut canonical = Vec::new();
    for component in components {
        if !canonical.is_empty() {
            canonical.push(b'/');
        }
        let Component::Normal(name) = component else {
            unreachable!();
        };
        canonical.extend_from_slice(name.as_bytes());
    }
    if canonical != path {
        bail!("Git path uses an ambiguous spelling");
    }
    Ok(())
}

fn validate_current_target(
    root: &str,
    request: &v1::GitRequest,
    status: &v1::GitStatusSnapshot,
) -> anyhow::Result<v1::GitStatusEntry> {
    validate_git_path(&request.path)?;
    if !request.original_path.is_empty() {
        validate_git_path(&request.original_path)?;
    }
    let entry = status
        .entries
        .iter()
        .find(|entry| entry.path == request.path)
        .ok_or_else(|| anyhow::anyhow!("Git mutation target is not in the current status"))?;
    if entry.original_path != request.original_path {
        bail!("Git mutation original path does not match the current status entry");
    }
    if entry.ignored {
        bail!("ignored paths cannot be mutated through the Git control lane");
    }
    if WorktreeRoot::capture(root)?
        .entry(&request.path)?
        .is_directory()?
        && !entry.submodule
    {
        bail!("directory mutation targets are ambiguous and are not supported");
    }
    Ok(entry.clone())
}

fn status_fingerprint(status: &v1::GitStatusSnapshot) -> String {
    let mut copy = status.clone();
    copy.generation = 0;
    blake3::hash(&prost::Message::encode_to_vec(&copy))
        .to_hex()
        .to_string()
}

pub(super) fn bound_command_response(request_id: u64, response: &mut v1::Response) {
    {
        let Some(command) = response.git.as_mut().and_then(|git| git.command.as_mut()) else {
            return;
        };
        bound_text(&mut command.error, MAX_GIT_DIAGNOSTIC);
        bound_text(&mut command.refresh_error, MAX_GIT_DIAGNOSTIC);
    }
    if response_envelope_len(request_id, response) <= MAX_GIT_COMMAND_ENVELOPE {
        return;
    }
    if let Some(command) = response.git.as_mut().and_then(|git| git.command.as_mut())
        && command.status.take().is_some()
    {
        command.status_omitted = true;
        command.refresh_failed = true;
        command.refresh_error =
            "post-command Git status omitted to preserve the bounded control response".into();
    }
    loop {
        let length = response_envelope_len(request_id, response);
        if length <= MAX_GIT_COMMAND_ENVELOPE {
            break;
        }
        let Some(command) = response.git.as_mut().and_then(|git| git.command.as_mut()) else {
            break;
        };
        if command.stdout.is_empty() && command.stderr.is_empty() {
            break;
        }
        let excess = length
            .saturating_sub(MAX_GIT_COMMAND_ENVELOPE)
            .saturating_add(1024);
        if command.stdout.len() >= command.stderr.len() && !command.stdout.is_empty() {
            let retained = command.stdout.len().saturating_sub(excess.max(1));
            command.stdout.truncate(retained);
            command.stdout_truncated = true;
        } else {
            let retained = command.stderr.len().saturating_sub(excess.max(1));
            command.stderr.truncate(retained);
            command.stderr_truncated = true;
        }
    }
}

fn response_envelope_len(request_id: u64, response: &v1::Response) -> usize {
    use prost::Message as _;
    tmux_agent_protocol::envelope(
        request_id,
        0,
        tmux_agent_protocol::v1::envelope::Payload::Response(response.clone()),
    )
    .encoded_len()
}

fn bound_text(value: &mut String, maximum: usize) {
    if value.len() <= maximum {
        return;
    }
    let mut boundary = maximum;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value.push('…');
}

#[cfg(test)]
mod tests;
