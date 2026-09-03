use std::{
    collections::HashMap,
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tmux_agent_protocol::{
    CAP_BULK_DOWNLOAD, CAP_TERMINAL_OUTPUT_CREDIT, FrameError, HELPER_VERSION, HOST_CAPABILITIES,
    PROTOCOL_MAJOR, envelope, read_frame,
    v1::{self, envelope::Payload},
    write_frame,
};
use tokio::{
    net::UnixStream,
    sync::mpsc,
    time::{sleep, timeout},
};

pub(crate) mod snapshot;
use snapshot::{server_identity, snapshot_from_identity};
mod active_root;
pub(crate) mod agents;
mod filesystem;
use filesystem::FileService;
mod git;
use git::GitService;
mod terminal;
use terminal::TerminalClients;
use terminal::{OUTPUT_WINDOW_BYTES, OUTPUT_WINDOW_RECORDS, OutputCharge, OutputCredit};
mod tmux_actions;
mod tmux_config;
pub(crate) mod voice;
pub(crate) use tmux_config::{
    apply_recommended_naming as apply_recommended_tmux_naming,
    remove_recommended_naming as remove_recommended_tmux_naming,
};
mod topology;
mod topology_output_trigger;
#[cfg(test)]
use terminal::validate_tmux_id;
use topology::{TopologyActor, TopologySignal};
use topology_output_trigger::TopologyOutputTrigger;
mod events;
#[cfg(test)]
use events::CONTROL_EVENT_HUB;
use events::{
    ConnectionTaskGuard, ProtocolSequencer, SequencerControl, register_control_event_sink,
};
pub(crate) use events::{broadcast_control_event, control_sink_alive, send_control_event_to};

/// Depth of the ordered host-event queue.
///
/// Overflow here is not a dropped frame but a connection-wide resync: the
/// desktop tears the bridge down and reseeds every pane. At 128 a burst of
/// terminal output could reach that cliff during ordinary use, and it took a
/// seed with it — a seed dropped by a full queue leaves the pane waiting for
/// bytes that will never come. The depth is chosen against the reader's 64 KiB
/// read size and tmux's own `pause-after` flow control, which bounds how far
/// ahead of a slow consumer the queue can run.
pub(crate) const EVENT_QUEUE: usize = 1024;
pub(crate) const TERMINAL_INPUT_QUEUE: usize = 256;

/// How long teardown lets the writer drain before aborting it.
///
/// The writer finishes on its own only when every sender clone is gone; after
/// the teardown wakes the topology actor and stops the terminal clients, the
/// stragglers drop within milliseconds, so this bound is an order of magnitude
/// above the expected wait — like the handshake timeout below. It exists for
/// the clone nothing released: without it, one parked sender kept a connection
/// task, its socket, and the remote bridge process alive for days.
const WRITER_DRAIN_GRACE: Duration = Duration::from_secs(5);

/// How long teardown waits for terminal worker threads to join.
///
/// By this point their child processes are dead and the event channel is
/// closed, so a join is milliseconds; a thread that still does not return is
/// wedged in a way no further waiting fixes, and leaking it is strictly better
/// than hanging the connection task with it.
const TERMINAL_JOIN_GRACE: Duration = Duration::from_secs(5);

/// Ordered mutations admitted ahead of the one currently executing.
///
/// The desktop has its own bounded input and control-write queues, so reaching
/// this bound means the ordered lane has stopped making progress. Refuse the
/// connection instead of blocking the frame reader: that reader is the only
/// task able to consume output acknowledgements and observe peer shutdown.
const ORDERED_REQUEST_QUEUE: usize = 1_024;

/// No ordinary ordered host operation is allowed to monopolize a connection.
///
/// The desktop gives ordinary requests five seconds. This larger host bound
/// leaves room for the response trip while still turning a lost wakeup or
/// lock cycle into a scoped reconnect instead of a minutes-long freeze.
const ORDERED_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A socket writer that cannot advance is no longer a usable connection.
const PROTOCOL_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a connection ended — the one field of its end-of-life line that tells an
/// ordinary teardown apart from a desktop that vanished mid-session.
///
/// Every exit from [`serve_connection`] maps to exactly one of these. They are
/// fixed labels rather than the error text they stand for: the line they end up
/// in is a log the user may share, and an error string can carry a path, a
/// session name or a byte of terminal output with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionEndReason {
    /// No first frame arrived inside the handshake window.
    HandshakeTimeout,
    /// The handshake frame could not be read, was not a `ClientHello`, or the
    /// hello answering it could not be written.
    HandshakeFailed,
    /// The client closed its side of the socket: the ordinary teardown.
    ClientEof,
    /// The transport dropped under a client that never closed it — the shape a
    /// killed SSH session leaves behind.
    ClientReset,
    /// The frame reader failed for any other reason.
    ReadFailed,
    /// The client acknowledged terminal output it was never charged for.
    InvalidAck,
    /// The event writer made no progress before [`PROTOCOL_WRITE_TIMEOUT`] —
    /// the deadline that finally reaps a connection whose peer is gone.
    WriterDeadline,
    /// The event writer's socket write failed outright.
    WriterFailed,
    /// Every event sender was released while the reader was still running.
    ///
    /// The reader holds one of those senders for its whole life, so today this
    /// says the writer drained for a reason nothing else can produce. It is
    /// here because the writer can stop this way, not because it is expected.
    SequencerClosed,
    /// An ordered host operation exceeded its execution bound, or its lane
    /// stopped.
    OrderedLaneStalled,
    /// The ordered lane stopped draining and its admission queue filled.
    OrderedQueueFull,
    /// This connection asked the daemon to shut down.
    DaemonShutdown,
    /// The connection was already marked closed when the reader came back for
    /// its next frame, and no lane had said why.
    ConnectionClosed,
}

