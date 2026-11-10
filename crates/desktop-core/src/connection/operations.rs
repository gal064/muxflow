use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

/// Which operation-id namespace a cancellable request belongs to.
///
/// Two lanes mint operation IDs independently, so a registry keyed by the ID
/// alone could cancel the wrong request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum OperationLane {
    Git,
    File,
}

impl OperationLane {
    fn label(self) -> &'static str {
        match self {
            Self::Git => "Git",
            Self::File => "file",
        }
    }
}

/// The renderer-visible operations one connection can currently cancel.
///
/// One ownership rule for every lane: a request claims its ID before anything
/// is dispatched for it, and the claim is the only thing that removes it. An
/// earlier arrangement where one lane claimed and the other appeared as a side
/// effect of dispatch left the second lane's entries behind forever — one per
/// request, for the life of the connection.
#[derive(Default)]
pub(super) struct OperationRegistry {
    slots: Mutex<HashMap<(OperationLane, String), Slot>>,
}

/// How many cancel-before-claim tombstones one connection retains.
///
/// Operation IDs are UUIDs used once, so a tombstone is normally consumed by
/// the claim it was left for. The bound is only for the case where the claim
/// never arrives at all — an aborted caller that never dispatched.
const MAX_TOMBSTONES: usize = 64;

#[derive(Default)]
struct Slot {
    /// Set once the request has been written, cleared when it completes.
    request_id: Option<u64>,
    /// A cancellation that arrived before the request had an ID. The request is
    /// refused rather than sent: a caller that abandoned its read faster than
    /// the worker thread could register it must not leave remote work running.
    cancelled: bool,
    /// When an *unclaimed* cancellation was recorded, so eviction can drop the
    /// one least likely to still be waiting for its claim.
    tombstoned_at: Option<Instant>,
}

/// A claimed operation ID. Dropping it releases the ID for reuse.
///
/// `pub(crate)` only because `TerminalClient` itself is: the Git diff-body
/// lane is a sibling module, so the client type is reachable crate-wide and a
/// method returning this would otherwise leak a less-visible type. The claim
/// is opaque either way — it has no constructor and no field outside here.
pub(crate) struct OperationClaim {
    registry: Arc<OperationRegistry>,
    key: (OperationLane, String),
}

impl Drop for OperationClaim {
    fn drop(&mut self) {
        self.registry.slots.lock().unwrap().remove(&self.key);
    }
}

/// What a claim's holder learned when it tried to bind a request id to it.
pub(super) enum Bound {
    Ready,
    /// Cancelled before dispatch. The caller must not send the request.
    Cancelled,
}

impl OperationRegistry {
    /// Claims an operation ID before any request is written for it.
    ///
    /// A cancellation can arrive before the claim does: the renderer mints the
    /// ID, issues the request, and may abort in the same tick, and those are two
    /// separate messages across the command boundary. A cancellation that finds
    /// no claim leaves a tombstone, which this consumes — so the request is
    /// refused rather than sent to a host nobody will tell to stop.
    pub(super) fn claim(
        self: &Arc<Self>,
        lane: OperationLane,
        operation_id: &str,
    ) -> Result<OperationClaim, String> {
        let key = (lane, operation_id.to_owned());
        let mut slots = self.slots.lock().unwrap();
        match slots.get(&key) {
            Some(slot) if slot.cancelled && slot.request_id.is_none() => {}
            Some(_) => return Err(format!("duplicate {} operation ID", lane.label())),
            None => {
                slots.insert(key.clone(), Slot::default());
            }
        }
        drop(slots);
        Ok(OperationClaim {
            registry: Arc::clone(self),
            key,
        })
    }

    /// Binds a written request to its claim, unless it was already cancelled.
    pub(super) fn bind(&self, claim: &OperationClaim, request_id: u64) -> Bound {
        let mut slots = self.slots.lock().unwrap();
        let Some(slot) = slots.get_mut(&claim.key) else {
            // The registry was cleared under this claim, which happens when the
            // connection it belongs to goes away. Reporting it ready would
            // dispatch onto a torn-down bridge with nothing left that could
            // ever cancel it.
            return Bound::Cancelled;
        };
        if slot.cancelled {
            return Bound::Cancelled;
        }
        slot.request_id = Some(request_id);
        Bound::Ready
    }

