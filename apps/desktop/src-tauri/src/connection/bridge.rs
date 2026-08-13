use std::{
    io::BufReader,
    process::{ChildStdin, ChildStdout},
    sync::{Arc, atomic::Ordering},
    thread,
    time::Duration,
};

use tauri::ipc::{Channel, InvokeResponseBody};
use tmux_agent_protocol::{
    HELPER_VERSION, HOST_CAPABILITIES, PROTOCOL_MAJOR, envelope, read_frame_sync,
    v1::{self, envelope::Payload},
    write_frame_sync,
};
use uuid::Uuid;

use super::event_frame::encode_event_with_sequence;
use super::transport::{BridgeStderr, spawn_bridge, with_bridge_diagnostic};
use super::{
    ConnectionSpec, InitialHostState, TerminalClient, TerminalEvent, mark_input_reconnected,
    send_event, snapshot_from_proto, validate_tmux_id,
};

pub(super) fn supervise_bridge(
    client_id: String,
    connection: ConnectionSpec,
    session_id: String,
    pane_ids: Vec<String>,
    channel: Channel<InvokeResponseBody>,
    client: Arc<TerminalClient>,
) {
    let mut attempt = 0_u32;
    while !client.stopped.load(Ordering::Acquire) {
        send_event(
            &channel,
            TerminalEvent::ConnectionState {
                state: if attempt == 0 {
                    "connecting"
                } else {
                    "reconnecting"
                }
                .into(),
            },
        );
        match run_bridge_once(
            &client_id,
            &connection,
            &session_id,
            &pane_ids,
            &channel,
            &client,
        ) {
            Ok(()) => {}
            Err(error) => send_event(&channel, TerminalEvent::Error { message: error }),
        }
        client.ready.store(false, Ordering::Release);
        client.stdin.lock().unwrap().take();
        client
            .fail_pending(
                "host transport disconnected; commit outcome is unknown and the request will not replay",
            );
        if let Some(mut child) = client.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if client.stopped.load(Ordering::Acquire) {
            break;
        }
        send_event(
            &channel,
            TerminalEvent::ConnectionState {
                state: "disconnected".into(),
            },
        );
        attempt = attempt.saturating_add(1);
        let delay = 200_u64.saturating_mul(2_u64.pow(attempt.min(5)))
            + reconnect_jitter(&client_id, attempt);
        thread::sleep(Duration::from_millis(delay));
    }
}

pub(super) fn reconnect_jitter(client_id: &str, attempt: u32) -> u64 {
    let mut value = u64::from(attempt).wrapping_mul(0x9e37_79b9);
    for byte in client_id.bytes() {
        value = value.rotate_left(5) ^ u64::from(byte);
    }
    value % 151
}