impl ConnectionEndReason {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::HandshakeTimeout => "handshake-timeout",
            Self::HandshakeFailed => "handshake-failed",
            Self::ClientEof => "client-eof",
            Self::ClientReset => "client-reset",
            Self::ReadFailed => "read-failed",
            Self::InvalidAck => "invalid-ack",
            Self::WriterDeadline => "writer-deadline",
            Self::WriterFailed => "writer-failed",
            Self::SequencerClosed => "sequencer-closed",
            Self::OrderedLaneStalled => "ordered-lane-stalled",
            Self::OrderedQueueFull => "ordered-queue-full",
            Self::DaemonShutdown => "daemon-shutdown",
            Self::ConnectionClosed => "connection-closed",
        }
    }
}

/// Why the event writer stopped, kept as a class beside the message the reader
/// reports, so the end-of-life line never has to parse — or print — the text.
enum WriterStop {
    SequencerClosed,
    Failed(String),
    Deadline,
}

impl WriterStop {
    fn reason(&self) -> ConnectionEndReason {
        match self {
            Self::SequencerClosed => ConnectionEndReason::SequencerClosed,
            Self::Failed(_) => ConnectionEndReason::WriterFailed,
            Self::Deadline => ConnectionEndReason::WriterDeadline,
        }
    }

    fn message(self) -> String {
        match self {
            Self::SequencerClosed => "host event sequencer closed".to_owned(),
            Self::Failed(message) => message,
            Self::Deadline => "host event writer made no progress before its deadline".to_owned(),
        }
    }
}

/// When this connection last carried a frame in each direction.
///
/// A connection killed with the laptop that owned it stays open and stays
/// silent, so the interesting number at teardown is not only how long it lived
/// but how long it had already been quiet. The socket's peer is the local
/// bridge process rather than the desktop itself, so these are the ages of the
/// last frame the bridge relayed and of the last frame the host handed it —
/// the desktop's silence only as closely as the bridge reflects it. Both marks
/// are milliseconds since
/// the connection started, in one relaxed atomic each: the writer lane and the
/// frame reader are the only writers, they never read each other's mark, and
/// neither can afford a lock on its hot path.
struct FrameActivity {
    started: Instant,
    last_client_frame_ms: AtomicU64,
    last_host_frame_ms: AtomicU64,
}

impl FrameActivity {
    fn started_now() -> Self {
        Self {
            started: Instant::now(),
            last_client_frame_ms: AtomicU64::new(0),
            last_host_frame_ms: AtomicU64::new(0),
        }
    }

    fn lifetime(&self) -> Duration {
        self.started.elapsed()
    }

    fn mark_client_frame(&self) {
        self.last_client_frame_ms
            .store(self.elapsed_ms(), Ordering::Relaxed);
    }

    fn mark_host_frame(&self) {
        self.last_host_frame_ms
            .store(self.elapsed_ms(), Ordering::Relaxed);
    }

    /// Zero until the first frame of that direction, which is the handshake:
    /// before it, the age of the connection is the age of the silence.
    fn since_last_client_frame(&self) -> Duration {
        self.since(&self.last_client_frame_ms)
    }

    fn since_last_host_frame(&self) -> Duration {
        self.since(&self.last_host_frame_ms)
    }

