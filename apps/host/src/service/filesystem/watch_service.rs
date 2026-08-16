use super::watch_fallback::{
    FALLBACK_TICK, FallbackTurn, advance_target_async, native_retry_due, record_native_retry,
    scan_due,
};
use super::*;

#[derive(Clone)]
pub(super) struct Watch {
    pub(super) root: Arc<RootCapability>,
    pub(super) root_token: String,
    pub(super) path: String,
    pub(super) target: PathBuf,
    pub(super) target_directory: Arc<File>,
    /// This exact target's fallback state. Only a target the native watcher
    /// refused ever polls; every healthy watch stays idle.
    pub(super) fallback: Arc<Mutex<FallbackTarget>>,
}

impl FileService {
    pub(crate) fn spawn_watcher(
        self: &Arc<Self>,
        closed: Arc<AtomicBool>,
        sender: mpsc::Sender<SequencerControl>,
        overflowed: Arc<AtomicBool>,
    ) {
        let (native_tx, mut native_rx) = mpsc::channel::<()>(1);
        let native_dirty = Arc::new(Mutex::new(BTreeSet::<PathBuf>::new()));
        let native_rescan = Arc::new(AtomicBool::new(false));
        let callback_dirty = Arc::clone(&native_dirty);
        let callback_rescan = Arc::clone(&native_rescan);
        let watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            match event {
                Ok(event) => {
                    if event.need_rescan() {
                        callback_rescan.store(true, Ordering::Release);
                    }
                    let mut dirty = callback_dirty.lock().unwrap();
                    for path in event.paths {
                        if dirty.len() >= 4096 {
                            callback_rescan.store(true, Ordering::Release);
                            break;
                        }
                        dirty.insert(path);
                    }
                }
                Err(_) => callback_rescan.store(true, Ordering::Release),
            }
            let _ = native_tx.try_send(());
        });
        self.spawn_polling_fallback(Arc::clone(&closed), sender.clone(), Arc::clone(&overflowed));
        let Ok(watcher) = watcher else {
            // No native watcher at all: every registration below fails closed
            // onto this connection's per-target fallback.
            overflowed.store(true, Ordering::Release);
            return;
        };
        *self.native_watcher.lock().unwrap() = Some(watcher);
        let service = Arc::clone(self);
        tokio::spawn(async move {
            while !closed.load(Ordering::Acquire) {
                let first = tokio::select! {
                    event = native_rx.recv() => event,
                    _ = sleep(Duration::from_millis(100)) => continue,
                };
                let Some(()) = first else { break };
                sleep(Duration::from_millis(75)).await;
                while native_rx.try_recv().is_ok() {}
                let changed: Vec<PathBuf> = std::mem::take(&mut *native_dirty.lock().unwrap())
                    .into_iter()
                    .collect();
                let watches = service.watch_entries();
                let all_rescan = native_rescan.swap(false, Ordering::AcqRel);
                for (watch_id, watch) in watches
                    .into_iter()
                    .filter(|(_, watch)| watch_matches_changes(watch, &changed, all_rescan))
                {
                    if !all_rescan {
                        // `precise_file_events` stats every dirty path, which on
                        // this host can be a slow filesystem and is now the
                        // primary path for the whole feature. It does not belong
                        // on a runtime worker.
                        let mapped = {
                            let watch = watch.clone();
                            let watch_id = watch_id.clone();
                            let changed = changed.clone();
                            tokio::task::spawn_blocking(move || {
                                precise_file_events(&watch_id, &watch, &changed)
                            })
                            .await
                        };
                        for event in mapped.unwrap_or_default() {
                            broadcast_control_event(event);
                        }
                        continue;
                    }
                    service
                        .publish_authoritative_listing(
                            &watch_id,
                            &watch,
                            true,
                            &sender,
                            &overflowed,
                        )
                        .await;
                }
            }
        });
    }

    fn watch_entries(&self) -> Vec<(String, Watch)> {
        self.watches
            .lock()
            .unwrap()
            .iter()
            .map(|(id, watch)| (id.clone(), watch.clone()))
            .collect()
    }

    /// Whether the registration this exact watch holds is still current.
    fn watch_is_current(&self, watch_id: &str, watch: &Watch) -> bool {
        self.watches
            .lock()
            .unwrap()
            .get(watch_id)
            .is_some_and(|current| {
                current.root_token == watch.root_token
                    && current.path == watch.path
                    && Arc::ptr_eq(&current.fallback, &watch.fallback)
            })
    }

    /// Re-lists a watched directory and publishes it as an authoritative
    /// snapshot the desktop replaces its cached listing from.
    async fn publish_authoritative_listing(
        self: &Arc<Self>,
        watch_id: &str,
        watch: &Watch,
        overflow_recovery: bool,
        sender: &mpsc::Sender<SequencerControl>,
        overflowed: &Arc<AtomicBool>,
    ) {
        let root = Arc::clone(&watch.root);
        let path = watch.path.clone();
        let listing_id = watch_id.to_owned();
        let generation = self.next_generation();
        let listed = tokio::task::spawn_blocking({
            let service = Arc::clone(self);
            move || {
                service.list_directory_snapshot(
                    &root,
                    &path,
                    &listing_id,
                    generation,
                    "",
                    0,
                    &NEVER_CANCELLED,
                )
            }
        })
        .await;
        let Ok(Ok(mut snapshot)) = listed else {
            return;
        };
        if !self.watch_is_current(watch_id, watch) {
            return;
        }
        snapshot.recovered_from_overflow = overflow_recovery;
        snapshot.authoritative = true;
        emit_event(
            sender,
            overflowed,
            v1::HostEvent {
                kind: v1::EventKind::DirectorySnapshot.into(),
                scope: watch_id.to_owned(),
                file: Some(v1::FileServiceEvent {
                    directory: Some(snapshot),
                    root_token: watch.root_token.clone(),
                    watch_id: watch_id.to_owned(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
    }

    /// Polls only the targets the native watcher refused.
    ///
    /// The set of such targets is derived from the watch registry on every
    /// turn rather than counted alongside it, so registration, replacement,
    /// and release can never leave the poller believing in work that does not
    /// exist. While the set is empty the task parks on
    /// [`FileService::fallback_signal`]; the bounded park is only so a closed
    /// connection's task observes `closed` and exits.
    fn spawn_polling_fallback(
        self: &Arc<Self>,
        closed: Arc<AtomicBool>,
        sender: mpsc::Sender<SequencerControl>,
        overflowed: Arc<AtomicBool>,
    ) {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            while !closed.load(Ordering::Acquire) {
                let pending = service.fallback_watches();
                if pending.is_empty() {
                    tokio::select! {
                        _ = service.fallback_signal.notified() => {}
                        _ = sleep(IDLE_PARK) => {}
                    }
                    continue;
                }
                sleep(FALLBACK_TICK).await;
                for (id, watch) in pending {
                    let now = Instant::now();
                    if native_retry_due(&watch.fallback, now) {
                        let restored = service.retry_native_registration(&watch);
                        record_native_retry(&watch.fallback, restored);
                        if restored {
                            continue;
                        }
                    }
                    // Backed-off targets are decided here rather than inside a
                    // blocking worker: dispatching one per target per tick to
                    // learn that none of them is due is thousands of no-op
                    // round trips a second on a connection with many watches.
                    if !scan_due(&watch.fallback, now) {
                        continue;
                    }
                    let stable_target = descriptor_path(watch.target_directory.as_raw_fd());
                    let turn =
                        advance_target_async(stable_target, Arc::clone(&watch.fallback), now).await;
                    if turn
                        .as_ref()
                        .is_ok_and(|turn| *turn == FallbackTurn::Changed)
                    {
                        service
                            .publish_authoritative_listing(&id, &watch, true, &sender, &overflowed)
                            .await;
                    }
                }
            }
        });
    }

    /// Exactly the watches whose native registration failed.
    fn fallback_watches(&self) -> Vec<(String, Watch)> {
        self.watch_entries()
            .into_iter()
            .filter(|(_, watch)| !watch.fallback.lock().unwrap().is_native())
            .collect()
    }

    fn retry_native_registration(&self, watch: &Watch) -> bool {
        let mut watcher = self.native_watcher.lock().unwrap();
        watcher.as_mut().is_some_and(|watcher| {
            watcher
                .watch(&watch.target, RecursiveMode::NonRecursive)
                .is_ok()
        })
    }

    #[cfg(test)]
    pub(crate) fn watch_directory(
        &self,
        root: &str,
        path: &str,
        watch_id: &str,
    ) -> anyhow::Result<v1::DirectorySnapshot> {
        let token = root_token(root)?;
        self.watch_directory_authorized(root, &token, path, watch_id)
    }

    #[cfg(test)]
    pub(crate) fn watch_directory_authorized(
        &self,
        root: &str,
        root_token: &str,
        path: &str,
        watch_id: &str,
    ) -> anyhow::Result<v1::DirectorySnapshot> {
        self.watch_directory_cancellable(root, root_token, path, watch_id, &NEVER_CANCELLED)
    }

    /// Arms a watch and returns the directory's authoritative listing.
    ///
    /// The listing is cancellable because it *is* the expansion's listing: a
    /// folder opened and closed again on a slow remote link must stop the
    /// enumeration it started, not pay for it and discard the answer.
    pub(crate) fn watch_directory_cancellable(
        &self,
        root: &str,
        root_token: &str,
        path: &str,
        watch_id: &str,
        cancellation: &AtomicBool,
    ) -> anyhow::Result<v1::DirectorySnapshot> {
        validate_token("watch ID", watch_id)?;
        let root = Arc::new(RootCapability::validate(root, root_token)?);
        let (target, target_directory) = resolve_watch_directory(&root, path)?;
        let path_value = target.to_string_lossy().into_owned();
        let stable_target = descriptor_path(target_directory.as_raw_fd());
        let fingerprint = watch_fingerprint(&stable_target)?;
        let mut watches = self.watches.lock().unwrap();
        if !watches.contains_key(watch_id) && watches.len() >= MAX_WATCHES {
            bail!("watch limit of {MAX_WATCHES} directories reached");
        }
        // Arm the native watch before publishing the snapshot. Any event that
        // races insertion is still represented by the subsequent listing;
        // every event after insertion is queued for a resnapshot.
        let native = self
            .native_watcher
            .lock()
            .unwrap()
            .as_mut()
            .is_some_and(|watcher| watcher.watch(&target, RecursiveMode::NonRecursive).is_ok());
        let fallback = Arc::new(Mutex::new(if native {
            FallbackTarget::native(fingerprint)
        } else {
            FallbackTarget::polling(fingerprint)
        }));
        let previous = watches.insert(
            watch_id.to_owned(),
            Watch {
                root: Arc::clone(&root),
                root_token: root.token().to_owned(),
                path: path_value,
                target: target.clone(),
                target_directory: Arc::new(target_directory),
                fallback,
            },
        );
        // Still under the registry lock: a target this ID no longer holds loses
        // its native registration only if no other watch covers it, and that
        // decision cannot be raced by a concurrent registration.
        if let Some(previous) = previous.as_ref()
            && previous.target != target
            && !watches
                .values()
                .any(|watch| watch.target == previous.target)
            && let Some(watcher) = self.native_watcher.lock().unwrap().as_mut()
        {
            let _ = watcher.unwatch(&previous.target);
        }
        drop(watches);
        if !native {
            self.fallback_signal.notify_one();
        }
        match self.list_directory_snapshot(
            &root,
            path,
            watch_id,
            self.next_generation(),
            "",
            0,
            cancellation,
        ) {
            Ok(snapshot) => Ok(snapshot),
            Err(error) => {
                let _ = self.unwatch_directory(watch_id);
                if let Some(previous) = previous {
                    if !self.retry_native_registration(&previous) {
                        previous.fallback.lock().unwrap().degrade_to_polling();
                        self.fallback_signal.notify_one();
                    }
                    self.watches
                        .lock()
                        .unwrap()
                        .insert(watch_id.to_owned(), previous);
                }
                Err(error)
            }
        }
    }

    pub(crate) fn unwatch_directory(&self, watch_id: &str) -> anyhow::Result<()> {
        validate_token("watch ID", watch_id)?;
        let removed = self.watches.lock().unwrap().remove(watch_id);
        let Some(removed) = removed else {
            return Ok(());
        };
        let still_watched = self
            .watches
            .lock()
            .unwrap()
            .values()
            .any(|watch| watch.target == removed.target);
        if !still_watched && let Some(watcher) = self.native_watcher.lock().unwrap().as_mut() {
            let _ = watcher.unwatch(&removed.target);
        }
        Ok(())
    }
}

/// Whether any dirty path belongs to this watch's directory.
///
/// The directory itself counts, so a change to the directory can still schedule
/// a rescan of it — it just never becomes an entry inside itself.
pub(super) fn watch_matches_changes(
    watch: &Watch,
    changed: &[PathBuf],
    authoritative_rescan: bool,
) -> bool {
    authoritative_rescan
        || changed
            .iter()
            .any(|path| path == &watch.target || path.parent() == Some(watch.target.as_path()))
}

pub(super) fn precise_file_events(
    watch_id: &str,
    watch: &Watch,
    changed: &[PathBuf],
) -> Vec<v1::HostEvent> {
    let root = watch.root.logical_root();
    let mut affected = BTreeMap::<PathBuf, bool>::new();
    for path in changed
        .iter()
        // Children only. An event about the watched directory *itself* is not
        // an entry in it: mapped as one it produced a row for the directory
        // inside its own listing, under a second path spelling with a trailing
        // separator that every downstream key then treated as a different
        // directory. Directory-level change reaches the desktop as the
        // authoritative rescan its own parent's watch reports.
        .filter(|path| path.parent() == Some(watch.target.as_path()))
        // An entry no listing reports must not wake the explorer either. On
        // macOS every folder Finder has ever opened gains a `.DS_Store` that is
        // rewritten behind the user's back, and each rewrite would otherwise
        // become a `FileChanged` the desktop answers with a full re-list of the
        // directory — for an entry the re-list then filters out.
        .filter(|path| !path.file_name().is_some_and(is_always_hidden))
    {
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let deleted = match fs::symlink_metadata(path) {
            Ok(_) => false,
            Err(error) if error.kind() == ErrorKind::NotFound => true,
            Err(_) => continue,
        };
        affected.insert(relative.to_owned(), deleted);
    }
    affected
        .into_iter()
        .map(|(relative, deleted)| {
            let path = root.join(&relative).to_string_lossy().into_owned();
            let metadata = if deleted {
                v1::FileMetadata {
                    path: path.clone(),
                    ..Default::default()
                }
            } else {
                metadata_for_anchored(
                    &watch.root.stable_root(),
                    &watch.root.stable_root().join(&relative),
                    &root.join(&relative),
                )
                .unwrap_or(v1::FileMetadata {
                    path: path.clone(),
                    ..Default::default()
                })
            };
            v1::HostEvent {
                kind: v1::EventKind::FileChanged.into(),
                scope: path,
                file: Some(v1::FileServiceEvent {
                    metadata: Some(metadata),
                    deleted,
                    state: "external".into(),
                    root_token: watch.root_token.clone(),
                    watch_id: watch_id.to_owned(),
                    ..Default::default()
                }),
                ..Default::default()
            }
        })
        .collect()
}
