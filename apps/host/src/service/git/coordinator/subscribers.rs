//! Subscriber multiplexing for one repository coordinator.
//!
//! Every consumer that asks to watch a repository lands here. They share one
//! native watcher and one status pipeline; this module is only responsible for
//! who receives the resulting snapshot, and for holding a brand-new
//! subscriber's events until its own bootstrap response has been enqueued.

use super::*;

/// Consumers one repository coordinator will multiplex at once.
///
/// The plan's own target is 32 consumers of one repository sharing one native
/// watcher, so this is well clear of the shape being optimized for. It exists
/// because the key is client-supplied: it bounds a client that opens watches
/// and never releases them, which is exactly what `MAX_WATCHES` bounds on the
/// filesystem side of the same round.
const MAX_SUBSCRIBERS: usize = 128;

/// One watching consumer. The snapshot is shared; the envelope is not, because
/// each consumer identifies its events by its own watch id and root token.
struct Subscriber {
    sender: mpsc::Sender<SequencerControl>,
    root_token: String,
    scope: String,
    /// Events are withheld until the `WatchGit` response carrying the bootstrap
    /// snapshot has been enqueued, so a consumer can never see a refresh for a
    /// watch it has not been told about yet.
    activated: bool,
    /// At most one withheld event: snapshots and errors are absolute states, so
    /// the newest publication supersedes anything still waiting.
    deferred: Option<v1::GitEvent>,
    /// The source generation this subscriber already received in its bootstrap
    /// response. Re-delivering it as an event would be a second, identical
    /// transition for a consumer that has not seen anything yet.
    bootstrap_source: String,
    /// Distinguishes this registration from any later one reusing its watch id,
    /// so a cancelled bootstrap can only ever cancel its own.
    slot: u64,
}

#[derive(Default)]
pub(super) struct SubscriberRegistry {
    entries: HashMap<String, Subscriber>,
    next_slot: u64,
}

impl SubscriberRegistry {
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Activates or cancels one registered subscription exactly once.
///
/// Dropping this without calling `activate` removes the subscription, so a
/// bootstrap whose response was never sent leaves nothing behind.
pub(in crate::service) struct SubscriberActivation {
    coordinator: Arc<RepositoryCoordinator>,
    watch_id: String,
    slot: u64,
    settled: bool,
}

impl SubscriberActivation {
    pub(in crate::service::git) fn new(
        coordinator: Arc<RepositoryCoordinator>,
        watch_id: String,
        slot: u64,
    ) -> Self {
        Self {
            coordinator,
            watch_id,
            slot,
            settled: false,
        }
    }

    pub(in crate::service) fn activate(mut self) {
        self.settled = true;
        let coordinator = Arc::clone(&self.coordinator);
        let watch_id = std::mem::take(&mut self.watch_id);
        let slot = self.slot;
        tokio::spawn(async move { coordinator.flush_deferred(&watch_id, slot).await });
    }
}

impl Drop for SubscriberActivation {
    fn drop(&mut self) {
        if !self.settled {
            self.coordinator.unsubscribe_slot(&self.watch_id, self.slot);
        }
    }
}

impl RepositoryCoordinator {
    /// Registers one consumer. The first registration starts the repository's
    /// only native watcher; every later one simply joins it.
    pub(in crate::service::git) fn subscribe(
        self: &Arc<Self>,
        request: &v1::GitRequest,
        sender: mpsc::Sender<SequencerControl>,
    ) -> anyhow::Result<SubscriberActivation> {
        if request.watch_id.is_empty() {
            bail!("Git watch ID is required");
        }
        let slot = {
            let mut subscribers = self.subscribers.lock().unwrap();
            // The watch ID is the client's, so without a cap a client that
            // opens watches and never closes them grows this map forever — and
            // a coordinator with any subscriber is never evicted, so it pins
            // its descriptors, its cached status and its native watcher with
            // it. The filesystem side of this same round refuses past
            // `MAX_WATCHES`; this is the same rule for the same reason.
            if !subscribers.entries.contains_key(&request.watch_id)
                && subscribers.entries.len() >= MAX_SUBSCRIBERS
            {
                bail!("Git watch limit of {MAX_SUBSCRIBERS} consumers reached");
            }
            subscribers.next_slot += 1;
            let slot = subscribers.next_slot;
            let replaced = subscribers.entries.insert(
                request.watch_id.clone(),
                Subscriber {
                    sender,
                    root_token: request.root_token.clone(),
                    scope: request.root.clone(),
                    activated: false,
                    deferred: None,
                    bootstrap_source: String::new(),
                    slot,
                },
            );
            if replaced.is_none() {
                self.observation.subscribers_changed(1);
            }
            slot
        };
        Ok(SubscriberActivation::new(
            Arc::clone(self),
            request.watch_id.clone(),
            slot,
        ))
    }

    /// Removes one consumer, stopping the shared watcher with the last of them.
    pub(in crate::service::git) fn unsubscribe(self: &Arc<Self>, watch_id: &str) -> bool {
        self.remove_subscriber(watch_id, None)
    }

    /// Removes one consumer only if it is still the registration identified by
    /// `slot`, so an abandoned bootstrap cannot cancel the watch that replaced
    /// it.
    fn unsubscribe_slot(self: &Arc<Self>, watch_id: &str, slot: u64) -> bool {
        self.remove_subscriber(watch_id, Some(slot))
    }

    fn remove_subscriber(self: &Arc<Self>, watch_id: &str, slot: Option<u64>) -> bool {
        let (removed, remaining) = {
            let mut subscribers = self.subscribers.lock().unwrap();
            let matches = subscribers
                .entries
                .get(watch_id)
                .is_some_and(|subscriber| slot.is_none_or(|slot| subscriber.slot == slot));
            let removed = matches && subscribers.entries.remove(watch_id).is_some();
            if removed {
                self.observation.subscribers_changed(-1);
            }
            (removed, subscribers.entries.len())
        };
        if remaining == 0 {
            self.stop_watcher();
        }
        removed
    }