    fn since(&self, mark: &AtomicU64) -> Duration {
        Duration::from_millis(
            self.elapsed_ms()
                .saturating_sub(mark.load(Ordering::Relaxed)),
        )
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
static TEST_LAST_TERMINAL_ACK_EPOCH: AtomicU64 = AtomicU64::new(0);

struct OrderedRequest {
    request_id: u64,
    request: v1::Request,
    policy: requests::operation_policy::OperationPolicy,
    cancellation: Arc<AtomicBool>,
    context: requests::RequestContext,
}

async fn run_ordered_requests(
    mut receiver: mpsc::Receiver<OrderedRequest>,
    failed: mpsc::UnboundedSender<String>,
) {
    while let Some(work) = receiver.recv().await {
        let operation = work
            .policy
            .known_operation()
            .map(|operation| operation.as_str_name())
            .unwrap_or("UNKNOWN");
        let started = Instant::now();
        let request_id = work.request_id;
        let cancellation = Arc::clone(&work.cancellation);
        let pending = Arc::clone(&work.context.pending);
        let closed = Arc::clone(&work.context.closed);
        let execution = async {
            // Test-only scheduling fault: delay the ordered lane without
            // touching tmux, the event writer, or the frame reader. This is the
            // production failure shape reduced to its load-bearing fact.
            #[cfg(test)]
            if work.policy.known_operation() == Some(v1::Operation::TestDelay)
                && work.request.scope == "stall-ordered-lane"
            {
                sleep(Duration::from_secs(2)).await;
            }
            handle_request(
                work.request_id,
                work.request,
                work.policy,
                work.cancellation,
                work.context,
            )
            .await;
        };
        match timeout(ORDERED_REQUEST_TIMEOUT, execution).await {
            Ok(()) => {
                crate::diagnostics::record_ordered_request_duration(operation, started.elapsed())
            }
            Err(_) => {
                cancellation.store(true, Ordering::Release);
                pending.lock().unwrap().remove(&request_id);
                closed.store(true, Ordering::Release);
                crate::diagnostics::record_ordered_request_timeout(operation, started.elapsed());
                let _ = failed.send(format!(
                    "ordered host operation {operation} exceeded its execution bound"
                ));
                break;
            }
        }
    }
}

pub async fn serve_with_shutdown(
    stream: UnixStream,
    shutdown: Option<tokio::sync::mpsc::UnboundedSender<()>>,
) -> anyhow::Result<()> {
    let activity = Arc::new(FrameActivity::started_now());
    serve_connection(stream, shutdown, &activity).await.0
}

async fn serve_connection(
    mut stream: UnixStream,
    shutdown: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    activity: &Arc<FrameActivity>,
) -> (anyhow::Result<()>, ConnectionEndReason) {
    // Reports the end and hands the reason back, so no exit below can name a
    // reason without leaving its line behind. It runs where the connection
    // ended rather than after the teardown that follows: teardown can spend
    // ten seconds on its two grace periods, and a silence measured across
    // those is not the silence this line exists to record.
    let ended = |reason: ConnectionEndReason| {
        crate::diagnostics::write_connection_ended_log(
            reason.label(),
            activity.lifetime(),
            activity.since_last_client_frame(),
            activity.since_last_host_frame(),
        );
        reason
    };
    let hello = match timeout(Duration::from_secs(5), read_frame(&mut stream)).await {
        Err(elapsed) => {
            return (
                Err(anyhow::Error::new(elapsed).context("client handshake timed out")),
                ended(ConnectionEndReason::HandshakeTimeout),
            );
        }
        Ok(Err(error)) => {
            return (
                Err(error.into()),
                ended(ConnectionEndReason::HandshakeFailed),
            );
        }
        Ok(Ok(None)) => {
            return (
                Err(anyhow::anyhow!("client disconnected before handshake")),
                ended(ConnectionEndReason::HandshakeFailed),
            );
        }
        Ok(Ok(Some(hello))) => hello,
    };
    activity.mark_client_frame();
    let Some(Payload::ClientHello(client_hello)) = hello.payload else {
        let outcome = match send_handshake_error(
            &mut stream,
            "handshake_required",
            "first frame must be ClientHello",
        )
        .await
        {
            Ok(()) => {
                activity.mark_host_frame();
                Err(anyhow::anyhow!("first frame was not ClientHello"))
            }
            Err(error) => Err(error),
        };
        return (outcome, ended(ConnectionEndReason::HandshakeFailed));
    };

    let host_protocol_major = advertised_protocol_major();
    let protocol_compatible = hello.protocol_major == host_protocol_major;
    let host_helper_version = advertised_helper_version();
    let helper_compatible = client_hello.expected_helper_version.is_empty()
        || client_hello.expected_helper_version == host_helper_version;
    let current_server_identity = server_identity();
    let identity_compatible = !client_hello.bulk_connection
        || (!client_hello.expected_server_identity.is_empty()
            && client_hello.expected_server_identity == current_server_identity);
    let read_only = !protocol_compatible || !helper_compatible || !identity_compatible;
    let incompatibility = if !protocol_compatible {
        format!(
            "protocol major {} is incompatible with host major {host_protocol_major}",
            hello.protocol_major
        )
    } else if !helper_compatible {
        format!(
            "helper {} does not match required {}",
            host_helper_version, client_hello.expected_helper_version
        )
    } else if !identity_compatible {
        format!(
            "bulk connection expected server identity {:?}, but the active server is {:?}",
            client_hello.expected_server_identity, current_server_identity
        )
    } else {
        String::new()
    };
    let negotiated_capabilities = HOST_CAPABILITIES & client_hello.requested_capabilities;
    let output_credit_enabled =
        !client_hello.bulk_connection && negotiated_capabilities & CAP_TERMINAL_OUTPUT_CREDIT != 0;
    let output_credit = Arc::new(OutputCredit::negotiated(output_credit_enabled));
    let mut server_hello = envelope(
        hello.request_id,
        0,
        Payload::ServerHello(v1::ServerHello {
            helper_version: host_helper_version,
            operating_system: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
            tmux_version: daemon_command_version(CommandVersion::Tmux),
            server_identity: current_server_identity,
            capabilities: negotiated_capabilities,
            read_only,
            incompatibility,
            git_version: daemon_command_version(CommandVersion::Git),
            connection_epoch: client_hello.connection_epoch,
            terminal_output_window_bytes: if output_credit_enabled {
                OUTPUT_WINDOW_BYTES
            } else {
                0
            },
            terminal_output_window_records: if output_credit_enabled {
                OUTPUT_WINDOW_RECORDS as u32
            } else {
                0
            },
        }),
    );
    server_hello.protocol_major = host_protocol_major;
    if let Err(error) = write_frame(&mut stream, &server_hello).await {
        return (
            Err(error.into()),
            ended(ConnectionEndReason::HandshakeFailed),
        );
    }
    activity.mark_host_frame();

    let (mut reader, mut writer) = stream.into_split();
    let (control_tx, mut control_rx) = mpsc::channel::<SequencerControl>(EVENT_QUEUE);
    let mut event_registration =
        (!client_hello.bulk_connection).then(|| register_control_event_sink(control_tx.clone()));
    // What a voice session records so a spoken reply can find this connection
    // again; zero for a bulk lane, which never carries a voice request.
    let connection_id = event_registration
        .as_ref()
        .map_or(0, |registration| registration.id);
    let closed = Arc::new(AtomicBool::new(false));
    let _connection_task_guard = ConnectionTaskGuard(Arc::clone(&closed));
    let topology_signal = TopologySignal::default();
    let writer_topology_signal = topology_signal.clone();
    let writer_closed = Arc::clone(&closed);
    let (writer_stopped_tx, mut writer_stopped_rx) = mpsc::unbounded_channel::<WriterStop>();
    let writer_activity = Arc::clone(activity);
    let mut writer_task = tokio::spawn(async move {
        let mut sequencer = ProtocolSequencer::default();
        let mut gap_fault = events::GapFaultInjector::for_connection();
        let mut pending_message = None;
        let mut stopped_reason = WriterStop::SequencerClosed;
        loop {
            let message = match pending_message.take() {
                Some(message) => message,
                None => match control_rx.recv().await {
                    Some(message) => message,
                    None => break,
                },
            };
            let (message, pending) =
                events::coalesce_adjacent_terminal_output(message, &mut control_rx);
            pending_message = pending;
            if let SequencerControl::TopologyEpochBarrier(completion) = message {
                let _ = completion.send(writer_topology_signal.current_epoch());
                continue;
            }
            writer_topology_signal.observe_event(&message);
            let injected_gap = gap_fault.after(&message);
            // The perf-log timeline's H3: which frame this is, so the writer
            // can name what it just spent its time on.
            let (timed_response, frame_kind) = match &message {
                SequencerControl::Response { request_id, .. } => (Some(*request_id), "response"),
                SequencerControl::FileStream { .. } => (None, "fileStream"),
                _ => (None, "event"),
            };
            let frame = sequencer.frame(message);
            // Which event, and whose pane, so a slow write names the frame that
            // blocked the writer rather than only its size. Read off the framed
            // envelope, which borrows: no allocation on the output fast path.
            let (frame_event_kind, frame_pane_id) = match &frame.payload {
                Some(tmux_agent_protocol::v1::envelope::Payload::Event(event)) => (
                    v1::EventKind::try_from(event.kind)
                        .ok()
                        .map(|kind| kind.as_str_name()),
                    event
                        .terminal
                        .as_ref()
                        .map(|terminal| terminal.pane_id.as_str()),
                ),
                _ => (None, None),
            };
            let write_started = Instant::now();
            match timeout(PROTOCOL_WRITE_TIMEOUT, write_frame(&mut writer, &frame)).await {
                Ok(Ok(())) => {
                    writer_activity.mark_host_frame();
                    let write_elapsed = write_started.elapsed();
                    if let Some(request_id) = timed_response {
                        crate::diagnostics::record_response_written(
                            request_id,
                            write_started,
                            write_elapsed,
                        );
                    }
                    crate::diagnostics::record_frame_write(
                        frame_kind,
                        frame_event_kind,
                        frame_pane_id,
                        frame.request_id,
                        || prost::Message::encoded_len(&frame),
                        write_elapsed,
                    );
                }
                Ok(Err(error)) => {
                    stopped_reason =
                        WriterStop::Failed(format!("host event writer failed: {error}"));
                    break;
                }
                Err(_) => {
                    stopped_reason = WriterStop::Deadline;
                    break;
                }
            }
            if let Some(injected_gap) = injected_gap {
                let frame = sequencer.frame(injected_gap);
                match timeout(PROTOCOL_WRITE_TIMEOUT, write_frame(&mut writer, &frame)).await {
                    Ok(Ok(())) => writer_activity.mark_host_frame(),
                    Ok(Err(error)) => {
                        stopped_reason =
                            WriterStop::Failed(format!("host event writer failed: {error}"));
                        break;
                    }
                    Err(_) => {
                        stopped_reason = WriterStop::Deadline;
                        break;
                    }
                }
            }
        }
        writer_closed.store(true, Ordering::Release);
        let _ = writer_stopped_tx.send(stopped_reason);
    });

    let generation = Arc::new(AtomicU64::new(0));
    let overflowed = Arc::new(AtomicBool::new(false));
    let subscribed = Arc::new(AtomicBool::new(false));
    let pending = Arc::new(Mutex::new(HashMap::<u64, Arc<AtomicBool>>::new()));
    // tmux says nothing when a window retitles itself or a pane's cwd moves, so
    // the reader threads turn the pane output that always accompanies those
    // changes into a debounced dirty mark for the actor below.
    let terminal = Arc::new(Mutex::new(TerminalClients::new(
        Arc::clone(&output_credit),
        TopologyOutputTrigger::new(topology_signal.clone(), tokio::runtime::Handle::current()),
    )));
    let topology_lock = Arc::new(tokio::sync::Mutex::new(()));
    let topology_baseline = Arc::new(Mutex::new(None::<(tmux_control::TmuxSnapshot, String)>));
    let files = Arc::new(FileService::new());
    let git = Arc::new(GitService::new(
        Arc::clone(&closed),
        client_hello.connection_epoch,
    ));
    files.spawn_watcher(
        Arc::clone(&closed),
        control_tx.clone(),
        Arc::clone(&overflowed),
    );

    TopologyActor {
        closed: Arc::clone(&closed),
        subscribed: Arc::clone(&subscribed),
        generation: Arc::clone(&generation),
        overflowed: Arc::clone(&overflowed),
        lock: Arc::clone(&topology_lock),
        baseline: Arc::clone(&topology_baseline),
        terminal: Arc::clone(&terminal),
        sender: control_tx.clone(),
        signal: topology_signal.clone(),
    }
    .spawn();

    let (ordered_tx, ordered_rx) = mpsc::channel(ORDERED_REQUEST_QUEUE);
    let (ordered_failed_tx, mut ordered_failed_rx) = mpsc::unbounded_channel();
    let mut ordered_task = tokio::spawn(run_ordered_requests(ordered_rx, ordered_failed_tx));

    let mut read_error = None;
    // Set at every exit below, so the end-of-life line names the same event the
    // returned result stands for. The loop condition is the one exit nothing
    // breaks out of: it means someone else marked the connection closed.
    let mut end_reason = ConnectionEndReason::ConnectionClosed;
    let mut shutdown_requested = false;
    while !closed.load(Ordering::Acquire) {
        let read = tokio::select! {
            read = read_frame(&mut reader) => read,
            failure = ordered_failed_rx.recv() => {
                end_reason = ConnectionEndReason::OrderedLaneStalled;
                read_error = Some(FrameError::Io(std::io::Error::other(
                    failure.unwrap_or_else(|| "ordered host operation lane stopped".into()),
                )));
                break;
            }
            failure = writer_stopped_rx.recv() => {
                match failure {
                    Some(stop) => {
                        end_reason = stop.reason();
                        read_error = Some(FrameError::Io(std::io::Error::other(stop.message())));
                    }
                    // The writer dropped its sender without reporting: it can
                    // only have been aborted or have panicked.
                    None => {
                        end_reason = ConnectionEndReason::WriterFailed;
                        read_error = Some(FrameError::Io(std::io::Error::other(
                            "host event writer stopped",
                        )));
                    }
                }
                break;
            }
        };
        let frame = match read {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                end_reason = ConnectionEndReason::ClientEof;
                break;
            }
            Err(error) if is_clean_peer_disconnect(&error) => {
                end_reason = ConnectionEndReason::ClientReset;
                break;
            }
            Err(error) => {
                end_reason = ConnectionEndReason::ReadFailed;
                read_error = Some(error);
                break;
            }
        };
        activity.mark_client_frame();
        if frame.protocol_major != host_protocol_major {
            send_response(
                &control_tx,
                frame.request_id,
                response_error(
                    "protocol_incompatible",
                    "frame protocol major does not match the negotiated connection",
                ),
            )
            .await;
            continue;
        }
        match frame.payload {
            Some(Payload::TerminalOutputAck(ack)) => {
                #[cfg(test)]
                TEST_LAST_TERMINAL_ACK_EPOCH.store(ack.connection_epoch, Ordering::Release);
                if output_credit_enabled
                    && ack.connection_epoch == client_hello.connection_epoch
                    && output_credit
                        .acknowledge(OutputCharge {
                            bytes: ack.cumulative_bytes,
                            records: ack.cumulative_records,
                        })
                        .is_err()
                {
                    end_reason = ConnectionEndReason::InvalidAck;
                    read_error = Some(FrameError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "invalid terminal delivery acknowledgement",
                    )));
                    break;
                }
            }
            Some(Payload::Cancel(cancel)) => {
                if let Some(token) = pending.lock().unwrap().get(&cancel.target_request_id) {
                    token.store(true, Ordering::Release);
                }
            }
            Some(Payload::Request(request)) => {
                // H1 of the switch timeline: the request frame is decoded and
                // this is the first instant the daemon could act on it. Inert
                // for every operation but a tmux action, and compiled out of a
                // plain release build — see `diagnostics::switch_timing`.
                crate::diagnostics::note_request_read(frame.request_id, request.operation);
                if frame.request_id == 0 {
                    send_response(
                        &control_tx,
                        frame.request_id,
                        response_error("invalid_request_id", "request IDs must be non-zero"),
                    )
                    .await;
                    continue;
                }
                let policy =
                    requests::operation_policy::OperationPolicy::for_raw(request.operation);
                if let Some(error) = policy.admission_error(read_only, client_hello.bulk_connection)
                {
                    send_response(
                        &control_tx,
                        frame.request_id,
                        response_error(error.code(), error.message()),
                    )
                    .await;
                    continue;
                }
                // Daemon shutdown intentionally remains a pre-dispatch exception: this endpoint
                // owns the shutdown sender, and acknowledging it must not register cancellable
                // work that can outlive the connection. Its mutation policy is still documented
                // and exhaustively tested with every other generated operation.
                if policy.handler == requests::operation_policy::Handler::Daemon {
                    if let Some(shutdown) = shutdown.clone() {
                        // The client hangs up right after this, and its EOF is
                        // the exit this loop actually takes. Remember the ask,
                        // so an orderly stop is not filed as a desktop that
                        // walked away.
                        shutdown_requested = true;
                        send_response(&control_tx, frame.request_id, response_ok()).await;
                        tokio::spawn(async move {
                            sleep(Duration::from_millis(100)).await;
                            let _ = shutdown.send(());
                        });
                    } else {
                        send_response(
                            &control_tx,
                            frame.request_id,
                            response_error(
                                "shutdown_unavailable",
                                "this protocol endpoint does not own a daemon",
                            ),
                        )
                        .await;
                    }
                    continue;
                }
                let cancellation = Arc::new(AtomicBool::new(false));
                let registration = {
                    use std::collections::hash_map::Entry;
                    let mut requests = pending.lock().unwrap();
                    if closed.load(Ordering::Acquire) {
                        Err("connection_closed")
                    } else {
                        match requests.entry(frame.request_id) {
                            Entry::Vacant(entry) => {
                                entry.insert(Arc::clone(&cancellation));
                                Ok(())
                            }
                            Entry::Occupied(_) => Err("duplicate_request_id"),
                        }
                    }
                };
                if let Err(code) = registration {
                    let message = if code == "connection_closed" {
                        "connection closed before request registration"
                    } else {
                        "request ID is already in flight"
                    };
                    send_response(&control_tx, frame.request_id, response_error(code, message))
                        .await;
                    continue;
                }
                if closed.load(Ordering::Acquire) {
                    cancellation.store(true, Ordering::Release);
                }
                let detached_work =
                    policy.scheduling == requests::operation_policy::Scheduling::Detached;
                let context = RequestContext {
                    control_tx: control_tx.clone(),
                    event_tx: control_tx.clone(),
                    generation: Arc::clone(&generation),
                    overflowed: Arc::clone(&overflowed),
                    subscribed: Arc::clone(&subscribed),
                    pending: Arc::clone(&pending),
                    terminal: Arc::clone(&terminal),
                    topology_lock: Arc::clone(&topology_lock),
                    topology_baseline: Arc::clone(&topology_baseline),
                    topology_signal: topology_signal.clone(),
                    files: Arc::clone(&files),
                    git: Arc::clone(&git),
                    bulk_connection: client_hello.bulk_connection,
                    // Both halves, because the comment on the field
                    // states a capability fact and `!read_only` alone is a
                    // proxy for it: a read-only host refuses a bulk
                    // connection outright, *and* a client that never asked
                    // for the bulk capability will not open one. Guessing
                    // from read-only alone hands any other non-read-only
                    // control client a diff body reference it cannot
                    // fetch, and an empty diff with it.
                    bulk_available: !read_only
                        && client_hello.requested_capabilities & CAP_BULK_DOWNLOAD != 0,
                    connection_epoch: client_hello.connection_epoch,
                    connection_id,
                    closed: Arc::clone(&closed),
                };
                if detached_work {
                    // Give the reader one bounded turn to consume an already
                    // buffered Cancel or EOF after synchronous registration.
                    // This closes the request+Cancel scheduling race without
                    // delaying ordinary work perceptibly.
                    tokio::spawn(async move {
                        sleep(Duration::from_millis(1)).await;
                        handle_request(frame.request_id, request, policy, cancellation, context)
                            .await;
                    });
                } else if ordered_tx
                    .try_send(OrderedRequest {
                        request_id: frame.request_id,
                        request,
                        policy,
                        cancellation,
                        context,
                    })
                    .is_err()
                {
                    end_reason = ConnectionEndReason::OrderedQueueFull;
                    read_error = Some(FrameError::Io(std::io::Error::other(
                        "ordered host operation queue is full or closed",
                    )));
                    break;
                }
            }
            _ => {
                send_response(
                    &control_tx,
                    frame.request_id,
                    response_error("invalid_payload", "expected request or cancellation"),
                )
                .await;
            }
        }
    }

    // Both the writer and the ordered lane mark the connection closed before
    // they report why, so the loop's own condition can win that race and the
    // exit arrives with the reason still sitting unread in a channel. Nothing
    // else consumes these, and taking one now only names the end: the result
    // this connection returns — and with it the safe log and the error
    // counters the daemon keeps — is deliberately left as the loop left it.
    if end_reason == ConnectionEndReason::ConnectionClosed {
        if let Ok(stop) = writer_stopped_rx.try_recv() {
            end_reason = stop.reason();
        } else if ordered_failed_rx.try_recv().is_ok() {
            end_reason = ConnectionEndReason::OrderedLaneStalled;
        }
    }
    // Only over an ordinary departure: a connection that asked for a shutdown
    // and then died of a writer deadline died of the writer deadline.
    if shutdown_requested
        && matches!(
            end_reason,
            ConnectionEndReason::ClientEof | ConnectionEndReason::ClientReset
        )
    {
        end_reason = ConnectionEndReason::DaemonShutdown;
    }
    let end_reason = ended(end_reason);
    closed.store(true, Ordering::Release);
    drop(ordered_tx);
    ordered_task.abort();
    let _ = (&mut ordered_task).await;
    output_credit.close();
    for (_, token) in pending.lock().unwrap().drain() {
        token.store(true, Ordering::Release);
    }
    // The topology actor sleeps up to its safety interval holding an event
    // sender; woken now, it observes `closed` and drops the clone immediately
    // instead of pushing every disconnect into the abort path below.
    topology_signal.wake();
    // Kill the terminal children and wake their waiters, but keep the worker
    // joins for after the writer is gone: a worker parked in the ordered-event
    // channel only unblocks once the receiver drops, so joining here — as this
    // teardown once did, under the terminal mutex, on a runtime worker — is
    // the deadlock this sequence exists to prevent.
    let terminal_teardown = terminal.lock().unwrap().signal_stop();
    // Git subscribers hold sender clones that are otherwise released only by
    // the service's drop — which runs after the await below, a circularity
    // that could never resolve.
    git.release_connection();
    drop(event_registration.take());
    drop(control_tx);
    if timeout(WRITER_DRAIN_GRACE, &mut writer_task).await.is_err() {
        // Something is still holding a sender. Aborting the writer drops the
        // receiver and the socket's write half in one move: every parked send
        // fails instantly, and the peer's bridge sees EOF and exits. A frame
        // truncated mid-write is acceptable — the peer's request stream has
        // already ended, so this connection is over either way.
        crate::diagnostics::record_connection_force_closed();
        eprintln!("connection teardown: writer did not drain within grace; aborting");
        writer_task.abort();
        let _ = writer_task.await;
    }
    let join = tokio::task::spawn_blocking(move || drop(terminal_teardown));
    if timeout(TERMINAL_JOIN_GRACE, join).await.is_err() {
        // The blocking task keeps running detached; leaking a wedged thread is
        // strictly better than hanging this connection task with it.
        crate::diagnostics::record_connection_force_closed();
        eprintln!("connection teardown: terminal workers did not join within grace; leaking them");
    }
    if let Some(error) = read_error {
        (Err(error.into()), end_reason)
    } else {
        (Ok(()), end_reason)
    }
}

