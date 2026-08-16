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
/// How often the native-watch wait rechecks stop/close state.
const STOP_POLL: Duration = Duration::from_millis(250);

pub(in crate::service::git) enum WatchSignal {
    Changed,
    Failed,
}

pub(in crate::service::git) struct RepositoryWatcher {
    _watcher: RecommendedWatcher,
    _capabilities: Arc<RepositoryCapabilities>,
    #[cfg(test)]
    observation: Arc<GitObservation>,
}

impl Drop for RepositoryWatcher {
    fn drop(&mut self) {
        #[cfg(test)]
        self.observation.watcher_dropped();
    }
}

#[derive(Default)]
pub(super) struct WatcherState {
    stop: Option<Arc<AtomicBool>>,
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
        let stop = {
            let mut state = self.watcher.lock().unwrap();
            if state.stop.is_some() {
                return;
            }
            let stop = Arc::new(AtomicBool::new(false));
            state.stop = Some(Arc::clone(&stop));
            stop
        };
        let established = {
            let capabilities = Arc::clone(capabilities);
            let root = self.key.root.clone();
            let token = self.key.root_token.clone();
            #[cfg(test)]
            let observation = Arc::clone(&self.observation);
            tokio::task::spawn_blocking(move || {
                start_repository_watcher(
                    &capabilities,
                    &root,
                    &token,
                    #[cfg(test)]
                    observation,
                )
            })
            .await
        };
        let native = match established {
            Ok(Ok(established)) => Some(established),
            _ => None,
        };
        self.observing.store(native.is_some(), Ordering::Release);
        let coordinator = Arc::clone(self);
        let capabilities = Arc::clone(capabilities);
        tokio::spawn(async move { coordinator.observe(capabilities, stop, native).await });
    }

    pub(in crate::service::git) fn stop_watcher(&self) {
        if let Some(stop) = self.watcher.lock().unwrap().stop.take() {
            stop.store(true, Ordering::Release);
        }
        self.observing.store(false, Ordering::Release);
    }

    fn observation_stopped(&self, stop: &AtomicBool) -> bool {
        stop.load(Ordering::Acquire) || self.closed.load(Ordering::Acquire)
    }

    async fn observe(
        self: Arc<Self>,
        capabilities: Arc<RepositoryCapabilities>,
        stop: Arc<AtomicBool>,
        established: Option<EstablishedWatcher>,
    ) {
        let (mut native, mut signals, mut native_failed) = match established {
            Some((watcher, receiver, failed)) => (Some(watcher), Some(receiver), failed),
            None => (None, None, Arc::new(AtomicBool::new(false))),
        };
        let mut fallback = FALLBACK_MIN;
        let mut last_source = self
            .cached_status()
            .map(|status| status.source_generation.clone())
            .unwrap_or_default();
        while !self.observation_stopped(&stop) {
            if native.is_none() {
                let capabilities = Arc::clone(&capabilities);
                let root = self.key.root.clone();
                let token = self.key.root_token.clone();
                #[cfg(test)]
                let observation = Arc::clone(&self.observation);
                let reestablished = tokio::task::spawn_blocking(move || {
                    start_repository_watcher(
                        &capabilities,
                        &root,
                        &token,
                        #[cfg(test)]
                        observation,
                    )
                })
                .await;
                match reestablished {
                    Ok(Ok((watcher, receiver, failed))) => {
                        native = Some(watcher);
                        signals = Some(receiver);
                        native_failed = failed;
                        fallback = FALLBACK_MIN;
                    }
                    _ => {
                        native = None;
                        signals = None;
                    }
                }
                self.observing.store(native.is_some(), Ordering::Release);
            }
            let observed = match signals.as_mut() {
                Some(receiver) => {
                    let signal = tokio::select! {
                        signal = receiver.recv() => signal,
                        _ = tokio::time::sleep(STOP_POLL) => continue,
                    };
                    match signal {
                        Some(WatchSignal::Changed) if !native_failed.load(Ordering::Acquire) => {
                            // Invalidated on arrival rather than after the
                            // debounce, so a status request landing during the
                            // settle window cannot be served a stale snapshot.
                            self.invalidate();
                            tokio::time::sleep(WATCH_DEBOUNCE).await;
                            while let Ok(WatchSignal::Failed) = receiver.try_recv() {
                                native_failed.store(true, Ordering::Release);
                            }
                            true
                        }
                        // A failed or closed native watcher is dropped so the
                        // next iteration re-establishes it, and the repository
                        // is re-read once because events may have been missed.
                        _ => {
                            native = None;
                            signals = None;
                            self.observing.store(false, Ordering::Release);
                            true
                        }
                    }
                }
                None => {
                    tokio::time::sleep(fallback).await;
                    true
                }
            };
            if self.observation_stopped(&stop) || !observed {
                break;
            }
            self.invalidate();
            // A mutation's own writes wake this watcher. Waiting for the
            // mutation to publish its post-command refresh means that refresh
            // is the one authoritative read, not the first of two.
            self.await_mutation_quiescence().await;
            if self.observation_stopped(&stop) {
                break;
            }
            match self.status(&capabilities, Freshness::Coalesced, None).await {
                Ok(snapshot) => {
                    let changed = snapshot.source_generation != last_source;
                    last_source = snapshot.source_generation.clone();
                    if signals.is_none() {
                        fallback = next_fallback(fallback, changed);
                    }
                }
                Err(error) => {
                    self.publish_error(error.to_string()).await;
                    if signals.is_none() {
                        fallback = next_fallback(fallback, false);
                    }
                }
            }
        }
        self.observing.store(false, Ordering::Release);
        drop(native);
    }
}

/// A live native watcher: the watcher itself, its signal stream, and the flag
/// its callback raises when the platform reports the watch is broken.
type EstablishedWatcher = (
    RepositoryWatcher,
    tokio::sync::mpsc::Receiver<WatchSignal>,
    Arc<AtomicBool>,
);

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
    #[cfg(test)] observation: Arc<GitObservation>,
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
    let failed = Arc::new(AtomicBool::new(false));
    let callback_failed = Arc::clone(&failed);
    let mut watcher =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
            Ok(event) if signals_change(&event.kind) => {
                let _ = sender.try_send(WatchSignal::Changed);
            }
            Ok(_) => {}
            Err(_) => {
                callback_failed.store(true, Ordering::Release);
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
    #[cfg(test)]
    observation.watcher_created(targets.len() as u64);
    Ok((
        RepositoryWatcher {
            _watcher: watcher,
            _capabilities: Arc::clone(capabilities),
            #[cfg(test)]
            observation,
        },
        receiver,
        failed,
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
