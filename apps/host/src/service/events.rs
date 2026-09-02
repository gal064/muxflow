use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use tmux_agent_protocol::{envelope, v1, v1::envelope::Payload};
use tokio::sync::mpsc;

use super::git;

#[derive(Clone)]
pub(super) struct ControlEventSink {
    pub(super) id: u64,
    sender: mpsc::Sender<SequencerControl>,
    overflow_pending: Arc<AtomicBool>,
}

pub(super) static CONTROL_EVENT_HUB: OnceLock<Mutex<Vec<ControlEventSink>>> = OnceLock::new();
static NEXT_CONTROL_SINK_ID: AtomicU64 = AtomicU64::new(1);

pub(super) struct ControlEventRegistration {
    pub(super) id: u64,
}

impl Drop for ControlEventRegistration {
    fn drop(&mut self) {
        if let Some(hub) = CONTROL_EVENT_HUB.get() {
            hub.lock().unwrap().retain(|sink| sink.id != self.id);
        }
    }
}

pub(super) struct ConnectionTaskGuard(pub(super) Arc<AtomicBool>);

impl Drop for ConnectionTaskGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub(super) fn register_control_event_sink(
    sender: mpsc::Sender<SequencerControl>,
) -> ControlEventRegistration {
    let id = NEXT_CONTROL_SINK_ID.fetch_add(1, Ordering::AcqRel);
    CONTROL_EVENT_HUB
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .push(ControlEventSink {
            id,
            sender,
            overflow_pending: Arc::new(AtomicBool::new(false)),
        });
    ControlEventRegistration { id }
}

/// Whether the connection registered under `connection_id` is still open.
pub(crate) fn control_sink_alive(connection_id: u64) -> bool {
    CONTROL_EVENT_HUB.get().is_some_and(|hub| {
        hub.lock()
            .unwrap()
            .iter()
            .any(|sink| sink.id == connection_id && !sink.sender.is_closed())
    })
}

/// Queues one ordered event for exactly one connection.
///
/// The hub otherwise fans every event out to every connection, which is right
/// for topology and agent state and wrong for a spoken agent reply: that
/// belongs to the one phone that registered a voice session for the agent
/// (docs/mobile/voice-mode-plan.md §4.3). Returns `false` when the connection
/// is gone or could not take the event, so the caller can drop what it was
/// holding for it. A full queue is not retried: the reply is several hundred
/// kilobytes the phone can ask for again, not a state change it must see.
pub(crate) fn send_control_event_to(connection_id: u64, event: v1::HostEvent) -> bool {
    let Some(hub) = CONTROL_EVENT_HUB.get() else {
        return false;
    };
    let sender = hub
        .lock()
        .unwrap()
        .iter()
        .find(|sink| sink.id == connection_id)
        .map(|sink| sink.sender.clone());
    sender.is_some_and(|sender| {
        sender
            .try_send(SequencerControl::OrderedEvent(event))
            .is_ok()
    })
}

pub(crate) fn broadcast_control_event(event: v1::HostEvent) {
    let Some(hub) = CONTROL_EVENT_HUB.get() else {
        return;
    };
    let mut saturated = Vec::new();
    hub.lock().unwrap().retain(|sink| {
        match sink
            .sender
            .try_send(SequencerControl::OrderedEvent(event.clone()))
        {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                if sink
                    .overflow_pending
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    crate::diagnostics::record_event_queue_overflow();
                    saturated.push(sink.clone());
                }
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    });
    for sink in saturated {
        let pending = Arc::clone(&sink.overflow_pending);
        // The most expensive event this host can send — it costs the client a
        // full resnapshot — and it used to carry no reason at all, so a log
        // full of them said only that something happened. Under a sustained
        // flood on a real link this is the event that fires, and naming it is
        // what turned P12-Q005 from "resyncs happen" into a measurement.
        let message = SequencerControl::InjectGap(v1::HostEvent {
            kind: v1::EventKind::ResyncRequired.into(),
            scope: "full".into(),
            detail: "host event queue overflowed; the client could not drain events as fast as tmux produced them".into(),
            ..Default::default()
        });
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = sink.sender.send(message).await;
                pending.store(false, Ordering::Release);
            });
        } else {
            std::thread::spawn(move || {
                let _ = sink.sender.blocking_send(message);
                pending.store(false, Ordering::Release);
            });
        }
    }
}