fn is_clean_peer_disconnect(error: &FrameError) -> bool {
    matches!(
        error,
        FrameError::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
            )
    )
}

mod requests;
use requests::{RequestContext, handle_request};

fn reconcile_terminal_clients(
    terminal: &Arc<Mutex<TerminalClients>>,
    snapshot: &tmux_control::TmuxSnapshot,
    event_sender: &mpsc::Sender<SequencerControl>,
    overflowed: &Arc<AtomicBool>,
) {
    let mut terminal = terminal.lock().unwrap();
    reconcile_terminal_clients_locked(&mut terminal, snapshot, event_sender, overflowed);
}

fn reconcile_terminal_clients_if_open(
    closed: &AtomicBool,
    terminal: &Arc<Mutex<TerminalClients>>,
    snapshot: &tmux_control::TmuxSnapshot,
    event_sender: &mpsc::Sender<SequencerControl>,
    overflowed: &Arc<AtomicBool>,
) {
    // Serialize the final connection-close check with attachment creation.
    // Otherwise a topology discovery that started before EOF can attach a new
    // tmux control client after `serve_with_shutdown` has already stopped and
    // cleared the connection's existing clients.
    let mut terminal = terminal.lock().unwrap();
    if closed.load(Ordering::Acquire) {
        return;
    }
    reconcile_terminal_clients_locked(&mut terminal, snapshot, event_sender, overflowed);
}

