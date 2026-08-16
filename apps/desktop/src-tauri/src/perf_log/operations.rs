use serde::Serialize;
use std::{
    sync::{Condvar, Mutex, OnceLock},
    thread,
    time::Duration,
};

use super::sink::{append_body, configured_path};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TransferMeasurementSnapshot {
    pub(crate) admission_attempts: u64,
    pub(crate) admission_accepted: u64,
    pub(crate) admission_rejected: u64,
    pub(crate) active: usize,
    pub(crate) active_high_water: usize,
    pub(crate) queued: usize,
    pub(crate) queued_high_water: usize,
    pub(crate) completed_successes: u64,
    pub(crate) completed_failures: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteMeasurementSnapshot {
    control_master_ensure_attempts: u64,
    control_master_check_attempts: u64,
    control_master_reuse_successes: u64,
    control_master_establishment_successes: u64,
    control_master_failures: u64,
    interactive_bridge_spawn_attempts: u64,
    interactive_bridge_spawn_successes: u64,
    interactive_bridge_spawn_failures: u64,
    bulk_bridge_spawn_attempts: u64,
    bulk_bridge_spawn_successes: u64,
    bulk_bridge_spawn_failures: u64,
}

fn transfer_measurements() -> Option<&'static Mutex<TransferMeasurementSnapshot>> {
    static VALUE: OnceLock<Mutex<TransferMeasurementSnapshot>> = OnceLock::new();
    if !cfg!(test) {
        configured_path()?;
    }
    Some(VALUE.get_or_init(|| Mutex::new(TransferMeasurementSnapshot::default())))
}

fn remote_measurements() -> Option<&'static Mutex<RemoteMeasurementSnapshot>> {
    static VALUE: OnceLock<Mutex<RemoteMeasurementSnapshot>> = OnceLock::new();
    configured_path()?;
    Some(VALUE.get_or_init(|| Mutex::new(RemoteMeasurementSnapshot::default())))
}

pub(crate) enum RemoteOperation {
    ControlMasterEnsureAttempt,
    ControlMasterCheckAttempt,
    ControlMasterReuseSuccess,
    ControlMasterEstablishmentSuccess,
    ControlMasterFailure,
    InteractiveBridgeSpawnAttempt,
    InteractiveBridgeSpawnSuccess,
    InteractiveBridgeSpawnFailure,
    BulkBridgeSpawnAttempt,
    BulkBridgeSpawnSuccess,
    BulkBridgeSpawnFailure,
}

pub(crate) fn record_remote_operation(operation: RemoteOperation) {
    let Some(measurements) = remote_measurements() else {
        return;
    };
    let mut measurements = measurements.lock().unwrap();
    match operation {
        RemoteOperation::ControlMasterEnsureAttempt => {
            measurements.control_master_ensure_attempts += 1
        }
        RemoteOperation::ControlMasterCheckAttempt => {
            measurements.control_master_check_attempts += 1
        }
        RemoteOperation::ControlMasterReuseSuccess => {
            measurements.control_master_reuse_successes += 1
        }
        RemoteOperation::ControlMasterEstablishmentSuccess => {
            measurements.control_master_establishment_successes += 1
        }
        RemoteOperation::ControlMasterFailure => measurements.control_master_failures += 1,
        RemoteOperation::InteractiveBridgeSpawnAttempt => {
            measurements.interactive_bridge_spawn_attempts += 1
        }
        RemoteOperation::InteractiveBridgeSpawnSuccess => {
            measurements.interactive_bridge_spawn_successes += 1
        }
        RemoteOperation::InteractiveBridgeSpawnFailure => {
            measurements.interactive_bridge_spawn_failures += 1
        }
        RemoteOperation::BulkBridgeSpawnAttempt => measurements.bulk_bridge_spawn_attempts += 1,
        RemoteOperation::BulkBridgeSpawnSuccess => measurements.bulk_bridge_spawn_successes += 1,
        RemoteOperation::BulkBridgeSpawnFailure => measurements.bulk_bridge_spawn_failures += 1,
    }
    drop(measurements);
    mark_native_measurements_dirty();
}

