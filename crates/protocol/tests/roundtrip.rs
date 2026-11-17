// Framing and schema invariants. Cross-language byte/u64 coverage lives in
// mobile_contract.rs and the mobile Rust-contract test.
use prost::Message;
use tmux_agent_protocol::{PROTOCOL_MAJOR, encode_frame, read_frame_sync, v1};

#[test]
fn assigned_operation_and_enum_numbers_do_not_move() {
    use v1::Operation::*;
    assert_eq!(PROTOCOL_MAJOR, 5);
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
        (ResizeTerminalWindow, 56),
        (Chat, 60),
        (SessionHistory, 61),
        (TestDelay, 100),
    ] {
        assert_eq!(operation as i32, number);
    }
    assert_eq!(v1::TmuxActionKind::SetPinned as i32, 20);
    assert_eq!(v1::TmuxActionKind::MoveWindow as i32, 21);
    assert_eq!(v1::TmuxActionKind::JoinPane as i32, 22);
    assert_eq!(v1::TmuxActionKind::BreakPane as i32, 23);
    assert_eq!(v1::PaneEdge::Bottom as i32, 4);
    assert_eq!(v1::EventKind::TerminalHistory as i32, 19);
    assert_eq!(v1::EventKind::VoiceProvision as i32, 20);
    assert_eq!(v1::EventKind::VoiceReply as i32, 21);
    assert_eq!(v1::EventKind::VoiceReplyText as i32, 23);
    assert_eq!(v1::EventKind::Chat as i32, 24);
    assert_eq!(v1::EventKind::SessionHistory as i32, 25);
    assert_eq!(v1::ChatAction::SetMode as i32, 10);
    assert_eq!(v1::HistoryAction::Wake as i32, 4);
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
    // Chat and history: Request 27/28, Response 16/17, HostEvent 15/16,
    // Pane 15 and AgentSnapshot 10.
    assert_eq!(
        v1::Request {
            chat: Some(v1::ChatRequest::default()),
            history: Some(v1::HistoryRequest::default()),
            ..Default::default()
        }
        .encode_to_vec(),
        [0xda, 1, 0, 0xe2, 1, 0]
    );
    assert_eq!(
        v1::Response {
            chat: Some(v1::ChatResponse::default()),
            history: Some(v1::HistoryResponse::default()),
            ..Default::default()
        }
        .encode_to_vec(),
        [0x82, 1, 0, 0x8a, 1, 0]
    );
    assert_eq!(
        v1::HostEvent {
            chat: Some(v1::ChatEvent {
                revision: 3,
                change: Some(v1::chat_event::Change::Truncate(v1::ChatTruncate::default())),
                ..Default::default()
            }),
            history: Some(v1::HistoryEvent::default()),
            ..Default::default()
        }
        .encode_to_vec(),
        [0x7a, 4, 0x10, 3, 0x3a, 0, 0x82, 1, 0]
    );
    assert_eq!(
        v1::Pane {
            chat_adapter_id: "x".into(),
            ..Default::default()
        }
        .encode_to_vec(),
        [0x7a, 1, b'x']
    );
    assert_eq!(
        v1::AgentSnapshot {
            agent_mode: v1::ChatAgentMode::Chat.into(),
            ..Default::default()
        }
        .encode_to_vec(),
        [0x50, 2]
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
fn chat_attachments_tasks_subagents_fork_and_summary_base_do_not_move() {
    // Each message is encoded, compared byte for byte, and decoded back.
    fn pinned<M: Message + Default + PartialEq + std::fmt::Debug>(message: M, bytes: &[u8]) {
        assert_eq!(message.encode_to_vec(), bytes);
        assert_eq!(M::decode(bytes).unwrap(), message);
    }
    assert_eq!(v1::ChatAction::Fork as i32, 11);
    assert_eq!(v1::ChatAction::StopTask as i32, 12);
    assert_eq!(v1::ChatTaskKind::Other as i32, 3);
    assert_eq!(v1::GitSummaryBase::Head as i32, 1);
    // ChatState.background 11 and compacting 12.
    pinned(
        v1::ChatState {
            background: vec![v1::ChatBackgroundTask {
                id: "t".into(),
                kind: v1::ChatTaskKind::Command.into(),
                description: "d".into(),
                started_at_unix_millis: 5,
                stoppable: true,
            }],
            compacting: true,
            ..Default::default()
        },
        &[
            0x5a, 12, 0x0a, 1, b't', 0x10, 1, 0x1a, 1, b'd', 0x20, 5, 0x28, 1, 0x60, 1,
        ],
    );
    // ChatTool's subagent figures, 11 to 15.
    pinned(
        v1::ChatTool {
            kind: v1::ChatToolKind::Subagent.into(),
            agent_type: "E".into(),
            activity: "a".into(),
            tool_uses: 3,
            tokens: 300,
            background: true,
            ..Default::default()
        },
        &[
            0x08, 7, 0x5a, 1, b'E', 0x62, 1, b'a', 0x68, 3, 0x70, 0xac, 0x02, 0x78, 1,
        ],
    );
    // SEND's attachments 13; a USER item's attachments 8.
    pinned(
        v1::ChatRequest {
            action: v1::ChatAction::Send.into(),
            attachments: vec![v1::ChatAttachment {
                path: "/p".into(),
                name: "n".into(),
                media_type: "i/p".into(),
                size_bytes: 7,
            }],
            ..Default::default()
        },
        &[
            0x08, 3, 0x6a, 14, 0x0a, 2, b'/', b'p', 0x12, 1, b'n', 0x1a, 3, b'i', b'/', b'p', 0x20,
            7,
        ],
    );
    pinned(
        v1::ChatItem {
            attachments: vec![v1::ChatAttachment::default()],
            ..Default::default()
        },
        &[0x42, 0],
    );
    // FORK takes item_id 8 and answers forked_pane_id 3.
    pinned(
        v1::ChatRequest {
            action: v1::ChatAction::Fork.into(),
            item_id: "i".into(),
            ..Default::default()
        },
        &[0x08, 11, 0x42, 1, b'i'],
    );
    pinned(
        v1::ChatResponse {
            forked_pane_id: "%9".into(),
            ..Default::default()
        },
        &[0x1a, 2, b'%', b'9'],
    );
    // STOP_TASK takes task_id 14.
    pinned(
        v1::ChatRequest {
            action: v1::ChatAction::StopTask.into(),
            task_id: "t".into(),
            ..Default::default()
        },
        &[0x08, 12, 0x72, 1, b't'],
    );
    // GitRequest.summary_base 19; the default branch is the zero value.
    pinned(
        v1::GitRequest {
            summary_base: v1::GitSummaryBase::Head.into(),
            ..Default::default()
        },
        &[0x98, 1, 1],
    );
    pinned(v1::GitRequest::default(), &[]);
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
    assert!(v1::Operation::try_from(i32::MAX).is_err());
}