fn reconcile_terminal_clients_locked(
    terminal: &mut TerminalClients,
    snapshot: &tmux_control::TmuxSnapshot,
    event_sender: &mpsc::Sender<SequencerControl>,
    overflowed: &Arc<AtomicBool>,
) {
    terminal.reconcile(snapshot);
    let mut panes_by_session: HashMap<&str, Vec<String>> = HashMap::new();
    for pane in &snapshot.panes {
        panes_by_session
            .entry(&pane.session_id)
            .or_default()
            .push(pane.id.clone());
    }
    for session in &snapshot.sessions {
        let Some(pane_ids) = panes_by_session.remove(session.id.as_str()) else {
            continue;
        };
        if let Err(error) = terminal.attach(
            &session.id,
            &pane_ids,
            false,
            event_sender.clone(),
            Arc::clone(overflowed),
        ) {
            emit_event(
                event_sender,
                overflowed,
                v1::HostEvent {
                    kind: v1::EventKind::TerminalExit.into(),
                    scope: session.id.clone(),
                    detail: format!("session control client failed: {error}"),
                    ..Default::default()
                },
            );
        }
    }
}

async fn lock_topology_generation<'a>(
    topology_lock: &'a tokio::sync::Mutex<()>,
    generation: &AtomicU64,
) -> (tokio::sync::MutexGuard<'a, ()>, u64) {
    let guard = topology_lock.lock().await;
    let generation = generation.load(Ordering::Acquire);
    (guard, generation)
}

