use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
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
}

/// A claimed operation ID. Dropping it releases the ID for reuse.
pub(super) struct OperationClaim {
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
            return Bound::Ready;
        };
        if slot.cancelled {
            return Bound::Cancelled;
        }
        slot.request_id = Some(request_id);
        Bound::Ready
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
            if let Some(request_id) = slot.request_id {
                return Some(request_id);
            }
            slot.cancelled = true;
            return None;
        }
        // Nothing claimed yet. The claim may still be on its way across the
        // command boundary, so the refusal is left here for it to find.
        if slots.len() >= MAX_TOMBSTONES {
            let oldest: Vec<_> = slots
                .iter()
                .filter(|(_, slot)| slot.cancelled && slot.request_id.is_none())
                .map(|(key, _)| key.clone())
                .collect();
            for key in oldest {
                slots.remove(&key);
            }
        }
        slots.insert(
            key,
            Slot {
                request_id: None,
                cancelled: true,
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
