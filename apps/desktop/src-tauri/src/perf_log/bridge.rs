use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};
use tauri::ipc::{Channel, InvokeResponseBody};
use uuid::Uuid;

use super::{operations::mark_native_measurements_dirty, sink::configured_path};

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BridgeMeasurementSnapshot {
    ingress_bytes: u64,
    channel_send_attempts: u64,
    channel_send_successes: u64,
    channel_send_success_bytes: u64,
    channel_send_success_nanos: u64,
    channel_send_failures: u64,
    js_acknowledgement_count: u64,
    js_admission_lag_sample_count: u64,
    js_admission_lag_nanos: u64,
    retained_bytes: u64,
    retained_bytes_high_water: u64,
    closed_with_outstanding_bytes: u64,
    acknowledgement_mismatches: u64,
}

#[derive(Debug, Default)]
struct BridgeMeasurements {
    snapshot: BridgeMeasurementSnapshot,
    channels: HashMap<Uuid, Arc<Mutex<BridgePending>>>,
    closed_channels: VecDeque<ClosedBridge>,
}

#[derive(Debug)]
struct ClosedBridge {
    measurement_id: Uuid,
    pending: Arc<Mutex<BridgePending>>,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BridgeFinalTotals {
    pub(crate) cumulative_frame_count: u64,
    pub(crate) cumulative_byte_length: u64,
    pub(crate) quiesced: bool,
}

#[derive(Debug, Default)]
struct BridgePending {
    frame_count: u64,
    retained_bytes: u64,
    sampled_starts: VecDeque<Instant>,
    unsampled_frames: u64,
    admitted_frames_total: u64,
    admitted_bytes_total: u64,
    acknowledged_frames_total: u64,
    acknowledged_bytes_total: u64,
    closed: bool,
}

#[derive(Debug, Default)]
struct AdmissionBatch {
    sampled_count: u64,
    lag_nanos: u64,
}

const MAX_ADMISSION_LAG_SAMPLES_PER_CHANNEL: usize = 1_024;
const MAX_CLOSED_BRIDGE_TOMBSTONES: usize = 256;

impl BridgePending {
    fn admit(&mut self, byte_length: usize) -> Result<(), &'static str> {
        if self.closed {
            return Err("bridge measurement channel is closed");
        }
        self.frame_count = self.frame_count.saturating_add(1);
        self.retained_bytes = self.retained_bytes.saturating_add(byte_length as u64);
        self.admitted_frames_total = self.admitted_frames_total.saturating_add(1);
        self.admitted_bytes_total = self.admitted_bytes_total.saturating_add(byte_length as u64);
        if self.unsampled_frames == 0
            && self.sampled_starts.len() < MAX_ADMISSION_LAG_SAMPLES_PER_CHANNEL
        {
            self.sampled_starts.push_back(Instant::now());
        } else {
            self.unsampled_frames = self.unsampled_frames.saturating_add(1);
        }
        Ok(())
    }

    fn rollback(&mut self, byte_length: usize) {
        self.frame_count = self.frame_count.saturating_sub(1);
        self.retained_bytes = self.retained_bytes.saturating_sub(byte_length as u64);
        self.admitted_frames_total = self.admitted_frames_total.saturating_sub(1);
        self.admitted_bytes_total = self.admitted_bytes_total.saturating_sub(byte_length as u64);
        if self.unsampled_frames > 0 {
            self.unsampled_frames -= 1
        } else {
            self.sampled_starts.pop_back();
        }
    }

    fn acknowledge_delta(
        &mut self,
        frame_count: u64,
        byte_length: u64,
    ) -> Result<AdmissionBatch, &'static str> {
        if frame_count > self.frame_count || byte_length > self.retained_bytes {
            return Err("bridge acknowledgement exceeds pending channel data");
        }
        let sampled_count = frame_count.min(self.sampled_starts.len() as u64);
        let mut lag_nanos = 0_u64;
        for _ in 0..sampled_count {
            let started = self.sampled_starts.pop_front().unwrap();
            lag_nanos = lag_nanos
                .saturating_add(started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
        }
        let unsampled_count = frame_count - sampled_count;
        if unsampled_count > self.unsampled_frames {
            return Err("bridge acknowledgement crossed the sampled channel boundary");
        }
        self.unsampled_frames -= unsampled_count;
        self.frame_count -= frame_count;
        self.retained_bytes -= byte_length;
        Ok(AdmissionBatch {
            sampled_count,
            lag_nanos,
        })
    }

