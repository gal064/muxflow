//! One coordinator per observed repository.
//!
//! Every Git read, watch and mutation for the same connection/server/epoch/root
//! goes through exactly one of these. It owns the discovered repository
//! identity and metadata capability, the single status pipeline, the single
//! native watcher, every subscriber, and the reconciliation of a mutation's
//! post-command refresh. Without it, each consumer ran its own discovery,
//! its own watcher and its own status pipeline, so N sidebar/diff consumers
//! cost N of everything.

use super::*;

mod subscribers;
mod watcher;

pub(in crate::service) use subscribers::SubscriberActivation;
#[cfg(test)]
pub(in crate::service::git) use watcher::start_repository_watcher;

/// How long a filesystem signal is allowed to settle before status is read.
/// Editors write several files in a burst; reading once after the burst is one
/// pipeline instead of one per file.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(75);

/// What a repository must be keyed by for reuse to be safe.
///
/// The server identity and connection epoch are in here because a cached
/// capability from a previous tmux server or a previous connection generation
/// describes a repository this client can no longer address. The epoch is the
/// *connection's*, not the request's: a client that omits it must still reach
/// the same coordinator, or its status generations would come from a second
/// counter and never match its own mutations. The root token binds the logical
/// path to the exact directory it named when the client captured it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct RepositoryKey {
    server_identity: String,
    connection_epoch: u64,
    root: String,
    root_token: String,
}

impl RepositoryKey {
    pub(super) fn for_connection(connection_epoch: u64, request: &v1::GitRequest) -> Self {
        Self {
            server_identity: request.expected_server_identity.clone(),
            connection_epoch,
            root: request.root.clone(),
            root_token: request.root_token.clone(),
        }
    }
}

/// The parts of a repository that do not change while it is the same
/// repository: its identity, its metadata directories, and a live capability
/// for its worktree root. Recomputing these was five Git subprocesses per
/// request; they are now discovered once and revalidated with `fstat`.
pub(super) struct RepositoryCapabilities {
    pub(super) identity: RepositoryIdentity,
    pub(super) metadata: GitMetadataCapability,
    root: WorktreeRoot,
}

impl RepositoryCapabilities {
    pub(super) fn stable_root(&self) -> String {
        self.root.stable_path()
    }

    pub(super) fn try_clone_root(&self) -> anyhow::Result<WorktreeRoot> {
        self.root.try_clone()
    }

    #[cfg(test)]
    pub(in crate::service::git) fn for_test(
        identity: RepositoryIdentity,
        metadata: GitMetadataCapability,
        root: WorktreeRoot,
    ) -> Self {
        Self {
            identity,
            metadata,
            root,
        }
    }

    /// Proves this cached capability still describes the requested root.
    ///
    /// The repository id is a hash over the logical root, the root's `dev`/`ino`
    /// and both metadata directories' paths and `dev`/`ino`, so recomputing it
    /// from live `fstat` results is a complete revalidation with no subprocess.
    fn revalidate(&self, logical_root: &str, root_identity: (u64, u64)) -> anyhow::Result<()> {
        validate_metadata_capability(logical_root, root_identity, &self.identity, &self.metadata)
    }
}

/// Whether a caller may accept a snapshot another caller's pipeline produced.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Freshness {
    /// Accepts any snapshot whose pipeline started at or after this caller
    /// observed the change ticket. Reads and watch refreshes use this, which is
    /// what collapses 32 simultaneous consumers into one Git status pipeline.
    Coalesced,
    /// Invalidates first, so nothing already observed can satisfy the caller.
    ///
    /// This is what an explicit user refresh means: the whole point of pressing
    /// it is to recover from a filesystem watcher that missed something, so it
    /// must not be answered from what that watcher last reported.
    Forced,
    /// Always runs its own pipeline. Mutation authority is never inherited.
    Exclusive,
}

