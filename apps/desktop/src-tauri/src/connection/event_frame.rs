#[derive(Debug)]
pub(super) enum TerminalEvent {
    GenerationEpoch {
        epoch: u64,
    },
    Seed {
        pane_id: String,
        generation: u64,
        data: Vec<u8>,
    },
    Output {
        pane_id: String,
        generation: u64,
        data: Vec<u8>,
    },
    TopologyDirty {
        name: String,
    },
    Snapshot {
        /// Absent on a reconciliation acknowledgement: the host found the
        /// world exactly as the desktop already holds it and sent the
        /// generation alone rather than the tree again.
        snapshot: Option<tmux_control::TmuxSnapshot>,
        sequence: u64,
        generation: u64,
        server_identity: String,
        authoritative: bool,
    },
    Error {
        message: String,
    },
    Exit {
        reason: String,
    },
    ConnectionState {
        state: String,
    },
    ProtocolProgress,
    PaneResource {
        pane_id: String,
        state: String,
        requires_seed: bool,
        resume_from_renderer: bool,
        recovery_reason: String,
        generation: u64,
        snapshot_generation: u64,
        tail_through_generation: u64,
        serialized_snapshot: Vec<u8>,
        raw_tail: Vec<u8>,
    },
    SeedDiagnostic {
        pane_id: String,
        message: String,
    },
    /// A pane tmux paused that the host could not get resumed.
    ///
    /// Not the ordinary `%pause`, which the host recovers from by itself: this
    /// is the host having run out of attempts, after which the pane delivers
    /// nothing until something asks for a seed.
    FlowStalled {
        pane_id: String,
        message: String,
    },
    /// The ordinary `%pause`: tmux clamped a pane the pipeline fell behind on.
    ///
    /// The host resumes and re-captures it by itself, so the renderer must not
    /// react — but the pane delivers nothing while the episode lasts, which is
    /// exactly the window the echo-lag journal keeps catching. This is that
    /// episode's timestamped name in the journal, nothing more.
    FlowPaused {
        pane_id: String,
        message: String,
    },
    ClipboardWrite {
        data: Vec<u8>,
    },
    /// The scrollback above a pane's screen, answering one history request.
    ///
    /// Deliberately not a `Seed`: it carries no generation because it claims no
    /// place in the output ordering, and the renderer splices it above what it
    /// is already showing rather than replacing it.
    History {
        pane_id: String,
        data: Vec<u8>,
        /// How much scrollback tmux holds for the pane, when it answered.
        ///
        /// The renderer compares it against the rows it asked for to decide
        /// whether this page reached the top. `None` is the probe not having
        /// answered — a pane that went away mid-request — and means "ask
        /// again", which is a different answer from a history of zero rows.
        history_size: Option<u32>,
    },
    FileService {
        scope: String,
        payload: Vec<u8>,
    },
    GitService {
        scope: String,
        payload: Vec<u8>,
    },
    AgentService {
        scope: String,
        payload: Vec<u8>,
    },
}

/// The one agent scope that is not a host agent event.
///
/// A snapshot-scoped agent frame is emitted beside the topology frame built
/// from the same host event, so the sequence they share has already crossed to
/// the hub by the time it is encoded. Every other scope is an agent identifier
/// from a standalone ordered `AgentState` event, and the renderer keys the same
/// distinction off this label when it decides whether the payload is a snapshot
/// or an event.
pub(super) const AGENT_SNAPSHOT_SCOPE: &str = "snapshot";

pub(super) fn encode_event(event: TerminalEvent) -> Vec<u8> {
    let sequence = snapshot_sequence(&event).unwrap_or(0);
    encode_event_with_sequence(event, sequence)
}