    /// Whether this claim was cancelled at any point since it was made.
    pub(super) fn cancelled(&self, claim: &OperationClaim) -> bool {
        self.slots
            .lock()
            .unwrap()
            .get(&claim.key)
            .is_some_and(|slot| slot.cancelled)
    }

    /// Marks a claim's request as finished while the claim itself lives on, so
    /// a cancellation racing completion still finds somewhere to land.
    pub(super) fn unbind(&self, claim: &OperationClaim) {
        if let Some(slot) = self.slots.lock().unwrap().get_mut(&claim.key) {
            slot.request_id = None;
        }
    }

    /// The request ID to cancel, or `None` when the operation has no request in
    /// flight — in which case a tombstone refuses the one that is coming.
    pub(super) fn cancel(&self, lane: OperationLane, operation_id: &str) -> Option<u64> {
        let key = (lane, operation_id.to_owned());
        let mut slots = self.slots.lock().unwrap();
        if let Some(slot) = slots.get_mut(&key) {
            // Recorded whether or not the request has an ID yet. A dispatcher
            // that has just finished writing re-reads this, because a cancel
            // written *between* the bind and the write reaches the host before
            // the request it names — and a cancel for a request the host has
            // never seen is discarded.
            slot.cancelled = true;
            return slot.request_id;
        }
        // Nothing claimed yet. The claim may still be on its way across the
        // command boundary, so the refusal is left here for it to find.
        //
        // Bounded per lane, by age, and counting only tombstones. Each of those
        // three is a defect on its own: a whole-map sweep threw away the
        // tombstones whose claims were still in flight — the entire case the
        // mechanism exists for; counting live claims meant sixty-four
        // concurrent operations disabled eviction altogether; and one budget
        // across both lanes let a Git cancellation evict a file read's refusal,
        // after which that read dispatched to a host nobody would tell to stop.
        let mine = |candidate: &(OperationLane, String)| candidate.0 == lane;
        let tombstones = slots
            .iter()
            .filter(|(candidate, slot)| mine(candidate) && slot.tombstoned_at.is_some())
            .count();
        if tombstones >= MAX_TOMBSTONES {
            let oldest = slots
                .iter()
                .filter(|(candidate, _)| mine(candidate))
                .filter_map(|(candidate, slot)| {
                    slot.tombstoned_at.map(|at| (at, candidate.clone()))
                })
                .min_by_key(|(at, _)| *at)
                .map(|(_, candidate)| candidate);
            if let Some(oldest) = oldest {
                slots.remove(&oldest);
            }
        }
        slots.insert(
            key,
            Slot {
                request_id: None,
                cancelled: true,
                tombstoned_at: Some(Instant::now()),
            },
        );
        None
    }