struct CachedStatus {
    snapshot: Arc<v1::GitStatusSnapshot>,
    /// The change ticket observed immediately before this pipeline started.
    /// A caller is satisfied when this is at least the ticket it observed.
    observed_ticket: u64,
    /// When the pipeline that produced it began. A snapshot taken at or after a
    /// request arrived is a correct answer to that request even without a
    /// watcher, which is what lets simultaneous consumers share one pipeline.
    started_at: Instant,
}

#[derive(Default)]
struct StatusIdentity {
    generation: u64,
    fingerprint: String,
}

/// What subscribers were last told.
///
/// One value rather than two fields, because "recovered from an error" is a
/// transition: with a separate error string, a transient failure over an
/// unchanged repository left the error latched and every later success
/// suppressed as a duplicate.
#[derive(Default, PartialEq, Eq)]
enum Publication {
    #[default]
    Nothing,
    Status(String),
    Error(String),
}

pub(super) struct RepositoryCoordinator {
    key: RepositoryKey,
    next_generation: Arc<AtomicU64>,
    closed: Arc<AtomicBool>,
    capabilities: tokio::sync::Mutex<Option<Arc<RepositoryCapabilities>>>,
    identity: Mutex<StatusIdentity>,
    /// Bumped whenever something may have changed the repository: a filesystem
    /// signal, or a completed mutation. A cached snapshot is only reusable by a
    /// caller whose observed ticket it covers.
    change_ticket: AtomicU64,
    /// Serializes status pipelines for this repository, so `generation` cannot
    /// be published out of order and two consumers cannot fork the work.
    refresh: tokio::sync::Mutex<()>,
    latest: Mutex<Option<CachedStatus>>,
    published: Mutex<Publication>,
    subscribers: Mutex<subscribers::SubscriberRegistry>,
    watcher: Mutex<watcher::WatcherState>,
    /// Whether a live native watcher is currently reporting this repository.
    /// Only then may a snapshot older than the request be reused: without one,
    /// nothing would have invalidated it.
    observing: AtomicBool,
    /// Mutations in progress. The watcher waits these out so a mutation's own
    /// filesystem events do not start a second, redundant status pipeline.
    mutations: AtomicUsize,
    mutations_idle: tokio::sync::Notify,
    /// Raised by the `notify` callback when the platform reports the watch is
    /// broken. Owned here rather than by the observe task so that whoever
    /// notices the failure and whoever acts on it are not the same code.
    native_failed: Arc<AtomicBool>,
    native_broken: tokio::sync::Notify,
    /// When this repository was last addressed, for eviction order only.
    last_use: AtomicU64,
    #[cfg(test)]
    observation: Arc<GitObservation>,
}

impl RepositoryCoordinator {
    pub(super) fn new(
        key: RepositoryKey,
        next_generation: Arc<AtomicU64>,
        closed: Arc<AtomicBool>,
        #[cfg(test)] observation: Arc<GitObservation>,
    ) -> Self {
        Self {
            key,
            next_generation,
            closed,
            capabilities: tokio::sync::Mutex::new(None),
            identity: Mutex::new(StatusIdentity::default()),
            change_ticket: AtomicU64::new(0),
            refresh: tokio::sync::Mutex::new(()),
            latest: Mutex::new(None),
            published: Mutex::new(Publication::default()),
            subscribers: Mutex::new(subscribers::SubscriberRegistry::default()),
            watcher: Mutex::new(watcher::WatcherState::default()),
            observing: AtomicBool::new(false),
            mutations: AtomicUsize::new(0),
            mutations_idle: tokio::sync::Notify::new(),
            native_failed: Arc::new(AtomicBool::new(false)),
            native_broken: tokio::sync::Notify::new(),
            last_use: AtomicU64::new(0),
            #[cfg(test)]
            observation,
        }
    }

    pub(super) fn key(&self) -> &RepositoryKey {
        &self.key
    }