    fn acknowledge_cumulative(
        &mut self,
        frame_count: u64,
        byte_length: u64,
    ) -> Result<AdmissionBatch, &'static str> {
        if frame_count < self.acknowledged_frames_total
            || byte_length < self.acknowledged_bytes_total
            || frame_count > self.admitted_frames_total
            || byte_length > self.admitted_bytes_total
        {
            return Err("bridge cumulative acknowledgement is outside admitted data");
        }
        let batch = self.acknowledge_delta(
            frame_count - self.acknowledged_frames_total,
            byte_length - self.acknowledged_bytes_total,
        )?;
        self.acknowledged_frames_total = frame_count;
        self.acknowledged_bytes_total = byte_length;
        Ok(batch)
    }

    fn close(&mut self) -> u64 {
        self.closed = true;
        let released_bytes = self.retained_bytes;
        self.frame_count = 0;
        self.retained_bytes = 0;
        self.sampled_starts.clear();
        self.unsampled_frames = 0;
        released_bytes
    }
}

fn bridge_measurements() -> Option<&'static Mutex<BridgeMeasurements>> {
    static VALUE: OnceLock<Mutex<BridgeMeasurements>> = OnceLock::new();
    configured_path()?;
    Some(VALUE.get_or_init(|| Mutex::new(BridgeMeasurements::default())))
}

fn bridge_pending(measurement_id: Uuid) -> Option<Arc<Mutex<BridgePending>>> {
    let measurements = bridge_measurements()?;
    let mut measurements = measurements.lock().unwrap();
    if let Some(pending) = measurements.channels.get(&measurement_id) {
        return Some(Arc::clone(pending));
    }
    if let Some(closed) = measurements
        .closed_channels
        .iter()
        .find(|closed| closed.measurement_id == measurement_id)
    {
        return Some(Arc::clone(&closed.pending));
    }
    let pending = Arc::new(Mutex::new(BridgePending::default()));
    measurements
        .channels
        .insert(measurement_id, Arc::clone(&pending));
    Some(pending)
}

pub(super) fn measurement_snapshot() -> BridgeMeasurementSnapshot {
    bridge_measurements()
        .map(|value| value.lock().unwrap().snapshot.clone())
        .unwrap_or_default()
}

