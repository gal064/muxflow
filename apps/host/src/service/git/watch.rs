use super::*;

pub(super) struct GitWatch {
    cancelled: Arc<AtomicBool>,
}

pub(super) struct RepositoryWatcher {
    _watcher: RecommendedWatcher,
    _root: WorktreeRoot,
    _metadata: GitMetadataCapability,
}

impl Drop for RepositoryWatcher {
    fn drop(&mut self) {
        phase14_git_watcher_dropped();
    }
}

pub(super) enum WatchSignal {
    Changed,
    Failed,
}

#[derive(Debug)]
pub(in crate::service) struct GitWatchBootstrap {
    pub(in crate::service) status: v1::GitStatusSnapshot,
    pub(in crate::service) activate: tokio::sync::oneshot::Sender<()>,
}

impl GitService {
    pub(in crate::service) fn watch_cancellable(
        self: &Arc<Self>,
        request: v1::GitRequest,
        sender: mpsc::Sender<SequencerControl>,
        closed: Arc<AtomicBool>,
        cancellation: Arc<AtomicBool>,
    ) -> anyhow::Result<GitWatchBootstrap> {
        if request.watch_id.is_empty() {
            bail!("Git watch ID is required");
        }
        if cancellation.load(Ordering::Acquire) {
            bail!("Git watch bootstrap cancelled");
        }
        let previous = self.watches.lock().unwrap().insert(
            request.watch_id.clone(),
            GitWatch {
                cancelled: Arc::clone(&cancellation),
            },
        );
        if let Some(previous) = previous {
            previous.cancelled.store(true, Ordering::Release);
        } else {
            phase14_git_subscribers(1);
        }
        let bootstrap = (|| {
            let root = capture_request_root(&request)?;
            let stable_root = root.stable_path();
            let repository = discover_repository(
                &stable_root,
                &request.root,
                root.identity()?,
                Some(&cancellation),
            )?;
            validate_repository_id(&request, &repository)?;
            let metadata = Arc::new(GitMetadataCapability::capture(
                &repository.git_dir,
                &repository.common_dir,
            )?);
            validate_metadata_capability(&request.root, root.identity()?, &repository, &metadata)?;
            let refresh_root = Arc::new(root.try_clone()?);
            let _metadata_guard = metadata.install();
            // Establish observation before the authoritative bootstrap read.
            // Any changes racing the read remain queued and are either folded
            // into a bootstrap resnapshot or emitted after activation.
            let watcher_root = root.try_clone()?;
            let (mut native_watcher, mut refresh_rx, watcher_failed) =
                match start_repository_watcher(&request, watcher_root, metadata.try_clone()?) {
                    Ok((watcher, receiver, failed)) => (Some(watcher), Some(receiver), failed),
                    Err(_) => (None, None, Arc::new(AtomicBool::new(true))),
                };
            let mut initial = self.status_with_repository(
                &request,
                &stable_root,
                repository.clone(),
                Some(&cancellation),
            )?;
            for _ in 0..4 {
                if cancellation.load(Ordering::Acquire) {
                    bail!("Git watch bootstrap cancelled");
                }
                let changed = refresh_rx
                    .as_mut()
                    .is_some_and(|receiver| receiver.try_recv().is_ok());
                if !changed && !watcher_failed.load(Ordering::Acquire) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(75));
                if let Some(receiver) = refresh_rx.as_mut() {
                    while receiver.try_recv().is_ok() {}
                }
                initial = self.status_with_repository(
                    &request,
                    &stable_root,
                    repository.clone(),
                    Some(&cancellation),
                )?;
                if watcher_failed.load(Ordering::Acquire) {
                    native_watcher = None;
                    refresh_rx = None;
                    break;
                }
            }
            Ok::<_, anyhow::Error>((
                initial,
                native_watcher,
                refresh_rx,
                watcher_failed,
                refresh_root,
                Arc::clone(&metadata),
                repository,
            ))
        })();
        let (
            initial,
            native_watcher,
            mut refresh_rx,
            watcher_failed,
            refresh_root,
            metadata,
            repository,
        ) = match bootstrap {
            Ok(value) => value,
            Err(error) => {
                self.remove_watch_if(&request.watch_id, &cancellation);
                return Err(error);
            }
        };
        if cancellation.load(Ordering::Acquire) {
            self.remove_watch_if(&request.watch_id, &cancellation);
            bail!("Git watch bootstrap cancelled");
        }
        let service = Arc::clone(self);
        let mut source_generation = initial.source_generation.clone();
        let mut last_error = String::new();
        let (activate, activated) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut native_watcher = native_watcher;
            if activated.await.is_err() {
                cancellation.store(true, Ordering::Release);
                service.remove_watch_if(&request.watch_id, &cancellation);
                return;
            }
            while !closed.load(Ordering::Acquire) && !cancellation.load(Ordering::Acquire) {
                if watcher_failed.swap(false, Ordering::AcqRel) {
                    native_watcher = None;
                    refresh_rx = None;
                }
                let native_failed = if let Some(receiver) = refresh_rx.as_mut() {
                    tokio::select! {
                        signal = receiver.recv() => {
                            let failed = !matches!(signal, Some(WatchSignal::Changed));
                            tokio::time::sleep(Duration::from_millis(75)).await;
                            while let Ok(signal) = receiver.try_recv() {
                                if matches!(signal, WatchSignal::Failed) {
                                    watcher_failed.store(true, Ordering::Release);
                                }
                            }
                            failed
                        }
                        _ = tokio::time::sleep(Duration::from_millis(250)) => continue,
                    }
                } else {
                    // Poll only when the platform-native watcher could not be
                    // established or was lost.
                    tokio::time::sleep(Duration::from_millis(750)).await;
                    false
                };
                if native_failed || watcher_failed.swap(false, Ordering::AcqRel) {
                    native_watcher = None;
                    refresh_rx = None;
                }
                if closed.load(Ordering::Acquire) || cancellation.load(Ordering::Acquire) {
                    break;
                }
                let refresh_service = Arc::clone(&service);
                let refresh_request = request.clone();
                let refresh_cancelled = Arc::clone(&cancellation);
                let refresh_root = Arc::clone(&refresh_root);
                let refresh_metadata = Arc::clone(&metadata);
                let refresh_repository = repository.clone();
                let refreshed = tokio::task::spawn_blocking(move || {
                    let _metadata_guard = refresh_metadata.install();
                    refresh_service.status_with_repository(
                        &refresh_request,
                        &refresh_root.stable_path(),
                        refresh_repository,
                        Some(&refresh_cancelled),
                    )
                })
                .await;
                let event = match refreshed {
                    Ok(result) => watch_refresh_event(
                        &request,
                        &mut source_generation,
                        &mut last_error,
                        result,
                    ),
                    Err(error) => coalesced_watch_error(
                        &request,
                        &mut last_error,
                        format!("Git refresh task failed: {error}"),
                    ),
                };
                if let Some(git) = event
                    && sender
                        .send(SequencerControl::OrderedEvent(v1::HostEvent {
                            kind: v1::EventKind::GitStatus.into(),
                            scope: request.root.clone(),
                            git: Some(git),
                            ..Default::default()
                        }))
                        .await
                        .is_err()
                {
                    break;
                }
            }
            drop(native_watcher);
            service.remove_watch_if(&request.watch_id, &cancellation);
        });
        Ok(GitWatchBootstrap {
            status: initial,
            activate,
        })
    }

    #[cfg(test)]
    pub(super) fn watch(
        self: &Arc<Self>,
        request: v1::GitRequest,
        sender: mpsc::Sender<SequencerControl>,
        closed: Arc<AtomicBool>,
    ) -> anyhow::Result<v1::GitStatusSnapshot> {
        let bootstrap =
            self.watch_cancellable(request, sender, closed, Arc::new(AtomicBool::new(false)))?;
        let _ = bootstrap.activate.send(());
        Ok(bootstrap.status)
    }

    fn remove_watch_if(&self, watch_id: &str, cancellation: &Arc<AtomicBool>) {
        let mut watches = self.watches.lock().unwrap();
        if watches
            .get(watch_id)
            .is_some_and(|watch| Arc::ptr_eq(&watch.cancelled, cancellation))
        {
            watches.remove(watch_id);
            phase14_git_subscribers(-1);
        }
    }

    pub(in crate::service) fn unwatch(&self, watch_id: &str) -> anyhow::Result<()> {
        let watch = self
            .watches
            .lock()
            .unwrap()
            .remove(watch_id)
            .ok_or_else(|| anyhow::anyhow!("unknown Git watch ID"))?;
        phase14_git_subscribers(-1);
        watch.cancelled.store(true, Ordering::Release);
        Ok(())
    }
}