    pub(super) fn touch(&self, use_order: u64) {
        self.last_use.store(use_order, Ordering::Release);
    }

    pub(super) fn last_use(&self) -> u64 {
        self.last_use.load(Ordering::Acquire)
    }

    /// The discovered repository identity, discovering it at most once.
    ///
    /// Concurrent callers queue on one async mutex rather than each running
    /// discovery, so a cold panel with a diff tab beside it still pays one
    /// `rev-parse`.
    pub(super) async fn capabilities(
        &self,
        request: &v1::GitRequest,
        cancellation: Option<Arc<AtomicBool>>,
    ) -> anyhow::Result<Arc<RepositoryCapabilities>> {
        let mut slot = self.capabilities.lock().await;
        let logical_root = self.key.root.clone();
        let root_token = self.key.root_token.clone();
        if let Some(cached) = slot.as_ref() {
            let cached = Arc::clone(cached);
            let logical = logical_root.clone();
            let token = root_token.clone();
            let revalidated = tokio::task::spawn_blocking(move || {
                let root = WorktreeRoot::capture(&logical)?;
                root.validate_token(&logical, &token)?;
                cached.revalidate(&logical, root.identity()?)?;
                Ok::<_, anyhow::Error>(())
            })
            .await
            .map_err(|error| anyhow::anyhow!("Git capability revalidation failed: {error}"))?;
            match revalidated {
                Ok(()) => return Ok(Arc::clone(slot.as_ref().expect("checked above"))),
                // The directory the root names is not the one this capability
                // describes any more. Rediscover rather than serve a stale one.
                Err(_) => *slot = None,
            }
        }
        let expected_repository_id = request.repository_id.clone();
        #[cfg(test)]
        self.observation.discovery();
        let discovered = tokio::task::spawn_blocking(move || {
            let root = WorktreeRoot::capture(&logical_root)?;
            root.validate_token(&logical_root, &root_token)?;
            let identity = discover_repository(
                &root.stable_path(),
                &logical_root,
                root.identity()?,
                cancellation.as_deref(),
            )?;
            if !expected_repository_id.is_empty()
                && expected_repository_id != identity.repository_id
            {
                bail!("stale repository identity");
            }
            let metadata = GitMetadataCapability::capture(&identity.git_dir, &identity.common_dir)?;
            validate_metadata_capability(&logical_root, root.identity()?, &identity, &metadata)?;
            Ok::<_, anyhow::Error>(RepositoryCapabilities {
                identity,
                metadata,
                root,
            })
        })
        .await
        .map_err(|error| anyhow::anyhow!("Git repository discovery failed: {error}"))??;
        let discovered = Arc::new(discovered);
        *slot = Some(Arc::clone(&discovered));
        Ok(discovered)
    }