fn baseline_changed(
    cached: Option<&(tmux_control::TmuxSnapshot, String)>,
    fresh: &tmux_control::TmuxSnapshot,
    fresh_identity: &str,
) -> bool {
    match cached {
        None => true,
        Some((snapshot, identity)) => {
            identity.as_str() != fresh_identity || !same_action_topology(snapshot, fresh)
        }
    }
}

fn same_action_topology(
    cached: &tmux_control::TmuxSnapshot,
    fresh: &tmux_control::TmuxSnapshot,
) -> bool {
    let cached = normalize_action_topology(cached.clone());
    let fresh = normalize_action_topology(fresh.clone());
    // Compare the normalized structural projection canonically so discovery
    // record ordering and every guarded field remain deterministic.
    let cached_json =
        serde_json::to_vec(&cached).expect("tmux snapshot serialization is infallible");
    let fresh_json = serde_json::to_vec(&fresh).expect("tmux snapshot serialization is infallible");
    cached_json == fresh_json
}

/// Names the first structural section where the cached baseline and a fresh
/// discovery disagree, so a `topologyDiff` in the timing log says *what* moved
/// rather than only that something did. Coarse on purpose: it runs once per
/// action, only on the path that already re-serializes both snapshots.
fn action_topology_diff(
    cached: Option<&(tmux_control::TmuxSnapshot, String)>,
    fresh: &tmux_control::TmuxSnapshot,
    fresh_identity: &str,
) -> &'static str {
    let Some((cached, identity)) = cached else {
        return "baseline:absent";
    };
    if identity != fresh_identity {
        return "identity";
    }
    let cached = normalize_action_topology(cached.clone());
    let fresh = normalize_action_topology(fresh.clone());
    if cached.sessions != fresh.sessions {
        return "sessions";
    }
    if cached.windows != fresh.windows {
        return "windows";
    }
    let membership = |snapshot: &tmux_control::TmuxSnapshot| {
        snapshot
            .panes
            .iter()
            .map(|pane| {
                (
                    pane.id.clone(),
                    pane.session_id.clone(),
                    pane.window_id.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    if membership(&cached) != membership(&fresh) {
        return "panes:membership";
    }
    let geometry = |snapshot: &tmux_control::TmuxSnapshot| {
        snapshot
            .panes
            .iter()
            .map(|pane| (pane.width, pane.height, pane.left, pane.top))
            .collect::<Vec<_>>()
    };
    if geometry(&cached) != geometry(&fresh) {
        return "panes:geometry";
    }
    if cached.panes != fresh.panes {
        return "panes:other";
    }
    "none"
}

fn normalize_action_topology(
    mut snapshot: tmux_control::TmuxSnapshot,
) -> tmux_control::TmuxSnapshot {
    // Control-client attachment counts and shell-driven automatic titles can
    // change asynchronously without invalidating an ID/layout operation.
    // Structural membership, indices, selection, layout, zoom and geometry
    // remain guarded and catch mutation-before-poll races.
    for session in &mut snapshot.sessions {
        session.attached_clients = 0;
        session.name.clear();
    }
    for window in &mut snapshot.windows {
        window.name.clear();
    }
    for pane in &mut snapshot.panes {
        pane.current_path.clear();
        pane.current_command.clear();
    }
    snapshot
}

async fn reconcile_internal_tmux_change(
    topology_lock: &Arc<tokio::sync::Mutex<()>>,
    topology_baseline: &Arc<Mutex<Option<(tmux_control::TmuxSnapshot, String)>>>,
    generation: &Arc<AtomicU64>,
    terminal: &Arc<Mutex<TerminalClients>>,
    event_tx: &mpsc::Sender<SequencerControl>,
    overflowed: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let _guard = topology_lock.lock().await;
    let (snapshot, identity) = tokio::task::spawn_blocking(tmux_actions::discover_before_action)
        .await
        .map_err(|error| anyhow::anyhow!("topology reconciliation task failed: {error}"))??;
    let changed = baseline_changed(
        topology_baseline.lock().unwrap().as_ref(),
        &snapshot,
        &identity,
    );
    if changed {
        let next_generation = generation.fetch_add(1, Ordering::AcqRel) + 1;
        *topology_baseline.lock().unwrap() = Some((snapshot.clone(), identity.clone()));
        reconcile_terminal_clients(terminal, &snapshot, event_tx, overflowed);
        event_tx
            .send(SequencerControl::OrderedEvent(v1::HostEvent {
                kind: v1::EventKind::TopologySnapshot.into(),
                scope: "topology".into(),
                snapshot: Some(snapshot_from_identity(snapshot, next_generation, identity)),
                detail: "app control-client topology reconciled".into(),
                ..Default::default()
            }))
            .await
            .map_err(|_| anyhow::anyhow!("topology event sequencer stopped"))?;
    }
    Ok(())
}

/// Queues one ordered event, reporting whether it was actually queued.
///
/// A caller that consumed the flag which *caused* this event needs the answer:
/// a dropped snapshot leaves the desktop believing a subtree is current when
/// the host already knows it is not.
fn emit_event(
    sender: &mpsc::Sender<SequencerControl>,
    overflowed: &AtomicBool,
    event: v1::HostEvent,
) -> bool {
    match sender.try_send(SequencerControl::OrderedEvent(event)) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(event)) => {
            let SequencerControl::OrderedEvent(event) = event else {
                unreachable!("try_send returns the message it was given")
            };
            // A recovery event is the desktop's only instruction to repair one
            // pane, and dropping it is how a pane stays frozen for the rest of
            // the session. Everything else keeps the old behaviour: the drop is
            // counted and the connection resyncs, which is a heavier repair
            // than the event would have been.
            if is_recovery_event(&event) && defer_recovery_event(sender, event) {
                return true;
            }
            crate::diagnostics::record_event_queue_overflow();
            overflowed.store(true, Ordering::Release);
            false
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            overflowed.store(true, Ordering::Release);
            false
        }
    }
}

/// Events whose loss strands a pane rather than merely delaying it.
///
/// Each one tells the desktop that a pane it is rendering needs an
/// authoritative seed, or that tmux has stopped that pane's output; none of
/// them carries state the next event supersedes, and all of them are idempotent
/// — which is what makes a late delivery acceptable and a lost one not.
fn is_recovery_event(event: &v1::HostEvent) -> bool {
    matches!(
        v1::EventKind::try_from(event.kind),
        Ok(v1::EventKind::TerminalResnapshotRequired
            | v1::EventKind::TerminalFlowStalled
            | v1::EventKind::TerminalFlowPaused
            | v1::EventKind::PaneResource)
    )
}

/// Deferred recovery events allowed to be in flight process-wide.
///
/// Past this a further deferral would be a queue of its own with no bound, so
/// the caller falls back to the connection-wide resync — which reseeds every
/// pane and therefore subsumes whatever the refused event was asking for.
const MAX_DEFERRED_RECOVERY_EVENTS: usize = 256;
static DEFERRED_RECOVERY_EVENTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Hands one refused recovery event to a sender that is allowed to wait.
///
/// The same shape `broadcast_control_event` already uses for its saturated
/// sinks, and for the same reason: the caller may be the control-stream reader,
/// which must never block — it is the only thread draining tmux's output, and
/// tmux stops reading its stdin while blocked writing to us. So the wait
/// happens somewhere else and this returns immediately. The event arrives after
/// the queue drains, out of order with respect to events queued behind it,
/// which is exactly the trade named above.
fn defer_recovery_event(sender: &mpsc::Sender<SequencerControl>, event: v1::HostEvent) -> bool {
    if DEFERRED_RECOVERY_EVENTS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |in_flight| {
            (in_flight < MAX_DEFERRED_RECOVERY_EVENTS).then_some(in_flight + 1)
        })
        .is_err()
    {
        return false;
    }
    crate::diagnostics::record_recovery_event_deferral();
    let sender = sender.clone();
    let message = SequencerControl::OrderedEvent(event);
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move {
            let _ = sender.send(message).await;
            DEFERRED_RECOVERY_EVENTS.fetch_sub(1, Ordering::AcqRel);
        });
    } else {
        std::thread::spawn(move || {
            let _ = sender.blocking_send(message);
            DEFERRED_RECOVERY_EVENTS.fetch_sub(1, Ordering::AcqRel);
        });
    }
    true
}

