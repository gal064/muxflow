//! Phase 12 latency-instrumentation sink.
//!
//! The renderer owns several of the phase's budgets (keystroke to painted
//! glyph, action to interactive pane, tab switch, explorer expand), so the only
//! honest way to close them is to record spans inside the renderer and persist
//! them for the harness to read. This sink exists for exactly that.
//!
//! It is opt-in from the process environment, never from the page: without
//! `ADE_PERF_LOG` naming an absolute path, every append is refused. A
//! compromised or merely buggy frontend therefore cannot use this to write
//! anywhere, and a normal launch has no instrumentation surface at all.

use serde::Serialize;
use std::{
    collections::VecDeque,
    fs::OpenOptions,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::Instant,
};
use tauri::ipc::{Channel, InvokeResponseBody};

/// Bounds one append so a runaway renderer cannot fill the disk in one call.
const MAX_LINES_PER_APPEND: usize = 4_096;
const MAX_LINE_BYTES: usize = 4_096;

fn configured_path() -> Option<&'static PathBuf> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        let value = std::env::var_os("ADE_PERF_LOG")?;
        let path = PathBuf::from(value);
        // An absolute path keeps the destination independent of the working
        // directory the app happened to inherit.
        path.is_absolute().then_some(path)
    })
    .as_ref()
}

/// True when this process was started with instrumentation enabled. The
/// frontend asks first and stays completely inert when the answer is no.
#[tauri::command]
pub fn perf_log_enabled() -> bool {
    configured_path().is_some()
}

#[tauri::command]
pub async fn append_perf_log(lines: Vec<String>) -> Result<(), String> {
    let Some(path) = configured_path() else {
        return Err("performance logging is not enabled for this process".into());
    };
    if lines.len() > MAX_LINES_PER_APPEND {
        return Err("performance log append exceeds its per-call line bound".into());
    }
    if lines
        .iter()
        .any(|line| line.len() > MAX_LINE_BYTES || line.contains(['\r', '\n']))
    {
        return Err("performance log lines must be single-line and bounded".into());
    }
    // One writer at a time keeps concurrent flushes from interleaving records.
    static WRITER: OnceLock<Mutex<()>> = OnceLock::new();
    let guard = WRITER.get_or_init(|| Mutex::new(()));
    let body = lines.join("\n");
    tauri::async_runtime::spawn_blocking(move || {
        let _lock = guard.lock().map_err(|_| "performance log lock poisoned")?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .map_err(|error| format!("open performance log: {error}"))?;
        writeln!(file, "{body}").map_err(|error| format!("write performance log: {error}"))
    })
    .await
    .map_err(|error| format!("performance log worker failed: {error}"))?
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeMeasurementSnapshot {
    ingress_bytes: u64,
    channel_send_bytes: u64,
    channel_send_count: u64,
    channel_send_nanos: u64,
    js_admission_count: u64,
    js_admission_lag_nanos: u64,
    retained_bytes: u64,
    retained_bytes_high_water: u64,
    acknowledgement_mismatches: u64,
}

#[derive(Debug, Default)]
struct BridgeMeasurements {
    snapshot: BridgeMeasurementSnapshot,
    pending: VecDeque<(usize, Instant)>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferMeasurementSnapshot {
    admission_attempts: u64,
    admission_accepted: u64,
    admission_rejected: u64,
    active: usize,
    active_high_water: usize,
    queued: usize,
    queued_high_water: usize,
    terminal_outcomes: u64,
}

fn transfer_measurements() -> Option<&'static Mutex<TransferMeasurementSnapshot>> {
    static VALUE: OnceLock<Mutex<TransferMeasurementSnapshot>> = OnceLock::new();
    configured_path()?;
    Some(VALUE.get_or_init(|| Mutex::new(TransferMeasurementSnapshot::default())))
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteMeasurementSnapshot {
    control_master_ensure_calls: u64,
    control_master_checks: u64,
    control_master_reuses: u64,
    control_master_establishments: u64,
    interactive_bridge_spawns: u64,
    bulk_bridge_spawns: u64,
}

fn remote_measurements() -> Option<&'static Mutex<RemoteMeasurementSnapshot>> {
    static VALUE: OnceLock<Mutex<RemoteMeasurementSnapshot>> = OnceLock::new();
    configured_path()?;
    Some(VALUE.get_or_init(|| Mutex::new(RemoteMeasurementSnapshot::default())))
}

pub(crate) fn record_remote_operation(name: &str) {
    let Some(measurements) = remote_measurements() else {
        return;
    };
    let mut measurements = measurements.lock().unwrap();
    match name {
        "controlMasterEnsure" => measurements.control_master_ensure_calls += 1,
        "controlMasterCheck" => measurements.control_master_checks += 1,
        "controlMasterReuse" => measurements.control_master_reuses += 1,
        "controlMasterEstablishment" => measurements.control_master_establishments += 1,
        "interactiveBridgeSpawn" => measurements.interactive_bridge_spawns += 1,
        "bulkBridgeSpawn" => measurements.bulk_bridge_spawns += 1,
        _ => {}
    }
}

pub(crate) fn record_transfer_admission(accepted: bool) {
    let Some(measurements) = transfer_measurements() else {
        return;
    };
    let mut measurements = measurements.lock().unwrap();
    measurements.admission_attempts += 1;
    if accepted {
        measurements.admission_accepted += 1
    } else {
        measurements.admission_rejected += 1
    }
}

pub(crate) fn record_transfer_state(active: usize, queued: usize) {
    let Some(measurements) = transfer_measurements() else {
        return;
    };
    let mut measurements = measurements.lock().unwrap();
    measurements.active = active;
    measurements.queued = queued;
    measurements.active_high_water = measurements.active_high_water.max(active);
    measurements.queued_high_water = measurements.queued_high_water.max(queued);
}

pub(crate) fn record_transfer_outcome() {
    let Some(measurements) = transfer_measurements() else {
        return;
    };
    measurements.lock().unwrap().terminal_outcomes += 1;
}

fn bridge_measurements() -> Option<&'static Mutex<BridgeMeasurements>> {
    static VALUE: OnceLock<Mutex<BridgeMeasurements>> = OnceLock::new();
    configured_path()?;
    Some(VALUE.get_or_init(|| Mutex::new(BridgeMeasurements::default())))
}

