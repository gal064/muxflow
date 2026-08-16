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
    InjectGap(v1::HostEvent),
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
            SequencerControl::InjectGap(event) => {
                self.sequence = self.sequence.saturating_add(2);
                envelope(0, self.sequence, Payload::Event(event))
            }
        }
    }
}
