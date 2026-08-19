//! Phase 12 performance harness (plan stage 12.8).
//!
//! Every number this driver prints is machine-measured against a live host over
//! the real protocol transport. Nothing here is an RPC-ack proxy for an
//! on-screen event: the keystroke probe blocks until tmux's echoed bytes arrive
//! back on the event stream, the create probe blocks until the new pane's seed
//! arrives, and the window-switch probe blocks until the revealed pane's
//! resource event arrives. Ack latency is reported separately and never
//! substituted for the observable number.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufReader, Write as _},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use tmux_agent_protocol::{
    HELPER_VERSION, HOST_CAPABILITIES, envelope, read_frame_sync,
    v1::{self, envelope::Payload},
    write_frame_sync,
};

#[derive(Clone)]
enum Transport {
    Local {
        host_binary: String,
        runtime: String,
        tmux_socket: String,
    },
    Ssh {
        config: String,
        target: String,
    },
}

impl Transport {
    fn bridge(&self) -> Result<Child> {
        let mut command = match self {
            Self::Local {
                host_binary,
                runtime,
                tmux_socket,
            } => {
                let mut command = Command::new(host_binary);
                command
                    .args(["bridge", "--stdio"])
                    .env("ADE_PHASE1_TESTING", "1")
                    .env("ADE_HOST_RUNTIME_DIR", runtime)
                    .env("ADE_TMUX_SOCKET_NAME", tmux_socket);
                command
            }
            Self::Ssh { config, target } => {
                let mut command = Command::new("ssh");
                command.args([
                    "-F",
                    config,
                    "-T",
                    "-o",
                    "BatchMode=yes",
                    target,
                    "env ADE_PHASE1_TESTING=1 $HOME/.local/bin/tmux-ide-host bridge --stdio",
                ]);
                command
            }
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("start Phase 12 perf bridge")
    }
}

// ---------------------------------------------------------------------------
// Statistics
// ---------------------------------------------------------------------------

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index]
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn stats(mut samples: Vec<f64>) -> serde_json::Value {
    samples.sort_by(|left, right| left.partial_cmp(right).unwrap());
    let sum: f64 = samples.iter().sum();
    let count = samples.len().max(1) as f64;
    json!({
        "n": samples.len(),
        "meanMs": round3(sum / count),
        "p50Ms": round3(percentile(&samples, 0.5)),
        "p95Ms": round3(percentile(&samples, 0.95)),
        "minMs": round3(samples.first().copied().unwrap_or_default()),
        "maxMs": round3(samples.last().copied().unwrap_or_default()),
    })
}

// ---------------------------------------------------------------------------
// Observations
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Observations {
    last_sequence: u64,
    gap: bool,
    frames: u64,
    event_counts: BTreeMap<String, usize>,
    pane_output_bytes: BTreeMap<String, u64>,
    seeded_panes: BTreeSet<String>,
    /// A pane the desktop can actually paint: either a fresh `TerminalSeed`
    /// arrived, or the reveal handed back a host-owned serialized snapshot.
    /// Both are the same user-visible event ("the pane has content").
    paintable_panes: BTreeSet<String>,
    visible_resources: BTreeSet<String>,
    /// Why each resync/resnapshot happened. A count alone cannot tell a benign
    /// pane-scoped reseed from a connection-wide resync, and the exit criteria
    /// distinguish them.
    resync_details: Vec<String>,
    /// Recovery the desktop cannot scope to one pane, which costs a full bridge
    /// reconnect and a reseed of every pane. The exit criteria budget this at
    /// zero; pane-scoped reseeds are ordinary and reported separately.
    connection_wide_resyncs: usize,
    topology_generation: u64,
    /// Latest topology pushed on the event stream. The desktop reads the new
    /// pane out of this push rather than paying an extra snapshot round trip,
    /// so the harness must do the same or it would measure a hop the product
    /// does not make.
    latest_snapshot: Option<v1::Snapshot>,
    /// Last delivered bytes per pane, seed-resetting, capped small. This is
    /// what lets a probe compare "what the app was told the pane contains"
    /// against tmux's own capture-pane ground truth.
    pane_tails: BTreeMap<String, Vec<u8>>,
    /// Panes whose delivered byte history is kept in full, uncapped. The capped
    /// `pane_tails` window answers "did this counter advance"; a vt parity check
    /// has to replay *every* byte the host delivered for the pane, in order,
    /// from its last seed, or the screen it reconstructs is not the screen the
    /// desktop would have.
    full_log_panes: BTreeSet<String>,
    pane_full_logs: BTreeMap<String, Vec<u8>>,
    /// Highest terminal generation delivered per pane. A hide has to declare
    /// the generation the client has actually applied, or the host replays
    /// output the client already has.
    pane_generations: BTreeMap<String, u64>,
}

const PANE_TAIL_BYTES: usize = 16 * 1024;

impl Observations {
    fn event_count(&self, kind: v1::EventKind) -> usize {
        self.event_counts
            .get(&format!("{kind:?}"))
            .copied()
            .unwrap_or_default()
    }

    fn resyncs(&self) -> usize {
        self.event_count(v1::EventKind::ResyncRequired)
            + self.event_count(v1::EventKind::TerminalResnapshotRequired)
    }

    fn output_bytes(&self, pane_id: &str) -> u64 {
        self.pane_output_bytes.get(pane_id).copied().unwrap_or(0)
    }

    fn track(&mut self, frame: &v1::Envelope) {
        self.frames += 1;
        let Some(Payload::Event(event)) = &frame.payload else {
            return;
        };
        if self.last_sequence != 0 && frame.sequence != self.last_sequence.saturating_add(1) {
            self.gap = true;
        }
        self.last_sequence = frame.sequence;
        let kind = v1::EventKind::try_from(event.kind).unwrap_or_default();
        *self.event_counts.entry(format!("{kind:?}")).or_default() += 1;
        if matches!(
            kind,
            v1::EventKind::ResyncRequired | v1::EventKind::TerminalResnapshotRequired
        ) {
            let pane_scoped = kind == v1::EventKind::TerminalResnapshotRequired
                && event.scope.len() > 1
                && event.scope.starts_with('%')
                && event.scope[1..].bytes().all(|byte| byte.is_ascii_digit());
            if !pane_scoped {
                self.connection_wide_resyncs += 1;
            }
            if self.resync_details.len() < 32 {
                self.resync_details
                    .push(format!("{kind:?} {} :: {}", event.scope, event.detail));
            }
        }
        if let Some(terminal) = &event.terminal {
            *self
                .pane_output_bytes
                .entry(terminal.pane_id.clone())
                .or_default() += terminal.data.len() as u64;
            let generation = self
                .pane_generations
                .entry(terminal.pane_id.clone())
                .or_default();
            *generation = (*generation).max(terminal.generation);
            let tail = self.pane_tails.entry(terminal.pane_id.clone()).or_default();
            if kind == v1::EventKind::TerminalSeed {
                self.seeded_panes.insert(terminal.pane_id.clone());
                self.paintable_panes.insert(terminal.pane_id.clone());
                tail.clear();
            }
            tail.extend_from_slice(&terminal.data);
            if tail.len() > PANE_TAIL_BYTES {
                tail.drain(..tail.len() - PANE_TAIL_BYTES);
            }
            if self.full_log_panes.contains(&terminal.pane_id) {
                let log = self
                    .pane_full_logs
                    .entry(terminal.pane_id.clone())
                    .or_default();
                // The renderer applies a seed as `replace(bytes, reset = true)`,
                // so a seed starts the pane's byte history over.
                if kind == v1::EventKind::TerminalSeed {
                    log.clear();
                }
                log.extend_from_slice(&terminal.data);
            }
        }
        if let Some(resource) = &event.pane_resource
            && resource.state == v1::PaneResourceState::Visible as i32
        {
            self.visible_resources.insert(resource.pane_id.clone());
            if !resource.serialized_snapshot.is_empty() {
                self.paintable_panes.insert(resource.pane_id.clone());
                if self.full_log_panes.contains(&resource.pane_id) {
                    // The desktop's reveal path is `restore(snapshot)` then the
                    // raw tail, and `restore` is `replace(bytes, reset = false)`
                    // — it does not reset the terminal — so these bytes extend
                    // the pane's history instead of replacing it.
                    let log = self
                        .pane_full_logs
                        .entry(resource.pane_id.clone())
                        .or_default();
                    log.extend_from_slice(&resource.serialized_snapshot);
                    log.extend_from_slice(&resource.raw_tail);
                }
            }
        }
        if let Some(snapshot) = &event.snapshot {
            self.observe_snapshot(snapshot);
        }
    }