fn run_bridge_once(
    client_id: &str,
    connection: &ConnectionSpec,
    session_id: &str,
    pane_ids: &[String],
    channel: &Channel<InvokeResponseBody>,
    client: &Arc<TerminalClient>,
) -> Result<(), String> {
    let mut bridge = spawn_bridge(connection, client_id)?;
    let mut stdin = bridge.stdin.take().ok_or("host bridge stdin unavailable")?;
    let stdout = bridge
        .stdout
        .take()
        .ok_or("host bridge stdout unavailable")?;
    let diagnostic = bridge.stderr.take().map(BridgeStderr::capture);
    let mut reader = BufReader::new(stdout);
    let terminal_epoch = ((Uuid::new_v4().as_u128() as u64) & ((1_u64 << 53) - 1)).max(1);
    let (hello, initial, negotiated_writable) =
        handshake_and_snapshot(&mut stdin, &mut reader, terminal_epoch)
            .map_err(|error| with_bridge_diagnostic(error, diagnostic.as_ref()))?;
    // Terminal generations are scoped to one helper protocol connection.  Tell
    // the renderer to discard same-server generation watermarks before any seed
    // or output from the new connection is delivered.
    client
        .terminal_epoch
        .store(terminal_epoch, Ordering::Release);
    *client.server_identity.lock().unwrap() = hello.server_identity.clone();
    send_event(
        channel,
        TerminalEvent::GenerationEpoch {
            epoch: terminal_epoch,
        },
    );
    let mut sequence = 0;
    let mut terminal_scope_value = None;
    if let Some(initial) = initial {
        sequence = initial.accepted_sequence;
        terminal_scope_value = Some(terminal_scope(&initial.snapshot, session_id, pane_ids));
        send_protocol_event(
            channel,
            sequence,
            TerminalEvent::Snapshot {
                server_identity: hello.server_identity.clone(),
                snapshot: initial.snapshot,
                sequence,
                generation: initial.generation,
                authoritative: true,
            },
        )?;
        if let Some(agent_snapshot) = initial.agent_snapshot {
            let host_profile_id = client.host_profile_id.lock().unwrap().clone();
            let payload = serde_json::to_vec(&super::agent::with_connection_epoch(
                super::agent::snapshot_json(&agent_snapshot, &host_profile_id),
                terminal_epoch,
            ))
            .map_err(|error| error.to_string())?;
            send_protocol_event(
                channel,
                sequence,
                TerminalEvent::AgentService {
                    scope: "snapshot".into(),
                    payload,
                },
            )?;
        }
        for frame in initial.buffered_events {
            let (next, scoped_seed) =
                process_event(frame, sequence, &hello.server_identity, channel, client)?;
            sequence = next;
            if let Some(pane_id) = scoped_seed {
                write_scoped_seed_request(&mut stdin, client, pane_id)?;
            }
        }
    }
    let missing_capabilities = HOST_CAPABILITIES & !hello.capabilities;
    let read_only = !negotiated_writable;
    client.read_only.store(read_only, Ordering::Release);
    if read_only {
        if !hello.incompatibility.is_empty() {
            send_event(
                channel,
                TerminalEvent::Error {
                    message: hello.incompatibility.clone(),
                },
            );
        }
        if missing_capabilities != 0 {
            send_event(
                channel,
                TerminalEvent::Error {
                    message: format!(
                        "host helper is missing required capabilities 0x{missing_capabilities:x}"
                    ),
                },
            );
        }
        send_event(
            channel,
            TerminalEvent::ConnectionState {
                state: "readOnly".into(),
            },
        );
    } else if let Some((attach_session, attach_panes)) = terminal_scope_value
        && !attach_session.is_empty()
    {
        let attach_id = 3;
        write_frame_sync(
            &mut stdin,
            &envelope(
                attach_id,
                0,
                Payload::Request(v1::Request {
                    operation: v1::Operation::AttachTerminal.into(),
                    session_id: attach_session,
                    pane_ids: attach_panes,
                    ..Default::default()
                }),
            ),
        )
        .map_err(|error| error.to_string())?;
        let buffered = read_until_response(&mut reader, attach_id)?;
        for frame in buffered {
            let (next, scoped_seed) =
                process_event(frame, sequence, &hello.server_identity, channel, client)?;
            sequence = next;
            if let Some(pane_id) = scoped_seed {
                write_scoped_seed_request(&mut stdin, client, pane_id)?;
            }
        }
    }
    *client.stdin.lock().unwrap() = Some(stdin);
    *client.child.lock().unwrap() = Some(bridge);
    if !read_only {
        mark_input_reconnected(client);
        client.ready.store(true, Ordering::Release);
        send_event(
            channel,
            TerminalEvent::ConnectionState {
                state: "connected".into(),
            },
        );
    }
    read_protocol_stream(reader, sequence, &hello.server_identity, channel, client)
}