// Protocol responses/events are generated protobuf values; boxing only this
// internal queue union would add allocation and churn without reducing the
// bounded wire frame or the payload values themselves.
#[allow(clippy::large_enum_variant)]
pub(super) enum SequencerControl {
    Response {
        request_id: u64,
        response: v1::Response,
        snapshot_barrier: bool,
    },
    OrderedEvent(v1::HostEvent),
    /// One body frame of an in-flight `OpenFileStream`.
    ///
    /// Bound to its request's own id and carried on the same ordered FIFO as
    /// that request's terminal response, so header, body, and response can
    /// never be reordered relative to each other. It deliberately does not
    /// consume an event sequence number: it is part of one request/response
    /// exchange, not an independent host event a resync could have missed.
    FileStream {
        request_id: u64,
        frame: v1::FileStreamFrame,
    },
    InjectGap(v1::HostEvent),
    /// Internal FIFO barrier. The connection writer resolves it after every
    /// earlier topology-dirty event has advanced `TopologySignal`.
    TopologyEpochBarrier(std::sync::mpsc::SyncSender<u64>),
}

/// A deliberately-triggerable sequence gap, for end-to-end verification only.
///
/// Gap recovery is the client's most expensive path and, in normal operation,
/// only a broadcast saturation reaches it — which is not something a test rig
/// can produce on demand against a real daemon. Setting
/// `ADE_FAULT_INJECT_GAP_AFTER=N` makes every connection skip exactly one
/// sequence number after its N-th ordered event and carry the same
/// `ResyncRequired` event the saturation path emits, so a desktop build can be
/// watched recovering from a real host gap. It fires once per connection and
/// then disarms itself.
///
/// Nothing else reads it: there is no flag, no config key, and no request. With
/// the variable unset — every ordinary run — this is a `None` that each framed
/// message is compared against, and the environment is never consulted again
/// after the first connection.
pub(super) struct GapFaultInjector {
    /// Ordered events still to pass before firing; `None` once it cannot fire.
    remaining: Option<u64>,
}

impl GapFaultInjector {
    /// Arms one connection from the daemon's environment, read once per process.
    pub(super) fn for_connection() -> Self {
        static CONFIGURED: OnceLock<Option<u64>> = OnceLock::new();
        Self::armed_after(*CONFIGURED.get_or_init(|| {
            std::env::var("ADE_FAULT_INJECT_GAP_AFTER")
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
                .filter(|after| *after > 0)
        }))
    }

    pub(super) fn armed_after(after: Option<u64>) -> Self {
        Self { remaining: after }
    }

    /// The gap to frame directly after `message`, if it was the N-th ordered
    /// event on this connection.
    pub(super) fn after(&mut self, message: &SequencerControl) -> Option<SequencerControl> {
        if !matches!(message, SequencerControl::OrderedEvent(_)) {
            return None;
        }
        let remaining = self.remaining?.saturating_sub(1);
        if remaining > 0 {
            self.remaining = Some(remaining);
            return None;
        }
        self.remaining = None;
        Some(SequencerControl::InjectGap(v1::HostEvent {
            kind: v1::EventKind::ResyncRequired.into(),
            scope: "full".into(),
            detail: "ADE_FAULT_INJECT_GAP_AFTER skipped a sequence to exercise client gap recovery"
                .into(),
            ..Default::default()
        }))
    }
}

const MAX_COALESCED_TERMINAL_OUTPUT_BYTES: usize = 64 * 1024;

