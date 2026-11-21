//! Opt-in native half of the terminal-input timeline.
//!
//! One record follows a coalesced input batch from admission to the desktop
//! queue through the physical write into the bridge. `requestId` is the same
//! envelope id the host logs, while `connectionEpoch` prevents ids reused by a
//! reconnect from joining. Persistence runs on a bounded background queue so
//! measuring a keystroke never performs file I/O on the input dispatcher.

use std::{
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

use super::sink::{append_body, configured_path};

const INPUT_LOG_QUEUE: usize = 4_096;
static DROPPED: AtomicU64 = AtomicU64::new(0);

struct Marks {
    pane_id: String,
    bytes: usize,
    messages: usize,
    oldest_queue: Duration,
    newest_queue: Duration,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ControlWriteTiming {
    pub(crate) queue_wait: Duration,
    pub(crate) physical_write: Duration,
    pub(crate) queue_depth_at_enqueue: usize,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct InputConnectionEpoch(u64);

impl InputConnectionEpoch {
    pub(crate) fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug)]
pub(crate) struct DispatchedInput {
    request_id: u64,
    connection_epoch: InputConnectionEpoch,
    control: ControlWriteTiming,
}

impl DispatchedInput {
    pub(crate) fn new(
        request_id: u64,
        connection_epoch: InputConnectionEpoch,
        control: ControlWriteTiming,
    ) -> Self {
        Self {
            request_id,
            connection_epoch,
            control,
        }
    }
}

pub(crate) struct DesktopInputTiming(Option<Marks>);

impl DesktopInputTiming {
    pub(crate) fn begin(
        pane_id: &str,
        bytes: usize,
        messages: usize,
        enqueued_at: Instant,
        coalesced_at: &[Instant],
    ) -> Self {
        if configured_path().is_none() {
            return Self(None);
        }
        let sampled_at = Instant::now();
        let oldest_queue = std::iter::once(enqueued_at)
            .chain(coalesced_at.iter().copied())
            .map(|queued_at| sampled_at.saturating_duration_since(queued_at))
            .max()
            .unwrap_or_default();
        let newest_queue = std::iter::once(enqueued_at)
            .chain(coalesced_at.iter().copied())
            .map(|queued_at| sampled_at.saturating_duration_since(queued_at))
            .min()
            .unwrap_or_default();
        Self(Some(Marks {
            pane_id: pane_id.to_owned(),
            bytes,
            messages,
            oldest_queue,
            newest_queue,
        }))
    }

    pub(crate) fn finish(self, dispatched: DispatchedInput) {
        let Some(marks) = self.0 else { return };
        enqueue(serde_json::json!({
            "atUnixMillis": unix_millis(),
            "subsystem": "desktop_native",
            "event": "terminalInput",
            "requestId": dispatched.request_id,
            "connectionEpoch": dispatched.connection_epoch.0,
            "paneId": marks.pane_id,
            "bytes": marks.bytes,
            "messageCount": marks.messages,
            "oldestQueueMs": millis(marks.oldest_queue),
            "newestQueueMs": millis(marks.newest_queue),
            "controlQueueMs": millis(dispatched.control.queue_wait),
            "bridgeWriteMs": millis(dispatched.control.physical_write),
            "controlQueueDepth": dispatched.control.queue_depth_at_enqueue,
        }));
    }
}

/// Records the instant one terminal-output frame has been decoded from the
/// host stream, before it crosses the native-to-WebView channel. The host log
/// carries the same connection epoch, sequence, pane and generation.
pub(crate) fn record_terminal_output_received(
    connection_epoch: impl FnOnce() -> u64,
    sequence: u64,
    pane_id: &str,
    generation: u64,
    bytes: usize,
) {
    if configured_path().is_none() {
        return;
    }
    let connection_epoch = connection_epoch();
    enqueue(serde_json::json!({
        "atUnixMillis": unix_millis(),
        "subsystem": "desktop_native",
        "event": "terminalOutput",
        "connectionEpoch": connection_epoch,
        "sequence": sequence,
        "paneId": pane_id,
        "generation": generation,
        "payloadBytes": bytes,
    }));
}

fn sender() -> Option<&'static mpsc::SyncSender<Value>> {
    static SENDER: OnceLock<Option<mpsc::SyncSender<Value>>> = OnceLock::new();
    SENDER
        .get_or_init(|| {
            configured_path()?;
            let (sender, receiver) = mpsc::sync_channel::<Value>(INPUT_LOG_QUEUE);
            thread::Builder::new()
                .name("perf-terminal-input-writer".into())
                .spawn(move || {
                    while let Ok(value) = receiver.recv() {
                        if let Some(path) = configured_path() {
                            let _ = append_body(path, &value.to_string());
                        }
                    }
                })
                .ok()?;
            Some(sender)
        })
        .as_ref()
}

fn enqueue(mut value: Value) {
    let Some(sender) = sender() else { return };
    let dropped_before = DROPPED.swap(0, Ordering::AcqRel);
    value["droppedBefore"] = dropped_before.into();
    if sender.try_send(value).is_err() {
        DROPPED.fetch_add(dropped_before.saturating_add(1), Ordering::Relaxed);
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_keep_sub_millisecond_resolution() {
        assert_eq!(millis(Duration::from_micros(375)), 0.375);
    }
}