    fn observe_snapshot(&mut self, snapshot: &v1::Snapshot) {
        if snapshot.generation >= self.topology_generation {
            self.topology_generation = snapshot.generation;
            self.latest_snapshot = Some(snapshot.clone());
        }
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

enum Incoming {
    Frame(Box<v1::Envelope>),
    Closed(String),
}

/// Matches `ACTION_RECONCILE_RETRIES` in `features/tmux/actionReconciliation.ts`.
const ACTION_RECONCILE_RETRIES: usize = 2;

struct PerfClient {
    child: Child,
    stdin: ChildStdin,
    incoming: mpsc::Receiver<Incoming>,
    next_request: u64,
    server_identity: String,
    obs: Observations,
    responses: BTreeMap<u64, v1::Response>,
    /// Matches `ACTION_RECONCILE_TIMEOUT_MS` in the desktop reconciliation loop.
    reconcile_timeout: Duration,
    connection_epoch: u64,
    output_credit: bool,
    delivered_terminal_bytes: u64,
    delivered_terminal_records: u64,
    acknowledged_terminal_bytes: u64,
    acknowledged_terminal_records: u64,
    terminal_record_ack_quantum: u64,
}

impl PerfClient {
    fn connect(transport: &Transport, reconcile_timeout: Duration) -> Result<Self> {
        let mut child = transport.bridge()?;
        let mut stdin = child.stdin.take().context("bridge stdin unavailable")?;
        let stdout = child.stdout.take().context("bridge stdout unavailable")?;
        let connection_epoch = 42;
        write_frame_sync(
            &mut stdin,
            &v1::Envelope {
                protocol_major: tmux_agent_protocol::PROTOCOL_MAJOR,
                protocol_minor: tmux_agent_protocol::PROTOCOL_MINOR,
                request_id: 1,
                sequence: 0,
                stream_id: 0,
                priority: v1::Priority::Control.into(),
                payload: Some(Payload::ClientHello(v1::ClientHello {
                    desktop_version: "performance-test-driver".into(),
                    requested_capabilities: HOST_CAPABILITIES,
                    expected_helper_version: HELPER_VERSION.into(),
                    bulk_connection: false,
                    connection_epoch,
                    ..Default::default()
                })),
            },
        )?;
        let mut reader: BufReader<ChildStdout> = BufReader::new(stdout);
        let hello = read_frame_sync(&mut reader)?.context("bridge closed during handshake")?;
        let Some(Payload::ServerHello(hello)) = hello.payload else {
            bail!("host did not return ServerHello");
        };
        ensure!(
            !hello.read_only,
            "Phase 12 perf bridge unexpectedly read-only"
        );
        let (sender, incoming) = mpsc::channel();
        thread::Builder::new()
            .name("phase12-perf-reader".into())
            .spawn(move || {
                loop {
                    match read_frame_sync(&mut reader) {
                        Ok(Some(frame)) => {
                            if sender.send(Incoming::Frame(Box::new(frame))).is_err() {
                                break;
                            }
                        }
                        Ok(None) => {
                            let _ = sender.send(Incoming::Closed("bridge closed".into()));
                            break;
                        }
                        Err(error) => {
                            let _ = sender.send(Incoming::Closed(error.to_string()));
                            break;
                        }
                    }
                }
            })?;
        let terminal_record_ack_quantum = u64::from(hello.terminal_output_window_records)
            .saturating_div(2)
            .max(1);
        Ok(Self {
            child,
            stdin,
            incoming,
            next_request: 10,
            server_identity: hello.server_identity,
            obs: Observations::default(),
            responses: BTreeMap::new(),
            reconcile_timeout,
            connection_epoch,
            output_credit: hello.capabilities
                & tmux_agent_protocol::CAP_TERMINAL_OUTPUT_CREDIT
                != 0,
            delivered_terminal_bytes: 0,
            delivered_terminal_records: 0,
            acknowledged_terminal_bytes: 0,
            acknowledged_terminal_records: 0,
            terminal_record_ack_quantum,
        })
    }

    /// Writes a request frame without waiting for its response.
    fn send(&mut self, operation: v1::Operation, mut request: v1::Request) -> Result<u64> {
        self.next_request += 1;
        let request_id = self.next_request;
        request.operation = operation.into();
        write_frame_sync(
            &mut self.stdin,
            &envelope(request_id, 0, Payload::Request(request)),
        )?;
        Ok(request_id)
    }

    /// Consumes frames until `predicate` holds or `deadline` expires. Returns
    /// whether the predicate was satisfied.
    fn pump_until(
        &mut self,
        deadline: Instant,
        mut predicate: impl FnMut(&Observations, &BTreeMap<u64, v1::Response>) -> bool,
    ) -> Result<bool> {
        loop {
            if predicate(&self.obs, &self.responses) {
                return Ok(true);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            match self.incoming.recv_timeout(deadline - now) {
                Ok(Incoming::Frame(frame)) => {
                    if let Some(Payload::Response(response)) = frame.payload.clone() {
                        self.responses.insert(frame.request_id, response);
                    }
                    self.obs.track(&frame);
                    if let Some(Payload::Event(event)) = frame.payload.as_ref() {
                        self.delivered_terminal_bytes = self
                            .delivered_terminal_bytes
                            .saturating_add(event.terminal_delivery_bytes);
                        self.delivered_terminal_records = self
                            .delivered_terminal_records
                            .saturating_add(event.terminal_delivery_records);
                    }
                    if self.output_credit
                        && (self
                            .delivered_terminal_bytes
                            .saturating_sub(self.acknowledged_terminal_bytes)
                            >= 512 * 1024
                            || self
                                .delivered_terminal_records
                                .saturating_sub(self.acknowledged_terminal_records)
                                >= self.terminal_record_ack_quantum)
                    {
                        write_frame_sync(
                            &mut self.stdin,
                            &envelope(
                                0,
                                0,
                                Payload::TerminalOutputAck(v1::TerminalOutputAck {
                                    connection_epoch: self.connection_epoch,
                                    cumulative_bytes: self.delivered_terminal_bytes,
                                    cumulative_records: self.delivered_terminal_records,
                                }),
                            ),
                        )?;
                        self.acknowledged_terminal_bytes = self.delivered_terminal_bytes;
                        self.acknowledged_terminal_records = self.delivered_terminal_records;
                    }
                }
                Ok(Incoming::Closed(reason)) => bail!("bridge closed: {reason}"),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Ok(predicate(&self.obs, &self.responses));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => bail!("bridge reader stopped"),
            }
        }
    }

    /// Counts frames arriving over an idle window without issuing any request.
    fn idle_frames(&mut self, window: Duration) -> Result<u64> {
        let before = self.obs.frames;
        let deadline = Instant::now() + window;
        self.pump_until(deadline, |_, _| false)?;
        Ok(self.obs.frames - before)
    }

    fn request(&mut self, operation: v1::Operation, request: v1::Request) -> Result<v1::Response> {
        let request_id = self.send(operation, request)?;
        let satisfied = self
            .pump_until(Instant::now() + Duration::from_secs(30), |_, responses| {
                responses.contains_key(&request_id)
            })?;
        ensure!(satisfied, "host request {request_id} timed out");
        Ok(self
            .responses
            .remove(&request_id)
            .expect("response present"))
    }

    fn subscribe(&mut self) -> Result<v1::Snapshot> {
        let response = self.request(v1::Operation::Subscribe, v1::Request::default())?;
        ensure!(
            response.ok,
            "subscribe failed: {}",
            response.display_message
        );
        self.obs.last_sequence = response.accepted_sequence;
        let snapshot = response.snapshot.context("subscribe omitted snapshot")?;
        self.obs.observe_snapshot(&snapshot);
        Ok(snapshot)
    }

    fn snapshot(&mut self) -> Result<v1::Snapshot> {
        let response = self.request(
            v1::Operation::FullSnapshot,
            v1::Request {
                scope: "topology".into(),
                ..Default::default()
            },
        )?;
        ensure!(response.ok, "snapshot failed: {}", response.display_message);
        let snapshot = response
            .snapshot
            .context("snapshot response omitted topology")?;
        self.obs.observe_snapshot(&snapshot);
        Ok(snapshot)
    }

    /// The topology the desktop currently holds, as pushed on the event stream.
    fn cached_snapshot(&self) -> Result<v1::Snapshot> {
        self.obs
            .latest_snapshot
            .clone()
            .context("no topology has been observed yet")
    }

    /// Sends an action against the cached topology, exactly as the desktop
    /// does: no extra snapshot round trip is paid before the mutation.
    fn send_action(&mut self, mut action: v1::TmuxAction) -> Result<u64> {
        action.expected_server_identity = self.server_identity.clone();
        action.expected_generation = self.obs.topology_generation;
        self.send(
            v1::Operation::TmuxAction,
            v1::Request {
                tmux_action: Some(action),
                ..Default::default()
            },
        )
    }

    /// Mirrors the desktop's `requestReconciledTmuxAction`: a `stale_topology`
    /// rejection waits for a newer pushed generation and retries. The elapsed
    /// time this costs is exactly what the user waits for, so the caller times
    /// the whole loop rather than the last attempt.
    fn action_reconciled(&mut self, action: v1::TmuxAction) -> Result<(v1::Response, usize)> {
        let reconcile_timeout = self.reconcile_timeout;
        for retry in 0..=ACTION_RECONCILE_RETRIES {
            let attempted_generation = self.obs.topology_generation;
            let request_id = self.send_action(action.clone())?;
            let satisfied = self
                .pump_until(Instant::now() + Duration::from_secs(30), |_, responses| {
                    responses.contains_key(&request_id)
                })?;
            ensure!(satisfied, "tmux action ack timed out");
            let response = self.responses.remove(&request_id).expect("ack present");
            if response.ok
                || response.error_code != "stale_topology"
                || retry == ACTION_RECONCILE_RETRIES
            {
                return Ok((response, retry));
            }
            let deadline = Instant::now() + reconcile_timeout;
            self.pump_until(deadline, |obs, _| {
                obs.topology_generation > attempted_generation
            })?;
            if self.obs.topology_generation <= attempted_generation {
                return Ok((response, retry));
            }
        }
        unreachable!("retry loop always returns")
    }

    /// Reveals a pane and blocks until its seed bytes arrive. This pair is what
    /// makes a freshly created pane paintable, so it belongs inside the
    /// create-action budget rather than after it.
    fn reveal_and_await_seed(&mut self, pane_id: &str, deadline: Instant) -> Result<()> {
        // Create-session can attach/select and publish the new pane's seed
        // before its ordered action acknowledgement. That seed is exactly the
        // backlog the desktop consumes when it mounts the pane; erasing it
        // here would demand a duplicate seed the product neither needs nor
        // promises. A pane not yet paintable still has to become so below.
        let reveal_id = self.set_visible(pane_id, true, Vec::new())?;
        let owned = pane_id.to_owned();
        let satisfied = self.pump_until(deadline, |obs, responses| {
            obs.paintable_panes.contains(&owned) && responses.contains_key(&reveal_id)
        })?;
        let response = self.responses.remove(&reveal_id);
        if let Some(ref response) = response {
            ensure!(
                response.ok,
                "reveal rejected: {} {}",
                response.error_code,
                response.display_message
            );
        }
        ensure!(
            satisfied,
            "pane {pane_id} was never seeded after reveal; response={response:?}; events={:?}; resyncs={:?}; visible={}; seeded={}; paintable={}",
            self.obs.event_counts,
            self.obs.resync_details,
            self.obs.visible_resources.contains(pane_id),
            self.obs.seeded_panes.contains(pane_id),
            self.obs.paintable_panes.contains(pane_id),
        );
        Ok(())
    }

    fn attach(&mut self, snapshot: &v1::Snapshot, session_id: &str) -> Result<()> {
        let pane_ids = snapshot
            .panes
            .iter()
            .filter(|pane| pane.session_id == session_id)
            .map(|pane| pane.id.clone())
            .collect();
        let response = self.request(
            v1::Operation::AttachTerminal,
            v1::Request {
                session_id: session_id.into(),
                pane_ids,
                ..Default::default()
            },
        )?;
        ensure!(
            response.ok,
            "terminal attach failed: {}",
            response.display_message
        );
        Ok(())
    }

    fn input(&mut self, pane_id: &str, data: &[u8]) -> Result<()> {
        let response = self.request(
            v1::Operation::TerminalInput,
            v1::Request {
                scope: pane_id.into(),
                data: data.to_vec(),
                ..Default::default()
            },
        )?;
        ensure!(
            response.ok,
            "terminal input failed: {}",
            response.display_message
        );
        Ok(())
    }

    fn action_with_snapshot(
        &mut self,
        snapshot: &v1::Snapshot,
        mut action: v1::TmuxAction,
    ) -> Result<v1::Response> {
        action.expected_server_identity = self.server_identity.clone();
        action.expected_generation = snapshot.generation;
        self.request(
            v1::Operation::TmuxAction,
            v1::Request {
                tmux_action: Some(action),
                ..Default::default()
            },
        )
    }

    fn set_visible(&mut self, pane_id: &str, visible: bool, snapshot: Vec<u8>) -> Result<u64> {
        self.send(
            v1::Operation::SetTerminalVisibility,
            v1::Request {
                scope: pane_id.into(),
                visible,
                data: snapshot,
                // The driver owns no renderer, so it declares a constant
                // non-zero epoch and a zero output cutoff: it never claims to
                // have rendered host output, which keeps the host's replay
                // journal authoritative for everything after the hide.
                terminal_epoch: 1,
                terminal_generation_cutoff: 0,
                ..Default::default()
            },
        )
    }

    /// Hides a pane the way the desktop does: with the client's own serialized
    /// screen, and the generation it has actually applied as the cutoff. The
    /// renderer-less form in [`set_visible`] leaves the host with nothing to
    /// hand back on reveal, so the restore path — the U003 mechanism the parity
    /// lane is built to catch — would never run.
    fn hide_with_screen(&mut self, pane_id: &str, snapshot: Vec<u8>) -> Result<u64> {
        let cutoff = self
            .obs
            .pane_generations
            .get(pane_id)
            .copied()
            .unwrap_or_default();
        self.send(
            v1::Operation::SetTerminalVisibility,
            v1::Request {
                scope: pane_id.into(),
                visible: false,
                data: snapshot,
                terminal_epoch: 1,
                terminal_generation_cutoff: cutoff,
                ..Default::default()
            },
        )
    }

    /// Waits until the pane is quiet for `quiet` with no new output bytes.
    fn settle(&mut self, pane_id: &str, quiet: Duration) -> Result<()> {
        let overall = Instant::now() + Duration::from_secs(20);
        loop {
            let before = self.obs.output_bytes(pane_id);
            self.pump_until(Instant::now() + quiet, |obs, _| {
                obs.output_bytes(pane_id) > before
            })?;
            if self.obs.output_bytes(pane_id) == before {
                return Ok(());
            }
            ensure!(Instant::now() < overall, "pane {pane_id} never went quiet");
        }
    }

    fn settle_topology(&mut self, quiet: Duration) -> Result<()> {
        let overall = Instant::now() + Duration::from_secs(20);
        loop {
            let before = self.obs.topology_generation;
            self.pump_until(Instant::now() + quiet, |obs, _| {
                obs.topology_generation > before
            })?;
            if self.obs.topology_generation == before {
                return Ok(());
            }
            ensure!(Instant::now() < overall, "topology never went quiet");
        }
    }
}

impl Drop for PerfClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn session<'a>(snapshot: &'a v1::Snapshot, name: &str) -> Result<&'a v1::Session> {
    snapshot
        .sessions
        .iter()
        .find(|session| session.name == name)
        .with_context(|| format!("session {name:?} missing from snapshot"))
}

fn action(kind: v1::TmuxActionKind) -> v1::TmuxAction {
    v1::TmuxAction {
        kind: kind.into(),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Probes
// ---------------------------------------------------------------------------

struct KeystrokeSamples {
    echo: Vec<f64>,
    ack: Vec<f64>,
}

/// Keystroke → glyph echo. One byte is written, and the probe blocks until the
/// pane's echoed bytes come back on the event stream. That round trip is the
/// observable "the character appeared" event for everything below the renderer.
fn probe_keystroke(
    client: &mut PerfClient,
    pane_id: &str,
    samples: usize,
) -> Result<KeystrokeSamples> {
    probe_keystroke_marked(client, pane_id, samples, None)
}

/// As [`probe_keystroke`], but for a pane that also produces output of its own.
///
/// A pane that repaints on a timer satisfies "more bytes arrived" without having
/// answered the keystroke at all, which would measure the timer instead of the
/// round trip. When `marker` is given, the sample ends only once the fixture's
/// own `<marker><n>` counter advances — that byte sequence exists only in the
/// keystroke response.
fn probe_keystroke_marked(
    client: &mut PerfClient,
    pane_id: &str,
    samples: usize,
    marker: Option<&str>,
) -> Result<KeystrokeSamples> {
    client.settle(pane_id, Duration::from_millis(300))?;
    let marker = marker.map(|value| value.as_bytes().to_vec());
    let responded = |obs: &Observations, before_bytes: u64, before_count: Option<u64>| match &marker
    {
        None => obs.output_bytes(pane_id) > before_bytes,
        Some(prefix) => {
            let current = obs
                .pane_tails
                .get(pane_id)
                .and_then(|tail| last_counter(tail, prefix));
            match (current, before_count) {
                (Some(current), Some(before)) => current > before,
                (Some(_), None) => true,
                _ => false,
            }
        }
    };
    let mut echo = Vec::new();
    let mut ack = Vec::new();
    for index in 0..samples {
        let before = client.obs.output_bytes(pane_id);
        let before_count = marker.as_ref().and_then(|prefix| {
            client
                .obs
                .pane_tails
                .get(pane_id)
                .and_then(|tail| last_counter(tail, prefix))
        });
        let started = Instant::now();
        let request_id = client.send(
            v1::Operation::TerminalInput,
            v1::Request {
                scope: pane_id.into(),
                data: b"x".to_vec(),
                ..Default::default()
            },
        )?;
        let mut ack_ms: Option<f64> = None;
        let satisfied =
            client.pump_until(started + Duration::from_secs(10), |obs, responses| {
                if ack_ms.is_none() && responses.contains_key(&request_id) {
                    ack_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
                }
                responded(obs, before, before_count) && ack_ms.is_some()
            })?;
        let echo_ms = started.elapsed().as_secs_f64() * 1000.0;
        ensure!(satisfied, "keystroke {index} was never echoed by the pane");
        let response = client
            .responses
            .remove(&request_id)
            .context("keystroke ack disappeared")?;
        ensure!(
            response.ok,
            "keystroke rejected: {} {}",
            response.error_code,
            response.display_message
        );
        // Discard warm-up samples: the first keystrokes pay one-time tmux
        // command-surface probes that never recur in a live session.
        if index >= 5 {
            echo.push(echo_ms);
            ack.push(ack_ms.unwrap_or(echo_ms));
        }
    }
    // Leave no typed text behind in the pane.
    client.input(pane_id, b"\x15")?;
    client.settle(pane_id, Duration::from_millis(200))?;
    Ok(KeystrokeSamples { echo, ack })
}

/// Pipelined burst: the Phase 2 driver's 128-keystroke + Enter shape, reported
/// per operation so it is comparable with the single-keystroke number.
fn probe_burst(client: &mut PerfClient, pane_id: &str, rounds: usize) -> Result<Vec<f64>> {
    let mut per_op = Vec::new();
    for _ in 0..rounds {
        client.settle(pane_id, Duration::from_millis(200))?;
        let started = Instant::now();
        let mut pending = Vec::new();
        for _ in 0..128 {
            pending.push(client.send(
                v1::Operation::TerminalInput,
                v1::Request {
                    scope: pane_id.into(),
                    data: b"#".to_vec(),
                    ..Default::default()
                },
            )?);
        }
        pending.push(client.send(
            v1::Operation::TerminalInput,
            v1::Request {
                scope: pane_id.into(),
                data: b"\r".to_vec(),
                ..Default::default()
            },
        )?);
        let satisfied = client.pump_until(started + Duration::from_secs(60), |_, responses| {
            pending.iter().all(|id| responses.contains_key(id))
        })?;
        ensure!(satisfied, "pipelined keystroke burst did not complete");
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        for id in &pending {
            let response = client.responses.remove(id).expect("burst response present");
            ensure!(
                response.ok,
                "burst keystroke rejected: {}",
                response.error_code
            );
        }
        per_op.push(elapsed / 129.0);
    }
    Ok(per_op)
}

struct ActionSamples {
    ack: Vec<f64>,
    interactive: Vec<f64>,
    stale_retries: usize,
}

/// New tab: action request → the new pane's seed event. "Interactive" is the
/// point at which the desktop has the bytes it needs to paint the pane, which
/// is the budget's definition, not the action ack.
fn probe_create_window(
    client: &mut PerfClient,
    session_id: &str,
    rounds: usize,
) -> Result<ActionSamples> {
    let mut ack = Vec::new();
    let mut interactive = Vec::new();
    let mut stale_retries = 0;
    for index in 0..rounds {
        let mut create = action(v1::TmuxActionKind::CreateWindow);
        create.session_id = session_id.into();
        create.name = format!("perf-window-{index}");
        let started = Instant::now();
        let (response, retries) = client.action_reconciled(create)?;
        let ack_ms = started.elapsed().as_secs_f64() * 1000.0;
        stale_retries += retries;
        ensure!(
            response.ok,
            "create window failed: {} {}",
            response.error_code,
            response.display_message
        );
        let window_id = response
            .tmux_action_result
            .context("create window omitted result")?
            .window_id;
        let topology_arrived = client.pump_until(started + Duration::from_secs(30), |obs, _| {
            obs.latest_snapshot.as_ref().is_some_and(|snapshot| {
                snapshot
                    .panes
                    .iter()
                    .any(|pane| pane.window_id == window_id)
            })
        })?;
        ensure!(topology_arrived, "created window topology was never pushed");
        let pane_id = client
            .cached_snapshot()?
            .panes
            .iter()
            .find(|pane| pane.window_id == window_id)
            .map(|pane| pane.id.clone())
            .context("created window has no pane in the pushed topology")?;
        client.reveal_and_await_seed(&pane_id, started + Duration::from_secs(30))?;
        interactive.push(started.elapsed().as_secs_f64() * 1000.0);
        ack.push(ack_ms);

        let mut close = action(v1::TmuxActionKind::CloseWindow);
        close.window_id = window_id;
        close.confirmed = true;
        let (response, _) = client.action_reconciled(close)?;
        ensure!(
            response.ok,
            "close window failed: {} {}",
            response.error_code,
            response.display_message
        );
        client.settle_topology(Duration::from_millis(300))?;
    }
    Ok(ActionSamples {
        ack,
        interactive,
        stale_retries,
    })
}

/// New workspace: the same measurement for CreateSession.
fn probe_create_session(
    client: &mut PerfClient,
    return_session_id: &str,
    rounds: usize,
) -> Result<ActionSamples> {
    let mut ack = Vec::new();
    let mut interactive = Vec::new();
    let mut stale_retries = 0;
    for index in 0..rounds {
        let mut create = action(v1::TmuxActionKind::CreateSession);
        create.name = format!("ade-phase12-perf-{index}");
        let started = Instant::now();
        let (response, retries) = client.action_reconciled(create)?;
        let ack_ms = started.elapsed().as_secs_f64() * 1000.0;
        stale_retries += retries;
        ensure!(
            response.ok,
            "create session failed: {} {}",
            response.error_code,
            response.display_message
        );
        let session_id = response
            .tmux_action_result
            .context("create session omitted result")?
            .session_id;
        let topology_arrived = client.pump_until(started + Duration::from_secs(30), |obs, _| {
            obs.latest_snapshot.as_ref().is_some_and(|snapshot| {
                snapshot
                    .panes
                    .iter()
                    .any(|pane| pane.session_id == session_id)
            })
        })?;
        ensure!(
            topology_arrived,
            "created session topology was never pushed"
        );
        let pane_id = client
            .cached_snapshot()?
            .panes
            .iter()
            .find(|pane| pane.session_id == session_id)
            .map(|pane| pane.id.clone())
            .context("created session has no pane in the pushed topology")?;
        client.reveal_and_await_seed(&pane_id, started + Duration::from_secs(30))?;
        interactive.push(started.elapsed().as_secs_f64() * 1000.0);
        ack.push(ack_ms);

        let mut close = action(v1::TmuxActionKind::CloseSession);
        close.session_id = session_id;
        close.confirmed = true;
        let (response, _) = client.action_reconciled(close)?;
        ensure!(
            response.ok,
            "close session failed: {} {}",
            response.error_code,
            response.display_message
        );
        // Create-session now atomically selects its new control client. Restore
        // the fixture's primary client after deleting the measured session so
        // later resize/switch probes have a live visible owner. This cleanup is
        // intentionally outside the create journey's timed interval.
        let mut select = action(v1::TmuxActionKind::SelectSession);
        select.session_id = return_session_id.to_owned();
        let (response, _) = client.action_reconciled(select)?;
        ensure!(
            response.ok,
            "primary-session restore failed: {} {}",
            response.error_code,
            response.display_message
        );
        client.settle_topology(Duration::from_millis(300))?;
    }
    Ok(ActionSamples {
        ack,
        interactive,
        stale_retries,
    })
}

/// Window switch: hide one pane and reveal the other, timed to the revealed
/// pane's authoritative resource event, with the snapshot payload the desktop
/// actually ships on a hide.
fn probe_window_switch(
    client: &mut PerfClient,
    left: &str,
    right: &str,
    snapshot_bytes: usize,
    rounds: usize,
) -> Result<Vec<f64>> {
    let payload = vec![b'.'; snapshot_bytes];
    let mut samples = Vec::new();
    for index in 0..rounds {
        let (hide, reveal) = if index % 2 == 0 {
            (left, right)
        } else {
            (right, left)
        };
        client.obs.visible_resources.remove(reveal);
        let started = Instant::now();
        let hide_id = client.set_visible(hide, false, payload.clone())?;
        let reveal_id = client.set_visible(reveal, true, Vec::new())?;
        let satisfied =
            client.pump_until(started + Duration::from_secs(30), |obs, responses| {
                obs.visible_resources.contains(reveal)
                    && responses.contains_key(&hide_id)
                    && responses.contains_key(&reveal_id)
            })?;
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        ensure!(satisfied, "window switch never revealed {reveal}");
        for id in [hide_id, reveal_id] {
            let response = client
                .responses
                .remove(&id)
                .context("visibility ack lost")?;
            ensure!(
                response.ok,
                "visibility change rejected: {} {}",
                response.error_code,
                response.display_message
            );
        }
        if index >= 2 {
            samples.push(elapsed);
        }
    }
    // Leave both panes visible.
    for pane in [left, right] {
        let id = client.set_visible(pane, true, Vec::new())?;
        client.pump_until(Instant::now() + Duration::from_secs(10), |_, responses| {
            responses.contains_key(&id)
        })?;
        client.responses.remove(&id);
    }
    Ok(samples)
}

struct FloodOutcome {
    megabytes_per_second: f64,
    bytes: u64,
    resyncs_during: usize,
    typing_during_flood: Vec<f64>,
    input_ack_during_flood: Vec<f64>,
}

/// Sustained output: floods one pane while typing into another, so the flood
/// throughput and the interactive latency under that flood are one measurement.
fn probe_flood(
    client: &mut PerfClient,
    flood_pane: &str,
    typing_pane: &str,
    seconds: u64,
) -> Result<FloodOutcome> {
    client.settle(flood_pane, Duration::from_millis(300))?;
    client.settle(typing_pane, Duration::from_millis(300))?;
    let resyncs_before = client.obs.connection_wide_resyncs;
    let bytes_before = client.obs.output_bytes(flood_pane);
    client.input(flood_pane, b"yes ade-phase12-flood-payload-line\r")?;
    let started = Instant::now();
    let flood_end = started + Duration::from_secs(seconds);
    let mut typing = Vec::new();
    let mut input_acks = Vec::new();
    while Instant::now() < flood_end {
        let before = client.obs.output_bytes(typing_pane);
        let keystroke_started = Instant::now();
        let request_id = client.send(
            v1::Operation::TerminalInput,
            v1::Request {
                scope: typing_pane.into(),
                data: b"y".to_vec(),
                ..Default::default()
            },
        )?;
        let deadline = keystroke_started + Duration::from_secs(10);
        let acknowledged = client.pump_until(deadline, |_, responses| {
            responses.contains_key(&request_id)
        })?;
        if acknowledged {
            input_acks.push(keystroke_started.elapsed().as_secs_f64() * 1000.0);
        }
        let echoed = client.obs.output_bytes(typing_pane) > before
            || client.pump_until(deadline, |obs, _| obs.output_bytes(typing_pane) > before)?;
        if acknowledged && echoed {
            typing.push(keystroke_started.elapsed().as_secs_f64() * 1000.0);
        }
        client.responses.remove(&request_id);
        let pause = Instant::now() + Duration::from_millis(60);
        client.pump_until(pause, |_, _| false)?;
    }
    let elapsed = started.elapsed().as_secs_f64();
    client.input(flood_pane, b"\x03")?;
    let bytes = client.obs.output_bytes(flood_pane) - bytes_before;
    let resyncs_during = client.obs.connection_wide_resyncs - resyncs_before;
    client.input(typing_pane, b"\x15")?;
    // Let the pane drain whatever tmux had already queued before Ctrl-C landed.
    let _ = client.settle(flood_pane, Duration::from_millis(500));
    Ok(FloodOutcome {
        megabytes_per_second: round3(bytes as f64 / 1_048_576.0 / elapsed),
        bytes,
        resyncs_during,
        typing_during_flood: typing,
        input_ack_during_flood: input_acks,
    })
}

/// Resize settle: the client-side resize round trip the app performs after its
/// own debounce.
fn probe_resize(client: &mut PerfClient, rounds: usize) -> Result<Vec<f64>> {
    let mut samples = Vec::new();
    for index in 0..rounds {
        let columns = if index % 2 == 0 { 120 } else { 100 };
        let started = Instant::now();
        let response = client.request(
            v1::Operation::ResizeTerminal,
            v1::Request {
                columns,
                rows: 32,
                ..Default::default()
            },
        )?;
        ensure!(response.ok, "resize failed: {}", response.display_message);
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    Ok(samples)
}

// ---------------------------------------------------------------------------
// Scenario
// ---------------------------------------------------------------------------

fn run_perf(
    transport: Transport,
    primary_name: &str,
    label: &str,
    reconcile_timeout: Duration,
    flood_seconds: u64,
) -> Result<()> {
    let mut client = PerfClient::connect(&transport, reconcile_timeout)?;
    let initial = client.subscribe()?;
    let primary = session(&initial, primary_name)?.clone();
    client.attach(&initial, &primary.id)?;

    let first_pane = initial
        .panes
        .iter()
        .find(|pane| pane.session_id == primary.id)
        .context("primary session has no pane")?
        .clone();

    // A second pane in the same window gives the flood/typing pair and the
    // window-switch pair without depending on the fixture's window layout.
    let snapshot = client.snapshot()?;
    let mut split = action(v1::TmuxActionKind::SplitPaneRight);
    split.pane_id = first_pane.id.clone();
    let response = client.action_with_snapshot(&snapshot, split)?;
    ensure!(
        response.ok,
        "perf split failed: {} {}",
        response.error_code,
        response.display_message
    );
    let second_pane = response
        .tmux_action_result
        .context("split omitted result")?
        .pane_id;
    client.reveal_and_await_seed(&second_pane, Instant::now() + Duration::from_secs(30))?;

    let keystroke = probe_keystroke(&mut client, &first_pane.id, 45)?;
    let burst = probe_burst(&mut client, &first_pane.id, 3)?;
    let create_window = probe_create_window(&mut client, &primary.id, 5)?;
    let create_session = probe_create_session(&mut client, &primary.id, 5)?;
    ensure!(
        create_window.stale_retries == 0 && create_session.stale_retries == 0,
        "settled create journeys must use exactly one action request each"
    );
    let window_switch_empty = probe_window_switch(&mut client, &first_pane.id, &second_pane, 0, 8)?;
    let window_switch_1mib =
        probe_window_switch(&mut client, &first_pane.id, &second_pane, 1024 * 1024, 6)?;
    let resize = probe_resize(&mut client, 8)?;
    let flood = probe_flood(&mut client, &second_pane, &first_pane.id, flood_seconds)?;

    // Idle traffic: no request is issued for the window, so any frame here is a
    // periodic host-side round trip.
    client.settle(&first_pane.id, Duration::from_millis(500))?;
    let idle_frames = client.idle_frames(Duration::from_secs(8))?;

    // Restore the fixture: close the pane this scenario created.
    let snapshot = client.snapshot()?;
    let mut close = action(v1::TmuxActionKind::ClosePane);
    close.pane_id = second_pane.clone();
    close.confirmed = true;
    let response = client.action_with_snapshot(&snapshot, close)?;
    ensure!(
        response.ok,
        "perf pane cleanup failed: {} {}",
        response.error_code,
        response.display_message
    );

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "label": label,
            "transport": match &transport { Transport::Local { .. } => "local", Transport::Ssh { .. } => "ssh" },
            "keystrokeEchoMs": stats(keystroke.echo),
            "keystrokeAckMs": stats(keystroke.ack),
            "burst129PerOpMs": stats(burst),
            "createWindowAckMs": stats(create_window.ack),
            "createWindowInteractiveMs": stats(create_window.interactive),
            "createWindowActionRequests": 5 + create_window.stale_retries,
            "createSessionAckMs": stats(create_session.ack),
            "createSessionInteractiveMs": stats(create_session.interactive),
            "createSessionActionRequests": 5 + create_session.stale_retries,
            // A successful create/select acknowledgement is the desktop's
            // matching visible-session fact. The performance client follows
            // that production contract and sends no second session-selection
            // request for either create journey.
            "matchingSessionRestatementRequests": 0,
            "staleTopologyRetries": create_window.stale_retries + create_session.stale_retries,
            "reconcileTimeoutMs": reconcile_timeout.as_millis() as u64,
            "windowSwitchMs": stats(window_switch_empty),
            "windowSwitchWith1MiBSnapshotMs": stats(window_switch_1mib),
            "resizeSettleMs": stats(resize),
            "floodSeconds": flood_seconds,
            "floodMegabytesPerSecond": flood.megabytes_per_second,
            "floodBytes": flood.bytes,
            "floodResyncEvents": flood.resyncs_during,
            "typingDuringFloodMs": stats(flood.typing_during_flood),
            "inputAckDuringFloodMs": stats(flood.input_ack_during_flood),
            "idleFramesIn8s": idle_frames,
            "totalResyncEvents": client.obs.resyncs(),
            "connectionWideResyncEvents": client.obs.connection_wide_resyncs,
            "resyncDetails": client.obs.resync_details.clone(),
            "sequenceGapObserved": client.obs.gap,
            "eventCounts": client.obs.event_counts.clone(),
        }))?
    );
    Ok(())
}

