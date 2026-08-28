use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::fd::AsRawFd,
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    os::unix::net::UnixStream,
    os::unix::process::CommandExt,
    path::{Component, Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{Arc, Mutex, OnceLock, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::paths;

const DIAGNOSTICS_SCHEMA_VERSION: u32 = 1;
const MAX_STATE_BYTES: u64 = 128 * 1024;
const MAX_RECENT_ERRORS: usize = 16;
const STATE_FILE: &str = "diagnostics.json";
const DEPENDENCY_PROBE_TIMEOUT: Duration = Duration::from_millis(750);
const DEPENDENCY_OUTPUT_LIMIT: usize = 512;

/// Unknown fields are ignored on purpose, in both directions: a counter added
/// later must load into an older helper — the install path can roll back to one
/// — and rejecting the file there would zero every counter in it. `RuntimeState`
/// keeps `deny_unknown_fields`, so a structurally wrong file is still refused.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FlowCounters {
    connections_accepted: u64,
    connections_active: u64,
    connections_ended: u64,
    connection_errors: u64,
    accept_errors: u64,
    hook_fallback_errors: u64,
    event_queue_overflows: u64,
    terminal_input_backpressure_rejections: u64,
    /// Client resizes refused for being outside the sane cell bound. Defaulted
    /// so a state file written before this counter existed still loads.
    #[serde(default)]
    terminal_client_resize_rejections: u64,
    /// Connection teardowns where the writer drain or the terminal-worker join
    /// outlived its grace period and was forced. Zero is the healthy value; a
    /// climbing count means some sender clone or worker thread is not
    /// releasing on its own and teardown is running on the backstop.
    #[serde(default)]
    connections_force_closed: u64,
    /// The six counters below are the persisted half of the pane-stranding
    /// story. Every one of them names an event that used to happen silently, on
    /// a path whose only other record was a stderr line the daemon sends to a
    /// file nobody reads once the process is detached. A pane that froze once,
    /// hours ago, leaves no live state to inspect; these are what is left
    /// afterwards. Each is a count of a host decision — never a pane id, a
    /// path or terminal content — so they stay inside the privacy declaration.
    ///
    /// A pane whose recovery material the global budget discarded.
    #[serde(default)]
    pane_evictions: u64,
    /// A pane whose recovery material was released outright, by that eviction
    /// or by a hidden tail outgrowing its budget.
    #[serde(default)]
    pane_releases: u64,
    /// Explicit seed requests that had to force a hidden pane back to visible.
    #[serde(default)]
    seed_forced_reveals: u64,
    /// Internal seed requests re-attempted after one failed.
    #[serde(default)]
    seed_request_retries: u64,
    /// Flow-control resumes tmux refused, retried or stalled.
    #[serde(default)]
    flow_resume_rejections: u64,
    /// Recovery events the ordered queue could not take immediately and that
    /// were handed to a deferred sender instead of being dropped.
    #[serde(default)]
    recovery_event_deferrals: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecentSafeError {
    class: SafeErrorClass,
    count: u64,
    last_seen_epoch_seconds: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeErrorClass {
    DaemonAcceptFailed,
    HostConnectionEnded,
    HookFallbackIngestionFailed,
    DiagnosticsStateInvalid,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeState {
    schema_version: u32,
    counters: FlowCounters,
    recent_errors: Vec<RecentSafeError>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            schema_version: DIAGNOSTICS_SCHEMA_VERSION,
            counters: FlowCounters::default(),
            recent_errors: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct RuntimeDiagnostics {
    state: Arc<Mutex<RuntimeState>>,
    persistence: Arc<PersistenceWorker>,
}

struct PersistenceWorker {
    sender: Mutex<Option<mpsc::SyncSender<()>>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    state_path: PathBuf,
    state: Arc<Mutex<RuntimeState>>,
    write_lock: Arc<Mutex<()>>,
}

static ACTIVE_RUNTIME: OnceLock<Mutex<Option<RuntimeDiagnostics>>> = OnceLock::new();

impl RuntimeDiagnostics {
    pub fn open(runtime: &Path) -> anyhow::Result<Self> {
        validate_private_directory(runtime)?;
        let state_path = runtime.join(STATE_FILE);
        let (mut state, invalid) = match read_runtime_state(&state_path) {
            Ok(Some(state)) => (state, false),
            Ok(None) => (RuntimeState::default(), false),
            Err(_) => (RuntimeState::default(), true),
        };
        // Active connections cannot survive a daemon restart. Never report a
        // stale gauge from a killed process.
        state.counters.connections_active = 0;
        if invalid {
            record_safe_error(&mut state, SafeErrorClass::DiagnosticsStateInvalid);
        }
        let state = Arc::new(Mutex::new(state));
        let bytes = serde_json::to_vec(&*state.lock().unwrap())?;
        atomic_write_private(&state_path, &bytes)?;
        let persistence = Arc::new(PersistenceWorker::start(state_path, Arc::clone(&state)));
        let value = Self { state, persistence };
        Ok(value)
    }

    pub fn connection_accepted(&self) {
        self.update(|state| {
            state.counters.connections_accepted =
                state.counters.connections_accepted.saturating_add(1);
            state.counters.connections_active = state.counters.connections_active.saturating_add(1);
        });
    }

    pub fn connection_ended(&self, failed: bool) {
        self.update(|state| {
            state.counters.connections_active = state.counters.connections_active.saturating_sub(1);
            state.counters.connections_ended = state.counters.connections_ended.saturating_add(1);
            if failed {
                state.counters.connection_errors =
                    state.counters.connection_errors.saturating_add(1);
                record_safe_error(state, SafeErrorClass::HostConnectionEnded);
            }
        });
    }

    pub fn record(&self, class: SafeErrorClass) {
        self.update(|state| {
            match class {
                SafeErrorClass::DaemonAcceptFailed => {
                    state.counters.accept_errors = state.counters.accept_errors.saturating_add(1)
                }
                SafeErrorClass::HookFallbackIngestionFailed => {
                    state.counters.hook_fallback_errors =
                        state.counters.hook_fallback_errors.saturating_add(1)
                }
                SafeErrorClass::HostConnectionEnded | SafeErrorClass::DiagnosticsStateInvalid => {}
            }
            record_safe_error(state, class);
        });
    }

    pub fn install_process_recorder(&self) {
        *ACTIVE_RUNTIME
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = Some(self.clone());
    }

    fn update(&self, update: impl FnOnce(&mut RuntimeState)) {
        {
            let mut state = self.state.lock().unwrap();
            update(&mut state);
        }
        // Never perform filesystem I/O on service/runtime threads. A capacity
        // one signal coalesces overload bursts while the ordered worker writes
        // the latest in-memory snapshot.
        self.persistence.schedule();
    }

    pub fn flush(&self) -> anyhow::Result<()> {
        self.persistence.persist_now()
    }
}

impl PersistenceWorker {
    fn start(state_path: PathBuf, state: Arc<Mutex<RuntimeState>>) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let write_lock = Arc::new(Mutex::new(()));
        let worker_state = Arc::clone(&state);
        let worker_path = state_path.clone();
        let worker_lock = Arc::clone(&write_lock);
        let worker = std::thread::Builder::new()
            .name("diagnostics-writer".into())
            .spawn(move || {
                while receiver.recv().is_ok() {
                    // Bound write frequency during floods without waiting for a
                    // quiet period that may never arrive.
                    std::thread::sleep(Duration::from_millis(50));
                    while receiver.try_recv().is_ok() {}
                    let _guard = worker_lock.lock().unwrap();
                    let _ = persist_state(&worker_path, &worker_state);
                }
                let _guard = worker_lock.lock().unwrap();
                let _ = persist_state(&worker_path, &worker_state);
            })
            .expect("spawn diagnostics persistence worker");
        Self {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
            state_path,
            state,
            write_lock,
        }
    }

    fn schedule(&self) {
        if let Some(sender) = self.sender.lock().unwrap().as_ref() {
            let _ = sender.try_send(());
        }
    }

    fn persist_now(&self) -> anyhow::Result<()> {
        let _guard = self.write_lock.lock().unwrap();
        persist_state(&self.state_path, &self.state)
    }
}

impl Drop for PersistenceWorker {
    fn drop(&mut self) {
        // Close the channel before joining so the writer performs its final
        // snapshot and cannot outlive a runtime-directory owner. This also
        // makes short-lived diagnostics/doctor processes deterministic.
        self.sender.lock().unwrap().take();
        if let Some(worker) = self.worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
}

fn persist_state(path: &Path, state: &Mutex<RuntimeState>) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(&*state.lock().unwrap())?;
    atomic_write_private(path, &bytes)
}

pub fn record_event_queue_overflow() {
    update_active_counter(|counters| {
        counters.event_queue_overflows = counters.event_queue_overflows.saturating_add(1);
    });
}

pub fn record_connection_force_closed() {
    update_active_counter(|counters| {
        counters.connections_force_closed = counters.connections_force_closed.saturating_add(1);
    });
}

pub fn record_terminal_input_backpressure() {
    update_active_counter(|counters| {
        counters.terminal_input_backpressure_rejections = counters
            .terminal_input_backpressure_rejections
            .saturating_add(1);
    });
}

/// Counts one pane whose recovery material the pane-resource store discarded on
/// its own, and whether that left the pane released.
pub fn record_pane_degradation(evicted: bool, released: bool) {
    update_active_counter(|counters| {
        if evicted {
            counters.pane_evictions = counters.pane_evictions.saturating_add(1);
        }
        if released {
            counters.pane_releases = counters.pane_releases.saturating_add(1);
        }
    });
}

/// Counts one explicit seed request that had to force its pane back to visible.
pub fn record_seed_forced_reveal() {
    update_active_counter(|counters| {
        counters.seed_forced_reveals = counters.seed_forced_reveals.saturating_add(1);
    });
}

/// Counts one re-attempt of a seed request the host owed a pane.
pub fn record_seed_request_retry() {
    update_active_counter(|counters| {
        counters.seed_request_retries = counters.seed_request_retries.saturating_add(1);
    });
}

/// Counts one recovery event deferred rather than dropped by a full queue.
pub fn record_recovery_event_deferral() {
    update_active_counter(|counters| {
        counters.recovery_event_deferrals = counters.recovery_event_deferrals.saturating_add(1);
    });
}

fn update_active_counter(update: impl FnOnce(&mut FlowCounters)) {
    let diagnostics = ACTIVE_RUNTIME
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .clone();
    if let Some(diagnostics) = diagnostics {
        diagnostics.update(|state| update(&mut state.counters));
    }
}

/// Names a rejected client resize in the daemon log.
///
/// The size is the whole point: a rejection without it cannot be told apart
/// from a transport failure afterwards, and the size is what identifies which
/// side computed nonsense. Cell counts carry no terminal content, no path and
/// no hostname, so this stays inside the privacy declaration above.
pub fn record_rejected_client_resize(columns: u32, rows: u32) {
    update_active_counter(|counters| {
        counters.terminal_client_resize_rejections =
            counters.terminal_client_resize_rejections.saturating_add(1);
    });
    write_rejected_client_resize_log(columns, rows);
}

fn write_rejected_client_resize_log(columns: u32, rows: u32) {
    let line = serde_json::json!({
        "subsystem": "host_daemon",
        "event": "terminalClientResizeRejected",
        "columns": columns,
        "rows": rows,
    });
    eprintln!("{line}");
}

/// Names every handoff of the one control client tmux sizes from.
///
/// The flag and the size are two `refresh-client` writes to a pipe, and a pipe
/// write that tmux ignores succeeds. When that happened the only symptom was a
/// user's windows sitting at 80x24 with nothing anywhere saying which client
/// had been asked for what — the whole of M13-E005 was reconstructed from a
/// live `list-clients`. Recording the handoff is what makes the next one
/// readable from a log.
///
/// tmux session identifiers (`$3`) are the server's own ordinals: not names,
/// not paths, not hostnames, and not terminal content. This stays inside the
/// privacy declaration above.
///
/// `error` is the one free-form field, and it is bounded rather than trusted;
/// see `bounded_log_text`.
pub fn write_terminal_sizing_handoff_log(
    previous_session: Option<&str>,
    session_id: &str,
    size: Option<(u32, u32)>,
    error: Option<&str>,
) {
    let error = error.map(bounded_log_text);
    let line = serde_json::json!({
        "subsystem": "host_daemon",
        "event": "terminalSizingHandoff",
        "previousSession": previous_session,
        "sessionId": session_id,
        // Null means the desktop has not asked for a size yet on this
        // connection, which is why a newly visible client can be correct and
        // still be at tmux's default.
        "size": size.map(|(columns, rows)| format!("{columns}x{rows}")),
        "ok": error.is_none(),
        "error": error,
    });
    eprintln!("{line}");
}

/// Names every reclaim of the per-window pointer tmux sizes from.
///
/// The claim is a `switch-client` this daemon issues on its own control client
/// and the only symptom of it working is that the user's pane stops being
/// letterboxed, so this line is the whole record that it happened. It is also
/// how the gate is audited from a log: one line per claim, and a session that
/// nobody else is attached to should produce none, because each claim costs a
/// topology reconcile. Paired with `terminalSizingHandoff` it says whether a
/// size was asserted and then whether tmux was told to follow it.
///
/// tmux session identifiers (`$3`) are the server's own ordinals: not names, not
/// paths, not hostnames, and not terminal content. This stays inside the privacy
/// declaration above.
pub fn write_sizing_latest_claim_log(session_id: &str) {
    let line = serde_json::json!({
        "subsystem": "host_daemon",
        "event": "sizingLatestClaim",
        "sessionId": session_id,
    });
    eprintln!("{line}");
}

/// Names every flow-control resume tmux refused, and what was done about it.
///
/// A retried rejection is deliberately not an event: the host is still handling
/// it and the desktop has nothing to do. That makes the log the only place it
/// exists, which is the point — a pane that recovered on the second attempt
/// recovered from something, and "it worked in the end" is not a diagnosis. It
/// is also what the pause lane's injected-fault run reads to prove the fault
/// fired at all.
///
/// A tmux pane identifier (`%3`) is the server's own ordinal, and `reason` is
/// tmux's own refusal text — a parse error about a command this crate composed,
/// never pane content, which the reader never puts in an error detail for
/// exactly that reason. Bounded anyway; see `bounded_log_text`.
pub fn write_flow_resume_rejected_log(pane_id: &str, disposition: &str, reason: &str) {
    update_active_counter(|counters| {
        counters.flow_resume_rejections = counters.flow_resume_rejections.saturating_add(1);
    });
    let reason = bounded_log_text(reason);
    let line = serde_json::json!({
        "subsystem": "host_daemon",
        "event": "flowResumeRejected",
        "paneId": pane_id,
        "disposition": disposition,
        "reason": reason,
    });
    eprintln!("{line}");
}

/// How long an ordered control operation may run before it is worth retaining.
const SLOW_ORDERED_REQUEST_THRESHOLD: Duration = Duration::from_millis(250);

/// Records a completed ordered operation only when it was materially slow.
///
/// Operation names come from the protocol enum and contain no user data. The
/// timestamp lets this line be joined to the desktop incident journal without
/// recording a session name, path, request body, or terminal content.
pub fn record_ordered_request_duration(operation: &str, elapsed: Duration) {
    if elapsed < SLOW_ORDERED_REQUEST_THRESHOLD {
        return;
    }
    write_ordered_request_log("orderedRequestSlow", operation, elapsed);
}

/// Records the operation that forced a connection-scoped watchdog recovery.
pub fn record_ordered_request_timeout(operation: &str, elapsed: Duration) {
    write_ordered_request_log("orderedRequestTimeout", operation, elapsed);
}

fn write_ordered_request_log(event: &str, operation: &str, elapsed: Duration) {
    let line = serde_json::json!({
        "subsystem": "host_daemon",
        "event": event,
        "operation": operation,
        "ms": u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        "atUnixMillis": now_epoch_millis(),
    });
    eprintln!("{line}");
}

/// How slow a daemon-internal echo leg has to be before it earns a log line.
///
/// The desktop already decomposes typing latency, and every leg it can see is
/// under 10 ms while the spikes being hunted are 100-300 ms end to end. 50 ms is
/// therefore far above anything either daemon leg does when healthy and far
/// below the incidents this log exists to explain, so a line here is evidence
/// rather than background noise.
const SLOW_LEG_THRESHOLD: Duration = Duration::from_millis(50);

/// The shortest gap between two logged occurrences of the same leg.
///
/// One stalled sequencer makes every record of a burst slow, and a line each
/// would be megabytes of stderr for a single incident — on a path whose whole
/// point is that it is not supposed to cost anything. One line per two seconds
/// still lands inside any incident the desktop journal marks, and `suppressed`
/// says how many occurrences that one line stands for.
const SLOW_LEG_LOG_INTERVAL: Duration = Duration::from_secs(2);

/// Rate-limiter state for one kind of slow-leg line: when the last one was
/// written, and how many occurrences have been dropped since.
struct SlowLegLimiter {
    last_emit: Option<Instant>,
    suppressed: u64,
}

impl SlowLegLimiter {
    const fn new() -> Self {
        Self {
            last_emit: None,
            suppressed: 0,
        }
    }
}

/// One limiter per kind, so a flooding output leg can never hide the input leg
/// that is the actual subject of an investigation.
static SLOW_INPUT_LEG: Mutex<SlowLegLimiter> = Mutex::new(SlowLegLimiter::new());
static SLOW_OUTPUT_LEG: Mutex<SlowLegLimiter> = Mutex::new(SlowLegLimiter::new());

/// Names one slow leg from an input batch leaving the dispatch queue to its
/// bytes being committed to tmux.
///
/// Below [`SLOW_LEG_THRESHOLD`] this returns before touching any lock, which is
/// what keeps the measurement free on the path it measures.
pub fn record_slow_input_leg(elapsed: Duration, bytes: usize, pane_id: &str) {
    write_slow_leg_log("slowInputLeg", &SLOW_INPUT_LEG, elapsed, bytes, pane_id);
}

/// Names one slow leg from a control-stream read returning to the terminal
/// output it produced being accepted by the outbound sequencer.
pub fn record_slow_output_leg(elapsed: Duration, bytes: usize, pane_id: &str) {
    write_slow_leg_log("slowOutputLeg", &SLOW_OUTPUT_LEG, elapsed, bytes, pane_id);
}

/// Writes one slow-leg line, or counts it as suppressed.
///
/// `atUnixMillis` is the whole reason this is a log line rather than a counter:
/// the desktop keeps its own incident journal in wall clock, and a slow leg is
/// only useful if it can be lined up with the echo spike it explains.
///
/// A tmux pane identifier (`%3`) is the server's own ordinal, and the other
/// fields are a duration and a byte count. No path, no hostname, and no
/// terminal content, so this stays inside the privacy declaration above.
fn write_slow_leg_log(
    event: &str,
    limiter: &Mutex<SlowLegLimiter>,
    elapsed: Duration,
    bytes: usize,
    pane_id: &str,
) {
    if elapsed < SLOW_LEG_THRESHOLD {
        return;
    }
    let Some(suppressed) = admit_slow_leg(&mut limiter.lock().unwrap(), Instant::now()) else {
        return;
    };
    let line = serde_json::json!({
        "subsystem": "host_daemon",
        "event": event,
        "ms": u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        "bytes": bytes,
        "paneId": pane_id,
        "atUnixMillis": now_epoch_millis(),
        "suppressed": suppressed,
    });
    eprintln!("{line}");
}

/// Decides whether this occurrence is written, returning the number of
/// occurrences the line it is about to write stands for.
fn admit_slow_leg(limiter: &mut SlowLegLimiter, now: Instant) -> Option<u64> {
    if let Some(last) = limiter.last_emit
        && now.duration_since(last) < SLOW_LEG_LOG_INTERVAL
    {
        limiter.suppressed = limiter.suppressed.saturating_add(1);
        return None;
    }
    limiter.last_emit = Some(now);
    Some(std::mem::replace(&mut limiter.suppressed, 0))
}

/// Bounds the one free-form field either of the two loggers above carries.
///
/// Both take text this crate composed from tmux's own refusal messages, which
/// name no path and no host — but "none of them do today" is not a property a
/// log line should rest on, and an unbounded string in a log is also how one
/// bad error becomes a megabyte of stderr. Cut on a character boundary, because
/// tmux's messages are not guaranteed ASCII.
fn bounded_log_text(text: &str) -> String {
    const MAX_CHARS: usize = 200;
    match text.char_indices().nth(MAX_CHARS) {
        Some((index, _)) => format!("{}…", &text[..index]),
        None => text.to_owned(),
    }
}

pub fn write_safe_log(class: SafeErrorClass) {
    let line = serde_json::json!({
        "subsystem": "host_daemon",
        "errorClass": class,
    });
    eprintln!("{line}");
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticsReport {
    schema_version: u32,
    generated_epoch_seconds: u64,
    privacy: PrivacyDeclaration,
    platform: PlatformReport,
    helper: HelperReport,
    dependencies: DependenciesReport,
    daemon: DaemonReport,
    flow: FlowReport,
    recent_errors: Vec<RecentSafeError>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PrivacyDeclaration {
    telemetry_uploaded: bool,
    terminal_output_included: bool,
    prompt_text_included: bool,
    file_content_included: bool,
    credentials_included: bool,
    paths_or_hostnames_included: bool,
}

#[derive(Debug, Serialize)]
struct PlatformReport {
    os: &'static str,
    architecture: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HelperReport {
    version: &'static str,
    protocol_major: u32,
    protocol_minor: u32,
    capability_bits: u64,
    capabilities: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct DependenciesReport {
    tmux: DependencyReport,
    git: DependencyReport,
    ssh: DependencyReport,
}

#[derive(Debug, Serialize)]
struct DependencyReport {
    available: bool,
    version: Option<String>,
    probe: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DaemonReport {
    endpoint: &'static str,
    state: &'static str,
    runtime_state: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FlowReport {
    event_queue_capacity: usize,
    terminal_input_queue_capacity: usize,
    maximum_frame_bytes: usize,
    maximum_bulk_ssh_connections: usize,
    counters_available: bool,
    counters: FlowCounters,
}

pub fn run_doctor(arguments: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let mut json = false;
    for argument in arguments {
        match argument.as_str() {
            "--json" => json = true,
            "--help" | "-h" => {
                println!("usage: muxflow-host doctor [--json]");
                return Ok(());
            }
            _ => bail!("unknown doctor option"),
        }
    }
    let report = build_report();
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human_report(&report);
    }
    Ok(())
}

pub fn write_support_bundle(arguments: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let arguments: Vec<_> = arguments.collect();
    let output = match arguments.as_slice() {
        [flag, path] if flag == "--output" => PathBuf::from(path),
        _ => bail!("usage: muxflow-host support-bundle --output PATH"),
    };
    let bytes = serde_json::to_vec_pretty(&build_report())?;
    write_new_private(&output, &bytes)?;
    println!("support bundle written");
    Ok(())
}

fn build_report() -> DiagnosticsReport {
    let runtime = paths::default_runtime_dir();
    let (runtime_state, runtime_health) = load_report_state(&runtime);
    let daemon_state = inspect_daemon_endpoint(&runtime.join("host.sock"));
    DiagnosticsReport {
        schema_version: DIAGNOSTICS_SCHEMA_VERSION,
        generated_epoch_seconds: now_epoch_seconds(),
        privacy: PrivacyDeclaration {
            telemetry_uploaded: false,
            terminal_output_included: false,
            prompt_text_included: false,
            file_content_included: false,
            credentials_included: false,
            paths_or_hostnames_included: false,
        },
        platform: PlatformReport {
            os: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
        },
        helper: HelperReport {
            version: tmux_agent_protocol::HELPER_VERSION,
            protocol_major: tmux_agent_protocol::PROTOCOL_MAJOR,
            protocol_minor: tmux_agent_protocol::PROTOCOL_MINOR,
            capability_bits: tmux_agent_protocol::HOST_CAPABILITIES,
            capabilities: tmux_agent_protocol::capability_names(
                tmux_agent_protocol::HOST_CAPABILITIES,
            ),
        },
        dependencies: DependenciesReport {
            tmux: dependency_version("tmux", "-V"),
            git: dependency_version("git", "--version"),
            ssh: dependency_version("ssh", "-V"),
        },
        daemon: DaemonReport {
            endpoint: "private_unix_socket",
            state: daemon_state,
            runtime_state: runtime_health,
        },
        flow: FlowReport {
            event_queue_capacity: crate::service::EVENT_QUEUE,
            terminal_input_queue_capacity: crate::service::TERMINAL_INPUT_QUEUE,
            maximum_frame_bytes: tmux_agent_protocol::MAX_FRAME_BYTES,
            maximum_bulk_ssh_connections: 2,
            counters_available: runtime_state.is_some(),
            counters: runtime_state
                .as_ref()
                .map(|state| state.counters.clone())
                .unwrap_or_default(),
        },
        recent_errors: runtime_state
            .map(|state| state.recent_errors)
            .unwrap_or_default(),
    }
}

fn print_human_report(report: &DiagnosticsReport) {
    println!(
        "muxflow helper {} (protocol {}.{})",
        report.helper.version, report.helper.protocol_major, report.helper.protocol_minor
    );
    println!(
        "platform: {}/{}",
        report.platform.os, report.platform.architecture
    );
    for (name, dependency) in [
        ("tmux", &report.dependencies.tmux),
        ("git", &report.dependencies.git),
        ("ssh", &report.dependencies.ssh),
    ] {
        println!(
            "{name}: {}",
            dependency.version.as_deref().unwrap_or("unavailable")
        );
    }
    println!("daemon: {}", report.daemon.state);
    println!(
        "runtime diagnostics: {} ({} recent safe error classes)",
        report.daemon.runtime_state,
        report.recent_errors.len()
    );
    println!("privacy: local-only, content and credentials excluded");
}

fn dependency_version(program: &str, version_argument: &str) -> DependencyReport {
    let output = bounded_dependency_probe(program, version_argument, DEPENDENCY_PROBE_TIMEOUT);
    let Ok((status, stdout, stderr)) = output else {
        return DependencyReport {
            available: false,
            version: None,
            probe: "unavailable_or_timeout",
        };
    };
    if !status.success() {
        return DependencyReport {
            available: false,
            version: None,
            probe: "failed",
        };
    }
    let bytes = if stdout.is_empty() { &stderr } else { &stdout };
    DependencyReport {
        available: true,
        version: sanitize_version(program, bytes),
        probe: "version_only",
    }
}

fn bounded_dependency_probe(
    program: &str,
    version_argument: &str,
    timeout: Duration,
) -> std::io::Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    let mut command = Command::new(program);
    command
        .arg(version_argument)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: this async-signal-safe call only creates a private process group
    // in the child between fork and exec so a timeout can reap wrappers too.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let started = Instant::now();
    let mut child = loop {
        match command.spawn() {
            Ok(child) => break child,
            // Linux can transiently report ETXTBSY for a just-published
            // executable while filesystem/security scanners release their
            // write handle. Bound the retry inside the probe's own timeout.
            Err(error)
                if error.raw_os_error() == Some(libc::ETXTBSY)
                    && started.elapsed() < timeout.min(Duration::from_millis(25)) =>
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error),
        }
    };
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    set_nonblocking(stdout.as_raw_fd())?;
    set_nonblocking(stderr.as_raw_fd())?;
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    loop {
        drain_bounded(&mut stdout, &mut stdout_bytes)?;
        drain_bounded(&mut stderr, &mut stderr_bytes)?;
        if let Some(status) = child.try_wait()? {
            drain_bounded(&mut stdout, &mut stdout_bytes)?;
            drain_bounded(&mut stderr, &mut stderr_bytes)?;
            return Ok((status, stdout_bytes, stderr_bytes));
        }
        if started.elapsed() >= timeout {
            // SAFETY: the child created its own process group with pgid=pid.
            let _ = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "dependency version probe timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn set_nonblocking(fd: std::os::fd::RawFd) -> std::io::Result<()> {
    // SAFETY: fcntl only reads/updates flags for the valid pipe descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the descriptor remains owned by the ChildStdout/ChildStderr.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn drain_bounded(reader: &mut impl Read, retained: &mut Vec<u8>) -> std::io::Result<()> {
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                let remaining = DEPENDENCY_OUTPUT_LIMIT.saturating_sub(retained.len());
                retained.extend_from_slice(&chunk[..count.min(remaining)]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

fn sanitize_version(program: &str, bytes: &[u8]) -> Option<String> {
    let value = String::from_utf8_lossy(&bytes[..bytes.len().min(DEPENDENCY_OUTPUT_LIMIT)]);
    let line = value.lines().next()?.trim();
    let (prefix, token, remainder) = match program {
        "tmux" => {
            let Some(value) = line.strip_prefix("tmux ") else {
                return Some("available (version redacted)".into());
            };
            let mut parts = value.split_ascii_whitespace();
            let Some(token) = parts.next() else {
                return Some("available (version redacted)".into());
            };
            ("tmux ", token, parts.next())
        }
        "git" => {
            let Some(value) = line.strip_prefix("git version ") else {
                return Some("available (version redacted)".into());
            };
            let mut parts = value.split_ascii_whitespace();
            let Some(token) = parts.next() else {
                return Some("available (version redacted)".into());
            };
            ("git version ", token, parts.next())
        }
        "ssh" => {
            let Some(token) = line.split([',', ' ']).next() else {
                return Some("available (version redacted)".into());
            };
            if !token.starts_with("OpenSSH_") {
                return Some("available (version redacted)".into());
            }
            ("", token, None)
        }
        _ => return Some("available (version redacted)".into()),
    };
    let version_token = token.strip_prefix("OpenSSH_").unwrap_or(token);
    if remainder.is_some()
        || !version_token.starts_with(|value: char| value.is_ascii_digit())
        || !strict_version_token(version_token)
    {
        return Some("available (version redacted)".into());
    }
    Some(format!("{prefix}{token}"))
}

fn strict_version_token(token: &str) -> bool {
    let mut alphabetic_run = 0_u8;
    token.chars().all(|value| {
        if value.is_ascii_alphabetic() {
            alphabetic_run = alphabetic_run.saturating_add(1);
            alphabetic_run <= 1
        } else {
            alphabetic_run = 0;
            value.is_ascii_digit() || matches!(value, '.' | '_' | '-' | '+')
        }
    })
}

fn load_report_state(runtime: &Path) -> (Option<RuntimeState>, &'static str) {
    if validate_private_directory(runtime).is_err() {
        return (None, "unavailable_or_unsafe");
    }
    match read_runtime_state(&runtime.join(STATE_FILE)) {
        Ok(Some(state)) => (Some(state), "valid"),
        Ok(None) => (None, "not_recorded"),
        Err(_) => (None, "invalid_or_unsafe"),
    }
}

fn inspect_daemon_endpoint(path: &Path) -> &'static str {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return "not_running";
    };
    if metadata.file_type().is_socket()
        && metadata.uid() == effective_uid()
        && metadata.permissions().mode() & 0o077 == 0
    {
        if UnixStream::connect(path).is_ok() {
            "connectable_unverified"
        } else {
            "stale_or_unreachable"
        }
    } else {
        "unsafe_or_invalid_endpoint"
    }
}

fn read_runtime_state(path: &Path) -> anyhow::Result<Option<RuntimeState>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("diagnostics state is not a regular file");
    }
    if metadata.uid() != effective_uid() || metadata.permissions().mode() & 0o077 != 0 {
        bail!("diagnostics state is not user-only");
    }
    if metadata.len() > MAX_STATE_BYTES {
        bail!("diagnostics state exceeds its bounded size");
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_STATE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        bail!("diagnostics state exceeds its bounded size");
    }
    let state: RuntimeState = serde_json::from_slice(&bytes)?;
    if state.schema_version != DIAGNOSTICS_SCHEMA_VERSION {
        bail!("unsupported diagnostics state schema");
    }
    if state.recent_errors.len() > MAX_RECENT_ERRORS {
        bail!("diagnostics state has too many recent errors");
    }
    Ok(Some(state))
}

fn validate_private_directory(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!("diagnostics runtime is not a real directory");
    }
    if metadata.uid() != effective_uid() || metadata.permissions().mode() & 0o077 != 0 {
        bail!("diagnostics runtime is not user-only");
    }
    Ok(())
}

fn atomic_write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if bytes.len() as u64 > MAX_STATE_BYTES {
        bail!("diagnostics state exceeds its bounded size");
    }
    let parent = path.parent().context("diagnostics state has no parent")?;
    validate_private_directory(parent)?;
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (!metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != effective_uid()
            || metadata.permissions().mode() & 0o077 != 0)
    {
        bail!("refusing unsafe diagnostics state destination");
    }
    let temporary = parent.join(format!(".diagnostics-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != effective_uid()
            || metadata.permissions().mode() & 0o077 != 0
        {
            bail!("diagnostics state publication was unsafe");
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn write_new_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    validate_output_parent(path)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .context("create support bundle without overwriting an existing path")?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != effective_uid()
            || metadata.permissions().mode() & 0o077 != 0
        {
            bail!("support bundle is not a private regular file");
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

fn validate_output_parent(path: &Path) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut current = if parent.is_absolute() {
        PathBuf::from("/")
    } else {
        fs::canonicalize(std::env::current_dir()?)?
    };
    for component in parent.components() {
        match component {
            Component::RootDir | Component::CurDir => continue,
            Component::ParentDir => bail!("support bundle path may not contain parent traversal"),
            Component::Normal(component) => current.push(component),
            Component::Prefix(_) => bail!("unsupported support bundle path prefix"),
        }
        let metadata = fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() {
            bail!("support bundle parent may not contain symlinks");
        }
    }
    let metadata = fs::symlink_metadata(&current)?;
    if !metadata.is_dir() {
        bail!("support bundle parent is not a directory");
    }
    Ok(())
}

fn record_safe_error(state: &mut RuntimeState, class: SafeErrorClass) {
    let now = now_epoch_seconds();
    if let Some(existing) = state
        .recent_errors
        .iter_mut()
        .find(|value| value.class == class)
    {
        existing.count = existing.count.saturating_add(1);
        existing.last_seen_epoch_seconds = now;
        return;
    }
    if state.recent_errors.len() == MAX_RECENT_ERRORS {
        state.recent_errors.remove(0);
    }
    state.recent_errors.push(RecentSafeError {
        class,
        count: 1,
        last_seen_epoch_seconds: now,
    });
}

fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Wall clock for a slow-leg line, in the same unit the desktop journal keeps.
fn now_epoch_millis() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

fn effective_uid() -> u32 {
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private_directory() -> PathBuf {
        #[cfg(target_os = "macos")]
        let temporary_root = Path::new("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let temporary_root = std::env::temp_dir();
        let path = temporary_root.join(format!("ade-diagnostics-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    fn bundle_serialization_has_no_channel_for_private_inputs() {
        let sensitive = [
            "token=ghp_super-secret",
            "password: hunter2",
            "https://user:pass@example.test/private",
            "HOME=/home/alice",
            "/home/alice/project/private.txt",
            "ssh-rsa private-key-content",
            "prompt: summarize the acquisition",
            "terminal output: customer-name",
        ];
        let report = serde_json::to_string(&build_report()).unwrap();
        for value in sensitive {
            assert!(!report.contains(value));
        }
        assert!(!report.contains("env"));
        assert!(!report.contains("currentDirectory"));
        assert!(!report.contains("hostname"));
    }

    #[test]
    fn version_sanitizer_redacts_tokens_passwords_urls_env_paths_and_content() {
        for value in [
            b"tmux https://example.test/token".as_slice(),
            b"tmux /home/alice/private",
            b"tmux C:\\Users\\alice",
            b"tmux alice@example.test",
            b"tmux TOKEN=secret",
            b"tmux $HOME",
            b"tmux password hunter2",
            b"tmux bearer abc123",
            b"tmux prompt customer-content",
            b"malicious helper output",
        ] {
            assert_eq!(
                sanitize_version("tmux", value).as_deref(),
                Some("available (version redacted)")
            );
        }
        assert_eq!(
            sanitize_version("tmux", b"tmux 3.7b").as_deref(),
            Some("tmux 3.7b")
        );
        assert_eq!(
            sanitize_version("git", b"git version 2.50.1").as_deref(),
            Some("git version 2.50.1")
        );
        assert_eq!(
            sanitize_version("ssh", b"OpenSSH_9.9p2, OpenSSL 3.4.1").as_deref(),
            Some("OpenSSH_9.9p2")
        );
        assert_eq!(
            sanitize_version("tmux", b"tmux 3.7 AcmeRoadmap").as_deref(),
            Some("available (version redacted)")
        );
        assert_eq!(
            sanitize_version("git", b"git version 2.50.1 customer").as_deref(),
            Some("available (version redacted)")
        );
    }

    #[test]
    fn version_redaction_is_invariant_under_sensitive_prefix_and_suffix_noise() {
        let sensitive = [
            "token",
            "password",
            "https://private.test/value",
            "HOME=/home/alice",
            "/home/alice/file",
            "prompt customer words",
            "file content customer words",
        ];
        let wrappers = [("", ""), ("alpha ", " omega"), ("123-", "-456")];
        for private in sensitive {
            for (prefix, suffix) in wrappers {
                let value = format!("tmux {prefix}{private}{suffix}");
                assert_eq!(
                    sanitize_version("tmux", value.as_bytes()).as_deref(),
                    Some("available (version redacted)"),
                    "failed to redact generated sensitive case"
                );
            }
        }
    }

    #[test]
    fn runtime_state_rejects_corruption_future_schema_and_unknown_fields() {
        let root = private_directory();
        let path = root.join(STATE_FILE);
        for bytes in [
            b"not json".as_slice(),
            br#"{"schemaVersion":99,"counters":{},"recentErrors":[]}"#,
            br#"{"schemaVersion":1,"counters":{"connectionsAccepted":0,"connectionsActive":0,"connectionsEnded":0,"connectionErrors":0,"acceptErrors":0,"hookFallbackErrors":0,"eventQueueOverflows":0,"terminalInputBackpressureRejections":0},"recentErrors":[],"surprise":true}"#,
        ] {
            fs::write(&path, bytes).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(read_runtime_state(&path).is_err());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dependency_probe_times_out_and_reaps_a_hung_executable() {
        let root = private_directory();
        let script = root.join("hung-version");
        fs::write(&script, b"#!/bin/sh\nsleep 10\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let started = Instant::now();
        let result = bounded_dependency_probe(
            script.to_str().unwrap(),
            "--version",
            Duration::from_millis(75),
        );
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dependency_probe_retains_only_a_bounded_prefix_of_output() {
        let root = private_directory();
        let script = root.join("noisy-version");
        fs::write(
            &script,
            b"#!/bin/sh\nhead -c 1048576 /dev/zero | tr '\\000' X\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let (_, stdout, stderr) = bounded_dependency_probe(
            script.to_str().unwrap(),
            "--version",
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(stdout.len() <= DEPENDENCY_OUTPUT_LIMIT);
        assert!(stderr.len() <= DEPENDENCY_OUTPUT_LIMIT);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn daemon_endpoint_distinguishes_reachable_from_stale_socket() {
        use std::os::unix::net::{UnixDatagram, UnixListener};

        let root = private_directory();
        let path = root.join("host.sock");
        let listener = UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(inspect_daemon_endpoint(&path), "connectable_unverified");
        drop(listener);
        fs::remove_file(&path).unwrap();
        let stale = UnixDatagram::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        drop(stale);
        assert_eq!(inspect_daemon_endpoint(&path), "stale_or_unreachable");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_state_rejects_symlinks_and_public_permissions() {
        let root = private_directory();
        let outside = root.join("outside");
        fs::write(&outside, b"{}").unwrap();
        let path = root.join(STATE_FILE);
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert!(read_runtime_state(&path).is_err());
        fs::remove_file(&path).unwrap();

        fs::write(&path, serde_json::to_vec(&RuntimeState::default()).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_runtime_state(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn support_bundle_is_private_and_never_overwrites() {
        let root = private_directory();
        let path = root.join("support.json");
        write_new_private(&path, b"{}").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(write_new_private(&path, b"replacement").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn support_bundle_refuses_a_symlink_destination() {
        let root = private_directory();
        let target = root.join("target");
        let link = root.join("bundle");
        fs::write(&target, b"unchanged").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(write_new_private(&link, b"private").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"unchanged");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn support_bundle_refuses_symlinked_parent_and_parent_traversal() {
        let root = private_directory();
        let real = root.join("real");
        let link = root.join("link");
        fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(write_new_private(&link.join("bundle"), b"private").is_err());
        assert!(!real.join("bundle").exists());
        assert!(write_new_private(&root.join("real/../traversal.json"), b"private").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_directory_rejects_symlinks_and_public_permissions() {
        let root = private_directory();
        let real = root.join("real");
        let link = root.join("link");
        fs::create_dir(&real).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(validate_private_directory(&link).is_err());
        fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(validate_private_directory(&real).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn safe_log_has_only_a_fixed_class_and_never_raw_detail() {
        let serialized = serde_json::json!({
            "subsystem": "host_daemon",
            "errorClass": SafeErrorClass::HostConnectionEnded,
        })
        .to_string();
        for private in [
            "token=secret",
            "password=hunter2",
            "https://private.test",
            "/home/alice/project",
            "prompt text",
            "terminal content",
        ] {
            assert!(!serialized.contains(private));
        }
        assert_eq!(
            serialized,
            r#"{"errorClass":"host_connection_ended","subsystem":"host_daemon"}"#
        );
    }

    /// A slow leg that fires in a burst must cost one line, and that line must
    /// still say how big the burst was — a rate limiter that silently drops the
    /// rest turns "one slow record" and "every record was slow" into the same
    /// evidence.
    #[test]
    fn slow_leg_lines_are_rate_limited_and_carry_what_they_stand_for() {
        let start = Instant::now();
        let mut limiter = SlowLegLimiter::new();

        assert_eq!(admit_slow_leg(&mut limiter, start), Some(0));
        assert_eq!(
            admit_slow_leg(&mut limiter, start + Duration::from_millis(1)),
            None
        );
        assert_eq!(
            admit_slow_leg(
                &mut limiter,
                start + SLOW_LEG_LOG_INTERVAL - Duration::from_nanos(1)
            ),
            None
        );
        assert_eq!(
            admit_slow_leg(&mut limiter, start + SLOW_LEG_LOG_INTERVAL),
            Some(2)
        );
        // The counter resets with the line that reported it, so the next line
        // describes only its own interval.
        assert_eq!(
            admit_slow_leg(&mut limiter, start + SLOW_LEG_LOG_INTERVAL * 2),
            Some(0)
        );
    }

    #[test]
    fn runtime_open_recovers_corrupt_state_with_safe_class_only() {
        let root = private_directory();
        let path = root.join(STATE_FILE);
        fs::write(&path, b"TOKEN=must-never-appear").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let diagnostics = RuntimeDiagnostics::open(&root).unwrap();
        diagnostics.connection_accepted();
        diagnostics.connection_ended(true);
        diagnostics.flush().unwrap();
        let serialized = fs::read_to_string(&path).unwrap();
        assert!(!serialized.contains("TOKEN"));
        assert!(!serialized.contains("must-never-appear"));
        assert!(serialized.contains("diagnostics_state_invalid"));
        assert!(serialized.contains("host_connection_ended"));
        drop(diagnostics);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn overload_updates_are_nonblocking_and_persist_monotonically() {
        let root = private_directory();
        let diagnostics = RuntimeDiagnostics::open(&root).unwrap();
        let started = std::time::Instant::now();
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let diagnostics = diagnostics.clone();
                std::thread::spawn(move || {
                    for _ in 0..1_000 {
                        diagnostics.connection_accepted();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "in-memory diagnostics updates blocked on persistence"
        );
        diagnostics.flush().unwrap();
        let first = read_runtime_state(&root.join(STATE_FILE))
            .unwrap()
            .unwrap()
            .counters
            .connections_accepted;
        assert_eq!(first, 8_000);
        diagnostics.connection_accepted();
        diagnostics.flush().unwrap();
        let second = read_runtime_state(&root.join(STATE_FILE))
            .unwrap()
            .unwrap()
            .counters
            .connections_accepted;
        assert_eq!(second, 8_001);
        drop(diagnostics);
        fs::remove_dir_all(root).unwrap();
    }
}