/// Combines only terminal-output records that are already adjacent in the one
/// ordered connection FIFO.
///
/// A remote tmux pipe often yields a few KiB per read. Encoding every read as a
/// separate protobuf frame lets a flood fill the record-count queue and puts
/// later input responses hundreds of milliseconds behind bytes the writer
/// could have sent together. This preserves byte order and the final generation
/// while stopping at the first different event, response, pane, or size bound.
pub(super) fn coalesce_adjacent_terminal_output(
    mut message: SequencerControl,
    receiver: &mut tokio::sync::mpsc::Receiver<SequencerControl>,
) -> (SequencerControl, Option<SequencerControl>) {
    let SequencerControl::OrderedEvent(first_event) = &mut message else {
        return (message, None);
    };
    if !is_plain_terminal_output(first_event) {
        return (message, None);
    }
    let Some(first_terminal) = first_event.terminal.as_mut() else {
        return (message, None);
    };
    loop {
        let next = match receiver.try_recv() {
            Ok(next) => next,
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => return (message, None),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return (message, None),
        };
        let SequencerControl::OrderedEvent(next_event) = &next else {
            return (message, Some(next));
        };
        let Some(next_terminal) = next_event.terminal.as_ref() else {
            return (message, Some(next));
        };
        if !is_plain_terminal_output(next_event)
            || next_event.scope != first_event.scope
            || next_terminal.pane_id != first_terminal.pane_id
            || first_terminal
                .data
                .len()
                .saturating_add(next_terminal.data.len())
                > MAX_COALESCED_TERMINAL_OUTPUT_BYTES
        {
            return (message, Some(next));
        }
        first_terminal.data.extend_from_slice(&next_terminal.data);
        first_terminal.generation = next_terminal.generation;
        first_event.terminal_delivery_bytes = first_event
            .terminal_delivery_bytes
            .saturating_add(next_event.terminal_delivery_bytes);
        first_event.terminal_delivery_records = first_event
            .terminal_delivery_records
            .saturating_add(next_event.terminal_delivery_records);
    }
}

fn is_plain_terminal_output(event: &v1::HostEvent) -> bool {
    event.kind == v1::EventKind::TerminalOutput as i32
        && event.terminal.is_some()
        && event.detail.is_empty()
        && event.snapshot.is_none()
        && event.pane_resource.is_none()
        && event.file.is_none()
        && event.git.is_none()
        && event.agent.is_none()
}

#[derive(Default)]
pub(super) struct ProtocolSequencer {
    sequence: u64,
}

impl ProtocolSequencer {
    pub(super) fn frame(&mut self, message: SequencerControl) -> v1::Envelope {
        match message {
            SequencerControl::Response {
                request_id,
                mut response,
                snapshot_barrier,
            } => {
                git::bound_command_response(request_id, &mut response);
                if snapshot_barrier {
                    response.accepted_sequence = self.sequence;
                }
                envelope(request_id, 0, Payload::Response(response))
            }
            SequencerControl::OrderedEvent(event) => {
                self.sequence = self.sequence.saturating_add(1);
                envelope(0, self.sequence, Payload::Event(event))
            }
            SequencerControl::FileStream { request_id, frame } => {
                envelope(request_id, 0, Payload::FileStream(frame))
            }
            SequencerControl::InjectGap(event) => {
                self.sequence = self.sequence.saturating_add(2);
                envelope(0, self.sequence, Payload::Event(event))
            }
            SequencerControl::TopologyEpochBarrier(_) => {
                unreachable!("the connection writer consumes topology epoch barriers")
            }
        }
    }
}

#[cfg(test)]
mod gap_fault_tests {
    use super::*;

    fn ordered() -> SequencerControl {
        SequencerControl::OrderedEvent(v1::HostEvent {
            kind: v1::EventKind::TopologyDirty.into(),
            scope: "topology".into(),
            ..Default::default()
        })
    }