pub(super) fn start_repository_watcher(
    request: &v1::GitRequest,
    root: WorktreeRoot,
    metadata: GitMetadataCapability,
) -> anyhow::Result<(
    RepositoryWatcher,
    tokio::sync::mpsc::Receiver<WatchSignal>,
    Arc<AtomicBool>,
)> {
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    root.validate_token(&request.root, &request.root_token)?;
    let root_identity = root.identity()?;
    let failed = Arc::new(AtomicBool::new(false));
    let callback_failed = Arc::clone(&failed);
    let mut watcher =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
            Ok(_) => {
                let _ = sender.try_send(WatchSignal::Changed);
            }
            Err(_) => {
                callback_failed.store(true, Ordering::Release);
                let _ = sender.try_send(WatchSignal::Failed);
            }
        })?;
    watcher.watch(&root.watch_path(), RecursiveMode::Recursive)?;
    let (git_dir, common_dir) = metadata.stable_paths();
    for directory in [&git_dir, &common_dir] {
        watcher.watch(Path::new(directory), RecursiveMode::Recursive)?;
    }
    let current = WorktreeRoot::capture(&request.root)?;
    current.validate_token(&request.root, &request.root_token)?;
    if current.identity()? != root_identity {
        bail!("repository root changed while establishing Git watch");
    }
    phase14_git_watcher_created();
    Ok((
        RepositoryWatcher {
            _watcher: watcher,
            _root: root,
            _metadata: metadata,
        },
        receiver,
        failed,
    ))
}

