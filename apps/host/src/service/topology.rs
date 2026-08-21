use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;

use tmux_agent_protocol::v1;
use tmux_control::LayoutGeneration;
use tokio::sync::{Notify, mpsc};
use tokio::time::sleep;

use super::snapshot::{discover_authoritative, snapshot_from_identity};
use super::terminal::TerminalClients;
use super::{SequencerControl, emit_event, reconcile_terminal_clients_if_open};

/// Backstop for a tmux notification the reader never saw.
///
/// tmux notifies on every structural change, and those notifications are what
/// actually drive reconciliation; this timer only exists for the case where one
/// is missed. Running it every two seconds meant a fully idle connection did a
/// tmux discovery and woke every consumer twice a second forever, which is the
/// opposite of "zero periodic round-trips at idle". Thirty seconds is still a
/// backstop and is invisible at rest.
const SAFETY_RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Default)]
pub(super) struct TopologySignal {
    epoch: Arc<AtomicU64>,
    acknowledged_epoch: Arc<AtomicU64>,
    notify: Arc<Notify>,
}

impl TopologySignal {
    pub(super) fn mark_dirty(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.notify.notify_one();
    }

    /// Wakes the actor without dirtying the topology, so a teardown that has
    /// just set `closed` is observed now rather than after the safety
    /// interval. Left asleep, the actor keeps its event-sender clone alive for
    /// up to thirty seconds, which turns every clean disconnect into a
    /// forced writer abort.
    pub(super) fn wake(&self) {
        self.notify.notify_one();
    }

    pub(super) fn observe_event(&self, message: &SequencerControl) {
        if matches!(
            message,
            SequencerControl::OrderedEvent(event)
                if v1::EventKind::try_from(event.kind).unwrap_or_default()
                    == v1::EventKind::TopologyDirty
        ) {
            self.mark_dirty();
        }
    }

    pub(super) fn current_epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    pub(super) fn acknowledge_through(&self, epoch: u64) {
        self.acknowledged_epoch.fetch_max(epoch, Ordering::AcqRel);
    }

    fn acknowledges(&self, epoch: u64) -> bool {
        self.acknowledged_epoch.load(Ordering::Acquire) >= epoch
    }
}

pub(super) struct TopologyActor {
    pub closed: Arc<AtomicBool>,
    pub subscribed: Arc<AtomicBool>,
    pub generation: Arc<AtomicU64>,
    pub overflowed: Arc<AtomicBool>,
    pub lock: Arc<tokio::sync::Mutex<()>>,
    pub baseline: Arc<Mutex<Option<(tmux_control::TmuxSnapshot, String)>>>,
    pub terminal: Arc<Mutex<TerminalClients>>,
    pub sender: mpsc::Sender<SequencerControl>,
    pub signal: TopologySignal,
}

