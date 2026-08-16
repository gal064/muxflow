#[derive(Debug, Clone)]
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
        snapshot: tmux_control::TmuxSnapshot,
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
    /// Not the ordinary `%pause`, which the host recovers from by itself and
    /// which the renderer is deliberately never told about: this is the host
    /// having run out of attempts, after which the pane delivers nothing until
    /// something asks for a seed.
    FlowStalled {
        pane_id: String,
        message: String,
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

pub(super) fn encode_event(event: TerminalEvent) -> Vec<u8> {
    let sequence = snapshot_sequence(&event).unwrap_or(0);
    encode_event_with_sequence(event, sequence)
}

pub(super) fn encode_event_with_sequence(event: TerminalEvent, protocol_sequence: u64) -> Vec<u8> {
    // A snapshot's sequence is its accepted/topology barrier and is part of the
    // snapshot payload. Keep the common binary header atomic with that value,
    // even for snapshots produced directly by handshake and resync responses.
    // Agent service frames are sideband state. They may be paired with the
    // topology frame at the same accepted protocol sequence, so they use the
    // hub's reserved sequence zero and carry their connection epoch in JSON.
    let sequence = if matches!(&event, TerminalEvent::AgentService { .. }) {
        0
    } else {
        snapshot_sequence(&event).unwrap_or(protocol_sequence)
    };
    let (kind, label, payload) = match event {
        TerminalEvent::GenerationEpoch { epoch } => {
            (10, "terminal".into(), epoch.to_be_bytes().to_vec())
        }
        TerminalEvent::Seed {
            pane_id,
            generation,
            data,
        } => (1, pane_id, terminal_payload(generation, data)),
        TerminalEvent::Output {
            pane_id,
            generation,
            data,
        } => (2, pane_id, terminal_payload(generation, data)),
        TerminalEvent::TopologyDirty { name } => (3, name, Vec::new()),
        TerminalEvent::Error { message } => (4, message, Vec::new()),
        TerminalEvent::Exit { reason } => (5, reason, Vec::new()),
        TerminalEvent::ConnectionState { state } => (6, state, Vec::new()),
        TerminalEvent::ProtocolProgress => (8, "protocol".into(), Vec::new()),
        TerminalEvent::PaneResource {
            pane_id,
            state,
            requires_seed,
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
            let reason = recovery_reason.into_bytes();
            let mut payload =
                Vec::with_capacity(38 + reason.len() + serialized_snapshot.len() + raw_tail.len());
            payload.push(state);
            payload.push(u8::from(requires_seed));
            payload.extend_from_slice(&generation.to_be_bytes());
            payload.extend_from_slice(&snapshot_generation.to_be_bytes());
            payload.extend_from_slice(&tail_through_generation.to_be_bytes());
            payload.extend_from_slice(&(reason.len() as u32).to_be_bytes());
            payload.extend_from_slice(&(serialized_snapshot.len() as u32).to_be_bytes());
            payload.extend_from_slice(&(raw_tail.len() as u32).to_be_bytes());
            payload.extend_from_slice(&reason);
            payload.extend_from_slice(&serialized_snapshot);
            payload.extend_from_slice(&raw_tail);
            (9, pane_id, payload)
        }
        TerminalEvent::SeedDiagnostic { pane_id, message } => (11, pane_id, message.into_bytes()),
        TerminalEvent::FlowStalled { pane_id, message } => (15, pane_id, message.into_bytes()),
        TerminalEvent::FileService { scope, payload } => (12, scope, payload),
        TerminalEvent::GitService { scope, payload } => (13, scope, payload),
        TerminalEvent::AgentService { scope, payload } => (14, scope, payload),
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
            (7, "snapshot".into(), payload)
        }
    };
    let label = label.into_bytes();
    let length = u16::try_from(label.len()).unwrap_or(u16::MAX);
    let mut frame = Vec::with_capacity(11 + usize::from(length) + payload.len());
    frame.push(kind);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&label[..usize::from(length)]);
    frame.extend_from_slice(&sequence.to_be_bytes());
    frame.extend_from_slice(&payload);
    frame
}

fn snapshot_sequence(event: &TerminalEvent) -> Option<u64> {
    match event {
        TerminalEvent::Snapshot { sequence, .. } => Some(*sequence),
        _ => None,
    }
}

fn terminal_payload(generation: u64, data: Vec<u8>) -> Vec<u8> {
    let mut payload = Vec::with_capacity(8 + data.len());
    payload.extend_from_slice(&generation.to_be_bytes());
    payload.extend_from_slice(&data);
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_service_is_sequence_zero_even_when_paired_with_protocol_event() {
        // These labels represent all production bridge paths: handshake or
        // topology snapshot sidebands and live AgentState events.
        for (scope, protocol_sequence) in [("snapshot", 17), ("agent:live", u64::MAX)] {
            let frame = encode_event_with_sequence(
                TerminalEvent::AgentService {
                    scope: scope.into(),
                    payload: br#"{"connectionEpoch":"41"}"#.to_vec(),
                },
                protocol_sequence,
            );
            assert_eq!(frame[0], 14);
            let label_len = usize::from(u16::from_be_bytes([frame[1], frame[2]]));
            let sequence_at = 3 + label_len;
            assert_eq!(
                u64::from_be_bytes(frame[sequence_at..sequence_at + 8].try_into().unwrap()),
                0
            );
        }

        let frame = encode_event(TerminalEvent::AgentService {
            scope: "snapshot".into(),
            payload: Vec::new(),
        });
        let label_len = usize::from(u16::from_be_bytes([frame[1], frame[2]]));
        assert_eq!(&frame[3 + label_len..11 + label_len], &0_u64.to_be_bytes());
    }
}