fn handshake_and_snapshot(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    connection_epoch: u64,
) -> Result<(v1::ServerHello, Option<InitialHostState>, bool), String> {
    write_frame_sync(
        stdin,
        &envelope(
            1,
            0,
            Payload::ClientHello(v1::ClientHello {
                desktop_version: env!("CARGO_PKG_VERSION").into(),
                requested_capabilities: HOST_CAPABILITIES,
                expected_helper_version: HELPER_VERSION.into(),
                bulk_connection: false,
                connection_epoch,
                ..Default::default()
            }),
        ),
    )
    .map_err(|error| error.to_string())?;
    let hello_frame = read_frame_sync(reader)
        .map_err(|error| error.to_string())?
        .ok_or("host closed during handshake")?;
    let envelope_major = hello_frame.protocol_major;
    let Some(Payload::ServerHello(hello)) = hello_frame.payload else {
        return Err("host did not return ServerHello".into());
    };
    if !handshake_allows_snapshot(envelope_major, &hello) {
        return Ok((hello, None, false));
    }

    write_frame_sync(
        stdin,
        &envelope(
            2,
            0,
            Payload::Request(v1::Request {
                operation: v1::Operation::Subscribe.into(),
                scope: "full".into(),
                ..Default::default()
            }),
        ),
    )
    .map_err(|error| error.to_string())?;
    let (response, buffered) = read_until_response_with_value(reader, 2)?;
    if !response.ok {
        return Err(format!(
            "{}: {}",
            response.error_code, response.display_message
        ));
    }
    let accepted_sequence = response.accepted_sequence;
    let snapshot = response
        .snapshot
        .ok_or("subscribe response omitted snapshot")?;
    if snapshot.server_identity != hello.server_identity {
        return Err(
            "tmux server changed during handshake; reconnecting for a coherent snapshot".into(),
        );
    }
    let generation = snapshot.generation;
    let agent_snapshot = snapshot.agents.clone();
    Ok((
        hello,
        Some(InitialHostState {
            snapshot: snapshot_from_proto(snapshot),
            agent_snapshot,
            accepted_sequence,
            generation,
            // A snapshot barrier already incorporates every ordered event at
            // or below its accepted sequence. Replaying an event that happened
            // to reach stdout before the response would make the bridge treat
            // that already-accepted sequence as a gap and reconnect. This race
            // is common while the first tmux controls are being attached.
            buffered_events: buffered
                .into_iter()
                .filter(|frame| event_follows_snapshot_barrier(frame, accepted_sequence))
                .collect(),
        }),
        true,
    ))
}

fn event_follows_snapshot_barrier(frame: &v1::Envelope, accepted_sequence: u64) -> bool {
    matches!(&frame.payload, Some(Payload::Event(_))) && frame.sequence > accepted_sequence
}

pub(super) fn handshake_allows_snapshot(envelope_major: u32, hello: &v1::ServerHello) -> bool {
    envelope_major == PROTOCOL_MAJOR
        && !hello.read_only
        && HOST_CAPABILITIES & !hello.capabilities == 0
}

fn read_until_response(
    reader: &mut BufReader<ChildStdout>,
    request_id: u64,
) -> Result<Vec<v1::Envelope>, String> {
    let (response, buffered) = read_until_response_with_value(reader, request_id)?;
    if response.ok {
        Ok(buffered)
    } else {
        Err(format!(
            "{}: {}",
            response.error_code, response.display_message
        ))
    }
}

fn read_until_response_with_value(
    reader: &mut BufReader<ChildStdout>,
    request_id: u64,
) -> Result<(v1::Response, Vec<v1::Envelope>), String> {
    let mut buffered = Vec::new();
    loop {
        let frame = read_frame_sync(reader)
            .map_err(|error| error.to_string())?
            .ok_or("host disconnected while awaiting response")?;
        if frame.request_id == request_id
            && let Some(Payload::Response(response)) = frame.payload.clone()
        {
            return Ok((response, buffered));
        }
        buffered.push(frame);
    }
}

