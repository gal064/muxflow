use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
    path::{Component, Path},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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

/// Repositories one connection keeps discovered state for.
///
/// Each retains a worktree descriptor and two metadata descriptors, so a client
/// that walks many roots must not accumulate them without limit. Watched
/// repositories are never evicted; a watch is a consumer saying it still wants
/// this one.
const MAX_TRACKED_REPOSITORIES: usize = 8;

mod command;
use command::{command_state, truthful_command_result};
mod content;
mod coordinator;
pub(in crate::service) use coordinator::SubscriberActivation;
#[cfg(test)]
use coordinator::start_repository_watcher;
use coordinator::{Freshness, RepositoryCapabilities, RepositoryCoordinator, RepositoryKey};
mod diff;
use diff::{DiffAudience, read_diff};
mod mutation;
use mutation::{discard_file, mutate_hunk, unstage_file};
mod measurements;
use measurements::GitObservation;
#[cfg(test)]
use measurements::phase14_git_process_started;
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
use status::{
    RepositoryIdentity, discover_repository, read_status_cancellable, validate_metadata_capability,
};
mod watch;

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

/// Every Git request for one connection.
///
/// The service itself holds no repository state: it routes each request to the
/// one coordinator that owns that repository's identity, watcher, subscribers
/// and status pipeline. Two consumers of the same repository therefore share
/// all of it instead of each running their own discovery and their own watcher.
pub(super) struct GitService {
    repositories: Mutex<HashMap<RepositoryKey, Arc<RepositoryCoordinator>>>,
    confirmations: Mutex<HashMap<String, ConfirmationBinding>>,
    diff_body: Mutex<Option<content::CachedDiffBody>>,
    next_generation: Arc<AtomicU64>,
    /// Monotonic clock for repository eviction order.
    next_use: AtomicU64,
    /// This connection's generation, taken from its `ClientHello` rather than
    /// from any individual request.
    connection_epoch: u64,
    closed: Arc<AtomicBool>,
    /// Deterministic observation counters for this connection. Always present:
    /// a conditionally compiled field would give test and release builds
    /// different shapes for the same code.
    observation: Arc<GitObservation>,
}

impl GitService {
    pub(super) fn new(closed: Arc<AtomicBool>, connection_epoch: u64) -> Self {
        Self {
            repositories: Mutex::new(HashMap::new()),
            confirmations: Mutex::new(HashMap::new()),
            diff_body: Mutex::new(None),
            next_generation: Arc::new(AtomicU64::new(0)),
            next_use: AtomicU64::new(0),
            connection_epoch,
            closed,
            observation: Arc::new(GitObservation::default()),
        }
    }

    /// The live coordinator for a request, for tests that drive it directly.
    #[cfg(test)]
    pub(in crate::service::git) fn coordinator_for_test(
        &self,
        request: &v1::GitRequest,
    ) -> Option<Arc<RepositoryCoordinator>> {
        let key = RepositoryKey::for_connection(self.connection_epoch, request);
        self.repositories.lock().unwrap().get(&key).map(Arc::clone)
    }

    /// Marks every established native watcher broken, as the platform would.
    #[cfg(test)]
    pub(in crate::service::git) fn fail_native_watcher_for_test(&self) {
        for coordinator in self.repositories.lock().unwrap().values() {
            coordinator.fail_native_watcher_for_test();
        }
    }

    #[cfg(test)]
    pub(in crate::service::git) fn observation(&self) -> measurements::GitObservationCounts {
        self.observation.snapshot()
    }

    /// The coordinator and cached identity for the requested repository.
    ///
    /// This is where a warm request stops costing Git subprocesses: the root
    /// capability is revalidated with `fstat`, and discovery only runs when no
    /// coordinator has ever resolved this root.
    async fn repository(
        &self,
        request: &v1::GitRequest,
        cancellation: Option<Arc<AtomicBool>>,
    ) -> anyhow::Result<(Arc<RepositoryCoordinator>, Arc<RepositoryCapabilities>)> {
        ensure_server_identity(request)?;
        let key = RepositoryKey::for_connection(self.connection_epoch, request);
        let use_order = self.next_use.fetch_add(1, Ordering::AcqRel);
        let coordinator = {
            let mut repositories = self.repositories.lock().unwrap();
            let created = key.clone();
            let coordinator = Arc::clone(repositories.entry(key.clone()).or_insert_with(|| {
                Arc::new(RepositoryCoordinator::new(
                    created,
                    Arc::clone(&self.next_generation),
                    Arc::clone(&self.closed),
                    Arc::clone(&self.observation),
                ))
            }));
            coordinator.touch(use_order);
            evict_unwatched_repositories(&mut repositories, &key);
            coordinator
        };
        match coordinator.capabilities(request, cancellation).await {
            Ok(capabilities) => Ok((coordinator, capabilities)),
            Err(error) => {
                self.retire(&coordinator);
                Err(error)
            }
        }
    }