/// Reconnect budget: a transport stall of `stall_seconds` must not tear the
/// session down. The probe holds a live subscription across the stall and then
/// proves the same connection still answers, which is the difference between
/// "the link paused" and "the app reconnected and reseeded".
fn run_stall(
    transport: Transport,
    label: &str,
    stall_script: &str,
    restore_script: &str,
    stall_seconds: u64,
) -> Result<()> {
    let mut client = PerfClient::connect(&transport, Duration::from_secs(2))?;
    let initial = client.subscribe()?;
    let sessions_before = initial.sessions.len();

    let run_script = |script: &str| -> Result<()> {
        let status = Command::new("sh")
            .args(["-c", script])
            .status()
            .context("run stall script")?;
        ensure!(status.success(), "stall script failed: {script}");
        Ok(())
    };

    run_script(stall_script)?;
    let stall_started = Instant::now();
    let stall_end = stall_started + Duration::from_secs(stall_seconds);
    let mut survived = true;
    // Any frame is welcome here; what matters is that the bridge does not close.
    if let Err(error) = client.pump_until(stall_end, |_, _| false) {
        survived = false;
        eprintln!("bridge closed during stall: {error}");
    }
    run_script(restore_script)?;

    let mut answered = false;
    let mut sessions_after = 0;
    if survived {
        match client.snapshot() {
            Ok(snapshot) => {
                answered = true;
                sessions_after = snapshot.sessions.len();
            }
            Err(error) => eprintln!("post-stall request failed: {error}"),
        }
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "label": label,
            "stallSeconds": stall_seconds,
            "connectionSurvivedStall": survived,
            "answeredAfterStall": answered,
            "sessionsBefore": sessions_before,
            "sessionsAfter": sessions_after,
            "topologyPreserved": answered && sessions_after == sessions_before,
        }))?
    );
    Ok(())
}

