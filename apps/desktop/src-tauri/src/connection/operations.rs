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
    pub(super) fn claim(
        self: &Arc<Self>,
        lane: OperationLane,
        operation_id: &str,
    ) -> Result<OperationClaim, String> {
        let key = (lane, operation_id.to_owned());
        let mut slots = self.slots.lock().unwrap();
        if slots.contains_key(&key) {
            return Err(format!("duplicate {} operation ID", lane.label()));
        }
        slots.insert(key.clone(), Slot::default());
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

    /// The request ID to cancel, or `None` when the operation is claimed but
    /// not yet dispatched — in which case a tombstone refuses it instead.
    pub(super) fn cancel(
        &self,
        lane: OperationLane,
        operation_id: &str,
    ) -> Result<Option<u64>, String> {
        let key = (lane, operation_id.to_owned());
        let mut slots = self.slots.lock().unwrap();
        let slot = slots
            .get_mut(&key)
            .ok_or_else(|| format!("unknown or completed {} operation ID", lane.label()))?;
        match slot.request_id {
            Some(request_id) => Ok(Some(request_id)),
            None => {
                slot.cancelled = true;
                Ok(None)
            }
        }
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
        assert_eq!(registry.cancel(OperationLane::File, "op").unwrap(), None);
        assert!(matches!(registry.bind(&claim, 7), Bound::Cancelled));
        drop(claim);
        assert_eq!(registry.len(), 0);
        // The ID is reusable, and no tombstone survives its claim.
        let reused = registry.claim(OperationLane::File, "op").unwrap();
        assert!(matches!(registry.bind(&reused, 8), Bound::Ready));
    }

    #[test]
    fn cancellation_targets_the_exact_lane_and_reports_an_unknown_one() {
        let registry = Arc::new(OperationRegistry::default());
        let claim = registry.claim(OperationLane::Git, "shared-id").unwrap();
        registry.bind(&claim, 11);
        assert!(registry.cancel(OperationLane::File, "shared-id").is_err());
        assert_eq!(
            registry.cancel(OperationLane::Git, "shared-id").unwrap(),
            Some(11)
        );
        assert!(registry.claim(OperationLane::Git, "shared-id").is_err());
        drop(claim);
        assert!(registry.cancel(OperationLane::Git, "shared-id").is_err());
    }
}