fn read_protocol_stream(
    mut reader: BufReader<ChildStdout>,
    mut sequence: u64,
    server_identity: &str,
    channel: &Channel<InvokeResponseBody>,
    client: &Arc<TerminalClient>,
) -> Result<(), String> {
    let mut resync_request_id = None;
    loop {
        let frame = read_frame_sync(&mut reader)
            .map_err(|error| error.to_string())?
            .ok_or("host bridge closed")?;
        if let Some(Payload::Response(response)) = frame.payload.clone() {
            if resync_request_id == Some(frame.request_id) {
                if !response.ok {
                    return Err(format!("resync failed: {}", response.display_message));
                }
                let protocol_snapshot = response.snapshot.ok_or("resync omitted snapshot")?;
                let snapshot_identity = protocol_snapshot.server_identity.clone();
                if snapshot_identity != server_identity {
                    return Err("tmux server changed during resync".into());
                }
                let generation = protocol_snapshot.generation;
                send_protocol_event(
                    channel,
                    response.accepted_sequence,
                    TerminalEvent::Snapshot {
                        snapshot: snapshot_from_proto(protocol_snapshot),
                        sequence: response.accepted_sequence,
                        generation,
                        server_identity: snapshot_identity,
                        authoritative: true,
                    },
                )?;
                return Err(
                    "resync complete; reconnecting terminal for an authoritative screen seed"
                        .into(),
                );
            }
            if let Some(waiter) = client.pending.lock().unwrap().remove(&frame.request_id) {
                let _ = waiter.send(Ok(response));
            }
            continue;
        }
        if resync_request_id.is_some() {
            // Events received after a detected gap are superseded by the
            // authoritative resync barrier. Correlated responses above must
            // still be delivered to callers.
            continue;
        }
        match process_event(frame, sequence, server_identity, channel, client) {
            Ok((next, scoped_seed)) => {
                sequence = next;
                if let Some(pane_id) = scoped_seed {
                    let mut guard = client.stdin.lock().unwrap();
                    let stdin = guard.as_mut().ok_or("host bridge is disconnected")?;
                    write_scoped_seed_request(stdin, client, pane_id)?;
                }
            }
            Err(error) if error.starts_with("sequence gap") || error == "host requested resync" => {
                client.ready.store(false, Ordering::Release);
                send_event(
                    channel,
                    TerminalEvent::ConnectionState {
                        state: "resyncing".into(),
                    },
                );
                let request_id = client.next_request_id.fetch_add(1, Ordering::AcqRel);
                let mut guard = client.stdin.lock().unwrap();
                let stdin = guard.as_mut().ok_or("host bridge is disconnected")?;
                write_frame_sync(
                    stdin,
                    &envelope(
                        request_id,
                        0,
                        Payload::Request(v1::Request {
                            operation: v1::Operation::Resync.into(),
                            scope: "full".into(),
                            ..Default::default()
                        }),
                    ),
                )
                .map_err(|write_error| write_error.to_string())?;
                drop(guard);
                resync_request_id = Some(request_id);
            }
            Err(error) => return Err(error),
        }
    }
}

pub(super) fn terminal_scope(
    snapshot: &tmux_control::TmuxSnapshot,
    requested_session: &str,
    requested_panes: &[String],
) -> (String, Vec<String>) {
    let session_id = snapshot
        .sessions
        .iter()
        .find(|session| session.id == requested_session)
        .or_else(|| snapshot.sessions.first())
        .map(|session| session.id.clone())
        .unwrap_or_default();
    let available: Vec<_> = snapshot
        .panes
        .iter()
        .filter(|pane| pane.session_id == session_id)
        .map(|pane| pane.id.clone())
        .collect();
    let selected: Vec<_> = requested_panes
        .iter()
        .filter(|pane_id| available.contains(pane_id))
        .cloned()
        .collect();
    let mounted = if requested_panes.is_empty() {
        let active_window = snapshot
            .windows
            .iter()
            .find(|window| window.session_id == session_id && window.active)
            .map(|window| window.id.as_str());
        snapshot
            .panes
            .iter()
            .filter(|pane| Some(pane.window_id.as_str()) == active_window)
            .map(|pane| pane.id.clone())
            .collect()
    } else {
        selected
    };
    (session_id, mounted)
}

