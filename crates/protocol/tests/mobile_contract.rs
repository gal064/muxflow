//! Rust owns the mobile handshake constants and the cross-language vectors.
//! Regenerate after intentional changes with:
//! MUXFLOW_UPDATE_CONTRACT=1 cargo test -p tmux-agent-protocol --test mobile_contract
use serde_json::{Value, json};
use tmux_agent_protocol::{
    HOST_CAPABILITIES, HostContractError, MAX_FRAME_BYTES, PROTOCOL_MAJOR, PROTOCOL_MINOR,
    capability_names, encode_frame, envelope, v1, validate_host_contract,
};

fn admission(major: u32, capabilities: u64, read_only: bool) -> Value {
    let hello = v1::ServerHello {
        capabilities,
        read_only,
        incompatibility: "test refusal".into(),
        ..Default::default()
    };
    let refusal = match validate_host_contract(major, &hello) {
        Ok(()) => Value::Null,
        Err(HostContractError::ProtocolMajor { .. }) => json!({"kind": "protocolMajor"}),
        Err(HostContractError::ReadOnly(_)) => json!({"kind": "readOnly"}),
        Err(HostContractError::MissingCapabilities(missing)) => {
            json!({"kind": "missingCapabilities", "missing": missing.to_string(), "names": capability_names(missing)})
        }
    };
    json!({"major": major, "capabilities": capabilities.to_string(), "readOnly": read_only, "refusal": refusal})
}

fn frame(payload: v1::envelope::Payload) -> String {
    encode_frame(&envelope(u64::MAX, (1 << 53) + 9, payload))
        .unwrap()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn mobile_contract_matches_rust() {
    let capabilities: Vec<Value> = (0..64)
        .map(|index| 1_u64 << index)
        .filter(|bit| HOST_CAPABILITIES & bit != 0)
        .map(|bit| json!({"bit": bit.to_string(), "name": capability_names(bit)[0]}))
        .collect();
    let mut admissions = vec![
        admission(PROTOCOL_MAJOR, HOST_CAPABILITIES, false),
        admission(PROTOCOL_MAJOR, u64::MAX, false),
        admission(PROTOCOL_MAJOR + 1, 0, true),
        admission(PROTOCOL_MAJOR, 0, true),
        admission(PROTOCOL_MAJOR, 0, false),
    ];
    admissions.extend(
        (0..64)
            .map(|index| 1_u64 << index)
            .filter(|bit| HOST_CAPABILITIES & bit != 0)
            .map(|bit| admission(PROTOCOL_MAJOR, HOST_CAPABILITIES & !bit, false)),
    );
    let constants = json!({
        "protocolMajor": PROTOCOL_MAJOR, "protocolMinor": PROTOCOL_MINOR,
        "maxFrameBytes": MAX_FRAME_BYTES, "hostCapabilities": HOST_CAPABILITIES.to_string(),
        "capabilities": capabilities,
    });
    let vectors = json!({
        "admissions": admissions,
        "frames": {
            "terminal": frame(v1::envelope::Payload::TerminalBytes(v1::TerminalBytes {
                pane_id: "%3".into(), data: vec![0, 0xff, 0x1b, b'[', b'H'], generation: (1 << 53) + 7,
                history_size: 0, history_size_known: true,
            })),
            "file": frame(v1::envelope::Payload::FileStream(v1::FileStreamFrame {
                operation_id: "open-1".into(), offset: u64::MAX, data: vec![0, 159, 146, 150], eof: true,
                blake3: "digest".into(), ..Default::default()
            })),
            "git": frame(v1::envelope::Payload::Request(v1::Request {
                operation: v1::Operation::GitMutation.into(), git: Some(v1::GitRequest {
                    path: vec![b'a', b'\n', 0xff], original_path: vec![b'b', b'\t', 0xfe],
                    connection_epoch: (1 << 53) + 9, ..Default::default()
                }), ..Default::default()
            })),
            "voice": frame(v1::envelope::Payload::Event(v1::HostEvent {
                kind: v1::EventKind::VoiceReply.into(), voice: Some(v1::VoiceEvent {
                    reply: Some(v1::VoiceSpeech {
                        audio: vec![0xff, 0xfb, 0x90], provider: v1::VoiceProvider::EdgeTts.into(),
                        state_generation: (1 << 53) + 5, ..Default::default()
                    }), ..Default::default()
                }), ..Default::default()
            })),
        },
    });
    for (file, value) in [
        ("gen/host_contract.json", constants),
        ("testing/rust_vectors.json", vectors),
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/mobile/src/protocol")
            .join(file);
        let generated = format!("{}\n", serde_json::to_string_pretty(&value).unwrap());
        if std::env::var_os("MUXFLOW_UPDATE_CONTRACT").is_some() {
            std::fs::write(&path, &generated).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            generated,
            "Rust/mobile contract drift; regenerate with the command at the top of this test"
        );
    }
}