/// Highest `TICK <n>` value present in a byte slice, or None. The pause-probe
/// fixture prints a monotonically increasing counter, so the largest value seen
/// is "how far this view of the pane has progressed".
fn last_tick(bytes: &[u8]) -> Option<u64> {
    last_counter(bytes, b"TICK ")
}

/// Highest `<prefix><n>` value in a byte slice. A counter a fixture prints is
/// the only honest "this specific output arrived" signal for a pane that also
/// produces output on its own.
fn last_counter(bytes: &[u8], prefix: &[u8]) -> Option<u64> {
    let mut best = None;
    let mut index = 0;
    while let Some(at) = bytes[index..]
        .windows(prefix.len())
        .position(|window| window == prefix)
    {
        let digits_start = index + at + prefix.len();
        let digits: Vec<u8> = bytes[digits_start..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .copied()
            .collect();
        if !digits.is_empty()
            && let Ok(value) = String::from_utf8(digits).unwrap().parse::<u64>()
        {
            best = Some(best.map_or(value, |current: u64| current.max(value)));
        }
        index = digits_start;
    }
    best
}

/// tmux's own view of the pane, for the ground-truth side of the comparison.
/// Local transport only: the probe runs where the tmux server runs.
fn capture_tick(tmux_socket: &str, pane_id: &str) -> Result<Option<u64>> {
    let output = Command::new("tmux")
        .args(["-L", tmux_socket, "capture-pane", "-p", "-t", pane_id])
        .output()
        .context("capture-pane ground truth")?;
    ensure!(output.status.success(), "capture-pane failed");
    Ok(last_tick(&output.stdout))
}

struct TickWatch {
    first: Option<u64>,
    last: Option<u64>,
    bytes_gained: u64,
}

/// Watches one pane for `window`, reporting the tick range observed in its
/// delivered tail and the bytes gained. This is the machine answer to "is the
/// pane advancing in the app while tmux advances".
fn watch_ticks(client: &mut PerfClient, pane_id: &str, window: Duration) -> Result<TickWatch> {
    let bytes_before = client.obs.output_bytes(pane_id);
    let first = client
        .obs
        .pane_tails
        .get(pane_id)
        .and_then(|tail| last_tick(tail));
    client.pump_until(Instant::now() + window, |_, _| false)?;
    let last = client
        .obs
        .pane_tails
        .get(pane_id)
        .and_then(|tail| last_tick(tail));
    Ok(TickWatch {
        first,
        last,
        bytes_gained: client.obs.output_bytes(pane_id) - bytes_before,
    })
}

/// Stage 12.9 investigation probe for P12-U001: does a pane keep delivering
/// output through the app after tmux's `pause-after` flow control engaged?
///
/// The fixture session has two panes: pane A prints `TICK <n>` once per second
/// forever; pane B is an interactive shell. The probe floods pane B, freezes
/// the host daemon (stall script — SIGSTOP locally) long enough for tmux to
/// pause the flooded panes, unfreezes it, and then requires BOTH panes to
/// advance again, comparing the delivered tail against capture-pane ground
/// truth. A second round repeats the cycle with pane A hidden, then reveals it
/// and requires the revealed content to be current.
fn run_pause_probe(
    transport: Transport,
    session_name: &str,
    label: &str,
    stall_script: &str,
    restore_script: &str,
    stall_seconds: u64,
) -> Result<()> {
    let tmux_socket = match &transport {
        Transport::Local { tmux_socket, .. } => tmux_socket.clone(),
        Transport::Ssh { .. } => bail!("pause-probe is a local-transport probe"),
    };
    let mut client = PerfClient::connect(&transport, Duration::from_secs(2))?;
    let initial = client.subscribe()?;
    let target = session(&initial, session_name)?.clone();
    client.attach(&initial, &target.id)?;
    let mut panes: Vec<_> = initial
        .panes
        .iter()
        .filter(|pane| pane.session_id == target.id)
        .map(|pane| pane.id.clone())
        .collect();
    panes.sort_by_key(|pane| pane[1..].parse::<u64>().unwrap_or(u64::MAX));
    ensure!(panes.len() >= 2, "fixture needs a ticker and a shell pane");
    let (ticker, shell) = (panes[0].clone(), panes[1].clone());
    // The attach itself seeds every visible pane; wait for both seeds so the
    // probe starts from a painted state, as the desktop would.
    let (ticker_seed, shell_seed) = (ticker.clone(), shell.clone());
    let seeded = client.pump_until(Instant::now() + Duration::from_secs(10), |obs, _| {
        obs.paintable_panes.contains(&ticker_seed) && obs.paintable_panes.contains(&shell_seed)
    })?;
    ensure!(seeded, "fixture panes were never seeded after attach");

    let run_script = |script: &str| -> Result<()> {
        let status = Command::new("sh")
            .args(["-c", script])
            .status()
            .with_context(|| format!("run script: {script}"))?;
        ensure!(status.success(), "script failed: {script}");
        Ok(())
    };
    let stall_and_restore = |client: &mut PerfClient| -> Result<()> {
        client.input(&shell, b"yes ade-pause-probe-flood-line\r")?;
        client.pump_until(Instant::now() + Duration::from_millis(1500), |_, _| false)?;
        run_script(stall_script)?;
        // The host is frozen; nothing arrives. Waiting with the reader live
        // mirrors the real app, which keeps reading the moment tmux unblocks.
        client.pump_until(
            Instant::now() + Duration::from_secs(stall_seconds),
            |_, _| false,
        )?;
        run_script(restore_script)?;
        Ok(())
    };

    // Round 1 — visible pane across a pause.
    let baseline = watch_ticks(&mut client, &ticker, Duration::from_secs(3))?;
    stall_and_restore(&mut client)?;
    let shell_before = client.obs.output_bytes(&shell);
    let flood_resumed = client.pump_until(Instant::now() + Duration::from_secs(15), |obs, _| {
        obs.output_bytes(&shell) > shell_before
    })?;
    let interrupt_result = client.input(&shell, b"\x03").err().map(|e| e.to_string());
    let visible = watch_ticks(&mut client, &ticker, Duration::from_secs(6))?;
    let visible_truth = capture_tick(&tmux_socket, &ticker)?;
    let paused_events_round1 = client.obs.event_count(v1::EventKind::TerminalFlowPaused);

    // Round 2 — the same cycle while the ticker is hidden, then reveal.
    let hide_id = client.set_visible(&ticker, false, Vec::new())?;
    client.pump_until(Instant::now() + Duration::from_secs(5), |_, responses| {
        responses.contains_key(&hide_id)
    })?;
    client.responses.remove(&hide_id);
    stall_and_restore(&mut client)?;
    let shell_before = client.obs.output_bytes(&shell);
    let flood_resumed_hidden = client
        .pump_until(Instant::now() + Duration::from_secs(15), |obs, _| {
            obs.output_bytes(&shell) > shell_before
        })?;
    let _ = client.input(&shell, b"\x03");
    client.pump_until(Instant::now() + Duration::from_millis(800), |_, _| false)?;
    let reveal_error = client
        .reveal_and_await_seed(&ticker, Instant::now() + Duration::from_secs(10))
        .err()
        .map(|error| error.to_string());
    let revealed = watch_ticks(&mut client, &ticker, Duration::from_secs(6))?;
    let revealed_truth = capture_tick(&tmux_socket, &ticker)?;

    let ticker_advanced =
        |watch: &TickWatch| matches!((watch.first, watch.last), (Some(a), Some(b)) if b >= a + 2);
    let close_to_truth = |watch: &TickWatch, truth: Option<u64>| matches!((watch.last, truth), (Some(seen), Some(actual)) if actual.saturating_sub(seen) <= 2);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "label": label,
            "stallSeconds": stall_seconds,
            "baselineTicking": ticker_advanced(&baseline),
            "pausedEventsAfterRound1": paused_events_round1,
            "round1": {
                "floodResumed": flood_resumed,
                "interruptError": interrupt_result,
                "tickerFirst": visible.first,
                "tickerLast": visible.last,
                "tickerBytesGained": visible.bytes_gained,
                "tickerAdvanced": ticker_advanced(&visible),
                "captureTick": visible_truth,
                "matchesGroundTruth": close_to_truth(&visible, visible_truth),
            },
            "round2Hidden": {
                "floodResumed": flood_resumed_hidden,
                "revealError": reveal_error,
                "tickerFirst": revealed.first,
                "tickerLast": revealed.last,
                "tickerAdvanced": ticker_advanced(&revealed),
                "captureTick": revealed_truth,
                "matchesGroundTruth": close_to_truth(&revealed, revealed_truth),
            },
            "flowPausedEvents": client.obs.event_count(v1::EventKind::TerminalFlowPaused),
            // Zero on a healthy tmux; the whole point of
            // ADE_TEST_REJECT_FLOW_RESUME is to make this non-zero on purpose
            // and still require both panes above to have recovered.
            "flowStalledEvents": client.obs.event_count(v1::EventKind::TerminalFlowStalled),
            "terminalSeeds": client.obs.event_count(v1::EventKind::TerminalSeed),
            "connectionWideResyncEvents": client.obs.connection_wide_resyncs,
            "resyncDetails": client.obs.resync_details.clone(),
            "sequenceGapObserved": client.obs.gap,
        }))?
    );
    Ok(())
}

/// Raw `ssh + tmux -C` comparator: the same physical link and the same tmux
/// server, with no part of this product in the path. `send-keys -H` is written
/// straight to a control client's stdin and the probe blocks until tmux reports
/// the echoed bytes. Whatever the app costs above this number is its overhead.
fn run_raw_tmux(
    transport: Transport,
    session_name: &str,
    pane_id: &str,
    label: &str,
    samples: usize,
) -> Result<()> {
    let mut command = match &transport {
        Transport::Local { tmux_socket, .. } => {
            let mut command = Command::new("tmux");
            command.args([
                "-L",
                tmux_socket,
                "-C",
                "attach-session",
                "-t",
                session_name,
            ]);
            command
        }
        Transport::Ssh { config, target } => {
            let mut command = Command::new("ssh");
            command.args([
                "-F",
                config,
                "-T",
                "-o",
                "BatchMode=yes",
                target,
                &format!("tmux -C attach-session -t {session_name}"),
            ]);
            command
        }
    };
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("start raw tmux control client")?;
    let mut stdin = child
        .stdin
        .take()
        .context("raw control stdin unavailable")?;
    let stdout = child
        .stdout
        .take()
        .context("raw control stdout unavailable")?;
    let (sender, lines) = mpsc::channel();
    let output_prefix = format!("%output {pane_id} ");
    thread::Builder::new()
        .name("phase12-raw-tmux-reader".into())
        .spawn(move || {
            use std::io::BufRead as _;
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        })?;

    // Drain the attach banner and any pending output before timing.
    let settle = Instant::now() + Duration::from_millis(800);
    while Instant::now() < settle {
        let _ = lines.recv_timeout(Duration::from_millis(100));
    }

    let mut echo = Vec::new();
    for index in 0..samples {
        while lines.try_recv().is_ok() {}
        let started = Instant::now();
        // 0x78 is 'x': one printable byte, exactly what a keypress sends.
        writeln!(stdin, "send-keys -H -t {pane_id} 78").context("write raw send-keys")?;
        stdin.flush()?;
        let deadline = started + Duration::from_secs(10);
        let mut observed = false;
        while Instant::now() < deadline {
            let remaining = deadline - Instant::now();
            match lines.recv_timeout(remaining) {
                Ok(line) if line.starts_with(&output_prefix) => {
                    observed = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        ensure!(observed, "raw tmux keystroke {index} was never echoed");
        if index >= 5 {
            echo.push(started.elapsed().as_secs_f64() * 1000.0);
        }
    }
    // 0x15 is Ctrl-U: clears whatever this probe typed.
    let _ = writeln!(stdin, "send-keys -H -t {pane_id} 15");
    let _ = stdin.flush();
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "label": label,
            "baseline": "raw-ssh-tmux",
            "transport": match &transport { Transport::Local { .. } => "local", Transport::Ssh { .. } => "ssh" },
            "keystrokeEchoMs": stats(echo),
        }))?
    );
    Ok(())
}