fn process_event(
    frame: v1::Envelope,
    last_sequence: u64,
    expected_server_identity: &str,
    channel: &Channel<InvokeResponseBody>,
    client: &Arc<TerminalClient>,
) -> Result<(u64, Option<String>), String> {
    let Some(Payload::Event(event)) = frame.payload else {
        return Ok((last_sequence, None));
    };
    validate_event_sequence(last_sequence, frame.sequence)?;
    let event_sequence = frame.sequence;
    let mut scoped_seed = None;
    match v1::EventKind::try_from(event.kind).unwrap_or_default() {
        v1::EventKind::TopologySnapshot => {
            let value = event
                .snapshot
                .ok_or("topology snapshot event omitted snapshot")?;
            let server_identity = value.server_identity.clone();
            if server_identity != expected_server_identity {
                return Err(
                    "tmux server identity changed; authoritative reconnect required".into(),
                );
            }
            let generation = value.generation;
            let agent_snapshot = value.agents.clone();
            send_protocol_event(
                channel,
                event_sequence,
                TerminalEvent::Snapshot {
                    snapshot: snapshot_from_proto(value),
                    sequence: frame.sequence,
                    generation,
                    server_identity,
                    authoritative: false,
                },
            )?;
            if let Some(agent_snapshot) = agent_snapshot {
                let host_profile_id = client.host_profile_id.lock().unwrap().clone();
                let payload = serde_json::to_vec(&super::agent::with_connection_epoch(
                    super::agent::snapshot_json(&agent_snapshot, &host_profile_id),
                    client.terminal_epoch.load(Ordering::Acquire),
                ))
                .map_err(|error| error.to_string())?;
                send_protocol_event(
                    channel,
                    event_sequence,
                    TerminalEvent::AgentService {
                        scope: "snapshot".into(),
                        payload,
                    },
                )?;
            }
        }
        v1::EventKind::TopologyDirty => send_protocol_event(
            channel,
            event_sequence,
            TerminalEvent::TopologyDirty { name: event.detail },
        )?,
        v1::EventKind::ResyncRequired => return Err("host requested resync".into()),
        v1::EventKind::TerminalResnapshotRequired => {
            if let Some(pane_id) = scoped_terminal_recovery(&event.scope) {
                send_protocol_event(channel, event_sequence, TerminalEvent::ProtocolProgress)?;
                scoped_seed = Some(pane_id);
            } else {
                return Err(format!(
                    "host requested terminal resnapshot: {}",
                    event.detail
                ));
            }
        }
        v1::EventKind::TerminalSeedDiagnostic => {
            send_protocol_event(
                channel,
                event_sequence,
                TerminalEvent::SeedDiagnostic {
                    pane_id: event.scope,
                    message: event.detail,
                },
            )?;
        }
        v1::EventKind::TerminalSeed | v1::EventKind::TerminalOutput => {
            let terminal = event.terminal.ok_or("terminal event omitted bytes")?;
            let value = if v1::EventKind::try_from(event.kind).unwrap_or_default()
                == v1::EventKind::TerminalSeed
            {
                TerminalEvent::Seed {
                    pane_id: terminal.pane_id,
                    generation: terminal.generation,
                    data: terminal.data,
                }
            } else {
                TerminalEvent::Output {
                    pane_id: terminal.pane_id,
                    generation: terminal.generation,
                    data: terminal.data,
                }
            };
            send_protocol_event(channel, event_sequence, value)?;
        }
        v1::EventKind::TerminalExit => send_protocol_event(
            channel,
            event_sequence,
            TerminalEvent::Exit {
                reason: event.detail,
            },
        )?,
        v1::EventKind::PaneResource => {
            let resource = event
                .pane_resource
                .ok_or("pane resource event omitted resource")?;
            let state = match v1::PaneResourceState::try_from(resource.state).unwrap_or_default() {
                v1::PaneResourceState::Visible => "visible",
                v1::PaneResourceState::HiddenBuffered => "hiddenBuffered",
                v1::PaneResourceState::Released => "released",
                v1::PaneResourceState::Unspecified => "unspecified",
            }
            .into();
            send_protocol_event(
                channel,
                event_sequence,
                TerminalEvent::PaneResource {
                    pane_id: resource.pane_id,
                    state,
                    requires_seed: resource.requires_seed,
                    recovery_reason: resource.recovery_reason,
                    generation: resource.generation,
                    snapshot_generation: resource.snapshot_generation,
                    tail_through_generation: resource.tail_through_generation,
                    serialized_snapshot: resource.serialized_snapshot,
                    raw_tail: resource.raw_tail,
                },
            )?;
        }
        v1::EventKind::ActiveRoot
        | v1::EventKind::DirectorySnapshot
        | v1::EventKind::FileChanged
        | v1::EventKind::TransferProgress => {
            let file = event.file.ok_or("file-service event omitted payload")?;
            let payload = serde_json::to_vec(&super::files::file_event_json(&file))
                .map_err(|error| error.to_string())?;
            send_protocol_event(
                channel,
                event_sequence,
                TerminalEvent::FileService {
                    scope: event.scope,
                    payload,
                },
            )?;
        }
        v1::EventKind::GitStatus => {
            let git = event.git.ok_or("Git event omitted payload")?;
            let payload = serde_json::to_vec(&super::git::git_event_json(&git))
                .map_err(|error| error.to_string())?;
            send_protocol_event(
                channel,
                event_sequence,
                TerminalEvent::GitService {
                    scope: event.scope,
                    payload,
                },
            )?;
        }
        v1::EventKind::AgentState => {
            let agent = event.agent.ok_or("agent event omitted payload")?;
            let host_profile_id = client.host_profile_id.lock().unwrap().clone();
            let payload = serde_json::to_vec(&super::agent::with_connection_epoch(
                super::agent::event_json(&agent, &host_profile_id),
                client.terminal_epoch.load(Ordering::Acquire),
            ))
            .map_err(|error| error.to_string())?;
            send_protocol_event(
                channel,
                event_sequence,
                TerminalEvent::AgentService {
                    scope: event.scope,
                    payload,
                },
            )?;
        }
        _ => send_protocol_event(channel, event_sequence, TerminalEvent::ProtocolProgress)?,
    }
    Ok((frame.sequence, scoped_seed))
}

