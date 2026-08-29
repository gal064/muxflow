use std::{
    io::{BufReader, Read, Write},
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use tmux_agent_protocol::{
    CAP_TERMINAL_OUTPUT_CREDIT, HELPER_VERSION, HOST_CAPABILITIES, HostContractError, envelope,
    read_frame_sync,
    v1::{self, envelope::Payload},
    validate_host_contract, write_frame_sync,
};
use uuid::Uuid;

use super::event_frame::{AGENT_SNAPSHOT_SCOPE, encode_event_with_sequence};
use super::transport::{
    BridgeStderr, SshLease, acquire_control_master_cancellable, spawn_bridge,
    with_bridge_diagnostic,
};
use super::{
    ConnectionSpec, CountingReader, InitialHostState, PendingAnswer, REQUEST_TIMEOUT,
    TerminalClient, TerminalEvent, TerminalEventChannel, mark_input_reconnected, send_event,
    snapshot_from_proto, validate_tmux_id, writer::ControlWriterHandle,
};

/// A teardown this side ordered reaches the supervisor as the reader's
/// symptom — "host bridge closed", "frame I/O failed" — because killing the
/// ssh child is how the order is carried out. Naming the order on the error is
/// what lets the journal tell a link the network dropped from one the app
/// dropped, and which of the app's deadlines did it.
fn name_local_teardown(error: String, reason: Option<&'static str>) -> String {
    match reason {
        Some(reason) => format!("{error} (torn down locally: {reason})"),
        None => error,
    }
}

pub(super) fn supervise_bridge(
    client_id: String,
    connection: ConnectionSpec,
    session_id: String,
    pane_ids: Vec<String>,
    channel: TerminalEventChannel,
    client: Arc<TerminalClient>,
) {
    let mut attempt = 0_u32;
    while !client.stop_signal.is_stopped() {
        // A reason older than this bridge cannot describe it.
        client.teardown_reason.lock().unwrap().take();
        if attempt != 0 {
            send_event(
                &channel,
                TerminalEvent::ConnectionState {
                    state: "reconnecting".into(),
                },
            );
        }
        // DNS, ProxyJump, authentication, and master establishment all happen
        // on this supervisor. `start_terminal` has already returned a local
        // connecting event, and the acquired lease is passed into bridge
        // startup so startup cannot immediately repeat `ssh -O check`.
        let lease =
            acquire_control_master_cancellable(&connection, || client.stop_signal.is_stopped());
        if client.stop_signal.is_stopped() {
            break;
        }
        let mut connected_at = None;
        let result = match lease.as_ref() {
            Ok(lease) => run_bridge_once(
                &connection,
                lease.as_ref(),
                &session_id,
                &pane_ids,
                &channel,
                &client,
                &mut connected_at,
            ),
            Err(error) => Err(error.clone()),
        };
        match result {
            Ok(()) => {}
            Err(error) => {
                let reason = client.teardown_reason.lock().unwrap().take();
                send_event(
                    &channel,
                    TerminalEvent::Error {
                        message: name_local_teardown(error, reason),
                    },
                );
            }
        }
        super::files::invalidate_bulk_scope(
            client.bulk_scope,
            "bulk transfer control connection disconnected",
        );
        client.ready.store(false, Ordering::Release);
        if let Some(window) = client.delivery_window.lock().unwrap().take() {
            window.close();
        }
        if let Some(writer) = client.writer.lock().unwrap().take() {
            writer.close();
        }
        client
            .fail_pending(
                "host transport disconnected; commit outcome is unknown and the request will not replay",
            );
        if let Some(mut child) = client.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if client.stop_signal.is_stopped() {
            break;
        }
        // A bridge failure makes the master's health unknown. The next lease
        // performs exactly one per-socket validation before reconnecting.
        if let Ok(Some(lease)) = lease {
            lease.require_revalidation();
        }
        send_event(
            &channel,
            TerminalEvent::ConnectionState {
                state: "disconnected".into(),
            },
        );
        attempt = next_reconnect_attempt(attempt, connected_at.map(|at| at.elapsed()));
        if !client.wait_for_reconnect(Duration::from_millis(reconnect_delay_millis(
            &client_id, attempt,
        ))) {
            break;
        }
    }
}

const STABLE_CONNECTION_RESET: Duration = Duration::from_secs(10);

fn next_reconnect_attempt(previous: u32, connected_for: Option<Duration>) -> u32 {
    if connected_for.is_some_and(|duration| duration >= STABLE_CONNECTION_RESET) {
        1
    } else {
        previous.saturating_add(1)
    }
}

/// Backs off from 400 ms to a one-minute ceiling.
///
/// The early attempts are unchanged, because the common case is a blip that
/// clears in a second and the user should not notice it. The ceiling used to be
/// 6.4 s, which means a machine that is asleep, off the network, or away for an
/// afternoon reconnects roughly ten times a minute forever; a minute is long
/// enough to stop being a cost and short enough that a returning laptop comes
/// back promptly.
pub(super) fn reconnect_delay_millis(client_id: &str, attempt: u32) -> u64 {
    const CEILING_MILLIS: u64 = 60_000;
    // Saturating arithmetic is what bounds a long outage: the doubling runs
    // away to u64::MAX and the ceiling below is what the caller actually sleeps.
    let backoff = 200_u64.saturating_mul(2_u64.saturating_pow(attempt));
    backoff.min(CEILING_MILLIS) + reconnect_jitter(client_id, attempt)
}

pub(super) fn reconnect_jitter(client_id: &str, attempt: u32) -> u64 {
    let mut value = u64::from(attempt).wrapping_mul(0x9e37_79b9);
    for byte in client_id.bytes() {
        value = value.rotate_left(5) ^ u64::from(byte);
    }
    value % 151
}

fn run_bridge_once(
    connection: &ConnectionSpec,
    ssh_lease: Option<&SshLease>,
    session_id: &str,
    pane_ids: &[String],
    channel: &TerminalEventChannel,
    client: &Arc<TerminalClient>,
    connected_at: &mut Option<Instant>,
) -> Result<(), String> {
    let mut bridge = spawn_bridge(connection, ssh_lease)?;
    let mut stdin = bridge.stdin.take().ok_or("host bridge stdin unavailable")?;
    let stdout = bridge
        .stdout
        .take()
        .ok_or("host bridge stdout unavailable")?;
    let diagnostic = bridge.stderr.take().map(BridgeStderr::capture);
    {
        let mut child = client.child.lock().unwrap();
        if client.stop_signal.is_stopped() {
            let _ = bridge.kill();
            let _ = bridge.wait();
            return Err("terminal bridge stopped during connection setup".into());
        }
        *child = Some(bridge);
    }
    let mut reader = BufReader::new(stdout);
    let terminal_epoch = ((Uuid::new_v4().as_u128() as u64) & ((1_u64 << 53) - 1)).max(1);
    let (hello, initial, admission, mut sequence) =
        handshake_and_snapshot(&mut stdin, &mut reader, terminal_epoch)
            .map_err(|error| with_bridge_diagnostic(error, diagnostic.as_ref()))?;
    if client.stop_signal.is_stopped() {
        return Err("terminal bridge stopped during handshake".into());
    }
    // Terminal generations are scoped to one helper protocol connection.  Tell
    // the renderer to discard same-server generation watermarks before any seed
    // or output from the new connection is delivered.
    super::files::invalidate_bulk_scope(
        client.bulk_scope,
        "bulk transfer control connection epoch was replaced",
    );
    super::files::bulk_pool::close_pooled_bulk_bridges(client.bulk_scope);
    let credit_negotiated = hello.capabilities & CAP_TERMINAL_OUTPUT_CREDIT != 0;
    if credit_negotiated
        && (hello.terminal_output_window_bytes == 0 || hello.terminal_output_window_records == 0)
    {
        return Err("host negotiated terminal output credit without a bounded window".into());
    }
    if hello.terminal_output_window_bytes > super::delivery_window::NATIVE_DELIVERY_WINDOW_BYTES {
        return Err("host terminal output window exceeds the native 2 MiB delivery bound".into());
    }
    let next_delivery = credit_negotiated.then(|| super::DeliveryWindow::new(terminal_epoch));
    let previous = {
        let mut delivery = client.delivery_window.lock().unwrap();
        std::mem::replace(&mut *delivery, next_delivery)
    };
    if let Some(previous) = previous {
        previous.close();
    }
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
            // Paired with the topology frame above at the same accepted
            // sequence, which is why the encoder stamps this one zero.
            send_protocol_event(
                channel,
                sequence,
                TerminalEvent::AgentService {
                    scope: AGENT_SNAPSHOT_SCOPE.into(),
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
    let read_only = admission.is_err();
    client.read_only.store(read_only, Ordering::Release);
    // The reason shown here *is* the refusal that put the connection in
    // read-only, so there is no handshake the contract turns down without the
    // user being told why. Recomputing the reason from the hello was how an
    // envelope-major mismatch entered read-only with no error event at all.
    if let Err(refusal) = admission {
        send_event(
            channel,
            TerminalEvent::Error {
                message: refusal.to_string(),
            },
        );
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
    let control_writer = ControlWriterHandle::start(stdin, &terminal_epoch.to_string())?;
    let published_writer = control_writer.clone();
    let published = client.stop_signal.if_running(|| {
        *client.writer.lock().unwrap() = Some(published_writer);
        if !read_only {
            mark_input_reconnected(client);
            client.lane_ready();
            client.resize_queue.reconnected();
            send_event(
                channel,
                TerminalEvent::ConnectionState {
                    state: "connected".into(),
                },
            );
        }
    });
    if !published {
        control_writer.close();
        return Err("terminal bridge stopped before becoming ready".into());
    }
    // Only now: `ready` is set and `read_only` is settled just above, and
    // `BulkBinding::capture` refuses anything else. Before this point the
    // capture would fail — which is the state that must never be pre-warmed
    // into, not merely a state where pre-warming is pointless.
    if !read_only
        && let Ok(binding) = super::files::scheduler::BulkBinding::capture(
            Arc::clone(client),
            hello.server_identity.clone(),
            terminal_epoch,
        )
    {
        super::files::bulk_pool::prewarm_bulk_bridge(connection.clone(), binding);
    }
    super::flush_delivery_ack(client)?;
    *connected_at = Some(Instant::now());
    // From here on the frame parser reads through a counter, so a late answer
    // can name the bytes that were ahead of it on the wire. Free in a build
    // without the measurement compiled in.
    read_protocol_stream(
        CountingReader::new(reader, &client.link_counters),
        sequence,
        &hello.server_identity,
        channel,
        client,
        !read_only,
    )
}

/// The handshake outcome: the hello, the quarantined-or-accepted snapshot, the
/// host contract's verdict — `Err` carrying the reason the connection may only
/// be read-only — and the accepted sequence watermark.
type HandshakeOutcome = (
    v1::ServerHello,
    Option<InitialHostState>,
    Result<(), HostContractError>,
    u64,
);

pub(super) fn handshake_and_snapshot(
    stdin: &mut impl Write,
    reader: &mut impl Read,
    connection_epoch: u64,
) -> Result<HandshakeOutcome, String> {
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
    // Subscribe is valid immediately after ClientHello. Pipeline both writes
    // before waiting for ServerHello so an SSH RTT does not sit between them;
    // the response remains quarantined until compatibility is validated.
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
    let hello_frame = read_frame_sync(reader)
        .map_err(|error| error.to_string())?
        .ok_or("host closed during handshake")?;
    let envelope_major = hello_frame.protocol_major;
    let Some(Payload::ServerHello(hello)) = hello_frame.payload else {
        return Err("host did not return ServerHello".into());
    };
    let (response, buffered) = read_until_response_with_value(reader, 2)?;
    let accepted_sequence = response.accepted_sequence;
    if let Err(refusal) = handshake_admission(envelope_major, &hello) {
        // Subscribe was intentionally pipelined, so its correlated response
        // must always be consumed. Keep its sequence watermark while
        // quarantining the incompatible snapshot and every later event.
        return Ok((hello, None, Err(refusal), accepted_sequence));
    }
    if !response.ok {
        return Err(format!(
            "{}: {}",
            response.error_code, response.display_message
        ));
    }
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
        Ok(()),
        accepted_sequence,
    ))
}

fn event_follows_snapshot_barrier(frame: &v1::Envelope, accepted_sequence: u64) -> bool {
    matches!(&frame.payload, Some(Payload::Event(_))) && frame.sequence > accepted_sequence
}

/// Whether this helper may serve the app at all — and, when it may not, why.
///
/// Every capability the desktop needs is required here, including the
/// single-request file open: a helper that cannot serve one is refused at the
/// handshake rather than accepted and then found wanting one operation at a
/// time. The daemon lives on a host the user upgrades separately from the app,
/// so this is a real state.
///
/// The rule is [`validate_host_contract`] and lives in the protocol crate, and
/// the refusal it returns is the message the read-only path above shows. The
/// verdict and the explanation are therefore one value: nothing can be refused
/// here and left unexplained there.
pub(super) fn handshake_admission(
    envelope_major: u32,
    hello: &v1::ServerHello,
) -> Result<(), HostContractError> {
    validate_host_contract(envelope_major, hello)
}

fn read_until_response(
    reader: &mut impl Read,
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
    reader: &mut impl Read,
    request_id: u64,
) -> Result<(v1::Response, Vec<v1::Envelope>), String> {
    let mut buffered = Vec::new();
    loop {
        let frame = read_frame_sync(reader)
            .map_err(|error| error.to_string())?
            .ok_or("host disconnected while awaiting response")?;
        let mut frame = frame;
        if frame.request_id == request_id && matches!(frame.payload, Some(Payload::Response(_))) {
            let Some(Payload::Response(response)) = frame.payload.take() else {
                unreachable!("payload kind was checked before it was moved")
            };
            return Ok((response, buffered));
        }
        buffered.push(frame);
    }
}

fn read_protocol_stream(
    mut reader: impl Read,
    mut sequence: u64,
    server_identity: &str,
    channel: &TerminalEventChannel,
    client: &Arc<TerminalClient>,
    admit_events: bool,
) -> Result<(), String> {
    let mut resync_request_id = None;
    // Terminal payload the host has already reserved credit for and this run
    // will never deliver: see the quarantine below.
    let mut quarantined_charge = super::HostCharge::default();
    loop {
        // Read before the frame, because the counting reader has already added
        // this frame's own bytes by the time the envelope exists — and this
        // answer's own bytes were never ahead of it.
        let bytes_before = client.link_counters.bytes_read();
        let frame = read_frame_sync(&mut reader)
            .map_err(|error| error.to_string())?
            .ok_or("host bridge closed")?;
        client.note_host_frame();
        let answer_mark = client.link_counters.note_frame_read(&frame, bytes_before);
        let mut frame = frame;
        if matches!(frame.payload, Some(Payload::Response(_))) {
            let Some(Payload::Response(response)) = frame.payload.take() else {
                unreachable!("payload kind was checked before it was moved")
            };
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
                super::forfeit_delivery_charge(client, std::mem::take(&mut quarantined_charge))?;
                // The barrier is the new watermark, and adopting it verbatim is
                // what makes the first event after it validate. The host's
                // `ProtocolSequencer` stamps a snapshot barrier with the
                // sequence it has *already* spent (`accepted_sequence =
                // self.sequence`, it does not advance) and numbers the next
                // ordered event `self.sequence + 1` — exactly the successor
                // `validate_event_sequence` demands.
                sequence = response.accepted_sequence;
                // The barrier is also what ends the quarantine below, so this
                // must clear before the loop reads another event. Nothing else
                // latches: a later gap re-arms the same two fields.
                resync_request_id = None;
                client.lane_ready();
                // The transport never went away, so the input epoch and the
                // resize queue are deliberately left alone: keystrokes queued
                // while the screen was reconciling are still bound for the same
                // host connection, and marking a reconnect here would discard
                // them. Only the renderer's `resyncing` banner has to retire.
                send_event(
                    channel,
                    TerminalEvent::ConnectionState {
                        state: "connected".into(),
                    },
                );
                continue;
            }
            if let Some(waiter) = client.pending.lock().unwrap().remove(&frame.request_id) {
                let _ = waiter.send(Ok(PendingAnswer {
                    response,
                    mark: answer_mark,
                }));
            }
            continue;
        }
        if resync_request_id.is_some() {
            // Events received after a detected gap are superseded by the
            // authoritative resync barrier. Correlated responses above must
            // still be delivered to callers.
            //
            // Their host delivery credit is not superseded, though. The host
            // reserved it before it sent them and releases it only against this
            // client's cumulative acknowledgement, so a charge dropped here is
            // credit it never gets back. That was harmless while the run ended
            // at the barrier and took the whole window with it; now that the run
            // resumes, every gap would shrink the terminal output window until
            // the host stopped sending. Release it as one exact total below.
            quarantined_charge.accumulate(event_delivery_charge(&frame));
            continue;
        }
        if !admit_events {
            // An incompatible helper remains connected only to surface its
            // read-only state. Its subscription was already established by
            // the pipelined request, so drain and quarantine those events
            // without applying payloads from a protocol we cannot trust.
            continue;
        }
        // Read before the frame is consumed: an event that trips the gap below
        // is refused by `validate_event_sequence` before any of its payload is
        // forwarded, so its charge joins the quarantined total rather than the
        // delivered one.
        let charge = event_delivery_charge(&frame);
        match process_event(frame, sequence, server_identity, channel, client) {
            Ok((next, scoped_seed)) => {
                sequence = next;
                if let Some(pane_id) = scoped_seed {
                    let writer = client
                        .writer
                        .lock()
                        .unwrap()
                        .clone()
                        .ok_or("host bridge is disconnected")?;
                    writer.write(
                        scoped_seed_request(client, pane_id),
                        Instant::now() + REQUEST_TIMEOUT,
                    )?;
                }
            }
            Err(error) if error.starts_with("sequence gap") || error == "host requested resync" => {
                quarantined_charge.accumulate(charge);
                client.ready.store(false, Ordering::Release);
                send_event(
                    channel,
                    TerminalEvent::ConnectionState {
                        state: "resyncing".into(),
                    },
                );
                let request_id = client.next_request_id.fetch_add(1, Ordering::AcqRel);
                let writer = client
                    .writer
                    .lock()
                    .unwrap()
                    .clone()
                    .ok_or("host bridge is disconnected")?;
                writer.write(
                    envelope(
                        request_id,
                        0,
                        Payload::Request(v1::Request {
                            operation: v1::Operation::Resync.into(),
                            scope: "full".into(),
                            ..Default::default()
                        }),
                    ),
                    Instant::now() + REQUEST_TIMEOUT,
                )?;
                resync_request_id = Some(request_id);
            }
            Err(error) => return Err(error),
        }
    }
}

/// The host delivery credit an event frame carries, without consuming it.
fn event_delivery_charge(frame: &v1::Envelope) -> super::HostCharge {
    match &frame.payload {
        Some(Payload::Event(event)) => super::HostCharge {
            bytes: event.terminal_delivery_bytes,
            records: event.terminal_delivery_records,
        },
        _ => super::HostCharge::default(),
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
    channel: &TerminalEventChannel,
    client: &Arc<TerminalClient>,
) -> Result<(u64, Option<String>), String> {
    let Some(Payload::Event(event)) = frame.payload else {
        return Ok((last_sequence, None));
    };
    let delivery_charge = super::HostCharge {
        bytes: event.terminal_delivery_bytes,
        records: event.terminal_delivery_records,
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
                // The second frame of one host event: the topology frame above
                // already delivered the sequence they share.
                send_protocol_event(
                    channel,
                    event_sequence,
                    TerminalEvent::AgentService {
                        scope: AGENT_SNAPSHOT_SCOPE.into(),
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
        // `TerminalFlowPaused` is a notice, never a recovery. The host resumes
        // and re-captures the pane by itself, and a second recovery raced in
        // from here would only re-photograph a pane that is already being
        // re-photographed — so this forwards the episode for the journal and
        // nothing may act on it. What reaches the recovery paths below is the
        // case the host could not fix.
        v1::EventKind::TerminalFlowPaused => {
            if let Some(pane_id) = scoped_terminal_recovery(&event.scope) {
                send_protocol_event(
                    channel,
                    event_sequence,
                    TerminalEvent::FlowPaused {
                        pane_id,
                        message: event.detail,
                    },
                )?;
            } else {
                send_protocol_event(channel, event_sequence, TerminalEvent::ProtocolProgress)?;
            }
        }
        v1::EventKind::TerminalFlowStalled => {
            if let Some(pane_id) = scoped_terminal_recovery(&event.scope) {
                send_protocol_event(
                    channel,
                    event_sequence,
                    TerminalEvent::FlowStalled {
                        pane_id: pane_id.clone(),
                        message: event.detail,
                    },
                )?;
                // The seed is the recovery, not the notice: `request_seed`
                // carries the resume for a pane the host knows is paused, so
                // this is what actually takes it out of tmux's flow control.
                scoped_seed = Some(pane_id);
            } else {
                return Err(format!("host reported a stalled pane: {}", event.detail));
            }
        }
        v1::EventKind::TerminalClipboardWrite => {
            let data = event.detail.into_bytes();
            if event.scope != "terminal-clipboard" || data.is_empty() || data.len() > 1024 * 1024 {
                send_protocol_event(channel, event_sequence, TerminalEvent::ProtocolProgress)?;
            } else {
                send_protocol_event(
                    channel,
                    event_sequence,
                    TerminalEvent::ClipboardWrite { data },
                )?;
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
            send_charged_protocol_event(channel, event_sequence, value, delivery_charge)?;
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
            send_charged_protocol_event(
                channel,
                event_sequence,
                TerminalEvent::PaneResource {
                    pane_id: resource.pane_id,
                    state,
                    requires_seed: resource.requires_seed,
                    resume_from_renderer: resource.resume_from_renderer,
                    recovery_reason: resource.recovery_reason,
                    generation: resource.generation,
                    snapshot_generation: resource.snapshot_generation,
                    tail_through_generation: resource.tail_through_generation,
                    serialized_snapshot: resource.serialized_snapshot,
                    raw_tail: resource.raw_tail,
                },
                delivery_charge,
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
            // A standalone ordered event: its scope is the agent's identifier,
            // never the reserved snapshot scope, so the frame keeps the
            // sequence this event spent. Dropping it would leave a hole the
            // renderer reads as a lost frame.
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
    channel: &TerminalEventChannel,
    sequence: u64,
    event: TerminalEvent,
) -> Result<(), String> {
    send_charged_protocol_event(channel, sequence, event, super::HostCharge::default())
}

fn send_charged_protocol_event(
    channel: &TerminalEventChannel,
    sequence: u64,
    event: TerminalEvent,
    charge: super::HostCharge,
) -> Result<(), String> {
    channel.send_charged(encode_event_with_sequence(event, sequence), charge)
}

pub(super) fn scoped_terminal_recovery(scope: &str) -> Option<String> {
    validate_tmux_id(scope, '%').ok()?;
    Some(scope.to_owned())
}

fn write_scoped_seed_request(
    stdin: &mut impl Write,
    client: &TerminalClient,
    pane_id: String,
) -> Result<(), String> {
    let request = scoped_seed_request(client, pane_id);
    write_frame_sync(stdin, &request).map_err(|error| error.to_string())
}

fn scoped_seed_request(client: &TerminalClient, pane_id: String) -> v1::Envelope {
    envelope(
        client.next_request_id.fetch_add(1, Ordering::AcqRel),
        0,
        Payload::Request(v1::Request {
            operation: v1::Operation::RequestTerminalSeed.into(),
            scope: pane_id,
            ..Default::default()
        }),
    )
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
mod tests;
