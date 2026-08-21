use prost::Message;
use tmux_agent_protocol::{
    PROTOCOL_MAJOR, PROTOCOL_MINOR, encode_frame, envelope, read_frame_sync, v1,
    v1::envelope::Payload,
};

#[test]
fn terminal_upload_reconciliation_operation_is_append_only() {
    assert_eq!(v1::Operation::ReconcileTerminalUpload as i32, 41);
    assert_eq!((PROTOCOL_MAJOR, PROTOCOL_MINOR), (2, 0));
}

#[test]
fn terminal_bytes_round_trip_without_utf8_conversion() {
    let envelope = v1::Envelope {
        protocol_major: PROTOCOL_MAJOR,
        protocol_minor: PROTOCOL_MINOR,
        request_id: 4,
        sequence: 9,
        stream_id: 12,
        priority: v1::Priority::Control.into(),
        payload: Some(v1::envelope::Payload::TerminalBytes(v1::TerminalBytes {
            pane_id: "%3".into(),
            data: vec![0, 0xff, 0x1b, b'[', b'H'],
            generation: 7,
        })),
    };

    let decoded = v1::Envelope::decode(envelope.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded, envelope);
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
fn typed_tmux_action_round_trips_with_stale_and_confirmation_preconditions() {
    let action = v1::TmuxAction {
        kind: v1::TmuxActionKind::ClosePane.into(),
        pane_id: "%9".into(),
        expected_server_identity: "socket:123".into(),
        expected_generation: 44,
        confirmed: true,
        ..Default::default()
    };
    let envelope = tmux_agent_protocol::envelope(
        8,
        0,
        v1::envelope::Payload::Request(v1::Request {
            operation: v1::Operation::TmuxAction.into(),
            tmux_action: Some(action.clone()),
            ..Default::default()
        }),
    );
    let decoded = v1::Envelope::decode(envelope.encode_to_vec().as_slice()).unwrap();
    let Some(v1::envelope::Payload::Request(request)) = decoded.payload else {
        panic!("expected request");
    };
    assert_eq!(request.tmux_action, Some(action));
}

#[test]
fn sparse_window_reorder_round_trips_explicit_relative_target() {
    let action = v1::TmuxAction {
        kind: v1::TmuxActionKind::ReorderWindow.into(),
        session_id: "$1".into(),
        window_id: "@5".into(),
        target_window_id: "@3".into(),
        relative_position: v1::WindowRelativePosition::Before.into(),
        ..Default::default()
    };
    let bytes = action.encode_to_vec();
    assert_eq!(v1::TmuxAction::decode(bytes.as_slice()).unwrap(), action);
}

#[test]
fn terminal_visibility_checkpoint_round_trips_without_reusing_fields() {
    let request = v1::Request {
        operation: v1::Operation::SetTerminalVisibility.into(),
        scope: "%2".into(),
        visible: false,
        data: b"snapshot".to_vec(),
        terminal_epoch: 7,
        terminal_generation_cutoff: 42,
        ..Default::default()
    };
    let bytes = request.encode_to_vec();
    assert_eq!(v1::Request::decode(bytes.as_slice()).unwrap(), request);
}

#[test]
fn topology_and_resource_recovery_fields_round_trip() {
    let event = v1::HostEvent {
        kind: v1::EventKind::PaneResource.into(),
        pane_resource: Some(v1::PaneResource {
            pane_id: "%9".into(),
            state: v1::PaneResourceState::Released.into(),
            generation: 12,
            snapshot_generation: 9,
            tail_through_generation: 12,
            requires_seed: true,
            recovery_reason: "hidden-pane LRU capacity was exceeded".into(),
            ..Default::default()
        }),
        snapshot: Some(v1::Snapshot {
            panes: vec![v1::Pane {
                id: "%9".into(),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let envelope =
        tmux_agent_protocol::envelope(0, 14, v1::envelope::Payload::Event(event.clone()));
    let decoded = v1::Envelope::decode(envelope.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded.payload, Some(v1::envelope::Payload::Event(event)));
}

#[test]
fn active_root_snapshot_round_trips_identity_generation_and_root_token() {
    let active_root = v1::ActiveRoot {
        pane_id: "%4".into(),
        root: "/work/tree".into(),
        root_token: "blake3-root-token".into(),
        git_worktree: true,
        server_identity: "socket:123".into(),
        topology_generation: 19,
        root_generation: 7,
    };
    let response = v1::Response {
        ok: true,
        file: Some(v1::FileServiceResponse {
            operation_id: "root-7".into(),
            active_root: Some(active_root.clone()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = response.encode_to_vec();
    let decoded = v1::Response::decode(bytes.as_slice()).unwrap();
    assert_eq!(decoded.file.unwrap().active_root, Some(active_root));
}

#[test]
fn file_mutation_confirmation_and_origin_root_capability_round_trip() {
    let file = v1::FileServiceRequest {
        operation_id: "move-2".into(),
        root: "/work/origin".into(),
        root_token: "origin-token".into(),
        path: "old/name.txt".into(),
        destination: "new/name.txt".into(),
        mutation: v1::FileMutationKind::Move.into(),
        overwrite_confirmed: true,
        non_empty_confirmed: true,
        ..Default::default()
    };
    let request = v1::Request {
        operation: v1::Operation::FileMutation.into(),
        file: Some(file.clone()),
        ..Default::default()
    };
    assert_eq!(
        v1::Request::decode(request.encode_to_vec().as_slice())
            .unwrap()
            .file,
        Some(file)
    );
}

#[test]
fn file_event_round_trips_root_capability_and_watch_lease_identity() {
    let event = v1::FileServiceEvent {
        operation_id: "external-change".into(),
        root_token: "root-capability".into(),
        watch_id: "editor-parent".into(),
        ..Default::default()
    };
    let decoded = v1::FileServiceEvent::decode(event.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded, event);
}

#[test]
fn transfer_chunk_uses_u64_offsets_and_carries_final_blake3() {
    let chunk = v1::TransferChunk {
        transfer_id: "transfer-1".into(),
        offset: u64::from(u32::MAX) + 4096,
        data: vec![1, 2, 3],
        eof: true,
        total_bytes: 5 * 1024 * 1024 * 1024,
        blake3: "digest".into(),
        total_known: true,
    };
    let decoded = v1::TransferChunk::decode(chunk.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded, chunk);
}

#[test]
fn bulk_write_chunk_round_trips_lane_and_u64_accounting() {
    let hello = v1::ClientHello {
        desktop_version: "desktop".into(),
        requested_capabilities: u64::MAX,
        expected_helper_version: tmux_agent_protocol::HELPER_VERSION.into(),
        bulk_connection: true,
        expected_server_identity: "tmux:9007199254740993".into(),
        connection_epoch: 9_007_199_254_740_993,
    };
    let decoded = v1::ClientHello::decode(hello.encode_to_vec().as_slice()).unwrap();
    assert!(decoded.bulk_connection);
    assert_eq!(decoded.connection_epoch, 9_007_199_254_740_993);
    assert_eq!(decoded.expected_server_identity, "tmux:9007199254740993");
    let request = v1::Request {
        operation: v1::Operation::WriteFileChunk.into(),
        file: Some(v1::FileServiceRequest {
            operation_id: "save-1".into(),
            transfer_id: "write-1".into(),
            offset: (1_u64 << 53) + 9,
            total_bytes: u64::MAX,
            content: vec![1, 2, 3],
            blake3: "digest".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        v1::Request::decode(request.encode_to_vec().as_slice()).unwrap(),
        request
    );
}

#[test]
fn phase5_git_contract_round_trips_raw_paths_and_stale_guards() {
    let request = v1::GitRequest {
        operation_id: "git-7".into(),
        root: "/work/repo".into(),
        root_token: "root-capability".into(),
        expected_server_identity: "tmux:socket:42".into(),
        repository_id: "repository-hash".into(),
        expected_status_generation: (1_u64 << 53) + 7,
        expected_source_generation: "diff-hash".into(),
        connection_epoch: (1_u64 << 53) + 9,
        path: b"line\nraw\xff".to_vec(),
        original_path: b"old\tname".to_vec(),
        diff_target: v1::GitDiffTarget::Unstaged.into(),
        mutation: v1::GitMutationKind::DiscardHunk.into(),
        hunk_index: 3,
        confirmation_token: "one-time-token".into(),
        ..Default::default()
    };
    let envelope = tmux_agent_protocol::envelope(
        99,
        0,
        v1::envelope::Payload::Request(v1::Request {
            operation: v1::Operation::GitMutation.into(),
            git: Some(request.clone()),
            ..Default::default()
        }),
    );
    let decoded = v1::Envelope::decode(envelope.encode_to_vec().as_slice()).unwrap();
    let Some(v1::envelope::Payload::Request(decoded)) = decoded.payload else {
        panic!("expected request")
    };
    assert_eq!(decoded.git, Some(request));

    let command = v1::GitCommandResult {
        exit_code: 0,
        applied: true,
        refresh_failed: true,
        refresh_error: "refresh cancelled after apply".into(),
        outcome: v1::GitCommandOutcome::PartialOrUnknown.into(),
        stdout_truncated: true,
        stderr_truncated: true,
        error: "transport cancelled after index changed".into(),
        pre_head_oid: "old-head".into(),
        post_head_oid: "new-head".into(),
        pre_index_generation: "old-index".into(),
        post_index_generation: "new-index".into(),
        post_state_authoritative: true,
        pre_status_generation: (1_u64 << 53) + 11,
        post_status_generation: (1_u64 << 53) + 12,
        status_omitted: true,
        ..Default::default()
    };
    assert_eq!(
        v1::GitCommandResult::decode(command.encode_to_vec().as_slice()).unwrap(),
        command
    );
}

#[test]
fn phase6_agent_contract_round_trips_exact_route_and_reconnect_watermark() {
    let record = v1::AgentRecord {
        agent_id: "codex:native-7".into(),
        adapter: v1::AgentAdapterKind::Codex.into(),
        adapter_id: "codex".into(),
        native_session_id: "native-7".into(),
        display_name: "Review agent".into(),
        route: Some(v1::AgentRoute {
            host_profile_id: "remote-dev".into(),
            server_identity: "tmux:/run/user/1000/tmux:42:1:2:3".into(),
            session_id: "$1".into(),
            session_name_fallback: "workspace".into(),
            window_id: "@2".into(),
            window_name_fallback: "agent".into(),
            pane_id: "%3".into(),
            pane_index_fallback: 0,
            agent_id: "codex:native-7".into(),
            attention_generation: (1_u64 << 53) + 7,
        }),
        lifecycle: v1::AgentLifecycleState::Blocked.into(),
        state_generation: (1_u64 << 53) + 5,
        attention_generation: (1_u64 << 53) + 7,
        attention_kind: "blocked".into(),
        present: true,
        ..Default::default()
    };
    let snapshot = v1::AgentSnapshot {
        generation: (1_u64 << 53) + 9,
        agents: vec![record],
        authoritative: true,
        notification_watermark: (1_u64 << 53) + 9,
        accepted_generation: (1_u64 << 53) + 9,
        adapters: vec![v1::AgentAdapterDescriptor {
            adapter: v1::AgentAdapterKind::Codex.into(),
            id: "codex".into(),
            display_name: "Codex".into(),
            supports_launch: true,
            supports_resume: true,
            supports_hooks: true,
            hook_config_path: "/home/test/.codex/hooks.json".into(),
            hook_events: vec!["Stop".into()],
            ..Default::default()
        }],
    };
    let decoded = v1::AgentSnapshot::decode(snapshot.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded, snapshot);
}

#[test]
fn phase6_hook_review_and_source_dedupe_identity_round_trip() {
    let request = v1::AgentRequest {
        adapter: v1::AgentAdapterKind::ClaudeCode.into(),
        adapter_id: "claude-code".into(),
        hook_management: v1::HookManagementAction::Review.into(),
        hook_management_target: v1::HookManagementAction::Uninstall.into(),
        hook_event: Some(v1::AgentHookEvent {
            adapter: v1::AgentAdapterKind::ClaudeCode.into(),
            adapter_id: "claude-code".into(),
            source_event_id: "event-9".into(),
            source_generation: u64::MAX,
            source_sequence_authoritative: true,
            pane_id: "%8".into(),
            origin_server_identity: "tmux:/run/user/1000/tmux:42".into(),
            payload_json: br#"{"hook_event_name":"Stop"}"#.to_vec(),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        v1::AgentRequest::decode(request.encode_to_vec().as_slice()).unwrap(),
        request
    );
    let plan = v1::HookManagementPlan {
        adapter_id: "claude-code".into(),
        before_preview: "{\n  \"token\": \"<redacted>\"\n}".into(),
        after_preview: "{\n  \"hooks\": {}\n}".into(),
        diff_preview: "--- before\n{}\n+++ after\n{}".into(),
        preview_truncated: true,
        ..Default::default()
    };
    assert_eq!(
        v1::HookManagementPlan::decode(plan.encode_to_vec().as_slice()).unwrap(),
        plan
    );
}

#[test]
fn phase7_terminal_upload_contract_round_trips_u64_and_opaque_names() {
    let request = v1::FileServiceRequest {
        operation_id: "upload-op".into(),
        transfer_id: "6ba7b810-9dad-11d1-80b4-00c04fd430c8".into(),
        destination: "- quote' \" newline\n東京.png".into(),
        source_name: "local source.png".into(),
        total_bytes: (5_u64 << 30) + 17,
        collision_policy: v1::CollisionPolicy::Rename.into(),
        large_upload_confirmed: true,
        image_png: true,
        ..Default::default()
    };
    let envelope = tmux_agent_protocol::envelope(
        701,
        0,
        v1::envelope::Payload::Request(v1::Request {
            operation: v1::Operation::PrepareTerminalUpload.into(),
            file: Some(request.clone()),
            ..Default::default()
        }),
    );
    let decoded = v1::Envelope::decode(envelope.encode_to_vec().as_slice()).unwrap();
    let Some(v1::envelope::Payload::Request(decoded)) = decoded.payload else {
        panic!("expected request")
    };
    assert_eq!(decoded.file, Some(request));

    let descriptor = v1::UploadDescriptor {
        transfer_id: "6ba7b810-9dad-11d1-80b4-00c04fd430c8".into(),
        destination_name: "東京.png".into(),
        total_bytes: (5_u64 << 30) + 17,
        available_bytes: u64::MAX,
        final_path: "/home/test/.cache/muxflow/uploads/東京.png".into(),
        verified: true,
        blake3: "a".repeat(64),
        ..Default::default()
    };
    assert_eq!(
        v1::UploadDescriptor::decode(descriptor.encode_to_vec().as_slice()).unwrap(),
        descriptor
    );

    let published_cleanup_failure = v1::Response {
        ok: false,
        error_code: "upload_commit_published_cleanup_failed".into(),
        display_message: "verified replacement published; backup retained".into(),
        publication_outcome: v1::PublicationOutcome::Published.into(),
        cleanup_failed: true,
        ..Default::default()
    };
    let decoded =
        v1::Response::decode(published_cleanup_failure.encode_to_vec().as_slice()).unwrap();
    assert_eq!(
        decoded.publication_outcome,
        v1::PublicationOutcome::Published as i32
    );
    assert!(decoded.cleanup_failed);
}

/// File-path/file-body operations are append-only: existing
/// operations keep their numbers and existing payload variants keep theirs, so
/// an older peer that does not know the operation still refuses it as
/// unsupported rather than misreading a neighbouring one.
///
/// 44 and 45 were added in the same round by two branches that each picked 44
/// independently. The file lane kept it and the Git lane took 45; this asserts
/// the settled assignment so neither can drift back.
#[test]
fn open_file_stream_operation_and_payload_are_append_only() {
    assert_eq!(v1::Operation::SelectTerminalSession as i32, 43);
    assert_eq!(v1::Operation::OpenFileStream as i32, 44);
    assert_eq!(v1::Operation::GitDiffContent as i32, 45);
    assert_eq!(v1::Operation::ResolveTerminalFile as i32, 46);
    assert_eq!(v1::Operation::TestDelay as i32, 100);
    assert!(v1::Operation::try_from(47).is_err());
}

#[test]
fn file_stream_header_and_body_round_trip_on_the_requests_own_id() {
    let header = envelope(
        77,
        0,
        Payload::FileStream(v1::FileStreamFrame {
            operation_id: "open-1".into(),
            header: Some(v1::FileStreamHeader {
                metadata: Some(v1::FileMetadata {
                    path: "/repo/note.txt".into(),
                    name: "note.txt".into(),
                    kind: v1::FileKind::File.into(),
                    size: u64::MAX,
                    generation: u64::MAX - 1,
                    ..Default::default()
                }),
                content_kind: v1::FileContentKind::Text.into(),
                generation: u64::MAX - 1,
                total_bytes: u64::MAX,
                content_streaming: true,
            }),
            ..Default::default()
        }),
    );
    let decoded = v1::Envelope::decode(header.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded.request_id, 77);
    let Some(Payload::FileStream(frame)) = decoded.payload else {
        panic!("expected a file stream frame")
    };
    let carried = frame.header.expect("a header");
    assert_eq!(carried.total_bytes, u64::MAX);
    assert_eq!(carried.generation, u64::MAX - 1);
    assert!(carried.content_streaming);
    assert_eq!(carried.metadata.unwrap().size, u64::MAX);
    // A header frame carries no body, so an offset of zero is unambiguous.
    assert!(frame.data.is_empty());
    assert!(!frame.eof);

    let body = envelope(
        77,
        0,
        Payload::FileStream(v1::FileStreamFrame {
            operation_id: "open-1".into(),
            offset: 1 << 40,
            data: vec![0, 159, 146, 150],
            eof: true,
            blake3: "digest".into(),
            ..Default::default()
        }),
    );
    let decoded = v1::Envelope::decode(body.encode_to_vec().as_slice()).unwrap();
    let Some(Payload::FileStream(frame)) = decoded.payload else {
        panic!("expected a file stream frame")
    };
    assert_eq!(frame.offset, 1 << 40);
    assert_eq!(
        frame.data,
        vec![0, 159, 146, 150],
        "bodies are opaque bytes"
    );
    assert!(frame.eof);
    assert_eq!(frame.blake3, "digest");
    assert!(frame.header.is_none());
}

#[test]
fn active_root_probe_round_trips_a_known_capability_and_its_unchanged_answer() {
    let request = v1::FileServiceRequest {
        operation_id: "probe".into(),
        pane_id: "%1".into(),
        known_root_token: "capability-7".into(),
        ..Default::default()
    };
    let decoded = v1::FileServiceRequest::decode(request.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded.known_root_token, "capability-7");

    let response = v1::FileServiceResponse {
        operation_id: "probe".into(),
        root_unchanged: true,
        ..Default::default()
    };
    let decoded = v1::FileServiceResponse::decode(response.encode_to_vec().as_slice()).unwrap();
    assert!(decoded.root_unchanged);
    assert!(decoded.directory.is_none());
}

#[test]
fn terminal_file_resolution_round_trips_its_exact_pane_route() {
    let request = v1::FileServiceRequest {
        pane_id: "%1".into(),
        path: "./src/main.rs".into(),
        expected_session_id: "$1".into(),
        expected_window_id: "@1".into(),
        expected_cwd: "/repo".into(),
        ..Default::default()
    };
    let decoded = v1::FileServiceRequest::decode(request.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded, request);
}

/// Single-request file opens are a required capability, and the requirement is
/// enforced by the same rule that names what is missing.
///
/// The daemon lives on a host the user upgrades separately from the app, so
/// "this helper predates the operation" is a real state. It is answered by
/// refusing the connection and reporting the exact missing capability by name —
/// not by a second, unexercised code path for opening files.
#[test]
fn operation_additions_are_required_capabilities() {
    use tmux_agent_protocol::{
        CAP_FILE_STREAM, CAP_TERMINAL_FILE_RESOLUTION, CAP_TERMINAL_OUTPUT_CREDIT,
        HOST_CAPABILITIES, capability_names, missing_host_capabilities,
    };
    assert_eq!(CAP_FILE_STREAM, 1 << 15);
    assert_eq!(CAP_TERMINAL_FILE_RESOLUTION, 1 << 16);
    // Append-only: every previously assigned bit keeps its position.
    assert_eq!(CAP_TERMINAL_OUTPUT_CREDIT, 1 << 14);

    // The rule the desktop's handshake applies — called, not restated. A copy
    // of it here would pass whatever the handshake did, which is the opposite
    // of what a drift test is for.
    assert_eq!(missing_host_capabilities(HOST_CAPABILITIES), 0);
    let outdated = HOST_CAPABILITIES & !CAP_FILE_STREAM;
    assert_eq!(
        missing_host_capabilities(outdated),
        CAP_FILE_STREAM,
        "a helper that cannot serve a single-request open must be refused"
    );
    // And the refusal names it, rather than reporting a hex mask nobody outside
    // the protocol crate can read.
    assert_eq!(
        capability_names(missing_host_capabilities(outdated)),
        vec!["fileStream"]
    );
    let pre_terminal_file_helper = HOST_CAPABILITIES & !CAP_TERMINAL_FILE_RESOLUTION;
    assert_eq!(
        missing_host_capabilities(pre_terminal_file_helper),
        CAP_TERMINAL_FILE_RESOLUTION,
        "a helper that cannot resolve terminal paths must be refused"
    );
    assert_eq!(
        capability_names(missing_host_capabilities(pre_terminal_file_helper)),
        vec!["terminalFileResolution"]
    );
    // Every required bit has a name, so no refusal can be unexplainable.
    assert!(!capability_names(HOST_CAPABILITIES).contains(&"unknown"));
    assert_eq!(
        capability_names(HOST_CAPABILITIES).len(),
        HOST_CAPABILITIES.count_ones() as usize
    );
}