    /// Records what the bootstrap response already told this subscriber.
    pub(in crate::service::git) fn record_bootstrap(
        &self,
        watch_id: &str,
        snapshot: &v1::GitStatusSnapshot,
    ) {
        let mut subscribers = self.subscribers.lock().unwrap();
        if let Some(subscriber) = subscribers.entries.get_mut(watch_id) {
            subscriber.bootstrap_source = snapshot.source_generation.clone();
        }
    }

    /// Connection teardown. Every subscription goes, so the shared watcher and
    /// its task go with them.
    pub(in crate::service::git) fn drop_all_subscribers(&self) {
        let mut subscribers = self.subscribers.lock().unwrap();
        self.observation
            .subscribers_changed(-(subscribers.entries.len() as isize));
        subscribers.entries.clear();
    }

    pub(in crate::service::git) fn subscriber_count(&self) -> usize {
        self.subscribers.lock().unwrap().len()
    }

    /// Releases what one newly activated subscriber missed.
    ///
    /// Held under the publication guard, so a withheld snapshot cannot overtake
    /// or be overtaken by a fan-out that is already running: this subscriber
    /// either receives the deferred event first and the live one after, or is
    /// activated in time to receive the live one directly.
    async fn flush_deferred(self: &Arc<Self>, watch_id: &str, slot: u64) {
        let _ordered = self.publish.lock().await;
        let deferred = {
            let mut subscribers = self.subscribers.lock().unwrap();
            let Some(subscriber) = subscribers
                .entries
                .get_mut(watch_id)
                .filter(|subscriber| subscriber.slot == slot)
            else {
                return;
            };
            subscriber.activated = true;
            subscriber
                .deferred
                .take()
                .filter(|event| {
                    event.status.as_ref().is_none_or(|status| {
                        status.source_generation != subscriber.bootstrap_source
                    })
                })
                .map(|event| (subscriber.sender.clone(), subscriber.scope.clone(), event))
        };
        if let Some((sender, scope, event)) = deferred
            && sender
                .send(SequencerControl::OrderedEvent(git_status_event(
                    scope, event,
                )))
                .await
                .is_err()
        {
            self.unsubscribe(watch_id);
        }
    }

    /// Fans one authoritative snapshot out to every consumer, at most once per
    /// distinct repository state.
    ///
    /// `sequence` is the order its pipeline ran in. Publications are serialized
    /// on that rather than on the pipeline lock, so a stalled consumer delays
    /// only publication and never the repository's status pipeline — nor,
    /// through a mutation, the process-global repository lock.
    pub(in crate::service::git) async fn publish_status(
        self: &Arc<Self>,
        snapshot: &Arc<v1::GitStatusSnapshot>,
        sequence: u64,
    ) {
        let mut ordered = self.publish.lock().await;
        if !claim_publication(
            &mut ordered,
            &self.published,
            sequence,
            Publication::Status(snapshot.source_generation.clone()),
        ) {
            return;
        }
        self.fan_out(|watch_id, root_token| v1::GitEvent {
            watch_id: watch_id.to_owned(),
            root_token: root_token.to_owned(),
            status: Some((**snapshot).clone()),
            ..Default::default()
        })
        .await;
    }

    /// Reports a refresh failure once, not once per failing cycle.
    pub(in crate::service::git) async fn publish_error(
        self: &Arc<Self>,
        error: String,
        sequence: u64,
    ) {
        let mut ordered = self.publish.lock().await;
        if !claim_publication(
            &mut ordered,
            &self.published,
            sequence,
            Publication::Error(error.clone()),
        ) {
            return;
        }
        self.fan_out(|watch_id, root_token| v1::GitEvent {
            watch_id: watch_id.to_owned(),
            root_token: root_token.to_owned(),
            error: error.clone(),
            ..Default::default()
        })
        .await;
    }

    async fn fan_out(self: &Arc<Self>, event: impl Fn(&str, &str) -> v1::GitEvent) {
        let mut deliveries = Vec::new();
        {
            let mut subscribers = self.subscribers.lock().unwrap();
            for (watch_id, subscriber) in subscribers.entries.iter_mut() {
                let built = event(watch_id, &subscriber.root_token);
                if subscriber.activated {
                    deliveries.push((
                        watch_id.clone(),
                        subscriber.sender.clone(),
                        subscriber.scope.clone(),
                        built,
                    ));
                } else {
                    subscriber.deferred = Some(built);
                }
            }
        }
        for (watch_id, sender, scope, event) in deliveries {
            if sender
                .send(SequencerControl::OrderedEvent(git_status_event(
                    scope, event,
                )))
                .await
                .is_err()
            {
                self.unsubscribe(&watch_id);
            }
        }
    }
}

/// Whether this publication is both newer than the last and different from it.
///
/// The caller keeps the ordering guard across its fan-out, which is what makes
/// delivery order match pipeline order: a slower older publication cannot
/// interleave with a faster newer one.
fn claim_publication(
    ordered: &mut u64,
    published: &Mutex<Publication>,
    sequence: u64,
    next: Publication,
) -> bool {
    if sequence <= *ordered {
        return false;
    }
    *ordered = sequence;
    let mut published = published.lock().unwrap();
    if *published == next {
        return false;
    }
    *published = next;
    true
}

fn git_status_event(scope: String, git: v1::GitEvent) -> v1::HostEvent {
    v1::HostEvent {
        kind: v1::EventKind::GitStatus.into(),
        scope,
        git: Some(git),
        ..Default::default()
    }
}
