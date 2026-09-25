// Framing and schema invariants. Cross-language byte/u64 coverage lives in
// mobile_contract.rs and the mobile Rust-contract test.
use prost::Message;
use tmux_agent_protocol::{PROTOCOL_MAJOR, encode_frame, read_frame_sync, v1};

#[test]
fn assigned_operation_and_enum_numbers_do_not_move() {
    use v1::Operation::*;
    assert_eq!(PROTOCOL_MAJOR, 4);
    for (operation, number) in [
        (ReconcileTerminalUpload, 41),
        (SelectTerminalSession, 43),
        (OpenFileStream, 44),
        (GitDiffContent, 45),
        (ResolveTerminalFile, 46),
        (GitPush, 47),
        (RequestTerminalHistory, 48),
        (VoiceStatus, 49),
        (VoiceProvision, 50),
        (VoiceTranscribe, 51),
        (VoiceSpeak, 52),
        (VoiceSession, 53),
        (AgentDiagnostics, 54),
        (YieldTerminalSizing, 55),
        (TestDelay, 100),
    ] {
        assert_eq!(operation as i32, number);
    }
    assert_eq!(v1::TmuxActionKind::SetPinned as i32, 20);
    assert_eq!(v1::EventKind::TerminalHistory as i32, 19);
    assert_eq!(v1::EventKind::VoiceProvision as i32, 20);
    assert_eq!(v1::EventKind::VoiceReply as i32, 21);
    assert_eq!(v1::VoiceProvider::EdgeTts as i32, 1);
    assert_eq!(v1::VoiceReadiness::Ready as i32, 4);
}

#[test]
fn field_numbers_and_wire_types_do_not_move() {
    // Minimal messages assert complete encodings: searching a populated
    // message for a tag can accidentally match unrelated payload bytes.
    let request = v1::Request {
        terminal_epoch: 7,
        terminal_generation_cutoff: 42,
        terminal_renderer_holds_snapshot: true,
        terminal_history_lines: 2000,
        terminal_history_skip_lines: 40,
        terminal_input_paste: true,
        terminal_input_voice: true,
        terminal_input_expected_server_identity: "server-a".into(),
        ..Default::default()
    };
    assert_eq!(
        request.encode_to_vec(),
        [
            0x58, 7, 0x60, 42, 0x80, 1, 1, 0x88, 1, 0xd0, 0x0f, 0x90, 1, 40, 0x98, 1, 1, 0xb0, 1,
            1, 0xba, 1, 8, b's', b'e', b'r', b'v', b'e', b'r', b'-', b'a',
        ]
    );
    assert_eq!(
        v1::PaneResource {
            resume_from_renderer: true,
            ..Default::default()
        }
        .encode_to_vec(),
        [0x50, 1]
    );
    assert_eq!(
        v1::Request {
            voice: Some(v1::VoiceRequest::default()),
            ..Default::default()
        }
        .encode_to_vec(),
        [0xa2, 1, 0]
    );
    assert_eq!(
        v1::Response {
            voice: Some(v1::VoiceResponse::default()),
            ..Default::default()
        }
        .encode_to_vec(),
        [0x6a, 0]
    );
    assert_eq!(
        v1::HostEvent {
            voice: Some(v1::VoiceEvent::default()),
            ..Default::default()
        }
        .encode_to_vec(),
        [0x6a, 0]
    );
    assert_eq!(
        v1::TerminalBytes {
            history_size_known: true,
            ..Default::default()
        }
        .encode_to_vec(),
        [0x28, 1]
    );
}

#[tokio::test]
async fn frame_readers_distinguish_disconnect_truncation_corruption_and_oversize() {
    use tmux_agent_protocol::{FrameAccumulator, FrameError, MAX_FRAME_BYTES, read_frame};
    assert!(read_frame_sync(&mut [].as_slice()).unwrap().is_none());
    assert!(read_frame(&mut [].as_slice()).await.unwrap().is_none());
    for bytes in [vec![0], vec![0, 0, 0], vec![0, 0, 0, 2, 8]] {
        assert!(matches!(
            read_frame_sync(&mut bytes.as_slice()),
            Err(FrameError::Io(_))
        ));
        assert!(matches!(
            read_frame(&mut bytes.as_slice()).await,
            Err(FrameError::Io(_))
        ));
    }
    let corrupt = [0, 0, 0, 1, 0xff];
    assert!(matches!(
        read_frame_sync(&mut corrupt.as_slice()),
        Err(FrameError::Decode(_))
    ));
    assert!(matches!(
        read_frame(&mut corrupt.as_slice()).await,
        Err(FrameError::Decode(_))
    ));
    let mut accumulator = FrameAccumulator::default();
    accumulator.push(&corrupt).unwrap();
    assert!(matches!(
        accumulator.next_frame(),
        Err(FrameError::Decode(_))
    ));
    let oversized = (MAX_FRAME_BYTES as u32 + 1).to_be_bytes();
    assert!(matches!(
        read_frame_sync(&mut oversized.as_slice()),
        Err(FrameError::TooLarge(_))
    ));
    assert!(matches!(
        read_frame(&mut oversized.as_slice()).await,
        Err(FrameError::TooLarge(_))
    ));
    let too_large = tmux_agent_protocol::envelope(
        1,
        0,
        v1::envelope::Payload::Request(v1::Request {
            data: vec![0; MAX_FRAME_BYTES],
            ..Default::default()
        }),
    );
    assert!(matches!(
        encode_frame(&too_large),
        Err(FrameError::TooLarge(_))
    ));
}

#[test]
fn length_delimited_frame_round_trips() {
    let envelope = tmux_agent_protocol::envelope(
        7,
        3,
        v1::envelope::Payload::ClientHello(v1::ClientHello {
            bulk_connection: false,
            ..Default::default()
        }),
    );
    let bytes = encode_frame(&envelope).unwrap();
    let decoded = read_frame_sync(&mut bytes.as_slice()).unwrap().unwrap();
    assert_eq!(decoded, envelope);
}

#[test]
fn truncated_length_prefix_is_an_error_not_a_clean_disconnect() {
    let error = read_frame_sync(&mut [0_u8, 0].as_slice()).unwrap_err();
    assert!(error.to_string().contains("frame I/O failed"));
}

#[test]
fn an_unknown_terminal_history_event_is_inert_rather_than_a_seed() {
    let answer = v1::HostEvent {
        kind: v1::EventKind::TerminalHistory.into(),
        terminal: Some(v1::TerminalBytes {
            pane_id: "%3".into(),
            data: b"scrollback".to_vec(),
            generation: 0,
            ..Default::default()
        }),
        ..Default::default()
    };
    let decoded = v1::HostEvent::decode(answer.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded.kind, 19);
    assert_ne!(decoded.kind, v1::EventKind::TerminalSeed as i32);
    assert_ne!(decoded.kind, v1::EventKind::TerminalOutput as i32);
    // The shape a peer that has never heard of 19 sees: `try_from` fails, and
    // the fallback is the unspecified kind — an event it drops, never a screen
    // it applies.
    assert!(v1::EventKind::try_from(999).is_err());
    assert_eq!(
        v1::EventKind::try_from(999).unwrap_or_default(),
        v1::EventKind::Unspecified
    );

    // An unknown operation number is refused at admission rather than run as
    // its neighbour.
    assert!(v1::Operation::try_from(53).is_ok());
    assert!(v1::Operation::try_from(54).is_ok());
    assert!(v1::Operation::try_from(55).is_ok());
    assert!(v1::Operation::try_from(56).is_err());
}