    pub(super) fn clear(&self) {
        self.slots.lock().unwrap().clear();
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.slots.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_completed_request_leaves_nothing_behind_on_either_lane() {
        let registry = Arc::new(OperationRegistry::default());
        for lane in [OperationLane::Git, OperationLane::File] {
            for round in 0..64 {
                let claim = registry.claim(lane, &format!("op-{round}")).unwrap();
                assert!(matches!(registry.bind(&claim, 100 + round), Bound::Ready));
                registry.unbind(&claim);
                drop(claim);
            }
            // One entry per request, retained for the life of the connection,
            // is what a second ownership rule for one map produced.
            assert_eq!(registry.len(), 0, "{} operations leaked", lane.label());
        }
    }

    #[test]
    fn a_cancellation_that_beats_dispatch_refuses_the_request() {
        let registry = Arc::new(OperationRegistry::default());
        let claim = registry.claim(OperationLane::File, "op").unwrap();
        assert_eq!(registry.cancel(OperationLane::File, "op"), None);
        assert!(matches!(registry.bind(&claim, 7), Bound::Cancelled));
        drop(claim);
        assert_eq!(registry.len(), 0);
        // The ID is reusable, and no tombstone survives its claim.
        let reused = registry.claim(OperationLane::File, "op").unwrap();
        assert!(matches!(registry.bind(&reused, 8), Bound::Ready));
    }

    /// The abort and the request are two separate messages across the command
    /// boundary, so the abort can genuinely arrive first. It must still refuse
    /// the request rather than let a remote scan run for nobody.
    #[test]
    fn a_cancellation_that_arrives_before_the_claim_still_refuses_it() {
        let registry = Arc::new(OperationRegistry::default());
        assert_eq!(registry.cancel(OperationLane::File, "not-yet"), None);
        let claim = registry.claim(OperationLane::File, "not-yet").unwrap();
        assert!(matches!(registry.bind(&claim, 3), Bound::Cancelled));
        drop(claim);
        assert_eq!(registry.len(), 0);
        // Tombstones are bounded even when their claim never arrives.
        for index in 0..MAX_TOMBSTONES * 3 {
            registry.cancel(OperationLane::File, &format!("abandoned-{index}"));
        }
        assert!(registry.len() <= MAX_TOMBSTONES);
        // The most recent survive — those are the ones whose claim may still be
        // crossing the command boundary, which is the whole point of keeping
        // any — and eviction is oldest-first rather than a whole-map sweep.
        for index in (MAX_TOMBSTONES * 2)..(MAX_TOMBSTONES * 3) {
            let recent = format!("abandoned-{index}");
            let refused = registry.claim(OperationLane::File, &recent).unwrap();
            assert!(
                matches!(registry.bind(&refused, 9), Bound::Cancelled),
                "{recent} lost its refusal to a newer one"
            );
        }
    }

    /// One lane's cancellations must not evict another's.
    ///
    /// A file read's refusal thrown away by an unrelated Git cancellation
    /// dispatches that read to a host nobody will tell to stop — which is the
    /// exact failure the tombstone exists to prevent.
    #[test]
    fn a_lane_cannot_evict_the_other_lanes_cancellations() {
        let registry = Arc::new(OperationRegistry::default());
        registry.cancel(OperationLane::File, "abandoned-read");
        for index in 0..MAX_TOMBSTONES * 2 {
            registry.cancel(OperationLane::Git, &format!("git-{index}"));
        }
        let claim = registry
            .claim(OperationLane::File, "abandoned-read")
            .unwrap();
        assert!(
            matches!(registry.bind(&claim, 4), Bound::Cancelled),
            "a Git cancellation evicted a file read's refusal"
        );
    }

    /// A cancellation raised after the bind is still visible to the dispatcher.
    ///
    /// The window this closes: `bind` hands the registry a request ID, a cancel
    /// arrives and writes its `Cancel` before the request itself reaches the
    /// wire, and the host discards a cancel for a request it has never seen.
    /// The dispatcher re-reads this on the far side of its write and re-sends,
    /// so the cancellation cannot be lost — without serializing every dispatch
    /// on this connection behind one lock.
    #[test]
    fn a_cancellation_raised_after_dispatch_is_still_visible_to_the_dispatcher() {
        let registry = Arc::new(OperationRegistry::default());
        let claim = registry.claim(OperationLane::File, "op").unwrap();
        assert!(matches!(registry.bind(&claim, 11), Bound::Ready));
        assert!(!registry.cancelled(&claim), "nothing has cancelled it yet");

        assert_eq!(registry.cancel(OperationLane::File, "op"), Some(11));

        assert!(
            registry.cancelled(&claim),
            "a cancellation racing the write was invisible to the dispatcher"
        );
        drop(claim);
    }

    /// A cleared registry is a connection that has gone.
    #[test]
    fn a_claim_whose_registry_was_cleared_is_refused_rather_than_dispatched() {
        let registry = Arc::new(OperationRegistry::default());
        let claim = registry.claim(OperationLane::File, "op").unwrap();
        registry.clear();
        assert!(
            matches!(registry.bind(&claim, 5), Bound::Cancelled),
            "a request was dispatched onto a connection nothing can cancel it on"
        );
        drop(claim);
    }

    #[test]
    fn cancellation_targets_the_exact_lane_and_leaves_the_other_alone() {
        let registry = Arc::new(OperationRegistry::default());
        let claim = registry.claim(OperationLane::Git, "shared-id").unwrap();
        registry.bind(&claim, 11);
        // A cancel on the other lane's identically named operation must not
        // reach this one; it leaves its own tombstone instead.
        assert_eq!(registry.cancel(OperationLane::File, "shared-id"), None);
        assert_eq!(registry.cancel(OperationLane::Git, "shared-id"), Some(11));
        assert!(registry.claim(OperationLane::Git, "shared-id").is_err());
        drop(claim);
    }
}