fn send_protocol_event(
    channel: &Channel<InvokeResponseBody>,
    sequence: u64,
    event: TerminalEvent,
) -> Result<(), String> {
    channel
        .send(InvokeResponseBody::Raw(encode_event_with_sequence(
            event, sequence,
        )))
        .map_err(|error| format!("desktop event channel closed: {error}"))
}

pub(super) fn scoped_terminal_recovery(scope: &str) -> Option<String> {
    validate_tmux_id(scope, '%').ok()?;
    Some(scope.to_owned())
}

fn write_scoped_seed_request(
    stdin: &mut ChildStdin,
    client: &TerminalClient,
    pane_id: String,
) -> Result<(), String> {
    let request_id = client.next_request_id.fetch_add(1, Ordering::AcqRel);
    write_frame_sync(
        stdin,
        &envelope(
            request_id,
            0,
            Payload::Request(v1::Request {
                operation: v1::Operation::RequestTerminalSeed.into(),
                scope: pane_id,
                ..Default::default()
            }),
        ),
    )
    .map_err(|error| error.to_string())
}

pub(super) fn validate_event_sequence(
    last_sequence: u64,
    received_sequence: u64,
) -> Result<(), String> {
    if received_sequence != last_sequence.saturating_add(1) {
        return Err(format!(
            "sequence gap: expected {}, received {}",
            last_sequence + 1,
            received_sequence
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sequence: u64) -> v1::Envelope {
        envelope(0, sequence, Payload::Event(v1::HostEvent::default()))
    }

    #[test]
    fn initial_snapshot_barrier_supersedes_events_already_in_its_sequence() {
        assert!(!event_follows_snapshot_barrier(&event(40), 41));
        assert!(!event_follows_snapshot_barrier(&event(41), 41));
        assert!(event_follows_snapshot_barrier(&event(42), 41));

        let response = envelope(9, 0, Payload::Response(v1::Response::default()));
        assert!(!event_follows_snapshot_barrier(&response, 41));
    }
}
