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

/// Zero-cost twin of the native input timeline. The real type owns the pane
/// label and clock stamps only in an opted-in measurement build.
pub(crate) mod input_timing {
    use std::time::Duration;

    pub(crate) struct DesktopInputTiming;

    impl DesktopInputTiming {
        #[inline(always)]
        pub(crate) fn begin(
            _pane_id: &str,
            _bytes: usize,
            _messages: usize,
            _oldest_queue: Duration,
            _newest_queue: Duration,
        ) -> Self {
            Self
        }

        #[inline(always)]
        pub(crate) fn finish(
            self,
            _request_id: u64,
            _connection_epoch: u64,
            _control_queue: Duration,
            _bridge_write: Duration,
            _control_queue_depth: usize,
        ) {
        }
    }

    #[inline(always)]
    pub(crate) fn record_terminal_output_received(
        _connection_epoch: u64,
        _sequence: u64,
        _pane_id: &str,
        _generation: u64,
        _bytes: usize,
    ) {
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

/// Inert twin of the switch-timeline instrumentation. Same paths and
/// signatures, no clocks, no counters, and `finish` never produces a record —
/// so `tmux_action` answers without a `timing` object and the renderer writes
/// no `perf.timeline` line.
pub(crate) mod switch_timing {
    use std::io::Read;

    #[derive(Clone, Copy)]
    pub(crate) struct AnswerMark;

    #[derive(Default)]
    pub(crate) struct LinkCounters;

    impl LinkCounters {
        #[inline(always)]
        pub(crate) fn new() -> Self {
            Self
        }

        #[inline(always)]
        pub(crate) fn bytes_read(&self) -> u64 {
            0
        }

        #[inline(always)]
        pub(crate) fn note_frame_read(
            &self,
            _frame: &tmux_agent_protocol::v1::Envelope,
            _bytes_before: u64,
        ) -> Option<AnswerMark> {
            None
        }

        #[inline(always)]
        pub(crate) fn totals(&self) -> Option<(u64, u64)> {
            None
        }
    }

    pub(crate) struct CountingReader<'a, R> {
        inner: R,
        counters: &'a LinkCounters,
    }

    impl<'a, R: Read> CountingReader<'a, R> {
        #[inline(always)]
        pub(crate) fn new(inner: R, counters: &'a LinkCounters) -> Self {
            Self { inner, counters }
        }
    }

    impl<R: Read> Read for CountingReader<'_, R> {
        #[inline(always)]
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let _ = self.counters;
            self.inner.read(buffer)
        }
    }

    pub(crate) struct RequestTiming;

    impl RequestTiming {
        #[inline(always)]
        pub(crate) fn begin() -> Self {
            Self
        }

        #[inline(always)]
        pub(crate) fn inert() -> Self {
            Self
        }

        #[inline(always)]
        pub(crate) fn mark_written(
            &mut self,
            _request_id: u64,
            _counters: &LinkCounters,
            _in_flight: impl FnOnce() -> Option<u64>,
        ) {
        }

        #[inline(always)]
        pub(crate) fn mark_answer(&mut self, _answer: Option<AnswerMark>) {}

        #[inline(always)]
        pub(crate) fn finish(self) -> Option<serde_json::Value> {
            None
        }
    }
}