pub(crate) enum TransferAdmission {
    Accepted,
    Rejected,
}

pub(crate) fn record_transfer_admission(admission: TransferAdmission) {
    let Some(measurements) = transfer_measurements() else {
        return;
    };
    let mut measurements = measurements.lock().unwrap();
    measurements.admission_attempts += 1;
    match admission {
        TransferAdmission::Accepted => measurements.admission_accepted += 1,
        TransferAdmission::Rejected => measurements.admission_rejected += 1,
    }
    drop(measurements);
    mark_native_measurements_dirty();
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
    drop(measurements);
    mark_native_measurements_dirty();
}

pub(crate) fn record_transfer_completion(succeeded: bool) {
    let Some(measurements) = transfer_measurements() else {
        return;
    };
    let mut measurements = measurements.lock().unwrap();
    if succeeded {
        measurements.completed_successes += 1;
    } else {
        measurements.completed_failures += 1;
    }
    drop(measurements);
    mark_native_measurements_dirty();
}

struct NativeSignal {
    version: Mutex<u64>,
    changed: Condvar,
}

fn native_signal() -> &'static NativeSignal {
    static SIGNAL: OnceLock<NativeSignal> = OnceLock::new();
    SIGNAL.get_or_init(|| NativeSignal {
        version: Mutex::new(0),
        changed: Condvar::new(),
    })
}

pub(super) fn mark_native_measurements_dirty() {
    if configured_path().is_none() {
        return;
    }
    let signal = native_signal();
    let mut version = signal.version.lock().unwrap();
    *version = version.saturating_add(1);
    signal.changed.notify_one();
}

#[tauri::command]
pub fn sample_native_measurements() -> Result<(), String> {
    write_native_snapshot()
}

fn write_native_snapshot() -> Result<(), String> {
    let Some(path) = configured_path() else {
        return Err("performance logging is not enabled for this process".into());
    };
    let bridge = super::bridge::measurement_snapshot();
    let transfer = transfer_measurements()
        .map(|value| value.lock().unwrap().clone())
        .unwrap_or_default();
    let remote = remote_measurements()
        .map(|value| value.lock().unwrap().clone())
        .unwrap_or_default();
    let record = serde_json::json!({
        "t": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        "kind": "nativeOperations",
        "bridge": bridge,
        "transfer": transfer,
        "remote": remote,
    });
    append_body(path, &record.to_string())
}

pub(super) fn start_native_measurement_writer() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let _ = thread::Builder::new()
            .name("phase14-native-measurements".into())
            .spawn(|| {
                let signal = native_signal();
                let mut emitted_version = 0_u64;
                let mut retry_delay = Duration::from_secs(2);
                loop {
                    let mut version = signal.version.lock().unwrap();
                    while *version == emitted_version {
                        version = signal.changed.wait(version).unwrap();
                    }
                    drop(version);
                    // Coalesce a burst without waking at all during true idle.
                    thread::sleep(Duration::from_secs(2));
                    let version = *signal.version.lock().unwrap();
                    if write_native_snapshot().is_err() {
                        // Keep the dirty version pending. A transient full disk
                        // or interrupted append must not permanently kill the
                        // sole writer and silently lose every later snapshot.
                        thread::sleep(retry_delay);
                        retry_delay = (retry_delay * 2).min(Duration::from_secs(30));
                        continue;
                    }
                    emitted_version = version;
                    retry_delay = Duration::from_secs(2);
                }
            });
    });
}

#[cfg(test)]
pub(crate) fn reset_transfer_measurements() {
    *transfer_measurements().unwrap().lock().unwrap() = TransferMeasurementSnapshot::default();
}

#[cfg(test)]
pub(crate) fn transfer_measurements_for_test() -> TransferMeasurementSnapshot {
    transfer_measurements().unwrap().lock().unwrap().clone()
}
