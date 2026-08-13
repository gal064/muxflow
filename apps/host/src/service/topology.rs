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
    notify: Arc<Notify>,
}

impl TopologySignal {
    pub(super) fn mark_dirty(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
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
                        last_reconciled_epoch = self.signal.epoch.load(Ordering::Acquire);
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
}
