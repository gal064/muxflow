//! The single native watcher a repository coordinator owns.
//!
//! One `notify` watcher per repository, registered on the smallest set of
//! directories that covers the worktree, the git directory and the common
//! directory. When the platform cannot give us a native watcher we fall back to
//! polling, but with a capped backoff: an unwatchable repository must not cost
//! a complete Git status pipeline every 750 ms for the life of the connection.

use super::*;
use std::path::{Path, PathBuf};

/// First fallback poll interval, matching the previous fixed cadence.
const FALLBACK_MIN: Duration = Duration::from_millis(750);
/// Ceiling for the fallback poll interval.
const FALLBACK_MAX: Duration = Duration::from_secs(30);

pub(in crate::service::git) enum WatchSignal {
    Changed,
    Failed,
}

pub(in crate::service::git) struct RepositoryWatcher {
    _watcher: RecommendedWatcher,
    _capabilities: Arc<RepositoryCapabilities>,
    observation: Arc<GitObservation>,
}

impl Drop for RepositoryWatcher {
    fn drop(&mut self) {
        self.observation.watcher_dropped();
    }
}

/// One generation of observation.
///
/// Establishing a watcher awaits a blocking call, and the last subscriber can
/// leave during that await. Stamping the generation is what stops the departing
/// generation's task from later publishing `observing` on behalf of a watcher
/// that has since been replaced.
pub(super) struct Observation {
    generation: u64,
    stopped: Arc<AtomicBool>,
    pub(super) wake: Arc<tokio::sync::Notify>,
}

impl Observation {
    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.wake.notify_waiters();
    }

    pub(super) fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
}

#[derive(Default)]
pub(super) struct WatcherState {
    installed: Option<Arc<Observation>>,
    next_generation: u64,
}

/// The next fallback interval.
///
/// Doubling on every unchanged poll and resetting on any observed change keeps
/// a quiet repository near the ceiling while a busy one stays responsive.
pub(super) fn next_fallback(current: Duration, changed: bool) -> Duration {
    if changed {
        FALLBACK_MIN
    } else {
        (current.saturating_mul(2)).min(FALLBACK_MAX)
    }
}

impl RepositoryCoordinator {
    /// Establishes the repository's only native watcher, once.
    ///
    /// Establishment is awaited rather than left to the background task: the
    /// first subscriber's bootstrap read must happen with observation already
    /// in place, and every later subscriber must be able to see that the
    /// repository is observed and so reuse the shared snapshot.
    pub(in crate::service::git) async fn ensure_watcher(
        self: &Arc<Self>,
        capabilities: &Arc<RepositoryCapabilities>,
    ) {
        let observation = {
            let mut state = self.watcher.lock().unwrap();
            if state.installed.is_some() {
                return;
            }
            state.next_generation += 1;
            let observation = Arc::new(Observation {
                generation: state.next_generation,
                stopped: Arc::new(AtomicBool::new(false)),
                wake: Arc::new(tokio::sync::Notify::new()),
            });
            state.installed = Some(Arc::clone(&observation));
            observation
        };
        let native = self.establish(capabilities).await;
        // The last subscriber can leave during establishment. A watcher with
        // nobody to publish to is a task that refreshes forever for nobody.
        if self.subscriber_count() == 0 {
            self.stop_watcher();
            return;
        }
        self.publish_observing(&observation, native.is_some());
        let coordinator = Arc::clone(self);
        let capabilities = Arc::clone(capabilities);
        tokio::spawn(async move { coordinator.observe(capabilities, observation, native).await });
    }

    /// Marks the established native watcher broken, as the platform does.
    ///
    /// Recovery runs through exactly the same flag, the same wake and the same
    /// top-of-loop retirement that a real `notify` failure uses.
    #[cfg(test)]
    pub(in crate::service::git) fn fail_native_watcher_for_test(&self) {
        self.native_failed.store(true, Ordering::Release);
        self.native_broken.notify_one();
    }

    pub(in crate::service::git) fn stop_watcher(&self) {
        let installed = self.watcher.lock().unwrap().installed.take();
        if let Some(observation) = installed {
            observation.stop();
            self.observing.store(false, Ordering::Release);
        }
    }

    /// Publishes observation state only while this generation is the live one.
    fn publish_observing(&self, observation: &Arc<Observation>, observing: bool) {
        let state = self.watcher.lock().unwrap();
        if state
            .installed
            .as_ref()
            .is_some_and(|installed| installed.generation == observation.generation)
        {
            self.observing.store(observing, Ordering::Release);
        }
    }