/// A deliberately small pass for a real, in-use host.
///
/// It attaches to one named session, measures keystroke echo and one create-tab
/// inside it, floods briefly, and closes what it made. It creates no sessions
/// and touches nothing outside the session it was pointed at, which is what
/// makes it safe to run against a machine somebody is working on.
fn run_smoke(
    transport: Transport,
    session_name: &str,
    label: &str,
    reconcile_timeout: Duration,
    flood_seconds: u64,
) -> Result<()> {
    let mut client = PerfClient::connect(&transport, reconcile_timeout)?;
    let initial = client.subscribe()?;
    let target = session(&initial, session_name)?.clone();
    client.attach(&initial, &target.id)?;
    let pane = initial
        .panes
        .iter()
        .find(|pane| pane.session_id == target.id)
        .context("scratch session has no pane")?
        .clone();

    let keystroke = probe_keystroke(&mut client, &pane.id, 30)?;
    let create = probe_create_window(&mut client, &target.id, 3)?;
    let flood = probe_flood(&mut client, &pane.id, &pane.id, flood_seconds)?;

    let after = client.snapshot()?;
    let leftover: Vec<_> = after
        .sessions
        .iter()
        .map(|session| session.name.clone())
        .collect();

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "label": label,
            "session": session_name,
            "keystrokeEchoMs": stats(keystroke.echo),
            "keystrokeAckMs": stats(keystroke.ack),
            "createWindowInteractiveMs": stats(create.interactive),
            "staleTopologyRetries": create.stale_retries,
            "floodSeconds": flood_seconds,
            "floodMegabytesPerSecond": flood.megabytes_per_second,
            "floodBytes": flood.bytes,
            "floodConnectionResyncs": flood.resyncs_during,
            "connectionWideResyncEvents": client.obs.connection_wide_resyncs,
            // A count alone cannot say whether a resync is the product's own
            // recovery or the link giving up; on a real host that difference is
            // the whole decision (plan 12.9 item 6).
            "resyncDetails": client.obs.resync_details.clone(),
            "eventCounts": client.obs.event_counts.clone(),
            "sequenceGapObserved": client.obs.gap,
            "sessionsAfter": leftover,
        }))?
    );
    Ok(())
}

/// Largest grid any window in a session occupies, from the pushed topology.
///
/// tmux does not report a window's size on the wire, but the panes tile it
/// exactly, so the extent of the panes *is* the window: this is the number that
/// read 108x298 on the user's server while every human client attached to it
/// was 188x51 (P12-U006).
fn window_grids(snapshot: &v1::Snapshot, session_id: &str) -> BTreeMap<String, (u32, u32)> {
    let mut grids: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for pane in snapshot
        .panes
        .iter()
        .filter(|pane| pane.session_id == session_id)
    {
        let entry = grids.entry(pane.window_id.clone()).or_insert((0, 0));
        entry.0 = entry.0.max(pane.left + pane.width);
        entry.1 = entry.1.max(pane.top + pane.height);
    }
    grids
}

/// Stage 12.10 item 4: does a client that asks for one fixed size ever leave a
/// window bigger than that size, and does an idle connection stay idle?
///
/// The desktop's fixed client size is the whole input — the corrected
/// computation depends only on the app's own surface, so under churn it asks
/// for the same numbers no matter what the panes do. This lane asks for those
/// numbers before and after every split, zoom, window switch and pane resize,
/// which is the worst case of what the desktop can send, and records the window
/// grids tmux ends up with. The bound is what a plain terminal attached to the
/// same session would tolerate: nothing may exceed the larger of the requested
/// size and that terminal's own.
struct ClientGeometryLane {
    /// The size the desktop's surface is worth, which is all it ever asks for.
    columns: u32,
    rows: u32,
    /// The plain terminal sharing the session; nothing may exceed it.
    bound_columns: u32,
    bound_rows: u32,
    churn_rounds: usize,
    idle_seconds: u64,
}

/// M13-E005: does a control client that has just become the visible one know
/// what size it is?
///
/// The host attaches one control client per session and takes only the visible
/// one out of `ignore-size`. Every lane before this one lived inside a single
/// session, so it could only ever observe the *first* client — the one the
/// desktop happens to size on connect. What the user hit is the second: a
/// workspace on another session, whose client started participating in sizing
/// having never been sent a `refresh-client -C`, so tmux sized the windows they
/// were looking at from its own 80x24 default.
///
/// Four client generations, each of which must end up at the asked-for size:
/// the session attached on connect, a session created and selected afterwards,
/// the first one again on the way back, and the second one once more — that
/// last one selected the way the desktop actually does it, through
/// `SelectTerminalSession` rather than a tmux action.
///
/// Both spellings are exercised because they are different claims. The tmux
/// action is a user switching workspace with a live server; the direct
/// selection is the desktop stating which workspace is on screen, which it also
/// has to do after a reconnect, when nothing about tmux changed at all and the
/// bridge has just re-attached to whichever session the snapshot listed first.
///
/// Deliberately host-level rather than through the packaged app: the desktop's
/// dedupe and its reassertion policy are covered by `useClientResize.test.tsx`,
/// and what this asserts is the half that has to hold whatever the desktop
/// sends — including sending no size at all, because its surface has not moved.
fn run_client_generations(
    transport: Transport,
    session_name: &str,
    label: &str,
    columns: u32,
    rows: u32,
) -> Result<()> {
    let tmux_socket = match &transport {
        Transport::Local { tmux_socket, .. } => tmux_socket.clone(),
        Transport::Ssh { .. } => bail!("client-generations is a local-transport probe"),
    };
    let mut client = PerfClient::connect(&transport, Duration::from_secs(2))?;
    let initial = client.subscribe()?;
    let primary = session(&initial, session_name)?.clone();
    client.attach(&initial, &primary.id)?;
    let response = client.request(
        v1::Operation::ResizeTerminal,
        v1::Request {
            columns,
            rows,
            ..Default::default()
        },
    )?;
    ensure!(
        response.ok,
        "the first client refused the size: {} {}",
        response.error_code,
        response.display_message
    );

    let mut created = action(v1::TmuxActionKind::CreateSession);
    created.name = format!("{session_name}-second");
    let (response, _) = client.action_reconciled(created)?;
    ensure!(
        response.ok,
        "creating the second session failed: {}",
        response.display_message
    );
    let second = response
        .tmux_action_result
        .context("create-session omitted its result")?
        .session_id;

    // The workspace switch itself. No resize is sent with it, because the
    // desktop has none to send: its surface did not move when the user clicked
    // another workspace, which is precisely why the host has to carry the size
    // across for it.
    let mut select = action(v1::TmuxActionKind::SelectSession);
    select.session_id = second.clone();
    let (response, _) = client.action_reconciled(select)?;
    ensure!(
        response.ok,
        "selecting the second session failed: {}",
        response.display_message
    );

    let mut samples: Vec<serde_json::Value> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    let mut observe = |client: &mut PerfClient, step: &str, session_id: &str| -> Result<()> {
        // tmux applies a client size asynchronously; the snapshot is re-read
        // until it settles rather than after a fixed sleep.
        let deadline = Instant::now() + Duration::from_secs(5);
        let grids;
        loop {
            let observed = window_grids(&client.snapshot()?, session_id);
            let settled = !observed.is_empty()
                && observed
                    .values()
                    .all(|(width, height)| *width == columns && *height == rows);
            if settled || Instant::now() >= deadline {
                grids = observed;
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        for (window, (width, height)) in &grids {
            if *width != columns || *height != rows {
                failures.push(format!(
                    "{step}: window {window} is {width}x{height}, not the {columns}x{rows} this client asked for"
                ));
            }
        }
        if grids.is_empty() {
            failures.push(format!("{step}: the session reported no windows at all"));
        }
        samples.push(json!({
            "step": step,
            "sessionId": session_id,
            "windows": grids
                .iter()
                .map(|(window, (width, height))| (window.clone(), format!("{width}x{height}")))
                .collect::<BTreeMap<_, _>>(),
        }));
        Ok(())
    };

    observe(&mut client, "selected-new-session", &second)?;
    let mut back = action(v1::TmuxActionKind::SelectSession);
    back.session_id = primary.id.clone();
    let (response, _) = client.action_reconciled(back)?;
    ensure!(
        response.ok,
        "selecting the first session again failed: {}",
        response.display_message
    );
    observe(&mut client, "back-to-first-session", &primary.id)?;

    // And the same switch as the desktop actually performs it. The tmux action
    // above is one of several paths that land the app on another workspace, and
    // it is the only one the host used to hear about: a reconnect re-attaches
    // to whichever session the fresh snapshot lists first, and a snapshot that
    // re-resolves the selection changes it with no action at all. So the
    // desktop states the fact directly, and this is that message — no tmux
    // action, no reconciliation, nothing but "this is the workspace on screen".
    let response = client.request(
        v1::Operation::SelectTerminalSession,
        v1::Request {
            session_id: second.clone(),
            ..Default::default()
        },
    )?;
    ensure!(
        response.ok,
        "selecting the visible session directly failed: {} {}",
        response.error_code,
        response.display_message
    );
    observe(&mut client, "selected-visible-session-directly", &second)?;

    // Defect B, which no step above can see: something *else* takes the size.
    //
    // A second client on the same session, at its own size, is the stand-in for
    // the user's plain terminal — and under tmux's default `window-size latest`
    // its size is the one tmux takes. The app's surface has not moved, so the
    // only thing it can send afterwards is the size it already sent.
    //
    // A floor rather than a discriminator, and the difference was measured
    // rather than assumed: on a local tmux with an idle stand-in, the app's
    // control client goes on to issue commands of its own and becomes the
    // latest client again, so the windows come back whether or not the resize
    // reached tmux. What this step does hold is the end state — an external
    // client took the windows and the app's size is what they finished at — and
    // the merge-QA matrix against a real remote host with an *active* second
    // terminal is where the re-assertion itself is observed.
    let rival_size = (columns / 2 + 4, rows / 2 + 3);
    let mut rival = Command::new("tmux")
        .args(["-L", &tmux_socket, "-C", "attach-session", "-t", &second])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("attach the stand-in plain terminal")?;
    let stole_it = (|| -> Result<bool> {
        let mut stdin = rival.stdin.take().context("stand-in client stdin")?;
        writeln!(stdin, "refresh-client -C {},{}", rival_size.0, rival_size.1)?;
        stdin.flush()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if window_grids(&client.snapshot()?, &second)
                .values()
                .all(|grid| *grid == rival_size)
            {
                return Ok(true);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Ok(false)
    })();
    let stole_it = stole_it.inspect_err(|_| {
        let _ = rival.kill();
    })?;
    // If the stand-in never won the size there is nothing to win back, and
    // passing the step below would mean nothing at all.
    ensure!(
        stole_it,
        "the stand-in terminal never took the windows to {}x{}; the recovery below would prove nothing",
        rival_size.0,
        rival_size.1
    );
    let response = client.request(
        v1::Operation::ResizeTerminal,
        v1::Request {
            columns,
            rows,
            ..Default::default()
        },
    );
    let recovery = response.and_then(|response| {
        ensure!(
            response.ok,
            "re-asserting the app's size failed: {} {}",
            response.error_code,
            response.display_message
        );
        observe(&mut client, "re-asserted-after-external-resize", &second)
    });
    let _ = rival.kill();
    let _ = rival.wait();
    recovery?;

    println!(
        "{}",
        json!({
            "label": label,
            "asked": format!("{columns}x{rows}"),
            "externalSize": format!("{}x{}", rival_size.0, rival_size.1),
            "samples": samples,
            "failures": failures,
        })
    );
    ensure!(
        failures.is_empty(),
        "a newly visible control client was never given the app's size: {}",
        failures.join("; ")
    );
    Ok(())
}

fn run_client_geometry(
    transport: Transport,
    session_name: &str,
    label: &str,
    lane: ClientGeometryLane,
) -> Result<()> {
    let ClientGeometryLane {
        columns,
        rows,
        bound_columns,
        bound_rows,
        churn_rounds,
        idle_seconds,
    } = lane;
    let mut client = PerfClient::connect(&transport, Duration::from_secs(2))?;
    let initial = client.subscribe()?;
    let target = session(&initial, session_name)?.clone();
    client.attach(&initial, &target.id)?;
    let first_pane = initial
        .panes
        .iter()
        .find(|pane| pane.session_id == target.id)
        .context("scratch session has no pane")?
        .clone();

    let mut samples: Vec<serde_json::Value> = Vec::new();
    let mut violations: Vec<serde_json::Value> = Vec::new();
    let mut resize_rejections: Vec<String> = Vec::new();

    // `live` asks the host for an authoritative snapshot; the idle window reads
    // the pushed topology instead, so that sampling an idle connection cannot
    // itself be the traffic the idle window is counting.
    let record = |client: &mut PerfClient,
                  step: &str,
                  live: bool,
                  samples: &mut Vec<serde_json::Value>,
                  violations: &mut Vec<serde_json::Value>|
     -> Result<()> {
        let snapshot = if live {
            client.snapshot()?
        } else {
            client.cached_snapshot()?
        };
        let grids = window_grids(&snapshot, &target.id);
        for (window, (width, height)) in &grids {
            if *width > bound_columns || *height > bound_rows {
                violations.push(json!({
                    "step": step,
                    "window": window,
                    "size": format!("{width}x{height}"),
                    "bound": format!("{bound_columns}x{bound_rows}"),
                }));
            }
        }
        samples.push(json!({
            "step": step,
            "windows": grids
                .iter()
                .map(|(window, (width, height))| (window.clone(), format!("{width}x{height}")))
                .collect::<BTreeMap<_, _>>(),
        }));
        Ok(())
    };

    // What the desktop sends: the same size on connect and on every trigger.
    let ask = |client: &mut PerfClient, rejections: &mut Vec<String>| -> Result<()> {
        let response = client.request(
            v1::Operation::ResizeTerminal,
            v1::Request {
                columns,
                rows,
                ..Default::default()
            },
        )?;
        if !response.ok {
            rejections.push(format!(
                "{} {}",
                response.error_code, response.display_message
            ));
        }
        Ok(())
    };

    ask(&mut client, &mut resize_rejections)?;
    record(&mut client, "attached", true, &mut samples, &mut violations)?;

    let mut extra_window: Option<String> = None;
    for round in 0..churn_rounds {
        // Split, zoom, unzoom, switch windows, resize a pane — the churn the
        // field report was made under — asking for the client size again after
        // each one, as the desktop does on an active-window change.
        let horizontal = round % 2 == 1;
        let mut split = action(if horizontal {
            v1::TmuxActionKind::SplitPaneRight
        } else {
            v1::TmuxActionKind::SplitPaneDown
        });
        split.pane_id = first_pane.id.clone();
        let (response, _) = client.action_reconciled(split)?;
        ensure!(response.ok, "split failed: {}", response.display_message);
        let split_pane = response
            .tmux_action_result
            .context("split omitted result")?
            .pane_id;
        ask(&mut client, &mut resize_rejections)?;
        record(
            &mut client,
            &format!("round{round}.split"),
            true,
            &mut samples,
            &mut violations,
        )?;

        for zoomed in [true, false] {
            let mut zoom = action(v1::TmuxActionKind::ZoomPane);
            zoom.pane_id = split_pane.clone();
            zoom.zoomed = zoomed;
            let (response, _) = client.action_reconciled(zoom)?;
            ensure!(response.ok, "zoom failed: {}", response.display_message);
            ask(&mut client, &mut resize_rejections)?;
            record(
                &mut client,
                &format!("round{round}.zoom{zoomed}"),
                true,
                &mut samples,
                &mut violations,
            )?;
        }

        // Along the split's own axis: a vertical resize of a side-by-side pane
        // moves nothing, and the host rejects an action whose postcondition did
        // not happen.
        let mut resize = action(if horizontal {
            v1::TmuxActionKind::ResizePaneLeft
        } else {
            v1::TmuxActionKind::ResizePaneUp
        });
        resize.pane_id = split_pane.clone();
        resize.resize_cells = 3;
        let (response, _) = client.action_reconciled(resize)?;
        ensure!(
            response.ok,
            "pane resize failed: {}",
            response.display_message
        );
        ask(&mut client, &mut resize_rejections)?;
        record(
            &mut client,
            &format!("round{round}.paneResize"),
            true,
            &mut samples,
            &mut violations,
        )?;

        if extra_window.is_none() {
            let mut create = action(v1::TmuxActionKind::CreateWindow);
            create.session_id = target.id.clone();
            let (response, _) = client.action_reconciled(create)?;
            ensure!(
                response.ok,
                "create window failed: {}",
                response.display_message
            );
            extra_window = Some(
                response
                    .tmux_action_result
                    .context("create window omitted result")?
                    .window_id,
            );
        }
        for window in [extra_window.clone().unwrap(), first_pane.window_id.clone()] {
            let mut select = action(v1::TmuxActionKind::SelectWindow);
            select.session_id = target.id.clone();
            select.window_id = window.clone();
            let (response, _) = client.action_reconciled(select)?;
            ensure!(
                response.ok,
                "select window failed: {}",
                response.display_message
            );
            ask(&mut client, &mut resize_rejections)?;
            record(
                &mut client,
                &format!("round{round}.select"),
                true,
                &mut samples,
                &mut violations,
            )?;
        }

        let mut close = action(v1::TmuxActionKind::ClosePane);
        close.pane_id = split_pane.clone();
        close.confirmed = true;
        let (response, _) = client.action_reconciled(close)?;
        ensure!(
            response.ok,
            "close pane failed: {}",
            response.display_message
        );
        ask(&mut client, &mut resize_rejections)?;
        record(
            &mut client,
            &format!("round{round}.closePane"),
            true,
            &mut samples,
            &mut violations,
        )?;
    }

    // Idle: nothing is sent, nothing is touched. A topology push here is the
    // "topology changed" churn the user sees, and a window that moves is a
    // window something is still resizing.
    // Let the last action's own pushes land before the idle window opens, or
    // they are counted as churn nobody asked for.
    client.pump_until(Instant::now() + Duration::from_secs(3), |_, _| false)?;

    // Does re-sending a size tmux already has cost anything? The desktop
    // deduplicates identical requests and this is the claim behind that: it is
    // measured here rather than asserted in a comment.
    let resend_topology_before = client.obs.event_count(v1::EventKind::TopologyDirty);
    let resend_snapshots_before = client.obs.event_count(v1::EventKind::TopologySnapshot);
    for _ in 0..3 {
        ask(&mut client, &mut resize_rejections)?;
        client.pump_until(Instant::now() + Duration::from_secs(1), |_, _| false)?;
    }
    let identical_resend = json!({
        "requests": 3,
        "topologyDirty": client.obs.event_count(v1::EventKind::TopologyDirty) - resend_topology_before,
        "topologySnapshots": client.obs.event_count(v1::EventKind::TopologySnapshot) - resend_snapshots_before,
    });

    // Fault injection for the bound (§12.10 item 2): ask for a size no display
    // has, and require the host to refuse it by name rather than pass it to
    // `refresh-client -C`. Nothing else in the suite exercises the host half of
    // that bound against a live daemon.
    let over_bound_rows = 501;
    let response = client.request(
        v1::Operation::ResizeTerminal,
        v1::Request {
            columns,
            rows: over_bound_rows,
            ..Default::default()
        },
    )?;
    let bound_probe = json!({
        "requested": format!("{columns}x{over_bound_rows}"),
        "accepted": response.ok,
        "errorCode": response.error_code,
        "namesTheSize": response.display_message.contains(&format!("{columns}x{over_bound_rows}")),
        "message": response.display_message,
    });
    // Whatever the host did with it, the session must be left at the size the
    // rest of the lane measured.
    ask(&mut client, &mut resize_rejections)?;
    record(
        &mut client,
        "afterBoundProbe",
        true,
        &mut samples,
        &mut violations,
    )?;
    client.pump_until(Instant::now() + Duration::from_secs(2), |_, _| false)?;
    let topology_before = client.obs.event_count(v1::EventKind::TopologyDirty);
    let snapshots_before = client.obs.event_count(v1::EventKind::TopologySnapshot);
    let seeds_before = client.obs.event_count(v1::EventKind::TerminalSeed);
    let resyncs_before = client.obs.connection_wide_resyncs;
    let idle_started = Instant::now();
    let idle_deadline = idle_started + Duration::from_secs(idle_seconds);
    let mut idle_samples: Vec<serde_json::Value> = Vec::new();
    while Instant::now() < idle_deadline {
        let next = (Instant::now() + Duration::from_secs(30)).min(idle_deadline);
        client.pump_until(next, |_, _| false)?;
        record(
            &mut client,
            &format!("idle{}s", idle_started.elapsed().as_secs()),
            false,
            &mut idle_samples,
            &mut violations,
        )?;
    }

    let final_grids = window_grids(&client.snapshot()?, &target.id);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "label": label,
            "session": session_name,
            "requested": format!("{columns}x{rows}"),
            "bound": format!("{bound_columns}x{bound_rows}"),
            "churnRounds": churn_rounds,
            "churnSamples": samples,
            "idleSeconds": idle_seconds,
            "idleSamples": idle_samples,
            "windowsExceedingBound": violations,
            "resizeRejections": resize_rejections,
            "identicalResend": identical_resend,
            "boundProbe": bound_probe,
            "idleTopologyDirty": client.obs.event_count(v1::EventKind::TopologyDirty) - topology_before,
            "idleTopologySnapshots": client.obs.event_count(v1::EventKind::TopologySnapshot) - snapshots_before,
            "idleTerminalSeeds": client.obs.event_count(v1::EventKind::TerminalSeed) - seeds_before,
            "idleConnectionWideResyncs": client.obs.connection_wide_resyncs - resyncs_before,
            "finalWindows": final_grids
                .iter()
                .map(|(window, (width, height))| (window.clone(), format!("{width}x{height}")))
                .collect::<BTreeMap<_, _>>(),
            "sequenceGapObserved": client.obs.gap,
        }))?
    );
    Ok(())
}