    /// Drops an unwatched coordinator, so the next request rediscovers rather
    /// than inheriting one whose root it could no longer describe.
    fn retire(&self, coordinator: &Arc<RepositoryCoordinator>) {
        let mut repositories = self.repositories.lock().unwrap();
        if repositories
            .get(coordinator.key())
            .is_some_and(|current| Arc::ptr_eq(current, coordinator))
            && coordinator.subscriber_count() == 0
        {
            repositories.remove(coordinator.key());
            coordinator.stop_watcher();
        }
    }

    /// An explicitly requested status.
    ///
    /// Every automatic refresh now arrives through the shared watch, so a
    /// client asking for status is a person asking for it — usually because
    /// something looked wrong. Invalidating first is what makes that a real
    /// re-read: answering from the snapshot the watcher last produced would
    /// remove the only recovery there is from a watcher that missed an event.
    pub(in crate::service) async fn status(
        &self,
        request: &v1::GitRequest,
        cancellation: Option<Arc<AtomicBool>>,
    ) -> anyhow::Result<v1::GitStatusSnapshot> {
        let (coordinator, capabilities) = self.repository(request, cancellation.clone()).await?;
        coordinator.invalidate();
        let status = coordinator
            .status(&capabilities, Freshness::Coalesced, cancellation)
            .await?;
        Ok((*status).clone())
    }