/// Owns the Rust-to-WebView measurement boundary. Acknowledgements are sent by
/// the renderer only when the Phase 12/14 probe is enabled; a deliberately
/// stalled renderer therefore leaves these bytes in `retained_bytes`.
pub fn send_bridge_frame(
    channel: &Channel<InvokeResponseBody>,
    frame: Vec<u8>,
) -> Result<(), String> {
    let byte_length = frame.len();
    let started = Instant::now();
    if let Some(measurements) = bridge_measurements() {
        let mut measurements = measurements.lock().unwrap();
        measurements.snapshot.ingress_bytes = measurements
            .snapshot
            .ingress_bytes
            .saturating_add(byte_length as u64);
        measurements.snapshot.retained_bytes = measurements
            .snapshot
            .retained_bytes
            .saturating_add(byte_length as u64);
        measurements.snapshot.retained_bytes_high_water = measurements
            .snapshot
            .retained_bytes_high_water
            .max(measurements.snapshot.retained_bytes);
        measurements
            .pending
            .push_back((byte_length, Instant::now()));
    }
    let result = channel
        .send(InvokeResponseBody::Raw(frame))
        .map_err(|error| format!("desktop event channel closed: {error}"));
    if let Some(measurements) = bridge_measurements() {
        let mut measurements = measurements.lock().unwrap();
        measurements.snapshot.channel_send_count += 1;
        measurements.snapshot.channel_send_bytes = measurements
            .snapshot
            .channel_send_bytes
            .saturating_add(byte_length as u64);
        measurements.snapshot.channel_send_nanos = measurements
            .snapshot
            .channel_send_nanos
            .saturating_add(started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
        if result.is_err() {
            measurements.pending.pop_back();
            measurements.snapshot.retained_bytes = measurements
                .snapshot
                .retained_bytes
                .saturating_sub(byte_length as u64);
        }
    }
    result
}

#[tauri::command]
pub fn acknowledge_bridge_event(byte_length: usize) -> Result<(), String> {
    let Some(measurements) = bridge_measurements() else {
        return Err("performance logging is not enabled for this process".into());
    };
    let mut measurements = measurements.lock().unwrap();
    let Some((expected, started)) = measurements.pending.pop_front() else {
        measurements.snapshot.acknowledgement_mismatches += 1;
        return Err("bridge acknowledgement has no pending frame".into());
    };
    if expected != byte_length {
        measurements.snapshot.acknowledgement_mismatches += 1;
    }
    measurements.snapshot.retained_bytes = measurements
        .snapshot
        .retained_bytes
        .saturating_sub(expected as u64);
    measurements.snapshot.js_admission_count += 1;
    measurements.snapshot.js_admission_lag_nanos = measurements
        .snapshot
        .js_admission_lag_nanos
        .saturating_add(started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
    Ok(())
}

#[tauri::command]
pub fn bridge_measurement_snapshot() -> Result<serde_json::Value, String> {
    let Some(measurements) = bridge_measurements() else {
        return Err("performance logging is not enabled for this process".into());
    };
    serde_json::to_value(&measurements.lock().unwrap().snapshot).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn transfer_measurement_snapshot() -> Result<serde_json::Value, String> {
    let Some(measurements) = transfer_measurements() else {
        return Err("performance logging is not enabled for this process".into());
    };
    serde_json::to_value(&*measurements.lock().unwrap()).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn remote_measurement_snapshot() -> Result<serde_json::Value, String> {
    let Some(measurements) = remote_measurements() else {
        return Err("performance logging is not enabled for this process".into());
    };
    serde_json::to_value(&*measurements.lock().unwrap()).map_err(|error| error.to_string())
}