/// Does an attached session generate work when nothing is happening?
///
/// A pane that is reseeded repeatedly repaints its whole screen repeatedly,
/// which is what "heavy continuous flickering" looks like from the outside, and
/// repeated topology pushes are what "topology changed" notices are. Neither is
/// visible in a latency number, so this lane counts events instead: first over a
/// window where the fixture is held completely still, then over a window where
/// it repaints once a second like an agent TUI.
///
/// A seed after the first is never routine. It means something asked for the
/// screen again.
fn run_idle_steady(
    transport: Transport,
    session_name: &str,
    label: &str,
    hold_file: &str,
    window_seconds: u64,
) -> Result<()> {
    let mut client = PerfClient::connect(&transport, Duration::from_secs(2))?;
    let initial = client.subscribe()?;
    let target = session(&initial, session_name)?.clone();
    client.attach(&initial, &target.id)?;
    let mut panes: Vec<_> = initial
        .panes
        .iter()
        .filter(|pane| pane.session_id == target.id)
        .map(|pane| pane.id.clone())
        .collect();
    panes.sort_by_key(|pane| pane[1..].parse::<u64>().unwrap_or(u64::MAX));
    ensure!(panes.len() >= 2, "fixture needs a shell and an agent pane");
    let (shell, agent) = (panes[0].clone(), panes[1].clone());
    let (shell_seed, agent_seed) = (shell.clone(), agent.clone());
    let seeded = client.pump_until(Instant::now() + Duration::from_secs(10), |obs, _| {
        obs.paintable_panes.contains(&shell_seed) && obs.paintable_panes.contains(&agent_seed)
    })?;
    ensure!(seeded, "fixture panes were never seeded after attach");

    let window = |client: &mut PerfClient| -> Result<serde_json::Value> {
        let seeds = client.obs.event_count(v1::EventKind::TerminalSeed);
        let topology = client.obs.event_count(v1::EventKind::TopologyDirty);
        let snapshots = client.obs.event_count(v1::EventKind::TopologySnapshot);
        let resnapshots = client
            .obs
            .event_count(v1::EventKind::TerminalResnapshotRequired);
        let resyncs = client.obs.connection_wide_resyncs;
        let bytes = client.obs.output_bytes(&agent);
        let started = Instant::now();
        client.pump_until(started + Duration::from_secs(window_seconds), |_, _| false)?;
        Ok(json!({
            "seconds": window_seconds,
            "terminalSeeds": client.obs.event_count(v1::EventKind::TerminalSeed) - seeds,
            "topologyDirty": client.obs.event_count(v1::EventKind::TopologyDirty) - topology,
            "topologySnapshots": client.obs.event_count(v1::EventKind::TopologySnapshot) - snapshots,
            "resnapshotsRequired": client.obs.event_count(v1::EventKind::TerminalResnapshotRequired) - resnapshots,
            "connectionWideResyncs": client.obs.connection_wide_resyncs - resyncs,
            "agentBytesDelivered": client.obs.output_bytes(&agent) - bytes,
        }))
    };

    let mut fixture = FixtureHold::new(PathBuf::from(hold_file));
    fixture.hold()?;
    quiesce(
        &mut client,
        &agent,
        Duration::from_millis(700),
        Duration::from_secs(10),
    )?;
    let idle = window(&mut client)?;
    fixture.release()?;
    let repainting = window(&mut client)?;

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "label": label,
            "idle": idle,
            "repainting": repainting,
            "resyncDetails": client.obs.resync_details.clone(),
            "sequenceGapObserved": client.obs.gap,
        }))?
    );
    Ok(())
}

/// Stage 12.9 item 3 lane for P12-U002: does an agent-shaped pane cost more on
/// the wire than a plain shell pane?
///
/// The fixture's second pane answers every keystroke with a 4 KiB
/// cursor-addressed frame wrapped in `ESC[?2026h/l`, and repaints the same frame
/// at 1 Hz — the output shape the user's claude/codex panes produce, and the
/// shape a plain `echo` lane never exercises. Both panes are measured with the
/// same keystroke probe, so the ratio is the answer to "is the lag on the wire
/// or in the renderer". The probe measures the host and the link only; it
/// contains no renderer.
fn run_agent_echo(
    transport: Transport,
    session_name: &str,
    label: &str,
    samples: usize,
) -> Result<()> {
    let mut client = PerfClient::connect(&transport, Duration::from_secs(2))?;
    let initial = client.subscribe()?;
    let target = session(&initial, session_name)?.clone();
    client.attach(&initial, &target.id)?;
    let mut panes: Vec<_> = initial
        .panes
        .iter()
        .filter(|pane| pane.session_id == target.id)
        .map(|pane| pane.id.clone())
        .collect();
    panes.sort_by_key(|pane| pane[1..].parse::<u64>().unwrap_or(u64::MAX));
    ensure!(panes.len() >= 2, "fixture needs a plain and an agent pane");
    let (plain, agent) = (panes[0].clone(), panes[1].clone());
    let (plain_seed, agent_seed) = (plain.clone(), agent.clone());
    let seeded = client.pump_until(Instant::now() + Duration::from_secs(10), |obs, _| {
        obs.paintable_panes.contains(&plain_seed) && obs.paintable_panes.contains(&agent_seed)
    })?;
    ensure!(seeded, "fixture panes were never seeded after attach");

    let plain_samples = probe_keystroke(&mut client, &plain, samples)?;
    let agent_bytes_before = client.obs.output_bytes(&agent);
    let agent_samples = probe_keystroke_marked(&mut client, &agent, samples, Some("KEY "))?;
    let agent_bytes = client.obs.output_bytes(&agent) - agent_bytes_before;

    let plain_stats = stats(plain_samples.echo);
    let agent_stats = stats(agent_samples.echo);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "label": label,
            "samples": samples,
            "plainEchoMs": plain_stats,
            "agentEchoMs": agent_stats,
            "agentBytesDelivered": agent_bytes,
            "connectionWideResyncEvents": client.obs.connection_wide_resyncs,
            "sequenceGapObserved": client.obs.gap,
        }))?
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// vt parity (stage 12.9 item 4, P12-U003)
// ---------------------------------------------------------------------------