/// Sends directly with no clock, allocation, or measurement lock unless the
/// process explicitly opted into the Phase 14 sink.
pub(crate) fn send_bridge_frame(
    measurement_id: Uuid,
    channel: &Channel<InvokeResponseBody>,
    frame: Vec<u8>,
) -> Result<(), String> {
    let Some(pending) = bridge_pending(measurement_id) else {
        return channel
            .send(InvokeResponseBody::Raw(frame))
            .map_err(|error| format!("desktop event channel closed: {error}"));
    };
    let byte_length = frame.len();
    let started = Instant::now();
    let mut pending = pending.lock().unwrap();
    pending.admit(byte_length).map_err(str::to_owned)?;
    {
        let measurements = bridge_measurements().unwrap();
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
    }
    let result = channel
        .send(InvokeResponseBody::Raw(frame))
        .map_err(|error| format!("desktop event channel closed: {error}"));
    let measurements = bridge_measurements().unwrap();
    let mut measurements = measurements.lock().unwrap();
    measurements.snapshot.channel_send_attempts = measurements
        .snapshot
        .channel_send_attempts
        .saturating_add(1);
    if result.is_ok() {
        measurements.snapshot.channel_send_successes = measurements
            .snapshot
            .channel_send_successes
            .saturating_add(1);
        measurements.snapshot.channel_send_success_bytes = measurements
            .snapshot
            .channel_send_success_bytes
            .saturating_add(byte_length as u64);
        measurements.snapshot.channel_send_success_nanos = measurements
            .snapshot
            .channel_send_success_nanos
            .saturating_add(started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
    } else {
        measurements.snapshot.channel_send_failures = measurements
            .snapshot
            .channel_send_failures
            .saturating_add(1);
        pending.rollback(byte_length);
        measurements.snapshot.retained_bytes = measurements
            .snapshot
            .retained_bytes
            .saturating_sub(byte_length as u64);
    }
    drop(measurements);
    drop(pending);
    mark_native_measurements_dirty();
    result
}

#[tauri::command]
pub fn acknowledge_bridge_events(
    measurement_id: String,
    cumulative_frame_count: u64,
    cumulative_byte_length: u64,
) -> Result<(), String> {
    let measurement_id =
        Uuid::parse_str(&measurement_id).map_err(|_| "invalid terminal measurement ID")?;
    let Some(measurements) = bridge_measurements() else {
        return Err("performance logging is not enabled for this process".into());
    };
    if cumulative_frame_count == 0 {
        if cumulative_byte_length == 0 {
            return Ok(());
        }
        let mut measurements = measurements.lock().unwrap();
        measurements.snapshot.acknowledgement_mismatches = measurements
            .snapshot
            .acknowledgement_mismatches
            .saturating_add(1);
        return Err("zero-frame acknowledgement cannot retain bytes".into());
    }
    let pending_state = {
        let measurements = measurements.lock().unwrap();
        measurements
            .channels
            .get(&measurement_id)
            .cloned()
            .or_else(|| {
                measurements
                    .closed_channels
                    .iter()
                    .find(|closed| closed.measurement_id == measurement_id)
                    .map(|closed| Arc::clone(&closed.pending))
            })
    };
    let Some(pending) = pending_state else {
        let mut measurements = measurements.lock().unwrap();
        measurements.snapshot.acknowledgement_mismatches = measurements
            .snapshot
            .acknowledgement_mismatches
            .saturating_add(1);
        return Err("bridge acknowledgement has no matching channel".into());
    };
    let mut pending = pending.lock().unwrap();
    let previous_frames = pending.acknowledged_frames_total;
    let previous_bytes = pending.acknowledged_bytes_total;
    let batch = match pending.acknowledge_cumulative(cumulative_frame_count, cumulative_byte_length)
    {
        Ok(batch) => batch,
        Err(error) => {
            drop(pending);
            let mut measurements = measurements.lock().unwrap();
            measurements.snapshot.acknowledgement_mismatches = measurements
                .snapshot
                .acknowledgement_mismatches
                .saturating_add(1);
            return Err(error.into());
        }
    };
    let frame_count = cumulative_frame_count - previous_frames;
    let byte_length = cumulative_byte_length - previous_bytes;
    drop(pending);
    let mut measurements = measurements.lock().unwrap();
    apply_acknowledgement(&mut measurements.snapshot, frame_count, byte_length, batch);
    drop(measurements);
    mark_native_measurements_dirty();
    Ok(())
}

fn apply_acknowledgement(
    snapshot: &mut BridgeMeasurementSnapshot,
    frame_count: u64,
    byte_length: u64,
    batch: AdmissionBatch,
) {
    snapshot.retained_bytes = snapshot.retained_bytes.saturating_sub(byte_length);
    snapshot.js_acknowledgement_count = snapshot
        .js_acknowledgement_count
        .saturating_add(frame_count);
    snapshot.js_admission_lag_sample_count = snapshot
        .js_admission_lag_sample_count
        .saturating_add(batch.sampled_count);
    snapshot.js_admission_lag_nanos = snapshot
        .js_admission_lag_nanos
        .saturating_add(batch.lag_nanos);
}

pub(crate) fn quiesce_bridge_measurement(measurement_id: Uuid) -> BridgeFinalTotals {
    let Some(measurements) = bridge_measurements() else {
        return BridgeFinalTotals::default();
    };
    let pending_state = {
        let measurements = measurements.lock().unwrap();
        measurements
            .channels
            .get(&measurement_id)
            .cloned()
            .or_else(|| {
                measurements
                    .closed_channels
                    .iter()
                    .find(|closed| closed.measurement_id == measurement_id)
                    .map(|closed| Arc::clone(&closed.pending))
            })
    };
    // A measured channel that emitted no frames still owns an explicit empty
    // lifecycle so the renderer can finalize it instead of receiving a false
    // "not quiesced" error.
    let pending_state =
        pending_state.unwrap_or_else(|| Arc::new(Mutex::new(BridgePending::default())));
    let mut pending = pending_state.lock().unwrap();
    pending.closed = true;
    let totals = BridgeFinalTotals {
        cumulative_frame_count: pending.admitted_frames_total,
        cumulative_byte_length: pending.admitted_bytes_total,
        quiesced: true,
    };
    drop(pending);
    let expired = {
        let mut measurements = measurements.lock().unwrap();
        measurements.channels.remove(&measurement_id);
        if !measurements
            .closed_channels
            .iter()
            .any(|closed| closed.measurement_id == measurement_id)
        {
            measurements.closed_channels.push_back(ClosedBridge {
                measurement_id,
                pending: Arc::clone(&pending_state),
            });
        }
        (measurements.closed_channels.len() > MAX_CLOSED_BRIDGE_TOMBSTONES)
            .then(|| measurements.closed_channels.pop_front())
            .flatten()
    };
    if let Some(expired) = expired {
        let released_bytes = expired.pending.lock().unwrap().close();
        let mut measurements = measurements.lock().unwrap();
        measurements.snapshot.retained_bytes = measurements
            .snapshot
            .retained_bytes
            .saturating_sub(released_bytes);
        measurements.snapshot.closed_with_outstanding_bytes = measurements
            .snapshot
            .closed_with_outstanding_bytes
            .saturating_add(released_bytes);
    }
    mark_native_measurements_dirty();
    totals
}

#[tauri::command]
pub fn bridge_final_totals(measurement_id: String) -> Result<BridgeFinalTotals, String> {
    let measurement_id =
        Uuid::parse_str(&measurement_id).map_err(|_| "invalid terminal measurement ID")?;
    let Some(measurements) = bridge_measurements() else {
        return Err("performance logging is not enabled for this process".into());
    };
    let pending = {
        let measurements = measurements.lock().unwrap();
        measurements
            .closed_channels
            .iter()
            .find(|closed| closed.measurement_id == measurement_id)
            .map(|closed| Arc::clone(&closed.pending))
    };
    let Some(pending) = pending else {
        return Ok(BridgeFinalTotals::default());
    };
    let pending = pending.lock().unwrap();
    Ok(BridgeFinalTotals {
        cumulative_frame_count: pending.admitted_frames_total,
        cumulative_byte_length: pending.admitted_bytes_total,
        quiesced: pending.closed,
    })
}

#[tauri::command]
pub fn finalize_bridge_measurement(measurement_id: String) -> Result<(), String> {
    let measurement_id =
        Uuid::parse_str(&measurement_id).map_err(|_| "invalid terminal measurement ID")?;
    let Some(measurements) = bridge_measurements() else {
        return Err("performance logging is not enabled for this process".into());
    };
    let pending = {
        let mut measurements = measurements.lock().unwrap();
        let Some(index) = measurements
            .closed_channels
            .iter()
            .position(|closed| closed.measurement_id == measurement_id)
        else {
            return Err("bridge measurement has not been quiesced".into());
        };
        measurements.closed_channels.remove(index).unwrap().pending
    };
    let released_bytes = pending.lock().unwrap().close();
    let mut measurements = measurements.lock().unwrap();
    measurements.snapshot.retained_bytes = measurements
        .snapshot
        .retained_bytes
        .saturating_sub(released_bytes);
    measurements.snapshot.closed_with_outstanding_bytes = measurements
        .snapshot
        .closed_with_outstanding_bytes
        .saturating_add(released_bytes);
    drop(measurements);
    mark_native_measurements_dirty();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lag_metadata_is_bounded_and_cumulative_acknowledgements_are_idempotent() {
        let mut pending = BridgePending::default();
        for _ in 0..10_000 {
            pending.admit(17).unwrap();
        }
        assert_eq!(
            pending.sampled_starts.len(),
            MAX_ADMISSION_LAG_SAMPLES_PER_CHANNEL
        );
        assert_eq!(pending.unsampled_frames, 8_976);
        let batch = pending.acknowledge_cumulative(10_000, 170_000).unwrap();
        assert_eq!(batch.sampled_count, 1_024);
        let duplicate = pending.acknowledge_cumulative(10_000, 170_000).unwrap();
        assert_eq!(duplicate.sampled_count, 0);
        assert_eq!(pending.frame_count, 0);
        assert_eq!(pending.retained_bytes, 0);
    }

    #[test]
    fn failed_send_rolls_back_only_that_channel_tail() {
        let mut pending = BridgePending::default();
        pending.admit(11).unwrap();
        pending.admit(13).unwrap();
        pending.rollback(13);
        assert_eq!(pending.frame_count, 1);
        assert_eq!(pending.retained_bytes, 11);
        assert_eq!(pending.sampled_starts.len(), 1);
    }

    #[test]
    fn cumulative_acknowledgement_can_finish_after_channel_close() {
        let mut pending = BridgePending::default();
        pending.admit(11).unwrap();
        pending.admit(13).unwrap();
        pending.acknowledge_cumulative(1, 11).unwrap();
        pending.closed = true;
        let final_batch = pending.acknowledge_cumulative(2, 24).unwrap();
        assert_eq!(final_batch.sampled_count, 1);
        assert_eq!(pending.frame_count, 0);
        assert_eq!(pending.retained_bytes, 0);
    }

    #[test]
    fn acknowledgement_handle_keeps_the_same_state_when_ownership_is_quiesced() {
        let active = Arc::new(Mutex::new(BridgePending::default()));
        active.lock().unwrap().admit(13).unwrap();
        let acknowledgement_handle = Arc::clone(&active);

        active.lock().unwrap().closed = true;
        let tombstone = ClosedBridge {
            measurement_id: Uuid::new_v4(),
            pending: Arc::clone(&active),
        };
        acknowledgement_handle
            .lock()
            .unwrap()
            .acknowledge_cumulative(1, 13)
            .unwrap();

        let pending = tombstone.pending.lock().unwrap();
        assert_eq!(pending.acknowledged_frames_total, 1);
        assert_eq!(pending.retained_bytes, 0);
    }
}