impl TopologyActor {
    pub(super) fn spawn(self) {
        tokio::spawn(async move {
            let mut layout_generation = LayoutGeneration::default();
            let mut discovery_failed = false;
            let mut last_reconciled_epoch = 0;
            while !self.closed.load(Ordering::Acquire) {
                let notified = tokio::select! {
                    _ = self.signal.notify.notified() => true,
                    _ = sleep(SAFETY_RECONCILE_INTERVAL) => false,
                };
                // A teardown wake must not be answered with one last discovery
                // pass; the connection this actor serves is already gone.
                if self.closed.load(Ordering::Acquire) {
                    break;
                }
                if !self.subscribed.load(Ordering::Acquire) {
                    continue;
                }
                if notified && self.signal.epoch.load(Ordering::Acquire) == last_reconciled_epoch {
                    continue;
                }
                if self.overflowed.swap(false, Ordering::AcqRel) {
                    emit_event(
                        &self.sender,
                        &self.overflowed,
                        v1::HostEvent {
                            kind: v1::EventKind::ResyncRequired.into(),
                            scope: "full".into(),
                            detail: "event queue overflow".into(),
                            ..Default::default()
                        },
                    );
                }

                loop {
                    let observed_epoch = self.signal.epoch.load(Ordering::Acquire);
                    layout_generation.mark_dirty();
                    let started_at = layout_generation
                        .begin()
                        .expect("topology actor owns the reconciliation pass");
                    let guard = self.lock.lock().await;
                    if notified && self.signal.acknowledges(observed_epoch) {
                        drop(guard);
                        if self.signal.epoch.load(Ordering::Acquire) != observed_epoch {
                            layout_generation.mark_dirty();
                        }
                        if !layout_generation.finish(started_at) {
                            last_reconciled_epoch = observed_epoch;
                            break;
                        }
                        continue;
                    }
                    let discovered = tokio::task::spawn_blocking(discover_authoritative).await;
                    match discovered {
                        Ok(Ok((current, identity))) => {
                            discovery_failed = false;
                            let changed = self.baseline.lock().unwrap().as_ref().is_none_or(
                                |(value, value_identity)| {
                                    value != &current || value_identity != &identity
                                },
                            );
                            if changed {
                                *self.baseline.lock().unwrap() =
                                    Some((current.clone(), identity.clone()));
                                let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
                                let _ = self
                                    .sender
                                    .send(SequencerControl::OrderedEvent(v1::HostEvent {
                                        kind: v1::EventKind::TopologySnapshot.into(),
                                        scope: "topology".into(),
                                        snapshot: Some(snapshot_from_identity(
                                            current.clone(),
                                            generation,
                                            identity,
                                        )),
                                        ..Default::default()
                                    }))
                                    .await;
                            } else if notified {
                                // A tmux notification can describe a transient
                                // change that has already settled back to the
                                // authoritative baseline. Close the frontend's
                                // reconciliation state even when no generation
                                // change is needed.
                                let generation = self.generation.load(Ordering::Acquire);
                                let _ = self
                                    .sender
                                    .send(SequencerControl::OrderedEvent(v1::HostEvent {
                                        kind: v1::EventKind::TopologySnapshot.into(),
                                        scope: "topology".into(),
                                        snapshot: Some(snapshot_from_identity(
                                            current.clone(),
                                            generation,
                                            identity.clone(),
                                        )),
                                        detail: "topology reconciliation completed".into(),
                                        ..Default::default()
                                    }))
                                    .await;
                            }
                            reconcile_terminal_clients_if_open(
                                &self.closed,
                                &self.terminal,
                                &current,
                                &self.sender,
                                &self.overflowed,
                            );
                        }
                        _ if !discovery_failed => {
                            discovery_failed = true;
                            emit_event(
                                &self.sender,
                                &self.overflowed,
                                v1::HostEvent {
                                    kind: v1::EventKind::ResyncRequired.into(),
                                    scope: "full".into(),
                                    detail: "tmux discovery failed".into(),
                                    ..Default::default()
                                },
                            );
                        }
                        _ => {}
                    }
                    drop(guard);

                    if self.signal.epoch.load(Ordering::Acquire) != observed_epoch {
                        layout_generation.mark_dirty();
                    }
                    if !layout_generation.finish(started_at) {
                        // This pass reconciled exactly the epoch captured at
                        // its start. A dirty notification may arrive between
                        // the comparison above and here; recording a newer
                        // load would incorrectly consume that notification.
                        last_reconciled_epoch = observed_epoch;
                        break;
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_dirty_coalesces_to_exactly_one_follow_up() {
        let signal = TopologySignal::default();
        let mut generation = LayoutGeneration::default();
        signal.mark_dirty();
        generation.mark_dirty();
        let pass = generation.begin().unwrap();
        for _ in 0..100 {
            signal.mark_dirty();
        }
        generation.mark_dirty();
        assert!(generation.finish(pass));
        let follow_up = generation.begin().unwrap();
        assert!(!generation.finish(follow_up));
        assert_eq!(signal.epoch.load(Ordering::Acquire), 101);
    }

    #[test]
    fn action_acknowledgement_consumes_only_covered_dirty_epochs() {
        let signal = TopologySignal::default();
        signal.mark_dirty();
        let covered = signal.current_epoch();
        signal.acknowledge_through(covered);
        assert!(signal.acknowledges(covered));

        signal.mark_dirty();
        let later = signal.current_epoch();
        assert!(!signal.acknowledges(later));
    }

    /// The actor's only reason to skip a discovery pass for an epoch it was
    /// notified about is [`TopologySignal::acknowledges`], and the sole writer
    /// of that state is a frontend tmux action acknowledging the dirtiness its
    /// own authoritative postcheck already covered. Output-driven dirtiness
    /// raises the epoch past anything such an action can have acknowledged, so
    /// it always reaches discovery — which is what makes the output trigger
    /// safe to point at the same `mark_dirty`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn output_driven_dirty_is_never_consumed_by_an_action_acknowledgement() {
        let signal = TopologySignal::default();
        // An action reconciled and acknowledged everything dirty so far.
        signal.mark_dirty();
        signal.acknowledge_through(signal.current_epoch());
        assert!(signal.acknowledges(signal.current_epoch()));

        // A quiet pane then prints the prompt that followed a `cd`.
        crate::service::topology_output_trigger::TopologyOutputTrigger::new(
            signal.clone(),
            tokio::runtime::Handle::current(),
        )
        .note_output();

        let observed_epoch = signal.current_epoch();
        assert_eq!(observed_epoch, 2);
        assert!(
            !signal.acknowledges(observed_epoch),
            "the actor would have skipped its discovery pass for this epoch"
        );
    }

    #[test]
    fn later_dirty_between_final_check_and_completion_is_not_consumed() {
        let signal = TopologySignal::default();
        signal.mark_dirty();
        let observed_epoch = signal.current_epoch();

        // Model the final comparison as having seen no later dirtiness, then
        // inject one in the exact window before the completed pass records
        // what it reconciled.
        assert_eq!(signal.current_epoch(), observed_epoch);
        signal.mark_dirty();
        let last_reconciled_epoch = observed_epoch;

        assert_ne!(signal.current_epoch(), last_reconciled_epoch);
    }
}
