//! The bridge's binary frames, and their acknowledgement. Port of the decoder
//! and `DeliveryAcknowledgements` in `features/terminal/api.ts`, for the events
//! a connection reacts to. Terminal and service payloads are T3/T4's: here they
//! only spend delivery credit.

use std::sync::Arc;

use desktop_core::platform::{EventBody, EventSink};
use futures::channel::mpsc::UnboundedSender;
use serde::Deserialize;
use tmux_control::TmuxSnapshot;

use super::connection_reducer::ConnectionPhase;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotEvent {
    pub snapshot: Option<Arc<TmuxSnapshot>>,
    pub sequence: u64,
    pub generation: u64,
    pub server_identity: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BridgeEvent {
    GenerationEpoch(u64),
    TopologyDirty {
        name: String,
    },
    Error {
        message: String,
    },
    Exit {
        reason: String,
    },
    ConnectionState {
        phase: ConnectionPhase,
        detail: Option<String>,
    },
    Snapshot(SnapshotEvent),
    FlowStalled {
        pane_id: String,
    },
    FlowPaused {
        pane_id: String,
    },
    /// A terminal program copied into a tmux paste buffer.
    ClipboardWrite {
        text: String,
    },
    /// A valid frame this module has no reaction to.
    Ignored,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotPayload {
    #[serde(default)]
    snapshot: Option<TmuxSnapshot>,
    sequence: u64,
    generation: u64,
    server_identity: String,
    authoritative: bool,
}

const HEADER_BYTES: usize = 1 + 2 + 8;

fn require_local_sequence(sequence: u64, what: &str) -> Result<(), String> {
    if sequence == 0 {
        Ok(())
    } else {
        Err(format!("{what} frame carries a host sequence"))
    }
}

fn require_host_sequence(sequence: u64, what: &str) -> Result<(), String> {
    if sequence == 0 {
        Err(format!("{what} frame has no host sequence"))
    } else {
        Ok(())
    }
}

fn require_empty(data: &[u8], what: &str) -> Result<(), String> {
    if data.is_empty() {
        Ok(())
    } else {
        Err(format!("{what} frame carries a payload"))
    }
}

fn require_pane_id(label: &str, what: &str) -> Result<(), String> {
    match label.strip_prefix('%') {
        Some(digits) if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) => {
            Ok(())
        }
        _ => Err(format!("{what} frame has an invalid pane id")),
    }
}

fn text(data: &[u8], what: &str) -> Result<String, String> {
    String::from_utf8(data.to_vec()).map_err(|_| format!("{what} is not valid UTF-8"))
}

/// Decodes one frame. An error is a frame to drop; its credit is still spent.
pub fn decode_frame(frame: &[u8]) -> Result<BridgeEvent, String> {
    if frame.len() < HEADER_BYTES {
        return Err("terminal frame is shorter than its common header".into());
    }
    let label_len = usize::from(u16::from_be_bytes([frame[1], frame[2]]));
    let payload_at = HEADER_BYTES + label_len;
    if frame.len() < payload_at {
        return Err("terminal frame label or sequence is truncated".into());
    }
    let label = text(&frame[3..3 + label_len], "terminal frame label")?;
    let mut sequence_bytes = [0; 8];
    sequence_bytes.copy_from_slice(&frame[3 + label_len..payload_at]);
    let sequence = u64::from_be_bytes(sequence_bytes);
    let data = &frame[payload_at..];
    match frame[0] {
        3 => {
            require_host_sequence(sequence, "topology dirty")?;
            require_empty(data, "topology dirty")?;
            // A layout change carries its pane grids after the name; those are T3's.
            let name = if label.starts_with("layout-change ") {
                "layout-change".to_owned()
            } else {
                label
            };
            Ok(BridgeEvent::TopologyDirty { name })
        }
        4 => {
            require_local_sequence(sequence, "error")?;
            require_empty(data, "error")?;
            Ok(BridgeEvent::Error { message: label })
        }
        5 => {
            require_host_sequence(sequence, "exit")?;
            require_empty(data, "exit")?;
            Ok(BridgeEvent::Exit { reason: label })
        }
        6 => {
            require_local_sequence(sequence, "connection state")?;
            let phase = ConnectionPhase::from_wire(&label)
                .ok_or_else(|| format!("unknown connection state {label}"))?;
            let detail = if data.is_empty() {
                None
            } else {
                Some(text(data, "connection state detail")?)
            };
            Ok(BridgeEvent::ConnectionState { phase, detail })
        }
        7 => decode_snapshot(&label, sequence, data),
        10 => {
            require_local_sequence(sequence, "terminal generation epoch")?;
            if label != "terminal" {
                return Err("terminal generation epoch has an invalid label".into());
            }
            let bytes: [u8; 8] = data
                .try_into()
                .map_err(|_| "terminal generation epoch is malformed".to_owned())?;
            match u64::from_be_bytes(bytes) {
                0 => Err("terminal generation epoch must be nonzero".into()),
                epoch => Ok(BridgeEvent::GenerationEpoch(epoch)),
            }
        }
        15 => {
            require_host_sequence(sequence, "terminal flow stall")?;
            require_pane_id(&label, "terminal flow stall")?;
            text(data, "terminal flow stall payload")?;
            Ok(BridgeEvent::FlowStalled { pane_id: label })
        }
        16 => {
            require_host_sequence(sequence, "terminal flow pause")?;
            require_pane_id(&label, "terminal flow pause")?;
            text(data, "terminal flow pause payload")?;
            Ok(BridgeEvent::FlowPaused { pane_id: label })
        }
        17 => {
            require_host_sequence(sequence, "terminal clipboard write")?;
            if label != "terminal-clipboard"
                || data.is_empty()
                || data.len() > tmux_control::MAX_INPUT_REQUEST_BYTES
            {
                return Err("terminal clipboard write frame is malformed or oversized".into());
            }
            let text = text(data, "terminal clipboard write payload")?;
            Ok(BridgeEvent::ClipboardWrite { text })
        }
        1 | 2 | 8 | 9 | 11 | 12 | 13 | 14 | 18 | 19 => Ok(BridgeEvent::Ignored),
        kind => Err(format!("unknown terminal frame kind {kind}")),
    }
}

fn decode_snapshot(label: &str, sequence: u64, data: &[u8]) -> Result<BridgeEvent, String> {
    if label != "snapshot" {
        return Err("topology snapshot has an invalid label".into());
    }
    let parsed: SnapshotPayload = serde_json::from_slice(data)
        .map_err(|error| format!("topology snapshot payload is invalid: {error}"))?;
    if parsed.sequence != sequence {
        return Err("snapshot sequence conflicts with its common frame header".into());
    }
    if !parsed.authoritative {
        require_host_sequence(sequence, "ordered topology snapshot")?;
    }
    Ok(BridgeEvent::Snapshot(SnapshotEvent {
        snapshot: parsed.snapshot.map(Arc::new),
        sequence,
        generation: parsed.generation,
        server_identity: parsed.server_identity,
    }))
}

/// What `desktop-core` delivers a bridge's frames through.
pub struct ChannelSink(pub UnboundedSender<EventBody>);

impl EventSink for ChannelSink {
    fn send(&self, body: EventBody) -> Result<(), String> {
        self.0
            .unbounded_send(body)
            .map_err(|_| "the connection UI stopped listening".to_owned())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AckBoundary {
    pub epoch: u64,
    pub frames: u64,
    pub bytes: u64,
}

/// Cumulative delivery credit. One acknowledgement in flight folds a burst
/// into its latest boundary without a timer.
#[derive(Debug, Default)]
pub struct DeliveryAcks {
    epoch: Option<u64>,
    frames: u64,
    bytes: u64,
    submitted_frames: u64,
    submitted_bytes: u64,
    in_flight: bool,
}

impl DeliveryAcks {
    pub fn begin_epoch(&mut self, epoch: u64) {
        if self.epoch == Some(epoch) {
            return;
        }
        self.epoch = Some(epoch);
        self.frames = 0;
        self.bytes = 0;
        self.submitted_frames = 0;
        self.submitted_bytes = 0;
    }

    /// A frame the engine has applied, credited under the epoch it applied in.
    /// Frames before the first epoch belong to no generation and are not owed,
    /// and a straggler from an epoch since replaced counts for nothing.
    pub fn record_applied(&mut self, byte_length: usize, epoch: Option<u64>) {
        if self.epoch.is_some() && self.epoch == epoch {
            self.frames += 1;
            self.bytes += byte_length as u64;
        }
    }

    pub fn take_boundary(&mut self) -> Option<AckBoundary> {
        let epoch = self.epoch?;
        if self.in_flight
            || (self.submitted_frames == self.frames && self.submitted_bytes == self.bytes)
        {
            return None;
        }
        self.in_flight = true;
        Some(AckBoundary {
            epoch,
            frames: self.frames,
            bytes: self.bytes,
        })
    }

    /// Returns whether another boundary may be sent. A failed acknowledgement
    /// forces the bridge to reconnect, and that new epoch is its own wakeup:
    /// a failed boundary is never retried on the same epoch.
    pub fn settle(&mut self, boundary: AckBoundary, delivered: bool) -> bool {
        self.in_flight = false;
        let same_epoch = self.epoch == Some(boundary.epoch);
        if delivered && same_epoch {
            self.submitted_frames = boundary.frames;
            self.submitted_bytes = boundary.bytes;
        }
        delivered || !same_epoch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(kind: u8, label: &str, sequence: u64, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![kind];
        bytes.extend_from_slice(&(label.len() as u16).to_be_bytes());
        bytes.extend_from_slice(label.as_bytes());
        bytes.extend_from_slice(&sequence.to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn decodes_the_lifecycle_frames_a_connection_reacts_to() {
        assert_eq!(
            decode_frame(&frame(10, "terminal", 0, &7u64.to_be_bytes())),
            Ok(BridgeEvent::GenerationEpoch(7))
        );
        assert_eq!(
            decode_frame(&frame(4, "ssh handshake failed", 0, &[])),
            Ok(BridgeEvent::Error {
                message: "ssh handshake failed".into()
            })
        );
        assert_eq!(
            decode_frame(&frame(5, "gone", 3, &[])),
            Ok(BridgeEvent::Exit {
                reason: "gone".into()
            })
        );
        assert_eq!(
            decode_frame(&frame(3, "layout", 4, &[])),
            Ok(BridgeEvent::TopologyDirty {
                name: "layout".into()
            })
        );
        assert_eq!(
            decode_frame(&frame(6, "resyncing", 0, b"gap")),
            Ok(BridgeEvent::ConnectionState {
                phase: ConnectionPhase::Resyncing,
                detail: Some("gap".into()),
            })
        );
        assert_eq!(
            decode_frame(&frame(15, "%3", 9, b"stalled")),
            Ok(BridgeEvent::FlowStalled {
                pane_id: "%3".into()
            })
        );
    }

    #[test]
    fn decodes_a_snapshot_and_a_reconciliation_acknowledgement() {
        let body = br#"{"sequence":5,"generation":2,"serverIdentity":"s","authoritative":false,
            "snapshot":{"sessions":[],"windows":[],"panes":[]}}"#;
        let BridgeEvent::Snapshot(snapshot) = decode_frame(&frame(7, "snapshot", 5, body)).unwrap()
        else {
            panic!("expected a snapshot");
        };
        assert_eq!(snapshot.generation, 2);
        assert_eq!(snapshot.server_identity, "s");
        assert!(snapshot.snapshot.is_some());

        let ack = br#"{"sequence":6,"generation":2,"serverIdentity":"s","authoritative":false,"snapshot":null}"#;
        let BridgeEvent::Snapshot(ack) = decode_frame(&frame(7, "snapshot", 6, ack)).unwrap()
        else {
            panic!("expected an acknowledgement");
        };
        assert_eq!(ack.snapshot, None);
    }

    #[test]
    fn refuses_malformed_frames() {
        assert!(decode_frame(&[1, 0]).is_err());
        assert!(decode_frame(&frame(6, "sideways", 0, &[])).is_err());
        assert!(decode_frame(&frame(4, "message", 1, &[])).is_err());
        assert!(decode_frame(&frame(10, "terminal", 0, &0u64.to_be_bytes())).is_err());
        assert!(decode_frame(&frame(99, "x", 0, &[])).is_err());
        let conflicting =
            br#"{"sequence":9,"generation":2,"serverIdentity":"s","authoritative":true,"snapshot":null}"#;
        assert!(decode_frame(&frame(7, "snapshot", 5, conflicting)).is_err());
        let unordered =
            br#"{"sequence":0,"generation":2,"serverIdentity":"s","authoritative":false,"snapshot":null}"#;
        assert!(decode_frame(&frame(7, "snapshot", 0, unordered)).is_err());
    }

    #[test]
    fn names_a_layout_change_without_its_pane_grids() {
        assert_eq!(
            decode_frame(&frame(3, "layout-change [[\"%1\",80,24]]", 4, &[])),
            Ok(BridgeEvent::TopologyDirty {
                name: "layout-change".into()
            })
        );
    }

    #[test]
    fn refuses_a_flow_frame_without_a_pane_id() {
        assert!(decode_frame(&frame(15, "pane", 9, b"stalled")).is_err());
        assert!(decode_frame(&frame(16, "%", 9, b"paused")).is_err());
    }

    #[test]
    fn decodes_a_forwarded_copy_and_refuses_a_malformed_one() {
        assert_eq!(
            decode_frame(&frame(17, "terminal-clipboard", 3, b"copied")),
            Ok(BridgeEvent::ClipboardWrite {
                text: "copied".into()
            })
        );
        assert!(decode_frame(&frame(17, "terminal-clipboard", 0, b"copied")).is_err());
        assert!(decode_frame(&frame(17, "elsewhere", 3, b"copied")).is_err());
        assert!(decode_frame(&frame(17, "terminal-clipboard", 3, b"")).is_err());
        assert!(decode_frame(&frame(17, "terminal-clipboard", 3, b"\xff")).is_err());
    }

    #[test]
    fn leaves_terminal_payload_frames_for_their_owners() {
        assert_eq!(
            decode_frame(&frame(2, "%1", 4, b"output")),
            Ok(BridgeEvent::Ignored)
        );
    }

    #[test]
    fn owes_nothing_before_the_first_epoch() {
        let mut acks = DeliveryAcks::default();
        acks.record_applied(10, None);
        assert_eq!(acks.take_boundary(), None);
        acks.begin_epoch(1);
        assert_eq!(acks.take_boundary(), None);
    }

    #[test]
    fn folds_a_burst_into_the_boundary_after_the_one_in_flight() {
        let mut acks = DeliveryAcks::default();
        acks.begin_epoch(1);
        acks.record_applied(10, Some(1));
        let first = acks.take_boundary().unwrap();
        assert_eq!(
            first,
            AckBoundary {
                epoch: 1,
                frames: 1,
                bytes: 10
            }
        );
        acks.record_applied(5, Some(1));
        acks.record_applied(5, Some(1));
        assert_eq!(acks.take_boundary(), None);
        assert!(acks.settle(first, true));
        let second = acks.take_boundary().unwrap();
        assert_eq!(
            second,
            AckBoundary {
                epoch: 1,
                frames: 3,
                bytes: 20
            }
        );
        assert!(acks.settle(second, true));
        assert_eq!(acks.take_boundary(), None);
    }

    #[test]
    fn a_frame_applied_under_a_replaced_epoch_earns_nothing() {
        let mut acks = DeliveryAcks::default();
        acks.begin_epoch(2);
        acks.record_applied(7, Some(1));
        assert_eq!(acks.take_boundary(), None);
        acks.record_applied(7, Some(2));
        assert_eq!(acks.take_boundary().unwrap().bytes, 7);
    }

    #[test]
    fn never_retries_a_failed_boundary_on_its_own_epoch_but_a_new_epoch_wakes_it() {
        let mut acks = DeliveryAcks::default();
        acks.begin_epoch(1);
        acks.record_applied(10, Some(1));
        let failed = acks.take_boundary().unwrap();
        assert!(!acks.settle(failed, false));

        acks.begin_epoch(2);
        acks.record_applied(3, Some(2));
        let in_flight = acks.take_boundary().unwrap();
        acks.begin_epoch(3);
        acks.record_applied(1, Some(3));
        assert!(acks.settle(in_flight, false));
        assert_eq!(
            acks.take_boundary().unwrap(),
            AckBoundary {
                epoch: 3,
                frames: 1,
                bytes: 1
            }
        );
    }
}
