//! The `WatchGit` / `UnwatchGit` request surface.
//!
//! A watch is a subscription to a repository coordinator, not a watcher of its
//! own. The first subscriber pays for discovery, the native watcher and one
//! status pipeline; every later one pays for nothing.

use super::*;

/// How many times a bootstrap re-resolves its repository before giving up.
/// Losing this race requires the registry to replace an entry mid-registration;
/// losing it repeatedly is a caller that should back off rather than spin.
const MAX_REGISTRATION_ATTEMPTS: usize = 4;

pub(in crate::service) struct GitWatchBootstrap {
    pub(in crate::service) status: v1::GitStatusSnapshot,
    /// Withholds this subscription's events until its bootstrap response has
    /// been enqueued. Dropping it instead cancels the subscription.
    pub(in crate::service) activate: SubscriberActivation,
}

impl std::fmt::Debug for GitWatchBootstrap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitWatchBootstrap")
            .field("generation", &self.status.generation)
            .finish()
    }
}

impl GitService {
    pub(in crate::service) async fn watch(
        &self,
        request: v1::GitRequest,
        sender: mpsc::Sender<SequencerControl>,
        cancellation: Arc<AtomicBool>,
    ) -> anyhow::Result<GitWatchBootstrap> {
        if request.watch_id.is_empty() {
            bail!("Git watch ID is required");
        }
        if cancellation.load(Ordering::Acquire) {
            bail!("Git watch bootstrap cancelled");
        }
        // Registered before the bootstrap read so a change racing that read is
        // either folded into it or delivered afterwards, never dropped.
        // Re-registering on the same coordinator replaces the entry in place,
        // so a reconnecting consumer never drops the subscriber count to zero
        // and tears the shared watcher down underneath its peers.
        let mut attempts = 0;
        let (coordinator, capabilities, activate) = loop {
            let (coordinator, capabilities) = self
                .repository(&request, Some(Arc::clone(&cancellation)))
                .await?;
            if let Some(activate) =
                self.subscribe_canonical(&coordinator, &request, sender.clone())?
            {
                break (coordinator, capabilities, activate);
            }
            attempts += 1;
            if attempts >= MAX_REGISTRATION_ATTEMPTS {
                bail!("Git repository registration kept being superseded; retry the watch");
            }
        };
        self.release_watch_elsewhere(&request.watch_id, Some(&coordinator));
        coordinator.ensure_watcher(&capabilities).await;
        let status = match coordinator
            .status(
                &capabilities,
                Freshness::Coalesced,
                Some(Arc::clone(&cancellation)),
            )
            .await
        {
            Ok(status) => status,
            Err(error) => {
                self.release_watch(&request.watch_id);
                return Err(error);
            }
        };
        if cancellation.load(Ordering::Acquire) {
            self.release_watch(&request.watch_id);
            bail!("Git watch bootstrap cancelled");
        }
        coordinator.record_bootstrap(&request.watch_id, &status);
        Ok(GitWatchBootstrap {
            status: (*status).clone(),
            activate,
        })
    }

    /// Subscribes, but only to the coordinator the registry still recognises.
    ///
    /// A coordinator with no subscribers can be evicted by any other request
    /// between discovery and registration. Subscribing to it after that would
    /// leave a watch on a coordinator nothing can reach: a second consumer of
    /// the same repository would build its own watcher, and this watch id could
    /// never be released. Returns `None` when some other coordinator has since
    /// become the registry's entry, which is the caller's signal to start over.
    fn subscribe_canonical(
        &self,
        coordinator: &Arc<RepositoryCoordinator>,
        request: &v1::GitRequest,
        sender: mpsc::Sender<SequencerControl>,
    ) -> anyhow::Result<Option<SubscriberActivation>> {
        let mut repositories = self.repositories.lock().unwrap();
        match repositories.get(coordinator.key()) {
            Some(current) if !Arc::ptr_eq(current, coordinator) => return Ok(None),
            Some(_) => {}
            // Evicted while unwatched. It is about to be watched, so it becomes
            // the registry's entry again rather than being abandoned with a
            // subscriber on it.
            None => {
                repositories.insert(coordinator.key().clone(), Arc::clone(coordinator));
            }
        }
        coordinator.subscribe(request, sender).map(Some)
    }

    /// Subscribes and immediately activates, which is what the dispatcher does
    /// once the bootstrap response is enqueued.
    #[cfg(test)]
    pub(super) async fn watch_activated(
        &self,
        request: v1::GitRequest,
        sender: mpsc::Sender<SequencerControl>,
    ) -> anyhow::Result<v1::GitStatusSnapshot> {
        let bootstrap = self
            .watch(request, sender, Arc::new(AtomicBool::new(false)))
            .await?;
        bootstrap.activate.activate();
        Ok(bootstrap.status)
    }

    pub(in crate::service) fn unwatch(&self, watch_id: &str) -> anyhow::Result<()> {
        if !self.release_watch(watch_id) {
            bail!("unknown Git watch ID");
        }
        Ok(())
    }

    /// Removes a watch id wherever it is registered.
    ///
    /// The subscriber registries are the only record of who is watching what;
    /// a second index keyed by client-supplied watch ids would be a duplicate
    /// of that fact, and every path that forgot to update it would leak. There
    /// are at most `MAX_TRACKED_REPOSITORIES` coordinators to ask.
    pub(in crate::service::git) fn release_watch(&self, watch_id: &str) -> bool {
        self.release_watch_elsewhere(watch_id, None)
    }

    /// Releases a watch id from every coordinator except one.
    ///
    /// `retained` is the coordinator that has just taken ownership of the id,
    /// which must not have its brand-new subscription removed by the same pass.
    fn release_watch_elsewhere(
        &self,
        watch_id: &str,
        retained: Option<&Arc<RepositoryCoordinator>>,
    ) -> bool {
        let coordinators: Vec<_> = self
            .repositories
            .lock()
            .unwrap()
            .values()
            .map(Arc::clone)
            .collect();
        let mut removed = false;
        for coordinator in coordinators {
            if retained.is_some_and(|kept| Arc::ptr_eq(kept, &coordinator)) {
                continue;
            }
            // Unsubscribing stops the shared watcher when the last consumer
            // leaves, but the discovered identity, metadata capability and
            // last status stay: reopening the panel must not re-run discovery.
            // The bounded LRU decides when a repository is actually forgotten.
            removed |= coordinator.unsubscribe(watch_id);
        }
        removed
    }
}