    /// One diff, plus the authoritative status it was read against.
    ///
    /// The desktop previously requested status and then diff, paying two round
    /// trips for one visible action.
    ///
    /// The status is taken *after* the read, so the response states the state
    /// the diff actually landed on rather than asserting that nothing moved
    /// during it. For an observed repository that read costs nothing — the
    /// coordinator answers from the snapshot it already has. What binds a
    /// mutation is not this pair but `GitDiff::source_generation`, which
    /// `mutate_hunk` re-derives and re-validates under the repository lock.
    pub(in crate::service) async fn diff(
        &self,
        request: &v1::GitRequest,
        bulk_available: bool,
        cancellation: Option<Arc<AtomicBool>>,
    ) -> anyhow::Result<(v1::GitDiff, v1::GitStatusSnapshot)> {
        require_repository_id(request)?;
        let (coordinator, capabilities) = self.repository(request, cancellation.clone()).await?;
        if cancellation
            .as_deref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            bail!("Git diff cancelled");
        }
        let before = coordinator
            .status(&capabilities, Freshness::Coalesced, cancellation.clone())
            .await?;
        if !before.authoritative || before.oversized {
            bail!("Git status is not authoritative");
        }
        let repository = before
            .repository
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Git status omitted its repository identity"))?;
        let work = request.clone();
        let read_capabilities = Arc::clone(&capabilities);
        let read_cancellation = cancellation.clone();
        let read = tokio::task::spawn_blocking(move || {
            let _guard = read_capabilities.metadata.install();
            read_diff(
                &read_capabilities.stable_root(),
                repository,
                &work,
                DiffAudience::for_client(bulk_available),
                read_cancellation.as_deref(),
            )
        })
        .await
        .map_err(|error| anyhow::anyhow!("Git diff task failed: {error}"))??;
        let landed = coordinator
            .status(&capabilities, Freshness::Coalesced, cancellation)
            .await?;
        if !landed.authoritative || landed.oversized {
            bail!("Git status is not authoritative");
        }
        Ok((read, (*landed).clone()))
    }

    /// The diff including its raw patch, which the client response omits.
    #[cfg(test)]
    pub(super) async fn diff_with_patch(
        &self,
        request: &v1::GitRequest,
    ) -> anyhow::Result<v1::GitDiff> {
        let (coordinator, capabilities) = self.repository(request, None).await?;
        let status = coordinator
            .status(&capabilities, Freshness::Coalesced, None)
            .await?;
        let repository = status.repository.clone().unwrap();
        let work = request.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = capabilities.metadata.install();
            read_diff(
                &capabilities.stable_root(),
                repository,
                &work,
                DiffAudience::Mutation,
                None,
            )
        })
        .await
        .map_err(|error| anyhow::anyhow!("Git diff task failed: {error}"))?
    }

    #[cfg(test)]
    pub(super) async fn diff_only(
        &self,
        request: &v1::GitRequest,
        cancellation: Option<Arc<AtomicBool>>,
    ) -> anyhow::Result<v1::GitDiff> {
        self.diff(request, true, cancellation)
            .await
            .map(|(diff, _)| diff)
    }

    pub(in crate::service) async fn prepare_discard(
        &self,
        request: &v1::GitRequest,
        connection_epoch: u64,
    ) -> anyhow::Result<v1::GitConfirmation> {
        require_repository_id(request)?;
        ensure_connection_epoch(request, connection_epoch)?;
        let (_coordinator, capabilities) = self.repository(request, None).await?;
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
                repository_id: capabilities.identity.repository_id.clone(),
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

    pub(in crate::service) async fn mutate(
        &self,
        request: v1::GitRequest,
        connection_epoch: u64,
        cancellation: Arc<AtomicBool>,
    ) -> anyhow::Result<v1::GitCommandResult> {
        require_repository_id(&request)?;
        ensure_connection_epoch(&request, connection_epoch)?;
        let (coordinator, capabilities) = self
            .repository(&request, Some(Arc::clone(&cancellation)))
            .await?;
        // The repository lock is process-global, so waiting for it can mean
        // waiting on another connection's five-minute commit hook. Suppressing
        // this connection's watcher only starts once this mutation actually
        // owns the repository.
        let _guard = repository_lock(&capabilities.identity.repository_id)
            .lock_owned()
            .await;
        let _mutation = coordinator.begin_mutation();
        if cancellation.load(Ordering::Acquire) {
            bail!("cancelled before mutation");
        }
        // Authority for a mutation is never inherited from another consumer's
        // snapshot: the target is validated against a status pipeline this
        // mutation ran itself, immediately before the command.
        let current = coordinator
            .status(
                &capabilities,
                Freshness::Exclusive,
                Some(Arc::clone(&cancellation)),
            )
            .await?;
        validate_status_generation(&request, &current)?;
        let repository = current
            .repository
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Git status omitted its repository identity"))?;
        let mutation = v1::GitMutationKind::try_from(request.mutation).unwrap_or_default();
        validate_mutation_target(&request, mutation)?;
        let stable_root = capabilities.stable_root();
        let target_entry = validate_current_target(&stable_root, &request, &current)?;
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
            self.consume_confirmation(&request, connection_epoch)?;
        }
        let execution_capabilities = Arc::clone(&capabilities);
        let execution_request = request.clone();
        let execution_repository = repository.clone();
        let execution_cancellation = Arc::clone(&cancellation);
        let executed = tokio::task::spawn_blocking(move || {
            let _metadata_guard = execution_capabilities.metadata.install();
            let root = execution_capabilities.stable_root();
            let pre_state = command_state(&root, &current);
            let outcome = (|| match mutation {
                v1::GitMutationKind::StageFile => git_path_cancellable(
                    &root,
                    &[b"add"],
                    &execution_request.path,
                    &execution_cancellation,
                ),
                v1::GitMutationKind::UnstageFile => unstage_file(
                    &root,
                    execution_repository.initial,
                    &mutation_request,
                    &execution_cancellation,
                ),
                v1::GitMutationKind::DiscardFile => discard_file(
                    &root,
                    &execution_capabilities.identity,
                    execution_repository.initial,
                    &mutation_request,
                    &execution_cancellation,
                ),
                v1::GitMutationKind::StageHunk
                | v1::GitMutationKind::UnstageHunk
                | v1::GitMutationKind::DiscardHunk => mutate_hunk(
                    &root,
                    &execution_repository,
                    &mutation_request,
                    mutation,
                    &execution_cancellation,
                ),
                v1::GitMutationKind::Unspecified => bail!("Git mutation kind is required"),
            })();
            (outcome, pre_state)
        })
        .await
        .map_err(|error| anyhow::anyhow!("Git mutation task failed: {error}"))?;
        let (execution, pre_state) = executed;
        Ok(self
            .reconcile_command(&coordinator, &capabilities, execution, pre_state, false)
            .await)
    }

    pub(in crate::service) async fn commit(
        &self,
        request: v1::GitRequest,
        connection_epoch: u64,
        cancellation: Arc<AtomicBool>,
    ) -> anyhow::Result<v1::GitCommandResult> {
        if request.commit_message.trim().is_empty() {
            bail!("commit message must not be empty");
        }
        require_repository_id(&request)?;
        ensure_connection_epoch(&request, connection_epoch)?;
        let (coordinator, capabilities) = self
            .repository(&request, Some(Arc::clone(&cancellation)))
            .await?;
        let _guard = repository_lock(&capabilities.identity.repository_id)
            .lock_owned()
            .await;
        let _mutation = coordinator.begin_mutation();
        if cancellation.load(Ordering::Acquire) {
            bail!("cancelled before commit");
        }
        let current = coordinator
            .status(
                &capabilities,
                Freshness::Exclusive,
                Some(Arc::clone(&cancellation)),
            )
            .await?;
        validate_status_generation(&request, &current)?;
        let execution_capabilities = Arc::clone(&capabilities);
        let executed = tokio::task::spawn_blocking(move || {
            let _metadata_guard = execution_capabilities.metadata.install();
            let root = execution_capabilities.stable_root();
            let pre_state = command_state(&root, &current);
            let outcome = git_output_with_deadline(
                &root,
                &[
                    OsStr::new("commit"),
                    OsStr::new("-m"),
                    OsStr::new(&request.commit_message),
                ],
                None,
                Some(&cancellation),
                GIT_COMMIT_DEADLINE,
            );
            (outcome, pre_state)
        })
        .await
        .map_err(|error| anyhow::anyhow!("Git commit task failed: {error}"))?;
        let (execution, pre_state) = executed;
        Ok(self
            .reconcile_command(&coordinator, &capabilities, execution, pre_state, true)
            .await)
    }

    /// The one authoritative refresh a completed Git command produces.
    ///
    /// Invalidating before refreshing means the watcher's own wake-up for these
    /// same writes coalesces onto this pipeline instead of running a second
    /// one, and publishing it here is what every other consumer of the
    /// repository receives. Transport cancellation deliberately does not reach
    /// this refresh: once a command has run, the client must be told what
    /// happened.
    async fn reconcile_command(
        &self,
        coordinator: &Arc<RepositoryCoordinator>,
        capabilities: &Arc<RepositoryCapabilities>,
        execution: anyhow::Result<GitOutput>,
        pre_state: command::CommandState,
        commit: bool,
    ) -> v1::GitCommandResult {
        coordinator.invalidate();
        let refresh = coordinator
            .status(capabilities, Freshness::Coalesced, None)
            .await
            .map(|status| (*status).clone());
        truthful_command_result(
            execution,
            pre_state,
            refresh,
            &capabilities.stable_root(),
            commit,
        )
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

impl Drop for GitService {
    fn drop(&mut self) {
        for (_, coordinator) in self.repositories.get_mut().unwrap().drain() {
            coordinator.stop_watcher();
            coordinator.drop_all_subscribers();
        }
    }
}

/// Drops the least recently used repositories nobody is watching.
///
/// `in_use` is the repository the current request is about to work with; it is
/// never a candidate, even when it is the only unwatched one, because the
/// caller already holds it and would otherwise proceed against a coordinator
/// this map no longer knows about.
fn evict_unwatched_repositories(
    repositories: &mut HashMap<RepositoryKey, Arc<RepositoryCoordinator>>,
    in_use: &RepositoryKey,
) {
    while repositories.len() > MAX_TRACKED_REPOSITORIES {
        let Some(evicted) = repositories
            .values()
            .filter(|coordinator| {
                coordinator.subscriber_count() == 0 && coordinator.key() != in_use
            })
            .min_by_key(|coordinator| coordinator.last_use())
            .map(|coordinator| coordinator.key().clone())
        else {
            return;
        };
        if let Some(coordinator) = repositories.remove(&evicted) {
            coordinator.stop_watcher();
        }
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

fn ensure_server_identity(request: &v1::GitRequest) -> anyhow::Result<()> {
    if request.expected_server_identity.is_empty()
        || request.expected_server_identity != server_identity()
    {
        bail!("stale or missing tmux server identity");
    }
    Ok(())
}

fn ensure_connection_epoch(request: &v1::GitRequest, epoch: u64) -> anyhow::Result<()> {
    if epoch == 0 || request.connection_epoch == 0 || request.connection_epoch != epoch {
        bail!("stale or missing connection generation");
    }
    Ok(())
}

fn require_repository_id(request: &v1::GitRequest) -> anyhow::Result<()> {
    if request.repository_id.is_empty() {
        bail!("repository identity is required");
    }
    Ok(())
}

/// Rejects a request whose asserted repository is not the one this root is.
///
/// An empty assertion means the client is still discovering; anything else is
/// a claim, and a claim that does not hold is a stale scope.
fn require_matching_repository(request: &v1::GitRequest, discovered: &str) -> anyhow::Result<()> {
    if !request.repository_id.is_empty() && request.repository_id != discovered {
        bail!("stale repository identity");
    }
    Ok(())
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