    fn response() -> SequencerControl {
        SequencerControl::Response {
            request_id: 4,
            response: v1::Response::default(),
            snapshot_barrier: false,
        }
    }

    /// The gap has to be a real one — a skipped sequence carrying the same
    /// `ResyncRequired` the saturation path sends — or it verifies nothing.
    #[test]
    fn the_armed_injector_skips_one_sequence_after_the_nth_ordered_event() {
        let mut injector = GapFaultInjector::armed_after(Some(2));
        let mut sequencer = ProtocolSequencer::default();

        // Only ordered events count toward N.
        assert!(injector.after(&response()).is_none());
        assert!(injector.after(&ordered()).is_none());
        assert_eq!(sequencer.frame(ordered()).sequence, 1);

        let gap = injector
            .after(&ordered())
            .expect("the second ordered event did not trip the injector");
        assert_eq!(sequencer.frame(ordered()).sequence, 2);
        let SequencerControl::InjectGap(event) = &gap else {
            panic!("the injector did not reuse the InjectGap mechanics: not a gap")
        };
        assert_eq!(event.kind, v1::EventKind::ResyncRequired as i32);
        assert_eq!(event.scope, "full");
        let frame = sequencer.frame(gap);
        assert_eq!(frame.sequence, 4, "sequence 3 was not skipped");
        assert!(matches!(frame.payload, Some(Payload::Event(_))));

        // The next ordered event resumes from the injected sequence, so the
        // client sees exactly one gap and then a coherent stream.
        assert_eq!(sequencer.frame(ordered()).sequence, 5);
    }

    #[test]
    fn the_injector_fires_once_per_connection_and_is_inert_when_unarmed() {
        let mut armed = GapFaultInjector::armed_after(Some(1));
        assert!(armed.after(&ordered()).is_some());
        for _ in 0..16 {
            assert!(
                armed.after(&ordered()).is_none(),
                "the injector re-armed itself on the same connection"
            );
        }

        let mut unarmed = GapFaultInjector::armed_after(None);
        for _ in 0..16 {
            assert!(unarmed.after(&ordered()).is_none());
        }
    }
}

#[cfg(test)]
mod coalescing_tests {
    use super::*;

    fn output(pane_id: &str, scope: &str, generation: u64, data: Vec<u8>) -> SequencerControl {
        SequencerControl::OrderedEvent(v1::HostEvent {
            kind: v1::EventKind::TerminalOutput.into(),
            scope: scope.into(),
            terminal: Some(v1::TerminalBytes {
                pane_id: pane_id.into(),
                data,
                generation,
                ..Default::default()
            }),
            ..Default::default()
        })
    }

    fn terminal(message: &SequencerControl) -> &v1::TerminalBytes {
        let SequencerControl::OrderedEvent(event) = message else {
            panic!("expected event")
        };
        event.terminal.as_ref().expect("terminal payload")
    }

