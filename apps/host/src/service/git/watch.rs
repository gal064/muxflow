//! The `WatchGit` / `UnwatchGit` request surface.
//!
//! A watch is a subscription to a repository coordinator, not a watcher of its
//! own. The first subscriber pays for discovery, the native watcher and one
//! status pipeline; every later one pays for nothing.

use super::*;

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
        let (coordinator, capabilities) = self
            .repository(&request, Some(Arc::clone(&cancellation)))
            .await?;
        // Registered before the bootstrap read so a change racing that read is
        // either folded into it or delivered afterwards, never dropped.
        let activate = coordinator.subscribe(&request, sender)?;
        self.rebind_watch(&request.watch_id, &coordinator);
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

    /// Points a watch id at its coordinator, retiring an earlier binding.
    ///
    /// Re-registering the same id on the same coordinator replaces the entry in
    /// place, so a reconnecting consumer never briefly drops the subscriber
    /// count to zero and tears the shared watcher down underneath its peers.
    fn rebind_watch(&self, watch_id: &str, coordinator: &Arc<RepositoryCoordinator>) {
        let previous = {
            let mut watches = self.watches.lock().unwrap();
            watches.insert(watch_id.to_owned(), Arc::clone(coordinator))
        };
        if let Some(previous) = previous
            && !Arc::ptr_eq(&previous, coordinator)
        {
            previous.unsubscribe(watch_id);
            self.retire(&previous);
        }
    }

    fn release_watch(&self, watch_id: &str) -> bool {
        let coordinator = self.watches.lock().unwrap().remove(watch_id);
        let Some(coordinator) = coordinator else {
            return false;
        };
        let removed = coordinator.unsubscribe(watch_id);
        self.retire(&coordinator);
        removed
    }
}