    /// Marks the repository as possibly changed. Snapshots taken before this
    /// point stop satisfying new readers.
    pub(super) fn invalidate(&self) -> u64 {
        self.change_ticket.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// The change ticket this coordinator is currently at. A diff brackets its
    /// read with this, so it can state which repository state it describes.
    pub(super) fn change_ticket(&self) -> u64 {
        self.change_ticket.load(Ordering::Acquire)
    }

    pub(super) fn cached_status(&self) -> Option<Arc<v1::GitStatusSnapshot>> {
        self.latest
            .lock()
            .unwrap()
            .as_ref()
            .map(|cached| Arc::clone(&cached.snapshot))
    }

    /// The authoritative status, running at most one Git pipeline per change.
    pub(super) async fn status(
        self: &Arc<Self>,
        capabilities: &Arc<RepositoryCapabilities>,
        freshness: Freshness,
        cancellation: Option<Arc<AtomicBool>>,
    ) -> anyhow::Result<Arc<v1::GitStatusSnapshot>> {
        let arrived = Instant::now();
        if freshness == Freshness::Forced {
            self.invalidate();
        }
        let required = self.change_ticket.load(Ordering::Acquire);
        if freshness != Freshness::Exclusive
            && let Some(cached) = self.satisfied_by_cache(required, arrived)
        {
            return Ok(cached);
        }
        let _pipeline = self.refresh.lock().await;
        if freshness != Freshness::Exclusive
            && let Some(cached) = self.satisfied_by_cache(required, arrived)
        {
            return Ok(cached);
        }
        let started_at = Instant::now();
        let observed = self.change_ticket.load(Ordering::Acquire);
        #[cfg(test)]
        self.observation.status_pipeline();
        let capabilities = Arc::clone(capabilities);
        let snapshot = tokio::task::spawn_blocking(move || {
            let _guard = capabilities.metadata.install();
            read_status_cancellable(
                &capabilities.stable_root(),
                &capabilities.identity,
                cancellation.as_deref(),
            )
        })
        .await
        .map_err(|error| anyhow::anyhow!("Git status task failed: {error}"))??;
        let snapshot = self.commit_snapshot(snapshot, observed, started_at);
        // Published under the refresh lock, so subscribers observe exactly the
        // order the pipelines ran in.
        self.publish_status(&snapshot).await;
        Ok(snapshot)
    }

    fn satisfied_by_cache(
        &self,
        required: u64,
        arrived: Instant,
    ) -> Option<Arc<v1::GitStatusSnapshot>> {
        let observing = self.observing.load(Ordering::Acquire);
        let latest = self.latest.lock().unwrap();
        latest
            .as_ref()
            .filter(|cached| {
                cached.observed_ticket >= required && (observing || cached.started_at >= arrived)
            })
            .map(|cached| Arc::clone(&cached.snapshot))
    }

    /// Stamps a raw snapshot with this repository's monotonic generation,
    /// caches it, and fans it out to every subscriber if it is new.
    fn commit_snapshot(
        self: &Arc<Self>,
        mut snapshot: v1::GitStatusSnapshot,
        observed_ticket: u64,
        started_at: Instant,
    ) -> Arc<v1::GitStatusSnapshot> {
        let fingerprint = status_fingerprint(&snapshot);
        {
            let mut identity = self.identity.lock().unwrap();
            if identity.fingerprint != fingerprint {
                identity.fingerprint = fingerprint.clone();
                identity.generation = self.next_generation.fetch_add(1, Ordering::AcqRel) + 1;
            }
            snapshot.generation = identity.generation;
        }
        snapshot.source_generation = fingerprint;
        let snapshot = Arc::new(snapshot);
        {
            let mut latest = self.latest.lock().unwrap();
            let newer = latest
                .as_ref()
                .is_none_or(|cached| cached.observed_ticket <= observed_ticket);
            if newer {
                *latest = Some(CachedStatus {
                    snapshot: Arc::clone(&snapshot),
                    observed_ticket,
                    started_at,
                });
            }
        }
        snapshot
    }

    pub(super) fn begin_mutation(self: &Arc<Self>) -> MutationGuard {
        self.mutations.fetch_add(1, Ordering::AcqRel);
        MutationGuard {
            coordinator: Arc::clone(self),
        }
    }

    /// Waits until no mutation is running, or until observation is stopped.
    async fn await_mutation_quiescence(&self, stopped: &AtomicBool) {
        loop {
            let notified = self.mutations_idle.notified();
            if self.mutations.load(Ordering::Acquire) == 0 || stopped.load(Ordering::Acquire) {
                return;
            }
            tokio::select! {
                _ = notified => {}
                _ = tokio::time::sleep(Duration::from_millis(250)) => {}
            }
        }
    }
}

/// Holds the watcher off while a mutation runs and releases it exactly once.
pub(super) struct MutationGuard {
    coordinator: Arc<RepositoryCoordinator>,
}

impl Drop for MutationGuard {
    fn drop(&mut self) {
        if self.coordinator.mutations.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.coordinator.mutations_idle.notify_waiters();
        }
    }
}