/// One pane compared at one quiesce point.
///
/// Both sides are screens, not byte streams: the delivered side is the pane's
/// last seed plus every byte the host has sent since, replayed through a vt
/// parser sized to the pane's tmux grid; the ground-truth side is tmux's own
/// `capture-pane -p -e` repainted through a second parser of the same size.
/// Text parity is the gate. Attribute parity is counted separately so a text
/// pass that carries attribute drift is visible instead of hidden.
struct PaneParity {
    pane: String,
    rows: u16,
    columns: u16,
    topology_rows: u16,
    topology_columns: u16,
    delivered_bytes: usize,
    quiesced: bool,
    delivered_alternate_screen: bool,
    text_mismatch_rows: usize,
    attribute_mismatch_cells: usize,
    styled_cells_delivered: usize,
    styled_cells_tmux: usize,
    text_diffs: Vec<serde_json::Value>,
    attribute_diffs: Vec<String>,
}

impl PaneParity {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "pane": self.pane,
            "rows": self.rows,
            "columns": self.columns,
            "topologyRows": self.topology_rows,
            "topologyColumns": self.topology_columns,
            "geometryMatchesTopology": self.rows == self.topology_rows
                && self.columns == self.topology_columns,
            "deliveredBytes": self.delivered_bytes,
            "quiesced": self.quiesced,
            "deliveredAlternateScreen": self.delivered_alternate_screen,
            "textMismatchRows": self.text_mismatch_rows,
            "attributeMismatchCells": self.attribute_mismatch_cells,
            "styledCellsDelivered": self.styled_cells_delivered,
            "styledCellsTmux": self.styled_cells_tmux,
            "textDiffs": self.text_diffs,
            "attributeDiffs": self.attribute_diffs,
        })
    }
}

fn render_screen(bytes: &[u8], rows: u16, columns: u16) -> vt100::Parser {
    let mut parser = vt100::Parser::new(rows, columns, 0);
    parser.process(bytes);
    parser
}

/// tmux's own screen, repainted so it can be rendered by the same emulator as
/// the delivered bytes. `capture-pane -e` emits one SGR-annotated line per row
/// with trailing blanks trimmed; each row is cursor-addressed and
/// attribute-reset here, and autowrap is off, so a full-width row cannot spill
/// into the next one and no row can inherit the previous row's attributes.
fn repaint_capture(capture: &[u8], rows: u16) -> Vec<u8> {
    let mut painted = b"\x1b[2J\x1b[H\x1b[?7l".to_vec();
    for (index, line) in capture.split(|byte| *byte == b'\n').enumerate() {
        if index >= rows as usize {
            break;
        }
        painted.extend_from_slice(format!("\x1b[m\x1b[{};1H", index + 1).as_bytes());
        painted.extend_from_slice(line);
    }
    painted
}

fn row_text(screen: &vt100::Screen, row: u16, columns: u16) -> String {
    let mut text = String::new();
    for column in 0..columns {
        match screen.cell(row, column) {
            // A wide glyph's continuation cell has no contents of its own; a
            // space here would shift the rest of the row.
            Some(cell) if cell.is_wide_continuation() => {}
            Some(cell) if cell.has_contents() => text.push_str(cell.contents()),
            _ => text.push(' '),
        }
    }
    text.trim_end().to_string()
}

type CellAttributes = (bool, bool, vt100::Color, vt100::Color);

fn cell_attributes(screen: &vt100::Screen, row: u16, column: u16) -> CellAttributes {
    screen.cell(row, column).map_or(
        (false, false, vt100::Color::Default, vt100::Color::Default),
        |cell| (cell.bold(), cell.dim(), cell.fgcolor(), cell.bgcolor()),
    )
}

fn is_styled(attributes: &CellAttributes) -> bool {
    attributes.0
        || attributes.1
        || attributes.2 != vt100::Color::Default
        || attributes.3 != vt100::Color::Default
}

fn truncate(text: &str) -> String {
    let limit = 140;
    if text.chars().count() <= limit {
        return text.into();
    }
    text.chars().take(limit).collect::<String>() + "…"
}

fn run_script(script: &str) -> Result<()> {
    let status = Command::new("sh")
        .args(["-c", script])
        .status()
        .with_context(|| format!("run script: {script}"))?;
    ensure!(status.success(), "script failed: {script}");
    Ok(())
}

/// Waits until no new bytes have arrived for the pane for `quiet`. The agent
/// fixture repaints at 1 Hz, so a comparison taken mid-repaint would diff two
/// different moments; the bounded overall timeout keeps a pane that never goes
/// quiet from hanging the lane, and the answer is reported rather than assumed.
fn quiesce(
    client: &mut PerfClient,
    pane_id: &str,
    quiet: Duration,
    timeout: Duration,
) -> Result<bool> {
    let overall = Instant::now() + timeout;
    loop {
        let before = client.obs.output_bytes(pane_id);
        client.pump_until(Instant::now() + quiet, |obs, _| {
            obs.output_bytes(pane_id) > before
        })?;
        if client.obs.output_bytes(pane_id) == before {
            return Ok(true);
        }
        if Instant::now() >= overall {
            return Ok(false);
        }
    }
}

/// tmux's authoritative grid for the pane. The topology snapshot carries the
/// same numbers, and the lane reports both: a divergence between them is
/// itself a U003 mechanism.
fn tmux_pane_size(tmux_socket: &str, pane_id: &str) -> Result<(u16, u16)> {
    let output = Command::new("tmux")
        .args([
            "-L",
            tmux_socket,
            "display-message",
            "-p",
            "-t",
            pane_id,
            "#{pane_width} #{pane_height}",
        ])
        .output()
        .context("read tmux pane size")?;
    ensure!(output.status.success(), "display-message failed");
    let text = String::from_utf8(output.stdout).context("pane size was not utf-8")?;
    let mut parts = text.split_whitespace();
    let columns: u16 = parts
        .next()
        .context("pane width missing")?
        .parse()
        .context("pane width")?;
    let rows: u16 = parts
        .next()
        .context("pane height missing")?
        .parse()
        .context("pane height")?;
    Ok((rows, columns))
}

/// Stops and restarts the agent fixture's 1 Hz repaint, through the hold file
/// the fixture polls.
///
/// A fixture that repaints unconditionally through a recovery repairs a wrong
/// delivery within a second, which would let this lane pass on a screen the
/// user never saw. Real agent TUIs repaint when something changes and are idle
/// the rest of the time — which is exactly when a user is looking at one — so
/// the screen the host delivers has to be right on its own, not right by the
/// next frame. (A signal would be the obvious lever, but SIGSTOP is silently
/// dropped for tmux-spawned processes under a sandboxed run; the fixture
/// cooperating through a file works everywhere.)
struct FixtureHold {
    hold_file: PathBuf,
    held: bool,
}

impl FixtureHold {
    fn new(hold_file: PathBuf) -> Self {
        Self {
            hold_file,
            held: false,
        }
    }

    /// Stops the repaint and waits out the frame already in flight — the
    /// fixture only notices the hold at the top of its next second. Every hold
    /// in this lane happens while the pane's output is not reaching the wire
    /// (before the attach, while the pane is hidden, while the daemon is
    /// frozen), so wall-clock time is the only way to know the frame landed.
    fn hold(&mut self) -> Result<()> {
        if !self.held {
            fs::write(&self.hold_file, b"held").context("engage fixture hold")?;
            self.held = true;
        }
        thread::sleep(Duration::from_millis(1600));
        Ok(())
    }

    fn release(&mut self) -> Result<()> {
        if self.held {
            fs::remove_file(&self.hold_file).context("release fixture hold")?;
            self.held = false;
        }
        Ok(())
    }
}

impl Drop for FixtureHold {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

fn capture_screen(tmux_socket: &str, pane_id: &str) -> Result<Vec<u8>> {
    let output = Command::new("tmux")
        .args(["-L", tmux_socket, "capture-pane", "-p", "-e", "-t", pane_id])
        .output()
        .context("capture-pane ground truth")?;
    ensure!(output.status.success(), "capture-pane failed");
    Ok(output.stdout)
}

/// The driver's stand-in for the desktop's serialize addon: this client's
/// current screen for the pane, in escape codes. The host stores whatever the
/// client hands over at hide time and hands it back at reveal, so a hide that
/// ships nothing exercises a path the product never takes.
fn serialized_screen(client: &PerfClient, pane_id: &str, rows: u16, columns: u16) -> Vec<u8> {
    let delivered = client
        .obs
        .pane_full_logs
        .get(pane_id)
        .cloned()
        .unwrap_or_default();
    render_screen(&delivered, rows, columns)
        .screen()
        .state_formatted()
}

struct ParityLane {
    tmux_socket: String,
    artifacts: PathBuf,
    quiet: Duration,
    panes: Vec<String>,
    points: BTreeMap<String, serde_json::Value>,
}

impl ParityLane {
    /// Compares every watched pane against tmux and files the result under
    /// `point`.
    fn record(&mut self, client: &mut PerfClient, point: &str) -> Result<()> {
        let panes = self.panes.clone();
        let mut results = Vec::new();
        for pane in &panes {
            let quiesced = quiesce(client, pane, self.quiet, Duration::from_secs(20))?;
            results.push(self.compare_pane(client, point, pane, quiesced)?);
        }
        self.points.insert(
            point.into(),
            json!({
                "textMismatchRows": results.iter().map(|r| r.text_mismatch_rows).sum::<usize>(),
                "attributeMismatchCells": results
                    .iter()
                    .map(|r| r.attribute_mismatch_cells)
                    .sum::<usize>(),
                "allPanesQuiesced": results.iter().all(|r| r.quiesced),
                "panes": results.iter().map(PaneParity::to_json).collect::<Vec<_>>(),
            }),
        );
        Ok(())
    }