pub(super) fn encode_event_with_sequence(event: TerminalEvent, protocol_sequence: u64) -> Vec<u8> {
    // A snapshot's sequence is its accepted/topology barrier and is part of the
    // snapshot payload. Keep the common binary header atomic with that value,
    // even for snapshots produced directly by handshake and resync responses.
    //
    // Otherwise the invariant is: a frame carries sequence zero iff its host
    // event consumed no sequence number or the sequence was already delivered
    // by a sibling frame. The snapshot-scoped agent frame is that second case —
    // it rides along with the topology frame that already carried their shared
    // accepted sequence, and repeating it would read as a duplicate. Every
    // other agent frame is a standalone ordered `AgentState` event that
    // consumed a sequence of its own; zeroing it would make the hub's next
    // frame look like a gap and tear the connection down.
    let sequence = match &event {
        TerminalEvent::AgentService { scope, .. } if scope == AGENT_SNAPSHOT_SCOPE => 0,
        _ => snapshot_sequence(&event).unwrap_or(protocol_sequence),
    };
    match event {
        TerminalEvent::GenerationEpoch { epoch } => {
            encode_parts(10, "terminal", sequence, 8, |frame| {
                frame.extend_from_slice(&epoch.to_be_bytes());
            })
        }
        TerminalEvent::Seed {
            pane_id,
            generation,
            data,
        } => encode_terminal(1, pane_id, sequence, generation, data),
        TerminalEvent::Output {
            pane_id,
            generation,
            data,
        } => encode_terminal(2, pane_id, sequence, generation, data),
        TerminalEvent::TopologyDirty { name } => encode_empty(3, name, sequence),
        TerminalEvent::Error { message } => encode_empty(4, message, sequence),
        TerminalEvent::Exit { reason } => encode_empty(5, reason, sequence),
        TerminalEvent::ConnectionState { state } => encode_empty(6, state, sequence),
        TerminalEvent::ProtocolProgress => encode_empty(8, "protocol".into(), sequence),
        TerminalEvent::PaneResource {
            pane_id,
            state,
            requires_seed,
            resume_from_renderer,
            recovery_reason,
            generation,
            snapshot_generation,
            tail_through_generation,
            serialized_snapshot,
            raw_tail,
        } => {
            let state = match state.as_str() {
                "visible" => 1,
                "hiddenBuffered" => 2,
                "released" => 3,
                _ => 0,
            };
            let payload_len =
                38 + recovery_reason.len() + serialized_snapshot.len() + raw_tail.len();
            encode_parts(9, &pane_id, sequence, payload_len, |frame| {
                frame.push(state);
                // One flags byte, not one byte per flag: bit 0 is the seed the
                // host owes this pane, bit 1 is the tail it verified against
                // the renderer's own screen. The two are exclusive, and the
                // decoder refuses any bit it does not know.
                frame.push(u8::from(requires_seed) | (u8::from(resume_from_renderer) << 1));
                frame.extend_from_slice(&generation.to_be_bytes());
                frame.extend_from_slice(&snapshot_generation.to_be_bytes());
                frame.extend_from_slice(&tail_through_generation.to_be_bytes());
                frame.extend_from_slice(&(recovery_reason.len() as u32).to_be_bytes());
                frame.extend_from_slice(&(serialized_snapshot.len() as u32).to_be_bytes());
                frame.extend_from_slice(&(raw_tail.len() as u32).to_be_bytes());
                frame.extend_from_slice(recovery_reason.as_bytes());
                frame.extend_from_slice(&serialized_snapshot);
                frame.extend_from_slice(&raw_tail);
            })
        }
        TerminalEvent::SeedDiagnostic { pane_id, message } => {
            encode_bytes(11, pane_id, sequence, message.into_bytes())
        }
        TerminalEvent::FlowStalled { pane_id, message } => {
            encode_bytes(15, pane_id, sequence, message.into_bytes())
        }
        TerminalEvent::FlowPaused { pane_id, message } => {
            encode_bytes(16, pane_id, sequence, message.into_bytes())
        }
        TerminalEvent::ClipboardWrite { data } => {
            encode_bytes(17, "terminal-clipboard".into(), sequence, data)
        }
        TerminalEvent::History {
            pane_id,
            data,
            history_size,
        } => encode_parts(18, &pane_id, sequence, 5 + data.len(), |frame| {
            // One presence byte and a big-endian u32, ahead of the rows. The
            // same shape as `encode_terminal`'s generation header: a fixed-size
            // number the payload's own bytes could never be mistaken for.
            frame.push(u8::from(history_size.is_some()));
            frame.extend_from_slice(&history_size.unwrap_or(0).to_be_bytes());
            frame.extend_from_slice(&data);
        }),
        TerminalEvent::FileService { scope, payload } => encode_bytes(12, scope, sequence, payload),
        TerminalEvent::GitService { scope, payload } => encode_bytes(13, scope, sequence, payload),
        TerminalEvent::AgentService { scope, payload } => {
            encode_bytes(14, scope, sequence, payload)
        }
        TerminalEvent::Snapshot {
            snapshot,
            sequence,
            generation,
            server_identity,
            authoritative,
        } => {
            let payload = serde_json::to_vec(&serde_json::json!({
                "sequence": sequence,
                "generation": generation,
                "serverIdentity": server_identity,
                "authoritative": authoritative,
                "snapshot": snapshot,
            }))
            .unwrap_or_default();
            encode_bytes(7, "snapshot".into(), sequence, payload)
        }
    }
}