async fn send_response(
    sender: &mpsc::Sender<SequencerControl>,
    request_id: u64,
    response: v1::Response,
) {
    let _ = sender
        .send(SequencerControl::Response {
            request_id,
            response,
            snapshot_barrier: false,
        })
        .await;
}

async fn send_snapshot_response(
    sender: &mpsc::Sender<SequencerControl>,
    request_id: u64,
    response: v1::Response,
) {
    let _ = sender
        .send(SequencerControl::Response {
            request_id,
            response,
            snapshot_barrier: true,
        })
        .await;
}

fn response_ok() -> v1::Response {
    v1::Response {
        ok: true,
        ..Default::default()
    }
}

fn response_error(code: &str, message: &str) -> v1::Response {
    v1::Response {
        ok: false,
        error_code: code.into(),
        display_message: message.into(),
        ..Default::default()
    }
}

fn response_snapshot(snapshot: v1::Snapshot) -> v1::Response {
    v1::Response {
        ok: true,
        snapshot: Some(snapshot),
        ..Default::default()
    }
}

async fn send_handshake_error(
    stream: &mut UnixStream,
    code: &str,
    message: &str,
) -> anyhow::Result<()> {
    write_frame(
        stream,
        &envelope(
            0,
            0,
            Payload::Error(v1::Error {
                code: code.into(),
                display_message: message.into(),
                retryable: false,
            }),
        ),
    )
    .await?;
    Ok(())
}

