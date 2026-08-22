//! Inert replacement for the measurement modules, compiled only when perf
//! logging is excluded from the binary (release without the `perf-log`
//! feature). Module paths and signatures mirror the real implementation so
//! every call site and the command registration in `lib.rs` stay
//! unconditional. The commands answer exactly like the real implementation
//! does when `ADE_PERF_LOG` is unset.

const DISABLED: &str = "performance logging is not enabled for this process";

pub(crate) mod bridge {
    use serde::Serialize;
    use tauri::ipc::{Channel, InvokeResponseBody};
    use uuid::Uuid;

    #[derive(Debug, Clone, Copy, Default, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct BridgeFinalTotals {
        pub(crate) cumulative_frame_count: u64,
        pub(crate) cumulative_byte_length: u64,
        pub(crate) quiesced: bool,
    }

    #[inline(always)]
    pub(crate) fn send_bridge_frame(
        _measurement_id: Uuid,
        channel: &Channel<InvokeResponseBody>,
        frame: Vec<u8>,
    ) -> Result<(), String> {
        channel
            .send(InvokeResponseBody::Raw(frame))
            .map_err(|error| format!("desktop event channel closed: {error}"))
    }

    #[inline(always)]
    pub(crate) fn quiesce_bridge_measurement(_measurement_id: Uuid) -> BridgeFinalTotals {
        BridgeFinalTotals::default()
    }

    #[tauri::command]
    pub fn acknowledge_bridge_events(
        measurement_id: String,
        cumulative_frame_count: u64,
        cumulative_byte_length: u64,
    ) -> Result<(), String> {
        let _ = (
            measurement_id,
            cumulative_frame_count,
            cumulative_byte_length,
        );
        Err(super::DISABLED.into())
    }

    #[tauri::command]
    pub fn bridge_final_totals(measurement_id: String) -> Result<BridgeFinalTotals, String> {
        let _ = measurement_id;
        Err(super::DISABLED.into())
    }

    #[tauri::command]
    pub fn finalize_bridge_measurement(measurement_id: String) -> Result<(), String> {
        let _ = measurement_id;
        Err(super::DISABLED.into())
    }
}

pub(crate) mod operations {
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

    pub(crate) enum TransferAdmission {
        Accepted,
        Rejected,
    }

    /// Zero-sized: the real per-open segment marks compile out with the rest.
    pub(crate) struct FileOpenTiming;

    impl FileOpenTiming {
        #[inline(always)]
        pub(crate) fn begin(_operation_id: &str) -> Self {
            Self
        }

        #[inline(always)]
        pub(crate) fn inert() -> Self {
            Self
        }

        #[inline(always)]
        pub(crate) fn mark_started(&self) {}

        #[inline(always)]
        pub(crate) fn mark_lease(&self, _reused: bool) {}

        #[inline(always)]
        pub(crate) fn mark_request(&self) {}

        #[inline(always)]
        pub(crate) fn mark_first_byte(&self) {}

        #[inline(always)]
        pub(crate) fn mark_last_byte(&self) {}
    }

    #[inline(always)]
    pub(crate) fn record_remote_operation(_operation: RemoteOperation) {}

    #[inline(always)]
    pub(crate) fn record_transfer_admission(_admission: TransferAdmission) {}

    #[inline(always)]
    pub(crate) fn record_transfer_state(_active: usize, _queued: usize) {}

    #[inline(always)]
    pub(crate) fn record_transfer_completion(_succeeded: bool) {}

    #[tauri::command]
    pub fn sample_native_measurements() -> Result<(), String> {
        Err(super::DISABLED.into())
    }
}

pub(crate) mod sink {
    #[tauri::command]
    pub fn perf_log_enabled() -> bool {
        false
    }

    #[tauri::command]
    pub async fn append_perf_log(lines: Vec<String>) -> Result<(), String> {
        let _ = lines;
        Err(super::DISABLED.into())
    }
}
