use std::{
    collections::BTreeMap,
    io::BufReader,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use anyhow::{Context, bail};
use serde::Serialize;
use tmux_agent_protocol::{
    PROTOCOL_MAJOR, envelope, read_frame_sync,
    v1::{self, envelope::Payload},
    write_frame_sync,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    transport: String,
    helper_version: String,
    server_identity: String,
    sessions: usize,
    windows: usize,
    panes: usize,
    sequence_gap_detected: bool,
    overflow_detected: bool,
    cancellation: String,
    scoped_snapshot: bool,
    resync_sequence: u64,
    terminal_input_routed: bool,
    offline_output_reseeded: bool,
    phase2_actions: bool,
    event_counts: BTreeMap<String, usize>,
}

pub fn run(arguments: Vec<String>) -> anyhow::Result<()> {
    let testing = std::env::var_os("ADE_PHASE1_TESTING").is_some();
    let mut child = spawn_bridge(&arguments)?;
    let mut stdin = child.stdin.take().context("bridge stdin unavailable")?;
    let stdout = child.stdout.take().context("bridge stdout unavailable")?;
    let mut reader = BufReader::new(stdout);

    let requested_major = value_after(&arguments, "--protocol-major")
        .and_then(|value| value.parse().ok())
        .unwrap_or(PROTOCOL_MAJOR);
    let hello_request = v1::Envelope {
        protocol_major: requested_major,
        request_id: 1,
        sequence: 0,
        stream_id: 0,
        priority: v1::Priority::Control.into(),
        payload: Some(Payload::ClientHello(v1::ClientHello {
            bulk_connection: false,
            ..Default::default()
        })),
    };
    write_frame_sync(&mut stdin, &hello_request)?;
    let hello_frame = read_frame_sync(&mut reader)?.context("bridge closed during handshake")?;
    let server_protocol_major = hello_frame.protocol_major;
    let Some(Payload::ServerHello(hello)) = hello_frame.payload else {
        bail!("host did not return ServerHello");
    };
    if arguments.iter().any(|value| value == "--handshake-only") {
        println!(
            "{}",
            serde_json::json!({
                "protocolMajor": server_protocol_major,
                "helperVersion": hello.helper_version,
                "serverIdentity": hello.server_identity,
            })
        );
        stop_child(&mut child);
        return Ok(());
    }

    send_request(
        &mut stdin,
        2,
        v1::Operation::Subscribe,
        v1::Request::default(),
    )?;
    let mut tracker = Tracker::default();
    let response = wait_response(&mut reader, 2, &mut tracker)?;
    let snapshot = response.snapshot.context("subscribe omitted snapshot")?;
    let mut report = Report {
        transport: if arguments.first().is_some_and(|value| value == "ssh") {
            "ssh".into()
        } else {
            "local".into()
        },
        helper_version: hello.helper_version,
        server_identity: hello.server_identity,
        sessions: snapshot.sessions.len(),
        windows: snapshot.windows.len(),
        panes: snapshot.panes.len(),
        sequence_gap_detected: false,
        overflow_detected: false,
        cancellation: "not-run".into(),
        scoped_snapshot: false,
        resync_sequence: response.accepted_sequence,
        terminal_input_routed: false,
        offline_output_reseeded: false,
        phase2_actions: false,
        event_counts: BTreeMap::new(),
    };
    tracker.last_sequence = response.accepted_sequence;

    if arguments.iter().any(|value| value == "--phase2-actions") {
        run_phase2_actions(
            &mut stdin,
            &mut reader,
            &mut tracker,
            &snapshot,
            &report.server_identity,
        )?;
        report.phase2_actions = true;
        report.event_counts = tracker.event_counts;
        println!("{}", serde_json::to_string_pretty(&report)?);
        stop_child(&mut child);
        return Ok(());
    }

    if let Some(expected) =
        value_after(&arguments, "--watch-panes").and_then(|value| value.parse::<usize>().ok())
    {
        tracker.max_panes = snapshot.panes.len();
        while tracker.max_panes < expected {
            read_event(&mut reader, &mut tracker)?;
        }
        report.event_counts = tracker.event_counts;
        report.resync_sequence = tracker.last_sequence;
        println!("{}", serde_json::to_string_pretty(&report)?);
        stop_child(&mut child);
        return Ok(());
    }

    if !testing {
        println!("{}", serde_json::to_string_pretty(&report)?);
        stop_child(&mut child);
        return Ok(());
    }

    let requested_terminal_session = value_after(&arguments, "--terminal-session");
    let terminal_session = if let Some(requested) = requested_terminal_session.as_deref() {
        Some(
            snapshot
                .sessions
                .iter()
                .find(|session| session.id == requested || session.name == requested)
                .with_context(|| format!("terminal session {requested:?} is unavailable"))?,
        )
    } else {
        snapshot.sessions.first()
    };
    let terminal_pane = terminal_session.and_then(|session| {
        snapshot
            .panes
            .iter()
            .find(|pane| pane.session_id == session.id)
    });
    if let (Some(session), Some(pane)) = (terminal_session, terminal_pane) {
        send_request(
            &mut stdin,
            3,
            v1::Operation::AttachTerminal,
            v1::Request {
                session_id: session.id.clone(),
                pane_ids: snapshot
                    .panes
                    .iter()
                    .filter(|item| item.session_id == session.id)
                    .map(|item| item.id.clone())
                    .collect(),
                ..Default::default()
            },
        )?;
        wait_response(&mut reader, 3, &mut tracker)?;

        if let Some(delay_millis) = value_after(&arguments, "--interrupt-delay-ms")
            .and_then(|value| value.parse::<u64>().ok())
        {
            send_request(
                &mut stdin,
                9,
                v1::Operation::TestDelay,
                v1::Request {
                    delay_millis,
                    ..Default::default()
                },
            )?;
            wait_response(&mut reader, 9, &mut tracker)?;
            bail!("interruptible request completed before transport interruption");
        }

        send_request(
            &mut stdin,
            4,
            v1::Operation::TerminalInput,
            v1::Request {
                scope: pane.id.clone(),
                data: b"printf x >> /tmp/muxflow-phase1-input-count # PHASE1_PROTOCOL_INPUT_OK\r"
                    .to_vec(),
                ..Default::default()
            },
        )?;
        wait_response(&mut reader, 4, &mut tracker)?;
        // The integration harness verifies the addressed shell side effect and
        // exact byte count independently on both local and SSH hosts.
        report.terminal_input_routed = true;
    }

    send_request(
        &mut stdin,
        5,
        v1::Operation::FullSnapshot,
        v1::Request {
            scope: "topology".into(),
            ..Default::default()
        },
    )?;
    let scoped = wait_response(&mut reader, 5, &mut tracker)?;
    report.scoped_snapshot = scoped.ok && scoped.snapshot.is_some();

    send_request(
        &mut stdin,
        10,
        v1::Operation::TestDelay,
        v1::Request {
            delay_millis: 5_000,
            ..Default::default()
        },
    )?;
    write_frame_sync(
        &mut stdin,
        &envelope(
            11,
            0,
            Payload::Cancel(v1::Cancel {
                target_request_id: 10,
            }),
        ),
    )?;
    let cancelled = wait_response(&mut reader, 10, &mut tracker)?;
    report.cancellation = if cancelled.error_code == "cancelled" {
        "pass"
    } else {
        "failed"
    }
    .into();

    send_request(
        &mut stdin,
        20,
        v1::Operation::TestInjectGap,
        v1::Request::default(),
    )?;
    wait_response(&mut reader, 20, &mut tracker)?;
    if !tracker.gap_detected {
        read_event(&mut reader, &mut tracker)?;
    }
    report.sequence_gap_detected = tracker.gap_detected;
    send_request(
        &mut stdin,
        21,
        v1::Operation::Resync,
        v1::Request::default(),
    )?;
    let resync = wait_response(&mut reader, 21, &mut tracker)?;
    report.resync_sequence = resync.accepted_sequence;
    tracker.last_sequence = resync.accepted_sequence;
    tracker.gap_detected = false;

    send_request(
        &mut stdin,
        30,
        v1::Operation::TestOverflow,
        v1::Request::default(),
    )?;
    wait_response(&mut reader, 30, &mut tracker)?;
    if !tracker.gap_detected && !tracker.resync_required {
        for _ in 0..256 {
            read_event(&mut reader, &mut tracker)?;
            if tracker.gap_detected || tracker.resync_required {
                break;
            }
        }
    }
    report.overflow_detected = tracker.gap_detected || tracker.resync_required;
    send_request(
        &mut stdin,
        31,
        v1::Operation::Resync,
        v1::Request::default(),
    )?;
    let resync = wait_response(&mut reader, 31, &mut tracker)?;
    report.resync_sequence = resync.accepted_sequence;
    report.offline_output_reseeded = tracker.saw_offline_output;
    report.event_counts = tracker.event_counts;

    println!("{}", serde_json::to_string_pretty(&report)?);
    stop_child(&mut child);
    Ok(())
}

fn run_phase2_actions(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    tracker: &mut Tracker,
    snapshot: &v1::Snapshot,
    server_identity: &str,
) -> anyhow::Result<()> {
    let original_session = snapshot
        .sessions
        .first()
        .context("phase2 actions need a session")?;
    let original_window = snapshot
        .windows
        .first()
        .context("phase2 actions need a window")?;
    let original_pane = snapshot
        .panes
        .first()
        .context("phase2 actions need a pane")?;
    let mut generation = snapshot.generation;
    let mut next_request = 1000;
    let mut invoke = |action: v1::TmuxAction| -> anyhow::Result<v1::TmuxActionResult> {
        next_request += 1;
        send_request(
            stdin,
            next_request,
            v1::Operation::FullSnapshot,
            v1::Request {
                scope: "topology".into(),
                ..Default::default()
            },
        )?;
        generation = wait_response(reader, next_request, tracker)?
            .snapshot
            .context("phase2 preflight omitted snapshot")?
            .generation;
        next_request += 1;
        send_request(
            stdin,
            next_request,
            v1::Operation::TmuxAction,
            v1::Request {
                tmux_action: Some(v1::TmuxAction {
                    expected_server_identity: server_identity.into(),
                    expected_generation: generation,
                    ..action
                }),
                ..Default::default()
            },
        )?;
        let response = wait_response(reader, next_request, tracker)?;
        if !response.ok {
            bail!(
                "phase2 action failed ({}): {}",
                response.error_code,
                response.display_message
            );
        }
        let result = response
            .tmux_action_result
            .context("phase2 action omitted result")?;
        generation = result.topology_generation;
        Ok(result)
    };

    let created_session = invoke(v1::TmuxAction {
        kind: v1::TmuxActionKind::CreateSession.into(),
        name: "ade-phase2-created".into(),
        ..Default::default()
    })?
    .session_id;
    invoke(v1::TmuxAction {
        kind: v1::TmuxActionKind::ReorderSession.into(),
        session_id: created_session.clone(),
        index: 0,
        ..Default::default()
    })?;
    invoke(v1::TmuxAction {
        kind: v1::TmuxActionKind::RenameSession.into(),
        session_id: created_session.clone(),
        name: "ade-phase2-renamed".into(),
        ..Default::default()
    })?;
    invoke(v1::TmuxAction {
        kind: v1::TmuxActionKind::SelectSession.into(),
        session_id: original_session.id.clone(),
        ..Default::default()
    })?;

    let created_window = invoke(v1::TmuxAction {
        kind: v1::TmuxActionKind::CreateWindow.into(),
        session_id: original_session.id.clone(),
        name: "phase2-window".into(),
        ..Default::default()
    })?
    .window_id;
    invoke(v1::TmuxAction {
        kind: v1::TmuxActionKind::RenameWindow.into(),
        window_id: created_window.clone(),
        name: "phase2-renamed".into(),
        ..Default::default()
    })?;
    invoke(v1::TmuxAction {
        kind: v1::TmuxActionKind::ReorderWindow.into(),
        session_id: original_session.id.clone(),
        window_id: created_window.clone(),
        target_window_id: original_window.id.clone(),
        relative_position: v1::WindowRelativePosition::Before.into(),
        ..Default::default()
    })?;
    invoke(v1::TmuxAction {
        kind: v1::TmuxActionKind::SelectWindow.into(),
        window_id: original_window.id.clone(),
        ..Default::default()
    })?;

    let split_pane = invoke(v1::TmuxAction {
        kind: v1::TmuxActionKind::SplitPaneRight.into(),
        pane_id: original_pane.id.clone(),
        split_size: 40,
        ..Default::default()
    })?
    .pane_id;
    invoke(v1::TmuxAction {
        kind: v1::TmuxActionKind::FocusPane.into(),
        pane_id: split_pane.clone(),
        ..Default::default()
    })?;
    invoke(v1::TmuxAction {
        kind: v1::TmuxActionKind::ResizePaneLeft.into(),
        pane_id: split_pane.clone(),
        resize_cells: 1,
        ..Default::default()
    })?;
    for zoomed in [true, false] {
        invoke(v1::TmuxAction {
            kind: v1::TmuxActionKind::ZoomPane.into(),
            pane_id: split_pane.clone(),
            zoomed,
            ..Default::default()
        })?;
    }
    invoke(v1::TmuxAction {
        kind: v1::TmuxActionKind::ClosePane.into(),
        pane_id: split_pane,
        confirmed: true,
        ..Default::default()
    })?;
    invoke(v1::TmuxAction {
        kind: v1::TmuxActionKind::CloseWindow.into(),
        window_id: created_window,
        confirmed: true,
        ..Default::default()
    })?;
    invoke(v1::TmuxAction {
        kind: v1::TmuxActionKind::CloseSession.into(),
        session_id: created_session,
        confirmed: true,
        ..Default::default()
    })?;
    Ok(())
}

fn spawn_bridge(arguments: &[String]) -> anyhow::Result<Child> {
    if arguments.first().is_some_and(|value| value == "ssh") {
        let target = arguments
            .get(1)
            .context("phase1-client ssh requires a target")?;
        let mut command = Command::new("ssh");
        if let Some(config) = value_after(arguments, "--config") {
            command.arg("-F").arg(config);
        }
        let remote_command = if std::env::var_os("ADE_PHASE1_TESTING").is_some() {
            "ADE_PHASE1_TESTING=1 $HOME/.local/bin/muxflow-host bridge --stdio"
        } else {
            "$HOME/.local/bin/muxflow-host bridge --stdio"
        };
        command.args(["-T", "-o", "BatchMode=yes"]);
        if let Some(socket) = value_after(arguments, "--control-socket") {
            command.arg("-S").arg(socket);
        }
        command.arg(target).arg(remote_command);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("start SSH bridge")
    } else {
        Command::new(std::env::current_exe()?)
            .args(["bridge", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("start local bridge")
    }
}

fn value_after(arguments: &[String], flag: &str) -> Option<String> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

fn send_request(
    stdin: &mut ChildStdin,
    request_id: u64,
    operation: v1::Operation,
    mut request: v1::Request,
) -> anyhow::Result<()> {
    request.operation = operation.into();
    write_frame_sync(stdin, &envelope(request_id, 0, Payload::Request(request)))?;
    Ok(())
}

#[derive(Default)]
struct Tracker {
    last_sequence: u64,
    gap_detected: bool,
    resync_required: bool,
    saw_offline_output: bool,
    event_counts: BTreeMap<String, usize>,
    max_panes: usize,
}

fn wait_response(
    reader: &mut BufReader<ChildStdout>,
    request_id: u64,
    tracker: &mut Tracker,
) -> anyhow::Result<v1::Response> {
    loop {
        let frame = read_frame_sync(reader)?.context("bridge closed while awaiting response")?;
        if frame.request_id == request_id
            && let Some(Payload::Response(response)) = frame.payload.clone()
        {
            return Ok(response);
        }
        track_event(frame, tracker);
    }
}

fn read_event(reader: &mut BufReader<ChildStdout>, tracker: &mut Tracker) -> anyhow::Result<()> {
    loop {
        let frame = read_frame_sync(reader)?.context("bridge closed while awaiting event")?;
        if matches!(frame.payload, Some(Payload::Event(_))) {
            track_event(frame, tracker);
            return Ok(());
        }
    }
}

fn track_event(frame: v1::Envelope, tracker: &mut Tracker) {
    let Some(Payload::Event(event)) = frame.payload else {
        return;
    };
    if frame.sequence != tracker.last_sequence.saturating_add(1) {
        tracker.gap_detected = true;
    }
    tracker.last_sequence = frame.sequence;
    let kind = format!(
        "{:?}",
        v1::EventKind::try_from(event.kind).unwrap_or_default()
    );
    *tracker.event_counts.entry(kind).or_default() += 1;
    if let Some(terminal) = event.terminal
        && terminal
            .data
            .windows(b"PHASE1_OFFLINE_OUTPUT".len())
            .any(|window| window == b"PHASE1_OFFLINE_OUTPUT")
    {
        tracker.saw_offline_output = true;
    }
    if let Some(snapshot) = event.snapshot {
        tracker.max_panes = tracker.max_panes.max(snapshot.panes.len());
    }
    if v1::EventKind::try_from(event.kind).unwrap_or_default() == v1::EventKind::ResyncRequired {
        tracker.resync_required = true;
    }
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}
