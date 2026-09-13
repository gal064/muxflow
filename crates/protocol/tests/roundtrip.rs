// Framing and schema invariants. Cross-language byte/u64 coverage lives in
// mobile_contract.rs and the mobile Rust-contract test.
use prost::Message;
use tmux_agent_protocol::{PROTOCOL_MAJOR, PROTOCOL_MINOR, encode_frame, read_frame_sync, v1};

#[test]
fn assigned_operation_and_enum_numbers_do_not_move() {
    use v1::Operation::*;
    assert_eq!((PROTOCOL_MAJOR, PROTOCOL_MINOR), (2, 1));
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
        terminal_input_agent_id: "a".into(),
        ..Default::default()
    };
    assert_eq!(
        request.encode_to_vec(),
        [
            0x58, 7, 0x60, 42, 0x80, 1, 1, 0x88, 1, 0xd0, 0x0f, 0x90, 1, 40, 0x98, 1, 1, 0xaa, 1,
            1, b'a',
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

#[test]
fn every_required_capability_is_named_and_enforced() {
    use tmux_agent_protocol::*;
    for (bit, index) in [
        (CAP_TERMINAL_OUTPUT_CREDIT, 14),
        (CAP_FILE_STREAM, 15),
        (CAP_TERMINAL_FILE_RESOLUTION, 16),
        (CAP_TMUX_EXECUTABLE_RESOLUTION, 17),
        (CAP_VOICE, 18),
    ] {
        assert_eq!(bit, 1 << index);
    }
    assert_eq!(missing_host_capabilities(HOST_CAPABILITIES), 0);
    assert_eq!(
        capability_names(HOST_CAPABILITIES).len(),
        HOST_CAPABILITIES.count_ones() as usize
    );
    assert!(!capability_names(HOST_CAPABILITIES).contains(&"unknown"));
    for index in 0..64 {
        let bit = 1_u64 << index;
        if HOST_CAPABILITIES & bit == 0 {
            continue;
        }
        let hello = v1::ServerHello {
            capabilities: HOST_CAPABILITIES & !bit,
            ..Default::default()
        };
        assert_eq!(
            validate_host_contract(PROTOCOL_MAJOR, &hello),
            Err(HostContractError::MissingCapabilities(bit))
        );
        assert_eq!(capability_names(bit).len(), 1);
    }
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
            desktop_version: "test".into(),
            requested_capabilities: u64::MAX,
            expected_helper_version: tmux_agent_protocol::HELPER_VERSION.into(),
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
fn a_renderer_handoff_across_a_version_skew_degrades_to_a_seed() {
    // Old desktop, new host: the hide still uploads a screen and neither the
    // hide nor the reveal carries the flag. The host's rule — a tail only for a
    // renderer that says it is still holding the screen the tail continues —
    // is therefore never satisfied, and every reveal is answered with a seed.
    let old_desktop_reveal = v1::Request {
        operation: v1::Operation::SetTerminalVisibility.into(),
        scope: "%2".into(),
        visible: true,
        data: b"a screen the host ignores".to_vec(),
        terminal_epoch: 7,
        terminal_generation_cutoff: 42,
        ..Default::default()
    };
    let decoded = v1::Request::decode(old_desktop_reveal.encode_to_vec().as_slice()).unwrap();
    assert!(!decoded.terminal_renderer_holds_snapshot);
    assert_eq!(decoded, old_desktop_reveal);

    // New desktop, old host: the flag rides in a field number the old host has
    // never heard of, and an unknown field is skipped rather than refused — so
    // the request is still a valid hide, just one whose empty payload that host
    // reads as "no recoverable screen". Its answer sets no
    // `resume_from_renderer`, which the desktop reads as seed debt.
    let new_desktop_hide = v1::Request {
        operation: v1::Operation::SetTerminalVisibility.into(),
        scope: "%2".into(),
        visible: false,
        terminal_epoch: 7,
        terminal_generation_cutoff: 42,
        terminal_renderer_holds_snapshot: true,
        ..Default::default()
    };
    let mut bytes = new_desktop_hide.encode_to_vec();
    // A field number neither peer assigns, to state the tolerance itself.
    bytes.extend_from_slice(&[0xf8, 0x06, 0x01]);
    let decoded = v1::Request::decode(bytes.as_slice()).unwrap();
    assert!(decoded.terminal_renderer_holds_snapshot);
    assert!(decoded.data.is_empty());

    let old_host_answer = v1::PaneResource {
        pane_id: "%2".into(),
        state: v1::PaneResourceState::Released.into(),
        requires_seed: true,
        recovery_reason: "renderer handoff omitted a recoverable snapshot".into(),
        ..Default::default()
    };
    let decoded = v1::PaneResource::decode(old_host_answer.encode_to_vec().as_slice()).unwrap();
    assert!(!decoded.resume_from_renderer);
    assert!(decoded.requires_seed);
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

    // And the same in the other direction: an operation number a host predating
    // 53 cannot resolve is refused at admission rather than run as its
    // neighbour.
    assert!(v1::Operation::try_from(53).is_ok());
    assert!(v1::Operation::try_from(54).is_err());
}
