use serde::Serialize;
use std::{
    sync::{Condvar, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
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

/// One editor open's native segment marks, correlated by the operation id the
/// stream frames already carry (the transfer id the renderer holds too).
///
/// Marks accumulate in memory and are appended as one batch when the job is
/// dropped, so measurement never inserts an I/O pause between the segments it
/// is measuring — and a failed or cancelled open still reports every segment
/// it completed, which is exactly the record that says where it stopped.
pub(crate) struct FileOpenTiming(Option<Mutex<FileOpenMarks>>);

struct FileOpenMarks {
    operation_id: String,
    /// The renderer's invoke reached native: the moment the job was enqueued.
    enqueued: Instant,
    /// The scheduler dispatched the job and its worker began running.
    started: Option<Instant>,
    /// A `BulkLease` was acquired, and whether it was idle-pool reuse.
    lease: Option<Instant>,
    lease_reused: bool,
    /// The `OpenFileStream` request was handed to the bridge for dispatch.
    request: Option<Instant>,
    /// The first body frame arrived. Never set for a metadata-only open.
    first_byte: Option<Instant>,
    /// The final (EOF) body frame arrived.
    last_byte: Option<Instant>,
}

impl FileOpenTiming {
    /// Starts a measured open. Inert unless the process opted into the log,
    /// except under test, where marks are recorded without a sink to write to.
    pub(crate) fn begin(operation_id: &str) -> Self {
        if !cfg!(test) && configured_path().is_none() {
            return Self(None);
        }
        Self(Some(Mutex::new(FileOpenMarks {
            operation_id: operation_id.to_owned(),
            enqueued: Instant::now(),
            started: None,
            lease: None,
            lease_reused: false,
            request: None,
            first_byte: None,
            last_byte: None,
        })))
    }

    /// A job that is not an editor open records nothing.
    pub(crate) fn inert() -> Self {
        Self(None)
    }

    fn mark(&self, apply: impl FnOnce(&mut FileOpenMarks)) {
        if let Some(marks) = &self.0 {
            apply(&mut marks.lock().unwrap());
        }
    }

    pub(crate) fn mark_started(&self) {
        self.mark(|marks| {
            marks.started.get_or_insert_with(Instant::now);
        });
    }

    pub(crate) fn mark_lease(&self, reused: bool) {
        self.mark(|marks| {
            if marks.lease.is_none() {
                marks.lease = Some(Instant::now());
                marks.lease_reused = reused;
            }
        });
    }

    pub(crate) fn mark_request(&self) {
        self.mark(|marks| {
            marks.request.get_or_insert_with(Instant::now);
        });
    }

    pub(crate) fn mark_first_byte(&self) {
        self.mark(|marks| {
            marks.first_byte.get_or_insert_with(Instant::now);
        });
    }

    pub(crate) fn mark_last_byte(&self) {
        self.mark(|marks| {
            marks.last_byte.get_or_insert_with(Instant::now);
        });
    }
}

impl Drop for FileOpenTiming {
    fn drop(&mut self) {
        let Some(marks) = self.0.take() else {
            return;
        };
        let Some(path) = configured_path() else {
            return;
        };
        let Ok(marks) = marks.into_inner() else {
            return;
        };
        let records = file_open_segment_records(
            &marks,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        );
        if records.is_empty() {
            return;
        }
        let _ = append_body(path, &records.join("\n"));
    }
}

/// One would-be segment: its name, its two endpoints, and the lease-reuse tag
/// only the lease segment carries.
type FileOpenSegmentSpec = (&'static str, Option<Instant>, Option<Instant>, Option<bool>);

/// Renders one open's completed segments, in the renderer sample shape
/// (`t`/`name`/`ms`) plus the correlation the join needs. Each segment exists
/// only when both of its endpoints were reached, so a stalled open reports the
/// prefix it earned and nothing invented.
fn file_open_segment_records(marks: &FileOpenMarks, timestamp: u128) -> Vec<String> {
    let segments: [FileOpenSegmentSpec; 5] = [
        (
            "file.open.segment.queueWait",
            Some(marks.enqueued),
            marks.started,
            None,
        ),
        (
            "file.open.segment.lease",
            marks.started,
            marks.lease,
            Some(marks.lease_reused),
        ),
        (
            "file.open.segment.leaseToRequest",
            marks.lease,
            marks.request,
            None,
        ),
        (
            "file.open.segment.requestToFirstByte",
            marks.request,
            marks.first_byte,
            None,
        ),
        (
            "file.open.segment.transfer",
            marks.first_byte,
            marks.last_byte,
            None,
        ),
    ];
    segments
        .into_iter()
        .filter_map(|(name, from, to, lease_reuse)| {
            let (from, to) = (from?, to?);
            // The renderer rounds sample milliseconds to three decimals; the
            // native segments match so the log stays one series.
            let ms = (to.duration_since(from).as_secs_f64() * 1_000_000.0).round() / 1_000.0;
            let mut record = serde_json::json!({
                "t": timestamp,
                "kind": "segment",
                "name": name,
                "operationId": marks.operation_id,
                "ms": ms,
            });
            if let Some(reused) = lease_reuse {
                record["leaseReuse"] = reused.into();
            }
            Some(record.to_string())
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn marks() -> FileOpenMarks {
        let enqueued = Instant::now();
        FileOpenMarks {
            operation_id: "open-1".into(),
            enqueued,
            started: Some(enqueued + Duration::from_millis(5)),
            lease: Some(enqueued + Duration::from_millis(25)),
            lease_reused: true,
            request: Some(enqueued + Duration::from_millis(26)),
            first_byte: Some(enqueued + Duration::from_millis(60)),
            last_byte: Some(enqueued + Duration::from_millis(110)),
        }
    }

    /// The whole point of the segments: one open decomposes into named,
    /// joinable durations that carry the operation id the frames already have.
    #[test]
    fn a_completed_open_reports_every_segment_under_its_operation_id() {
        let records = file_open_segment_records(&marks(), 7);
        let parsed = records
            .iter()
            .map(|record| serde_json::from_str::<serde_json::Value>(record).unwrap())
            .collect::<Vec<_>>();
        let expected = [
            ("file.open.segment.queueWait", 5.0),
            ("file.open.segment.lease", 20.0),
            ("file.open.segment.leaseToRequest", 1.0),
            ("file.open.segment.requestToFirstByte", 34.0),
            ("file.open.segment.transfer", 50.0),
        ];
        assert_eq!(parsed.len(), expected.len());
        for (record, (name, ms)) in parsed.iter().zip(expected) {
            assert_eq!(record["name"], name);
            assert_eq!(record["kind"], "segment");
            assert_eq!(record["operationId"], "open-1");
            assert_eq!(record["t"], 7);
            assert!(
                (record["ms"].as_f64().unwrap() - ms).abs() < 0.001,
                "{name} reported {}",
                record["ms"]
            );
        }
        // The lease segment alone carries the reuse-versus-fresh-spawn tag.
        assert_eq!(parsed[1]["leaseReuse"], true);
        assert!(parsed[0].get("leaseReuse").is_none());
    }

    /// A stalled open reports the prefix it earned: segments whose endpoint
    /// was never reached are absent, not invented as zeros.
    #[test]
    fn an_open_that_stopped_early_reports_only_the_segments_it_completed() {
        let mut stalled = marks();
        stalled.first_byte = None;
        stalled.last_byte = None;
        let names = file_open_segment_records(&stalled, 7)
            .iter()
            .map(|record| {
                serde_json::from_str::<serde_json::Value>(record).unwrap()["name"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "file.open.segment.queueWait",
                "file.open.segment.lease",
                "file.open.segment.leaseToRequest",
            ]
        );
    }

    /// The marks a job's phases set through the shared handle land in order,
    /// and only their first observation counts.
    #[test]
    fn timing_marks_are_first_write_wins_and_ordered() {
        let timing = FileOpenTiming::begin("open-2");
        timing.mark_started();
        timing.mark_lease(false);
        let first_lease = timing.0.as_ref().unwrap().lock().unwrap().lease;
        timing.mark_lease(true);
        let marks = timing.0.as_ref().unwrap().lock().unwrap();
        assert_eq!(marks.lease, first_lease);
        assert!(!marks.lease_reused, "a later mark rewrote the lease origin");
        assert!(marks.started.unwrap() >= marks.enqueued);
        assert!(marks.request.is_none());
    }
}