    #[test]
    fn same_pane_outputs_merge_exact_bytes_and_take_the_last_generation() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        sender
            .try_send(output("%1", "terminal", 3, Vec::new()))
            .unwrap();
        sender
            .try_send(output("%1", "terminal", 9, b"bc".to_vec()))
            .unwrap();
        let (merged, deferred) = coalesce_adjacent_terminal_output(
            output("%1", "terminal", 1, b"a".to_vec()),
            &mut receiver,
        );
        assert!(deferred.is_none());
        assert_eq!(terminal(&merged).data, b"abc");
        assert_eq!(terminal(&merged).generation, 9);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn other_pane_and_scope_boundaries_are_preserved_in_the_deferred_slot() {
        let (_sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let (first, deferred) =
            coalesce_adjacent_terminal_output(output("%1", "one", 1, b"a".to_vec()), &mut receiver);
        assert_eq!(terminal(&first).data, b"a");
        assert!(deferred.is_none());

        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        sender
            .try_send(output("%2", "one", 2, b"b".to_vec()))
            .unwrap();
        let (_, deferred) =
            coalesce_adjacent_terminal_output(output("%1", "one", 1, b"a".to_vec()), &mut receiver);
        assert_eq!(terminal(deferred.as_ref().unwrap()).pane_id, "%2");

        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        sender
            .try_send(output("%1", "two", 2, b"b".to_vec()))
            .unwrap();
        let (_, deferred) =
            coalesce_adjacent_terminal_output(output("%1", "one", 1, b"a".to_vec()), &mut receiver);
        assert_eq!(terminal(deferred.as_ref().unwrap()).data, b"b");
    }

    #[test]
    fn a_response_boundary_is_never_crossed_or_lost() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        sender
            .try_send(SequencerControl::Response {
                request_id: 41,
                response: v1::Response::default(),
                snapshot_barrier: false,
            })
            .unwrap();
        let (_, deferred) = coalesce_adjacent_terminal_output(
            output("%1", "terminal", 1, b"a".to_vec()),
            &mut receiver,
        );
        assert!(matches!(
            deferred,
            Some(SequencerControl::Response { request_id: 41, .. })
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn sixty_four_kib_is_an_exact_boundary_and_oversize_records_stay_atomic() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        sender.try_send(output("%1", "", 2, vec![2])).unwrap();
        let (merged, deferred) = coalesce_adjacent_terminal_output(
            output("%1", "", 1, vec![1; 64 * 1024 - 1]),
            &mut receiver,
        );
        assert!(deferred.is_none());
        assert_eq!(terminal(&merged).data.len(), 64 * 1024);

        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        sender.try_send(output("%1", "", 2, vec![2])).unwrap();
        let (first, deferred) = coalesce_adjacent_terminal_output(
            output("%1", "", 1, vec![1; 64 * 1024]),
            &mut receiver,
        );
        assert_eq!(terminal(&first).data.len(), 64 * 1024);
        assert!(deferred.is_some());

        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        sender.try_send(output("%1", "", 2, vec![2])).unwrap();
        let (oversize, deferred) = coalesce_adjacent_terminal_output(
            output("%1", "", 1, vec![1; 64 * 1024 + 1]),
            &mut receiver,
        );
        assert_eq!(terminal(&oversize).data.len(), 64 * 1024 + 1);
        assert!(deferred.is_some());
    }

    #[test]
    fn detail_or_additional_payload_prevents_coalescing() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        let mut decorated = output("%1", "terminal", 2, b"b".to_vec());
        let SequencerControl::OrderedEvent(event) = &mut decorated else {
            unreachable!()
        };
        event.detail = "diagnostic".into();
        sender.try_send(decorated).unwrap();
        let (_, deferred) = coalesce_adjacent_terminal_output(
            output("%1", "terminal", 1, b"a".to_vec()),
            &mut receiver,
        );
        assert!(deferred.is_some());
    }

    #[test]
    fn coalescing_preserves_all_tiny_record_and_byte_credit() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        for generation in 2..=64 {
            let mut next = output("%1", "terminal", generation, vec![b'x']);
            let SequencerControl::OrderedEvent(event) = &mut next else {
                unreachable!()
            };
            event.terminal_delivery_bytes = 1;
            event.terminal_delivery_records = 1;
            sender.try_send(next).unwrap();
        }
        let mut first = output("%1", "terminal", 1, vec![b'x']);
        let SequencerControl::OrderedEvent(event) = &mut first else {
            unreachable!()
        };
        event.terminal_delivery_bytes = 1;
        event.terminal_delivery_records = 1;
        let (merged, deferred) = coalesce_adjacent_terminal_output(first, &mut receiver);
        assert!(deferred.is_none());
        let SequencerControl::OrderedEvent(merged) = merged else {
            unreachable!()
        };
        assert_eq!(merged.terminal.unwrap().data.len(), 64);
        assert_eq!(merged.terminal_delivery_bytes, 64);
        assert_eq!(merged.terminal_delivery_records, 64);
    }
}