    fn compare_pane(
        &self,
        client: &mut PerfClient,
        point: &str,
        pane_id: &str,
        quiesced: bool,
    ) -> Result<PaneParity> {
        let (rows, columns) = tmux_pane_size(&self.tmux_socket, pane_id)?;
        let (topology_rows, topology_columns) = client
            .cached_snapshot()
            .ok()
            .and_then(|snapshot| {
                snapshot
                    .panes
                    .iter()
                    .find(|pane| pane.id == pane_id)
                    .map(|pane| (pane.height as u16, pane.width as u16))
            })
            .unwrap_or((0, 0));

        // The two sides must describe the same moment. Take the delivered log,
        // ask tmux, then prove nothing arrived in between; if something did,
        // wait for quiet and take both again.
        let mut attempt = 0;
        let (delivered, capture) = loop {
            let before = client.obs.output_bytes(pane_id);
            let delivered = client
                .obs
                .pane_full_logs
                .get(pane_id)
                .cloned()
                .unwrap_or_default();
            let capture = capture_screen(&self.tmux_socket, pane_id)?;
            // Bytes tmux had already emitted are still in flight; give them a
            // moment to land before believing the pane held still.
            client.pump_until(Instant::now() + Duration::from_millis(150), |_, _| false)?;
            if client.obs.output_bytes(pane_id) == before {
                break (delivered, capture);
            }
            attempt += 1;
            ensure!(
                attempt < 6,
                "pane {pane_id} never held still long enough to compare at {point}"
            );
            quiesce(client, pane_id, self.quiet, Duration::from_secs(20))?;
        };

        let delivered_parser = render_screen(&delivered, rows, columns);
        let delivered_screen = delivered_parser.screen();
        let truth_parser = render_screen(&repaint_capture(&capture, rows), rows, columns);
        let truth_screen = truth_parser.screen();

        let mut text_mismatch_rows = 0;
        let mut attribute_mismatch_cells = 0;
        let mut styled_cells_delivered = 0;
        let mut styled_cells_tmux = 0;
        let mut text_diffs = Vec::new();
        let mut attribute_diffs = Vec::new();
        let mut delivered_rows = Vec::new();
        let mut truth_rows = Vec::new();
        for row in 0..rows {
            let delivered_text = row_text(delivered_screen, row, columns);
            let truth_text = row_text(truth_screen, row, columns);
            if delivered_text != truth_text {
                text_mismatch_rows += 1;
                if text_diffs.len() < 6 {
                    text_diffs.push(json!({
                        "row": row,
                        "delivered": truncate(&delivered_text),
                        "tmux": truncate(&truth_text),
                    }));
                }
            }
            for column in 0..columns {
                let delivered_attributes = cell_attributes(delivered_screen, row, column);
                let truth_attributes = cell_attributes(truth_screen, row, column);
                if is_styled(&delivered_attributes) {
                    styled_cells_delivered += 1;
                }
                if is_styled(&truth_attributes) {
                    styled_cells_tmux += 1;
                }
                if delivered_attributes != truth_attributes {
                    attribute_mismatch_cells += 1;
                    if attribute_diffs.len() < 6 {
                        attribute_diffs.push(format!(
                            "row {row} col {column}: delivered {delivered_attributes:?} tmux {truth_attributes:?}"
                        ));
                    }
                }
            }
            delivered_rows.push(delivered_text);
            truth_rows.push(truth_text);
        }

        let stem = format!("{point}-{}", pane_id.trim_start_matches('%'));
        fs::write(
            self.artifacts.join(format!("{stem}-delivered.bin")),
            &delivered,
        )?;
        fs::write(self.artifacts.join(format!("{stem}-capture.bin")), &capture)?;
        fs::write(
            self.artifacts.join(format!("{stem}-delivered-screen.txt")),
            delivered_rows.join("\n"),
        )?;
        fs::write(
            self.artifacts.join(format!("{stem}-tmux-screen.txt")),
            truth_rows.join("\n"),
        )?;

        Ok(PaneParity {
            pane: pane_id.into(),
            rows,
            columns,
            topology_rows,
            topology_columns,
            delivered_bytes: delivered.len(),
            quiesced,
            delivered_alternate_screen: delivered_screen.alternate_screen(),
            text_mismatch_rows,
            attribute_mismatch_cells,
            styled_cells_delivered,
            styled_cells_tmux,
            text_diffs,
            attribute_diffs,
        })
    }
}

/// Attaches, and waits until every watched pane has content the desktop could
/// paint. The full byte log is armed before the attach, because the attach is
/// what produces the seeds it has to capture.
fn attach_and_seed(
    client: &mut PerfClient,
    snapshot: &v1::Snapshot,
    session_id: &str,
    panes: &[String],
) -> Result<()> {
    for pane in panes {
        client.obs.full_log_panes.insert(pane.clone());
    }
    client.attach(snapshot, session_id)?;
    let wanted = panes.to_vec();
    let seeded = client.pump_until(Instant::now() + Duration::from_secs(15), |obs, _| {
        wanted.iter().all(|pane| obs.paintable_panes.contains(pane))
    })?;
    ensure!(seeded, "fixture panes were never seeded after attach");
    Ok(())
}

/// Stage 12.9 item 4 lane for P12-U003: is the screen the host delivered to
/// this client the screen tmux itself has?
///
/// The fixture session pairs a flood-able plain shell pane with an agent-TUI
/// pane (alternate screen, cursor addressing, DEC 2026 brackets, bold/dim runs,
/// 1 Hz repaint). At five quiesce points — seed, hide/reveal, pause/continue,
/// an explicit seed request, and a full bridge reconnect — both panes are
/// rendered from what the host delivered and diffed against `capture-pane -e`.
/// Nothing about the renderer is in this path: a mismatch here is the host's
/// delivery being wrong before any desktop code has run.
fn run_vt_parity(
    transport: Transport,
    session_name: &str,
    label: &str,
    stall_script: &str,
    restore_script: &str,
    stall_seconds: u64,
    quiet_millis: u64,
) -> Result<()> {
    let (tmux_socket, runtime) = match &transport {
        Transport::Local {
            tmux_socket,
            runtime,
            ..
        } => (tmux_socket.clone(), runtime.clone()),
        Transport::Ssh { .. } => bail!("vt-parity is a local-transport probe"),
    };
    // The pause round spends its first two seconds with the agent pane still
    // repainting, so tmux has something to pause.
    ensure!(stall_seconds >= 3, "vt-parity needs a stall of at least 3s");
    // The runtime directory is the contract between the lane script and this
    // driver; `run-vt-parity.sh` starts the fixture watching this same path.
    let hold_file = PathBuf::from(&runtime).join("agent-hold");
    let artifacts = PathBuf::from(&runtime).join("vt-parity");
    fs::create_dir_all(&artifacts).context("create parity artifact directory")?;

    let mut client = PerfClient::connect(&transport, Duration::from_secs(2))?;
    let initial = client.subscribe()?;
    let target = session(&initial, session_name)?.clone();
    let mut panes: Vec<_> = initial
        .panes
        .iter()
        .filter(|pane| pane.session_id == target.id)
        .map(|pane| pane.id.clone())
        .collect();
    panes.sort_by_key(|pane| pane[1..].parse::<u64>().unwrap_or(u64::MAX));
    ensure!(
        panes.len() >= 2,
        "fixture needs a shell pane and an agent pane"
    );
    let (shell, agent) = (panes[0].clone(), panes[1].clone());

    let mut lane = ParityLane {
        tmux_socket,
        artifacts: artifacts.clone(),
        quiet: Duration::from_millis(quiet_millis),
        panes: vec![shell.clone(), agent.clone()],
        points: BTreeMap::new(),
    };

    // The agent fixture is held still for every recovery and the comparison
    // that follows it. Letting it repaint through a recovery would repair a
    // wrong delivery within a second and hide exactly the defect class this
    // lane exists to find; an idle agent TUI is also the state a user is
    // looking at when they see a corrupt pane.
    let mut writer = FixtureHold::new(hold_file);
    writer.hold()?;

    // 1. seed — the screens the attach itself produced.
    attach_and_seed(&mut client, &initial, &target.id, &lane.panes)?;
    lane.record(&mut client, "seed")?;

    // 2. hideReveal — the pane repaints several times while the host is not
    //    delivering it, then the reveal has to hand back a current screen.
    let (agent_rows, agent_columns) = tmux_pane_size(&lane.tmux_socket, &agent)?;
    let hidden_screen = serialized_screen(&client, &agent, agent_rows, agent_columns);
    let hide_id = client.hide_with_screen(&agent, hidden_screen)?;
    let hidden = client.pump_until(Instant::now() + Duration::from_secs(5), |_, responses| {
        responses.contains_key(&hide_id)
    })?;
    let hide_response = client.responses.remove(&hide_id);
    if let Some(response) = hide_response {
        ensure!(
            response.ok,
            "hide rejected: {} {}",
            response.error_code,
            response.display_message
        );
    }
    ensure!(hidden, "hide of {agent} was never acknowledged");
    // Repaint several times while hidden, then go idle again before the reveal.
    writer.release()?;
    client.pump_until(Instant::now() + Duration::from_millis(3500), |_, _| false)?;
    // The pane is hidden, so nothing of it reaches the wire: wait the hold out
    // in wall-clock time, the same as before the attach.
    writer.hold()?;
    client.reveal_and_await_seed(&agent, Instant::now() + Duration::from_secs(15))?;
    lane.record(&mut client, "hideReveal")?;

    // 3. pauseContinue — the P12-U001 shape: flood the shell pane and freeze the
    //    daemon until tmux's pause-after engages, then let both panes resume.
    //    The agent pane repaints into the pause and then goes idle, so its
    //    screen can only come back through the host's recovery.
    writer.release()?;
    client.input(&shell, b"yes ade-vt-parity-flood-line\r")?;
    client.pump_until(Instant::now() + Duration::from_millis(1500), |_, _| false)?;
    let flood_started_at = client.obs.output_bytes(&shell);
    run_script(stall_script)?;
    let stall_end = Instant::now() + Duration::from_secs(stall_seconds);
    client.pump_until(Instant::now() + Duration::from_secs(2), |_, _| false)?;
    // The daemon is frozen, so the agent pane's frames are not reaching the
    // wire either; the hold takes effect in tmux while the stall runs out.
    writer.hold()?;
    client.pump_until(stall_end, |_, _| false)?;
    // Nothing may reach the client while the daemon is frozen. If this is not
    // ~0 the stall did not take, and the pause round proved nothing.
    let stall_delivered = client.obs.output_bytes(&shell) - flood_started_at;
    run_script(restore_script)?;
    let shell_before = client.obs.output_bytes(&shell);
    let flood_resumed = client.pump_until(Instant::now() + Duration::from_secs(15), |obs, _| {
        obs.output_bytes(&shell) > shell_before
    })?;
    let interrupt_error = client.input(&shell, b"\x03").err().map(|e| e.to_string());
    lane.record(&mut client, "pauseContinue")?;

    // 4. recovery — the pane-scoped reseed the desktop asks for when it has lost
    //    confidence in a pane. The fresh seed must land on a current screen.
    client.obs.seeded_panes.remove(&agent);
    let seed_request = client.send(
        v1::Operation::RequestTerminalSeed,
        v1::Request {
            scope: agent.clone(),
            ..Default::default()
        },
    )?;
    let reseeded = client.pump_until(
        Instant::now() + Duration::from_secs(15),
        |obs, responses| obs.seeded_panes.contains(&agent) && responses.contains_key(&seed_request),
    )?;
    let seed_response = client.responses.remove(&seed_request);
    if let Some(response) = seed_response {
        ensure!(
            response.ok,
            "seed request rejected: {} {}",
            response.error_code,
            response.display_message
        );
    }
    ensure!(reseeded, "pane {agent} was never reseeded on request");
    lane.record(&mut client, "recovery")?;

    // 5. reconnect — drop the bridge and rebuild the whole client view from a
    //    fresh subscribe + attach, which is what the desktop does after a
    //    connection-level failure.
    let carried_resyncs = client.obs.connection_wide_resyncs;
    let carried_details = client.obs.resync_details.clone();
    let carried_gap = client.obs.gap;
    let carried_seeds = client.obs.event_count(v1::EventKind::TerminalSeed);
    let carried_paused = client.obs.event_count(v1::EventKind::TerminalFlowPaused);
    drop(client);
    let mut client = PerfClient::connect(&transport, Duration::from_secs(2))?;
    let reconnected = client.subscribe()?;
    attach_and_seed(&mut client, &reconnected, &target.id, &lane.panes)?;
    lane.record(&mut client, "reconnect")?;

    let mut resync_details = carried_details;
    resync_details.extend(client.obs.resync_details.clone());
    let text_mismatch_total: u64 = lane
        .points
        .values()
        .map(|point| point["textMismatchRows"].as_u64().unwrap_or_default())
        .sum();
    let attribute_mismatch_total: u64 = lane
        .points
        .values()
        .map(|point| point["attributeMismatchCells"].as_u64().unwrap_or_default())
        .sum();

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "label": label,
            "stallSeconds": stall_seconds,
            "quiesceMillis": quiet_millis,
            "shellPane": shell,
            "agentPane": agent,
            "floodResumedAfterPause": flood_resumed,
            "bytesDeliveredDuringStall": stall_delivered,
            "interruptError": interrupt_error,
            "eventCounts": client.obs.event_counts.clone(),
            "quiescePoints": lane.points,
            "textMismatchRowsTotal": text_mismatch_total,
            "attributeMismatchCellsTotal": attribute_mismatch_total,
            "flowPausedEvents": carried_paused + client.obs.event_count(v1::EventKind::TerminalFlowPaused),
            "terminalSeeds": carried_seeds + client.obs.event_count(v1::EventKind::TerminalSeed),
            "connectionWideResyncEvents": carried_resyncs + client.obs.connection_wide_resyncs,
            "resyncDetails": resync_details,
            "sequenceGapObserved": carried_gap || client.obs.gap,
            "artifacts": artifacts.display().to_string(),
        }))?
    );
    Ok(())
}

/// How long the flood probe should run. The exit criteria ask for a 60 s flood
/// with zero connection-wide resyncs; the default keeps every existing lane's
/// duration exactly as it was.
fn flood_seconds(arguments: &[String], default: u64) -> u64 {
    value_after(arguments, "--flood-seconds")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn value_after(arguments: &[String], name: &str) -> Result<String> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
        .with_context(|| format!("missing {name}"))
}

fn main() -> Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let scenario = arguments.first().cloned().unwrap_or_default();
    let transport = match value_after(&arguments, "--transport")?.as_str() {
        "local" => Transport::Local {
            host_binary: value_after(&arguments, "--host-binary")?,
            runtime: value_after(&arguments, "--runtime")?,
            tmux_socket: value_after(&arguments, "--tmux-socket")?,
        },
        "ssh" => Transport::Ssh {
            config: value_after(&arguments, "--ssh-config")?,
            target: value_after(&arguments, "--ssh-target")?,
        },
        other => bail!("unknown transport {other:?}"),
    };
    let primary = value_after(&arguments, "--primary").unwrap_or_else(|_| "primary".into());
    let label = value_after(&arguments, "--label").unwrap_or_else(|_| "unlabeled".into());
    let reconcile_timeout = Duration::from_millis(
        value_after(&arguments, "--reconcile-timeout-ms")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(2_000),
    );
    match scenario.as_str() {
        "perf" => run_perf(
            transport,
            &primary,
            &label,
            reconcile_timeout,
            flood_seconds(&arguments, 10),
        ),
        "smoke" => run_smoke(
            transport,
            &primary,
            &label,
            reconcile_timeout,
            flood_seconds(&arguments, 3),
        ),
        "stall" => run_stall(
            transport,
            &label,
            &value_after(&arguments, "--stall-script")?,
            &value_after(&arguments, "--restore-script")?,
            value_after(&arguments, "--stall-seconds")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(2),
        ),
        "pause-probe" => run_pause_probe(
            transport,
            &primary,
            &label,
            &value_after(&arguments, "--stall-script")?,
            &value_after(&arguments, "--restore-script")?,
            value_after(&arguments, "--stall-seconds")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(7),
        ),
        "vt-parity" => run_vt_parity(
            transport,
            &primary,
            &label,
            &value_after(&arguments, "--stall-script")?,
            &value_after(&arguments, "--restore-script")?,
            value_after(&arguments, "--stall-seconds")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(8),
            value_after(&arguments, "--quiesce-ms")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(700),
        ),
        "idle-steady" => run_idle_steady(
            transport,
            &primary,
            &label,
            &value_after(&arguments, "--hold-file")?,
            value_after(&arguments, "--window-seconds")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(60),
        ),
        "client-geometry" => {
            let number = |name: &str, fallback: u64| {
                value_after(&arguments, name)
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(fallback)
            };
            run_client_geometry(
                transport,
                &primary,
                &label,
                ClientGeometryLane {
                    columns: number("--columns", 180) as u32,
                    rows: number("--rows", 45) as u32,
                    bound_columns: number("--bound-columns", 188) as u32,
                    bound_rows: number("--bound-rows", 51) as u32,
                    churn_rounds: number("--churn-rounds", 6) as usize,
                    idle_seconds: number("--idle-seconds", 600),
                },
            )
        }
        "client-generations" => {
            let number = |name: &str, fallback: u64| {
                value_after(&arguments, name)
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(fallback)
            };
            run_client_generations(
                transport,
                &primary,
                &label,
                number("--columns", 180) as u32,
                number("--rows", 45) as u32,
            )
        }
        "agent-echo" => run_agent_echo(
            transport,
            &primary,
            &label,
            value_after(&arguments, "--samples")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(30),
        ),
        "raw-tmux" => run_raw_tmux(
            transport,
            &primary,
            &value_after(&arguments, "--pane")?,
            &label,
            45,
        ),
        other => bail!("unknown scenario {other:?}"),
    }
}