fn command_version(mut command: Command, argument: &str) -> String {
    command
        .arg(argument)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default()
}

enum CommandVersion {
    Tmux,
    Git,
}

fn daemon_command_version(version: CommandVersion) -> String {
    static TMUX: OnceLock<String> = OnceLock::new();
    static GIT: OnceLock<String> = OnceLock::new();
    match version {
        CommandVersion::Tmux => TMUX
            .get_or_init(|| {
                tmux_control::tmux_command()
                    .map(|command| command_version(command, "-V"))
                    .unwrap_or_default()
            })
            .clone(),
        CommandVersion::Git => GIT
            .get_or_init(|| command_version(Command::new("git"), "--version"))
            .clone(),
    }
}

fn testing_enabled() -> bool {
    std::env::var_os("ADE_PHASE1_TESTING").is_some()
}

fn advertised_protocol_major() -> u32 {
    if testing_enabled()
        && let Ok(value) = std::env::var("ADE_PHASE1_TEST_PROTOCOL_MAJOR")
        && let Ok(value) = value.parse()
    {
        return value;
    }
    PROTOCOL_MAJOR
}

fn advertised_helper_version() -> String {
    if testing_enabled()
        && let Ok(value) = std::env::var("ADE_PHASE1_TEST_HELPER_VERSION")
        && !value.is_empty()
    {
        return value;
    }
    HELPER_VERSION.into()
}

#[cfg(test)]
#[path = "service/tests.rs"]
mod tests;
