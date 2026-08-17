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
        let native_failed = Arc::new(AtomicBool::new(false));
        let callback_dirty = Arc::clone(&native_dirty);
        let callback_rescan = Arc::clone(&native_rescan);
        let callback_failed = Arc::clone(&native_failed);
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
                // The watcher's own stream is compromised, not one target's.
                // A registration that has silently stopped delivering reports
                // nothing at all — `is_native()` stays true, so the target is
                // excluded from the polling fallback and watched by nobody —
                // so every target is put back on polling, from where the
                // ordinary native-retry backoff restores the healthy ones.
                Err(_) => {
                    callback_rescan.store(true, Ordering::Release);
                    callback_failed.store(true, Ordering::Release);
                }
            }
            let _ = native_tx.try_send(());
        });
        self.spawn_polling_fallback(Arc::clone(&closed), sender.clone(), Arc::clone(&overflowed));
        let Ok(watcher) = watcher else {
            // No native watcher at all: every registration below fails closed
            // onto this connection's per-target fallback. Deliberately *not* the
            // sequencer's overflow flag — that one says ordered events were
            // dropped and forces a connection-wide resync, and "this host has no
            // inotify capacity" is a different fact with a different remedy.
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
                // Grouped by parent once, rather than every watch scanning the
                // whole dirty set: with the 4,096-path cap and 128 watches that
                // was half a million comparisons and 128 full clones per batch.
                let by_parent =
                    changes_by_parent(std::mem::take(&mut *native_dirty.lock().unwrap()));
                let all_rescan = native_rescan.swap(false, Ordering::AcqRel);
                if native_failed.swap(false, Ordering::AcqRel) {
                    service.degrade_all_to_polling();
                }
                for (watch_id, watch) in service.watch_entries() {
                    if all_rescan {
                        // A rescan that could not be listed must not be
                        // forgotten: the flag was already consumed, so failing
                        // silently here left the client believing its cached
                        // subtree was current when the watcher had told the
                        // host otherwise.
                        if !service
                            .publish_authoritative_listing(
                                &watch_id,
                                &watch,
                                &sender,
                                &overflowed,
                                true,
                            )
                            .await
                        {
                            native_rescan.store(true, Ordering::Release);
                        }
                        continue;
                    }
                    let Some(children) = by_parent.get(&watch.target) else {
                        continue;
                    };
                    if children.len() > MAX_PRECISE_EVENTS_PER_BATCH {
                        // One authoritative listing instead of hundreds of
                        // precise events. A build or a checkout touching a
                        // whole directory would otherwise fill the ordered
                        // event queue, and an overflow there is a
                        // connection-wide resync — a far more expensive
                        // recovery than the one re-list this costs.
                        if !service
                            .publish_authoritative_listing(
                                &watch_id,
                                &watch,
                                &sender,
                                &overflowed,
                                false,
                            )
                            .await
                        {
                            native_rescan.store(true, Ordering::Release);
                        }
                        continue;
                    }
                    // `precise_file_events` stats every dirty path, which on
                    // this host can be a slow filesystem and is now the
                    // primary path for the whole feature. It does not belong
                    // on a runtime worker.
                    let mapped = {
                        let watch = watch.clone();
                        let watch_id = watch_id.clone();
                        let children = children.clone();
                        tokio::task::spawn_blocking(move || {
                            precise_file_events(&watch_id, &watch, &children)
                        })
                        .await
                    };
                    // A batch that never came back described changes nobody
                    // else will describe. Dropping it silently left the desktop
                    // showing rows that no longer exist, so the next turn
                    // re-lists everything instead.
                    let Ok(mapped) = mapped else {
                        native_rescan.store(true, Ordering::Release);
                        continue;
                    };
                    for event in mapped {
                        broadcast_control_event(event);
                    }
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
    ///
    /// `recovery` says whether events were *lost* rather than merely replaced
    /// by this listing. It is the client's signal to drop what it has cached
    /// below this directory, so a listing that simply stands in for a burst of
    /// precise events — nothing was lost — must not claim it.
    ///
    /// Returns whether the snapshot was published, so a caller that has already
    /// consumed the flag which caused it can put that flag back.
    pub(super) async fn publish_authoritative_listing(
        self: &Arc<Self>,
        watch_id: &str,
        watch: &Watch,
        sender: &mpsc::Sender<SequencerControl>,
        overflowed: &Arc<AtomicBool>,
        recovery: bool,
    ) -> bool {
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
            return false;
        };
        if !self.watch_is_current(watch_id, watch) {
            // Not a failure: this registration was replaced, and the watch that
            // replaced it published — or will publish — its own listing.
            return true;
        }
        snapshot.recovered_from_overflow = recovery;
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
        true
    }

    /// Puts every native target back on polling after a watcher-level failure.
    pub(super) fn degrade_all_to_polling(&self) {
        for (_, watch) in self.watch_entries() {
            watch.fallback.lock().unwrap().degrade_to_polling();
        }
        self.fallback_signal.notify_one();
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
                            // The native watcher only reports what happens
                            // next, and this target has just stopped being
                            // scanned. Anything that changed between the last
                            // completed scan and the registration taking effect
                            // is reported by nobody unless it is published now.
                            service
                                .publish_authoritative_listing(
                                    &id,
                                    &watch,
                                    &sender,
                                    &overflowed,
                                    true,
                                )
                                .await;
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
                            .publish_authoritative_listing(&id, &watch, &sender, &overflowed, false)
                            .await;
                    }
                }
            }
        });
    }

    /// Exactly the watches whose native registration failed.
    pub(super) fn fallback_watches(&self) -> Vec<(String, Watch)> {
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
        // The fingerprint is the polling scanner's baseline, so it is computed
        // only when this target is actually going to poll. Every accepted
        // native watch was otherwise paying a second full directory walk — one
        // extra stat per entry — for a number nothing would ever read.
        let fallback = Arc::new(Mutex::new(if native {
            FallbackTarget::native(0)
        } else {
            FallbackTarget::polling(watch_fingerprint(&stable_target)?)
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
                    let restored = self.retry_native_registration(&previous);
                    // Recorded either way: a target left believing it was native
                    // while still on the polling list published duplicate
                    // authoritative snapshots and scanned a healthy directory
                    // every few seconds forever.
                    record_native_retry(&previous.fallback, restored);
                    if !restored {
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
        // One critical section across removal, the still-watched decision, and
        // the native call — the same rule registration follows. Releasing the
        // registry between them let a concurrent re-watch of the same target
        // arm a registration this unwatch then tore down, leaving a directory
        // that believed it was native, was excluded from the polling fallback,
        // and silently stopped reporting anything at all.
        let mut watches = self.watches.lock().unwrap();
        let Some(removed) = watches.remove(watch_id) else {
            return Ok(());
        };
        if !watches.values().any(|watch| watch.target == removed.target)
            && let Some(watcher) = self.native_watcher.lock().unwrap().as_mut()
        {
            let _ = watcher.unwatch(&removed.target);
        }
        Ok(())
    }
}

/// Groups dirty paths by the directory that owns them.
///
/// Grouped once for the whole batch rather than scanned once per watch: with
/// the 4,096-path dirty cap and 128 registrations, the per-watch scan was half
/// a million comparisons and a full clone of the dirty set each time.
///
/// A path that *is* a watched directory owns nothing inside it, which is why
/// it groups under its own parent and never under itself — a self-event once
/// became a row inside its own listing.
pub(super) fn changes_by_parent(
    changed: impl IntoIterator<Item = PathBuf>,
) -> BTreeMap<PathBuf, Vec<PathBuf>> {
    let mut grouped = BTreeMap::<PathBuf, Vec<PathBuf>>::new();
    for path in changed {
        let Some(parent) = path.parent().map(Path::to_path_buf) else {
            continue;
        };
        grouped.entry(parent).or_default().push(path);
    }
    grouped
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