    fn observation_stopped(&self, observation: &Observation) -> bool {
        observation.stopped() || self.closed.load(Ordering::Acquire)
    }

    async fn establish(
        self: &Arc<Self>,
        capabilities: &Arc<RepositoryCapabilities>,
    ) -> Option<EstablishedWatcher> {
        let capabilities = Arc::clone(capabilities);
        let root = self.key.root.clone();
        let token = self.key.root_token.clone();
        let failed = Arc::clone(&self.native_failed);
        let broken = Arc::clone(&self.native_broken);
        let observation = Arc::clone(&self.observation);
        tokio::task::spawn_blocking(move || {
            start_repository_watcher(&capabilities, &root, &token, failed, broken, observation)
        })
        .await
        .ok()
        .and_then(Result::ok)
    }

    /// Sleeps, unless observation is already stopped or stops during it.
    ///
    /// The waiter is enabled before the condition is read: `notify_waiters`
    /// stores no permit, so a stop landing in that window would be lost and the
    /// task would sleep out its whole fallback interval — up to 30 seconds —
    /// after its last consumer had gone.
    async fn rest(&self, observation: &Observation, duration: Duration) {
        let stopping = observation.wake.notified();
        tokio::pin!(stopping);
        stopping.as_mut().enable();
        if self.observation_stopped(observation) {
            return;
        }
        tokio::select! {
            () = tokio::time::sleep(duration) => {}
            () = &mut stopping => {}
        }
    }

    async fn observe(
        self: Arc<Self>,
        capabilities: Arc<RepositoryCapabilities>,
        observation: Arc<Observation>,
        established: Option<EstablishedWatcher>,
    ) {
        // True while a change could have happened with nobody watching.
        let mut unobserved = established.is_none();
        let (mut native, mut signals) = match established {
            Some((watcher, receiver)) => (Some(watcher), Some(receiver)),
            None => (None, None),
        };
        let mut fallback = FALLBACK_MIN;
        let mut last_source = self
            .cached_status()
            .map(|status| status.source_generation.clone())
            .unwrap_or_default();
        while !self.observation_stopped(&observation) {
            // A watcher the platform has reported broken is retired here rather
            // than at the point the failure was noticed, so there is exactly one
            // place that decides whether this repository is still observed.
            if native.is_some() && self.native_failed.load(Ordering::Acquire) {
                native = None;
                signals = None;
                unobserved = true;
                self.publish_observing(&observation, false);
            }
            if native.is_none() {
                match self.establish(&capabilities).await {
                    Some((watcher, receiver)) => {
                        native = Some(watcher);
                        signals = Some(receiver);
                        fallback = FALLBACK_MIN;
                    }
                    None => {
                        native = None;
                        signals = None;
                    }
                }
                self.publish_observing(&observation, native.is_some());
                // Anything that happened while this repository was unwatched was
                // never reported, so the first cycle after a gap always reads.
                let polling = native.is_none();
                if unobserved || polling {
                    unobserved = false;
                    self.invalidate();
                    self.await_mutation_quiescence(&observation).await;
                    self.refresh(
                        &capabilities,
                        &observation,
                        &mut last_source,
                        &mut fallback,
                        polling,
                    )
                    .await;
                }
                if polling {
                    self.rest(&observation, fallback).await;
                    continue;
                }
            }
            let Some(receiver) = signals.as_mut() else {
                continue;
            };
            // The failure notify stores a permit, so a watcher flagged broken
            // while this task was elsewhere still wakes it. That is why there
            // is no periodic health poll here: an idle repository costs nothing.
            let broken = self.native_broken.notified();
            let stopping = observation.wake.notified();
            tokio::pin!(broken, stopping);
            broken.as_mut().enable();
            stopping.as_mut().enable();
            if self.native_failed.load(Ordering::Acquire) {
                continue;
            }
            let signal = tokio::select! {
                signal = receiver.recv() => signal,
                () = &mut stopping => break,
                // The top of the loop retires a watcher flagged broken.
                () = &mut broken => continue,
            };
            match signal {
                Some(WatchSignal::Changed) => {
                    // Invalidated on arrival rather than after the debounce, so
                    // a status request landing during the settle window cannot
                    // be served a stale snapshot.
                    self.invalidate();
                    self.rest(&observation, WATCH_DEBOUNCE).await;
                }
                // A failed or closed native watcher is retired at the top of the
                // next iteration, which also re-reads once.
                _ => {
                    self.native_failed.store(true, Ordering::Release);
                    self.invalidate();
                }
            }
            if self.observation_stopped(&observation) {
                break;
            }
            // A mutation's own writes wake this watcher. Waiting for the
            // mutation to publish its post-command refresh means that refresh
            // is the one authoritative read, not the first of two.
            self.await_mutation_quiescence(&observation).await;
            if self.observation_stopped(&observation) {
                break;
            }
            self.refresh(
                &capabilities,
                &observation,
                &mut last_source,
                &mut fallback,
                false,
            )
            .await;
        }
        self.publish_observing(&observation, false);
        drop(native);
    }