fn encode_parts(
    kind: u8,
    label: &str,
    sequence: u64,
    payload_len: usize,
    append_payload: impl FnOnce(&mut Vec<u8>),
) -> Vec<u8> {
    let length = u16::try_from(label.len()).unwrap_or(u16::MAX);
    let mut frame = Vec::with_capacity(11 + usize::from(length) + payload_len);
    frame.push(kind);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&label.as_bytes()[..usize::from(length)]);
    frame.extend_from_slice(&sequence.to_be_bytes());
    append_payload(&mut frame);
    frame
}

fn encode_empty(kind: u8, label: String, sequence: u64) -> Vec<u8> {
    encode_parts(kind, &label, sequence, 0, |_| {})
}

fn encode_bytes(kind: u8, label: String, sequence: u64, payload: Vec<u8>) -> Vec<u8> {
    encode_parts(kind, &label, sequence, payload.len(), |frame| {
        frame.extend_from_slice(&payload);
    })
}

fn encode_terminal(
    kind: u8,
    pane_id: String,
    sequence: u64,
    generation: u64,
    data: Vec<u8>,
) -> Vec<u8> {
    encode_parts(kind, &pane_id, sequence, 8 + data.len(), |frame| {
        frame.extend_from_slice(&generation.to_be_bytes());
        frame.extend_from_slice(&data);
    })
}

fn snapshot_sequence(event: &TerminalEvent) -> Option<u64> {
    match event {
        TerminalEvent::Snapshot { sequence, .. } => Some(*sequence),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_frame_sequence(scope: &str, protocol_sequence: u64) -> u64 {
        let frame = encode_event_with_sequence(
            TerminalEvent::AgentService {
                scope: scope.into(),
                payload: br#"{"connectionEpoch":"41"}"#.to_vec(),
            },
            protocol_sequence,
        );
        assert_eq!(frame[0], 14);
        let label_len = usize::from(u16::from_be_bytes([frame[1], frame[2]]));
        assert_eq!(&frame[3..3 + label_len], scope.as_bytes());
        let sequence_at = 3 + label_len;
        u64::from_be_bytes(frame[sequence_at..sequence_at + 8].try_into().unwrap())
    }

    /// The paired frame must not re-spend its sibling's sequence.
    #[test]
    fn a_snapshot_scoped_agent_frame_defers_to_the_topology_frame_beside_it() {
        assert_eq!(agent_frame_sequence(AGENT_SNAPSHOT_SCOPE, 17), 0);

        let frame = encode_event(TerminalEvent::AgentService {
            scope: AGENT_SNAPSHOT_SCOPE.into(),
            payload: Vec::new(),
        });
        let label_len = usize::from(u16::from_be_bytes([frame[1], frame[2]]));
        assert_eq!(&frame[3 + label_len..11 + label_len], &0_u64.to_be_bytes());
    }

    /// A standalone `AgentState` event consumed a host sequence of its own, and
    /// swallowing it made the hub read the next ordered frame as a gap and tear
    /// the whole connection down mid-agent-run.
    #[test]
    fn a_standalone_agent_event_frame_carries_the_sequence_it_consumed() {
        assert_eq!(agent_frame_sequence("claude-code:2f9a", 18), 18);
        assert_eq!(agent_frame_sequence("codex:live", u64::MAX), u64::MAX);
    }

    #[test]
    fn clipboard_write_frame_keeps_host_order_and_exact_text() {
        let payload = "selected remotely 🚀".as_bytes().to_vec();
        let frame = encode_event_with_sequence(
            TerminalEvent::ClipboardWrite {
                data: payload.clone(),
            },
            23,
        );
        assert_eq!(frame[0], 17);
        let label_len = usize::from(u16::from_be_bytes([frame[1], frame[2]]));
        assert_eq!(&frame[3..3 + label_len], b"terminal-clipboard");
        let sequence_at = 3 + label_len;
        assert_eq!(&frame[sequence_at..sequence_at + 8], &23_u64.to_be_bytes());
        assert_eq!(&frame[sequence_at + 8..], payload);
    }
}