impl Drop for GitService {
    fn drop(&mut self) {
        let watches = self.watches.get_mut().unwrap();
        let count = watches.len();
        for (_, watch) in watches.drain() {
            watch.cancelled.store(true, Ordering::Release);
        }
        phase14_git_subscribers(-(count as isize));
    }
}

fn git_watch_error(request: &v1::GitRequest, error: String) -> v1::GitEvent {
    v1::GitEvent {
        watch_id: request.watch_id.clone(),
        root_token: request.root_token.clone(),
        error,
        ..Default::default()
    }
}

fn coalesced_watch_error(
    request: &v1::GitRequest,
    previous: &mut String,
    error: String,
) -> Option<v1::GitEvent> {
    if *previous == error {
        None
    } else {
        *previous = error.clone();
        Some(git_watch_error(request, error))
    }
}

pub(super) fn watch_refresh_event(
    request: &v1::GitRequest,
    source_generation: &mut String,
    last_error: &mut String,
    refreshed: anyhow::Result<v1::GitStatusSnapshot>,
) -> Option<v1::GitEvent> {
    match refreshed {
        Ok(status) if status.source_generation != *source_generation => {
            *source_generation = status.source_generation.clone();
            last_error.clear();
            Some(v1::GitEvent {
                watch_id: request.watch_id.clone(),
                root_token: request.root_token.clone(),
                status: Some(status),
                ..Default::default()
            })
        }
        Ok(_) => {
            last_error.clear();
            None
        }
        Err(error) if error.is::<StatusRefreshSuperseded>() => None,
        Err(error) => coalesced_watch_error(request, last_error, error.to_string()),
    }
}
