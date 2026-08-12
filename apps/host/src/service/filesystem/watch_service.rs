use super::*;

const FALLBACK_SCAN_INTERVAL: Duration = Duration::from_millis(25);
pub(super) const FALLBACK_SCAN_ENTRY_BUDGET: usize = 2_048;
const FALLBACK_SCAN_TIME_BUDGET: Duration = Duration::from_millis(8);
const FALLBACK_AUTHORITATIVE_CYCLES: u64 = 16;

pub(super) struct FallbackScan {
    iterator: Option<fs::ReadDir>,
    accumulator: u64,
    last_completed: u64,
    completed_cycles: u64,
}

impl FallbackScan {
    pub(super) fn new(initial: u64) -> Self {
        Self {
            iterator: None,
            accumulator: 0,
            last_completed: initial,
            completed_cycles: 0,
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct FallbackShard {
    pub(super) processed: usize,
    pub(super) completed: bool,
    pub(super) emit_authoritative: bool,
}

#[derive(Clone)]
pub(super) struct Watch {
    pub(super) root: Arc<RootCapability>,
    pub(super) root_token: String,
    pub(super) path: String,
    pub(super) target: PathBuf,
    pub(super) target_directory: Arc<File>,
    pub(super) fallback_scan: Arc<Mutex<FallbackScan>>,
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
        let Ok(watcher) = watcher else {
            overflowed.store(true, Ordering::Release);
            self.polling_fallback.store(true, Ordering::Release);
            self.spawn_polling_fallback(closed, sender, overflowed);
            return;
        };
        *self.native_watcher.lock().unwrap() = Some(watcher);
        self.spawn_polling_fallback(Arc::clone(&closed), sender.clone(), Arc::clone(&overflowed));
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
                let paths = std::mem::take(&mut *native_dirty.lock().unwrap());
                let events: Vec<_> = paths
                    .into_iter()
                    .map(|path| {
                        let mut event = Event::new(notify::EventKind::Any);
                        event.paths.push(path);
                        Ok(event)
                    })
                    .collect();
                let watches: Vec<_> = service
                    .watches
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(id, watch)| (id.clone(), watch.clone()))
                    .collect();
                let all_rescan = native_rescan.swap(false, Ordering::AcqRel);
                for (watch_id, watch) in watches
                    .into_iter()
                    .filter(|(_, watch)| watch_matches_events(watch, &events, all_rescan))
                {
                    if !all_rescan {
                        for event in precise_file_events(&watch_id, &watch, &events) {
                            broadcast_control_event(event);
                        }
                        continue;
                    }
                    let path = watch.path.clone();
                    let listing_watch_id = watch_id.clone();
                    let root = Arc::clone(&watch.root);
                    let listed = tokio::task::spawn_blocking(move || {
                        list_directory_impl(&root, &path, &listing_watch_id, 0, "", 0)
                    })
                    .await;
                    let Ok(Ok(mut snapshot)) = listed else {
                        continue;
                    };
                    let changed =
                        service
                            .watches
                            .lock()
                            .unwrap()
                            .get(&watch_id)
                            .is_some_and(|current| {
                                current.root_token == watch.root_token && current.path == watch.path
                            });
                    if changed {
                        snapshot.generation = service.next_generation();
                        snapshot.overflowed = all_rescan;
                        snapshot.authoritative = true;
                        emit_event(
                            &sender,
                            &overflowed,
                            v1::HostEvent {
                                kind: v1::EventKind::DirectorySnapshot.into(),
                                scope: watch_id.clone(),
                                file: Some(v1::FileServiceEvent {
                                    directory: Some(snapshot),
                                    root_token: watch.root_token.clone(),
                                    watch_id: watch_id.clone(),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                        );
                    }
                }
            }
        });
    }

    fn spawn_polling_fallback(
        self: &Arc<Self>,
        closed: Arc<AtomicBool>,
        sender: mpsc::Sender<SequencerControl>,
        overflowed: Arc<AtomicBool>,
    ) {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            while !closed.load(Ordering::Acquire) {
                sleep(FALLBACK_SCAN_INTERVAL).await;
                if !service.polling_fallback.load(Ordering::Acquire) {
                    continue;
                }
                let watches: Vec<_> = service
                    .watches
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(id, watch)| (id.clone(), watch.clone()))
                    .collect();
                for (id, watch) in watches {
                    let stable_target = descriptor_path(watch.target_directory.as_raw_fd());
                    let scan = Arc::clone(&watch.fallback_scan);
                    let scanned = scan_fallback_shard_async(stable_target, scan).await;
                    let Ok(shard) = scanned else { continue };
                    if !shard.emit_authoritative {
                        continue;
                    }
                    let root = Arc::clone(&watch.root);
                    let path = watch.path.clone();
                    let listing_id = id.clone();
                    let listed = tokio::task::spawn_blocking(move || {
                        list_directory_impl(&root, &path, &listing_id, 0, "", 0)
                    })
                    .await;
                    if let Ok(Ok(mut snapshot)) = listed {
                        let current = service.watches.lock().unwrap().get(&id).cloned();
                        let still_current = current.is_some_and(|current| {
                            current.root_token == watch.root_token
                                && current.path == watch.path
                                && Arc::ptr_eq(&current.fallback_scan, &watch.fallback_scan)
                        });
                        if !still_current {
                            continue;
                        }
                        snapshot.generation = service.next_generation();
                        snapshot.overflowed = true;
                        snapshot.authoritative = true;
                        emit_event(
                            &sender,
                            &overflowed,
                            v1::HostEvent {
                                kind: v1::EventKind::DirectorySnapshot.into(),
                                scope: id.clone(),
                                file: Some(v1::FileServiceEvent {
                                    directory: Some(snapshot),
                                    root_token: watch.root_token.clone(),
                                    watch_id: id.clone(),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                        );
                    }
                }
            }
        });
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

    pub(crate) fn watch_directory_authorized(
        &self,
        root: &str,
        root_token: &str,
        path: &str,
        watch_id: &str,
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
        if let Some(watcher) = self.native_watcher.lock().unwrap().as_mut() {
            if watcher.watch(&target, RecursiveMode::NonRecursive).is_err() {
                self.polling_fallback.store(true, Ordering::Release);
            }
        } else {
            self.polling_fallback.store(true, Ordering::Release);
        }
        let previous = watches.insert(
            watch_id.to_owned(),
            Watch {
                root: Arc::clone(&root),
                root_token: root.token().to_owned(),
                path: path_value,
                target: target.clone(),
                target_directory: Arc::new(target_directory),
                fallback_scan: Arc::new(Mutex::new(FallbackScan::new(watch_fingerprint(
                    &stable_target,
                )?))),
            },
        );
        drop(watches);
        let mut watcher_guard = self.native_watcher.lock().unwrap();
        if let Some(watcher) = watcher_guard.as_mut()
            && let Some(previous) = previous.as_ref()
            && previous.target != target
            && !self
                .watches
                .lock()
                .unwrap()
                .values()
                .any(|watch| watch.target == previous.target)
        {
            let _ = watcher.unwatch(&previous.target);
        }
        drop(watcher_guard);
        match list_directory_impl(&root, path, watch_id, self.next_generation(), "", 0) {
            Ok(snapshot) => Ok(snapshot),
            Err(error) => {
                let _ = self.unwatch_directory(watch_id);
                if let Some(previous) = previous {
                    if let Some(watcher) = self.native_watcher.lock().unwrap().as_mut() {
                        let _ = watcher.watch(&previous.target, RecursiveMode::NonRecursive);
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
        if let Some(removed) = removed
            && !self
                .watches
                .lock()
                .unwrap()
                .values()
                .any(|watch| watch.target == removed.target)
            && let Some(watcher) = self.native_watcher.lock().unwrap().as_mut()
        {
            let _ = watcher.unwatch(&removed.target);
        }
        Ok(())
    }
}

pub(super) fn watch_matches_events(
    watch: &Watch,
    events: &[notify::Result<Event>],
    authoritative_rescan: bool,
) -> bool {
    authoritative_rescan
        || events.iter().any(|event| {
            event.as_ref().is_ok_and(|event| {
                event.paths.iter().any(|changed| {
                    changed == &watch.target || changed.parent() == Some(watch.target.as_path())
                })
            })
        })
}

pub(super) fn precise_file_events(
    watch_id: &str,
    watch: &Watch,
    events: &[notify::Result<Event>],
) -> Vec<v1::HostEvent> {
    let root = watch.root.logical_root();
    let mut affected = BTreeMap::<PathBuf, bool>::new();
    for path in events
        .iter()
        .filter_map(|event| event.as_ref().ok())
        .flat_map(|event| event.paths.iter())
        .filter(|path| *path == &watch.target || path.parent() == Some(watch.target.as_path()))
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

fn scan_fallback_shard(
    stable_target: &Path,
    state: &Mutex<FallbackScan>,
) -> anyhow::Result<FallbackShard> {
    scan_fallback_shard_with_limits(
        stable_target,
        state,
        FALLBACK_SCAN_ENTRY_BUDGET,
        FALLBACK_SCAN_TIME_BUDGET,
    )
}

pub(super) async fn scan_fallback_shard_async(
    stable_target: PathBuf,
    state: Arc<Mutex<FallbackScan>>,
) -> anyhow::Result<FallbackShard> {
    tokio::task::spawn_blocking(move || scan_fallback_shard(&stable_target, &state))
        .await
        .context("fallback filesystem scan worker stopped")?
}

pub(super) fn scan_fallback_shard_with_limits(
    stable_target: &Path,
    state: &Mutex<FallbackScan>,
    entry_budget: usize,
    time_budget: Duration,
) -> anyhow::Result<FallbackShard> {
    let mut state = state.lock().unwrap();
    if state.iterator.is_none() {
        state.accumulator = metadata_generation(&fs::metadata(stable_target)?);
        state.iterator = Some(fs::read_dir(stable_target)?);
    }

    let iterator = state
        .iterator
        .as_mut()
        .expect("fallback iterator initialized");
    let (additions, processed, completed) =
        fold_fingerprint_records(entry_budget, time_budget, || {
            loop {
                let Some(entry) = iterator.next() else {
                    return Ok(None);
                };
                let entry = entry?;
                match fs::symlink_metadata(entry.path()) {
                    Ok(metadata) => {
                        return Ok(Some(watch_entry_fingerprint(&entry.file_name(), &metadata)));
                    }
                    Err(error) if error.kind() == ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                }
            }
        })?;
    state.accumulator = state.accumulator.wrapping_add(additions);

    let mut emit_authoritative = false;
    if completed {
        let fingerprint = state.accumulator;
        state.completed_cycles += 1;
        emit_authoritative = fingerprint != state.last_completed
            || state
                .completed_cycles
                .is_multiple_of(FALLBACK_AUTHORITATIVE_CYCLES);
        state.last_completed = fingerprint;
        state.iterator = None;
    }
    Ok(FallbackShard {
        processed,
        completed,
        emit_authoritative,
    })
}

pub(super) fn fold_fingerprint_records(
    entry_budget: usize,
    time_budget: Duration,
    mut next: impl FnMut() -> anyhow::Result<Option<u64>>,
) -> anyhow::Result<(u64, usize, bool)> {
    let started = Instant::now();
    let mut accumulator = 0_u64;
    let mut processed = 0;
    while processed < entry_budget && (processed == 0 || started.elapsed() < time_budget) {
        let Some(record) = next()? else {
            return Ok((accumulator, processed, true));
        };
        accumulator = accumulator.wrapping_add(record);
        processed += 1;
    }
    Ok((accumulator, processed, false))
}