    /// One shared refresh, plus the fallback interval it implies.
    async fn refresh(
        self: &Arc<Self>,
        capabilities: &Arc<RepositoryCapabilities>,
        observation: &Observation,
        last_source: &mut String,
        fallback: &mut Duration,
        polling: bool,
    ) {
        if self.observation_stopped(observation) {
            return;
        }
        // Claimed before the read, so a failure is ordered ahead of any
        // pipeline that starts afterwards and cannot overwrite its success.
        let sequence = self.next_publication();
        match self.status(capabilities, Freshness::Coalesced, None).await {
            Ok(snapshot) => {
                let changed = snapshot.source_generation != *last_source;
                *last_source = snapshot.source_generation.clone();
                if polling {
                    *fallback = next_fallback(*fallback, changed);
                }
            }
            Err(error) => {
                self.publish_error(error.to_string(), sequence).await;
                if polling {
                    *fallback = next_fallback(*fallback, false);
                }
            }
        }
    }
}

/// A live native watcher and its signal stream. Whether it is still healthy is
/// the coordinator's `native_failed` flag, which its callback raises.
type EstablishedWatcher = (RepositoryWatcher, tokio::sync::mpsc::Receiver<WatchSignal>);

/// Whether a filesystem event could have changed what Git would report.
///
/// inotify reports opens and reads, and reading a repository is exactly what a
/// status refresh does: `git status` alone produced dozens of access events on
/// the directories being watched. Treating those as changes made every refresh
/// schedule the next one, so an untouched repository re-ran a complete Git
/// status pipeline forever. Any real modification still arrives as
/// create/modify/remove, and anything the backend cannot classify is treated
/// conservatively as a change.
fn signals_change(kind: &notify::EventKind) -> bool {
    use notify::event::{AccessKind, EventKind};

    match kind {
        EventKind::Access(AccessKind::Close(notify::event::AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        EventKind::Any
        | EventKind::Create(_)
        | EventKind::Modify(_)
        | EventKind::Remove(_)
        | EventKind::Other => true,
    }
}

/// One directory this repository must observe.
struct WatchTarget {
    /// The path handed to `notify`, kept descriptor-backed where the capability
    /// is descriptor-backed.
    watch: PathBuf,
    /// The resolved location, used only to decide containment.
    resolved: PathBuf,
    identity: (u64, u64),
}

/// The smallest set of directories covering worktree, git dir and common dir.
///
/// A conventional repository has all three inside the worktree, so the previous
/// three recursive registrations were the same subtree three times. A linked
/// worktree has its git dir inside the common dir, so that pair collapses too.
fn deduplicate_watch_targets(mut targets: Vec<WatchTarget>) -> Vec<WatchTarget> {
    targets.sort_by_key(|target| target.resolved.components().count());
    let mut retained: Vec<WatchTarget> = Vec::with_capacity(targets.len());
    for target in targets {
        let covered = retained.iter().any(|kept| {
            kept.identity == target.identity || target.resolved.starts_with(&kept.resolved)
        });
        if !covered {
            retained.push(target);
        }
    }
    retained
}

fn watch_target(path: &Path) -> anyhow::Result<WatchTarget> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = std::fs::metadata(path)?;
    Ok(WatchTarget {
        watch: path.to_path_buf(),
        resolved: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        identity: (metadata.dev(), metadata.ino()),
    })
}

pub(in crate::service::git) fn start_repository_watcher(
    capabilities: &Arc<RepositoryCapabilities>,
    logical_root: &str,
    root_token: &str,
    failed: Arc<AtomicBool>,
    broken: Arc<tokio::sync::Notify>,
    observation: Arc<GitObservation>,
) -> anyhow::Result<EstablishedWatcher> {
    let root = capabilities.try_clone_root()?;
    let root_identity = root.identity()?;
    let (git_dir, common_dir) = capabilities.metadata.stable_paths();
    let targets = deduplicate_watch_targets(
        [
            root.watch_path(),
            PathBuf::from(&git_dir),
            PathBuf::from(&common_dir),
        ]
        .iter()
        .map(|path| watch_target(path))
        .collect::<anyhow::Result<Vec<_>>>()?,
    );

    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    failed.store(false, Ordering::Release);
    let callback_failed = Arc::clone(&failed);
    let callback_broken = Arc::clone(&broken);
    let mut watcher =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
            Ok(event) if signals_change(&event.kind) => {
                let _ = sender.try_send(WatchSignal::Changed);
            }
            Ok(_) => {}
            Err(_) => {
                callback_failed.store(true, Ordering::Release);
                // A permit, not a wakeup: the one-slot signal channel may
                // already be full, and a dropped failure notice would leave the
                // observer waiting on a stream that will never speak again.
                callback_broken.notify_one();
                let _ = sender.try_send(WatchSignal::Failed);
            }
        })?;
    for target in &targets {
        watcher.watch(&target.watch, RecursiveMode::Recursive)?;
    }
    // The directories were opened before this watch existed. Re-resolving the
    // logical path closes the window in which the whole repository could have
    // been replaced underneath it: the held descriptor would keep reporting the
    // old directory, so only a fresh capture can detect the swap.
    let current = WorktreeRoot::capture(logical_root)?;
    current.validate_token(logical_root, root_token)?;
    if current.identity()? != root_identity {
        bail!("repository root changed while establishing Git watch");
    }
    observation.watcher_created(targets.len() as u64);
    Ok((
        RepositoryWatcher {
            _watcher: watcher,
            _capabilities: Arc::clone(capabilities),
            observation,
        },
        receiver,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(resolved: &str, identity: (u64, u64)) -> WatchTarget {
        WatchTarget {
            watch: PathBuf::from(resolved),
            resolved: PathBuf::from(resolved),
            identity,
        }
    }

    #[test]
    fn conventional_repository_collapses_to_one_recursive_registration() {
        let retained = deduplicate_watch_targets(vec![
            target("/repo", (1, 10)),
            target("/repo/.git", (1, 11)),
            target("/repo/.git", (1, 11)),
        ]);
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].resolved, PathBuf::from("/repo"));
    }

    #[test]
    fn linked_worktree_retains_the_external_common_directory_once() {
        let retained = deduplicate_watch_targets(vec![
            target("/work/tree", (1, 20)),
            target("/main/.git/worktrees/tree", (1, 21)),
            target("/main/.git", (1, 22)),
        ]);
        let resolved: Vec<_> = retained
            .iter()
            .map(|target| target.resolved.clone())
            .collect();
        assert_eq!(
            resolved,
            vec![PathBuf::from("/work/tree"), PathBuf::from("/main/.git")]
        );
    }

    #[test]
    fn separate_directories_sharing_an_inode_are_registered_once() {
        let retained =
            deduplicate_watch_targets(vec![target("/a/one", (7, 99)), target("/b/two", (7, 99))]);
        assert_eq!(retained.len(), 1);
    }

    #[test]
    fn reads_do_not_count_as_repository_changes() {
        use notify::event::{
            AccessKind, AccessMode, CreateKind, EventKind, ModifyKind, RemoveKind,
        };

        assert!(!signals_change(&EventKind::Access(AccessKind::Open(
            AccessMode::Any
        ))));
        assert!(!signals_change(&EventKind::Access(AccessKind::Read)));
        assert!(!signals_change(&EventKind::Access(AccessKind::Close(
            AccessMode::Read
        ))));
        assert!(!signals_change(&EventKind::Access(AccessKind::Any)));

        assert!(signals_change(&EventKind::Access(AccessKind::Close(
            AccessMode::Write
        ))));
        assert!(signals_change(&EventKind::Create(CreateKind::File)));
        assert!(signals_change(&EventKind::Modify(ModifyKind::Any)));
        assert!(signals_change(&EventKind::Remove(RemoveKind::File)));
        assert!(signals_change(&EventKind::Any));
        assert!(signals_change(&EventKind::Other));
    }

    #[test]
    fn fallback_backs_off_to_the_ceiling_and_resets_on_change() {
        let mut interval = FALLBACK_MIN;
        let mut total = Duration::ZERO;
        for _ in 0..10 {
            total += interval;
            interval = next_fallback(interval, false);
        }
        assert_eq!(interval, FALLBACK_MAX);
        // The same ten unchanged polls at the old fixed cadence spanned 7.5 s.
        assert!(total > Duration::from_secs(60), "{total:?}");
        assert_eq!(next_fallback(interval, true), FALLBACK_MIN);
    }
}
