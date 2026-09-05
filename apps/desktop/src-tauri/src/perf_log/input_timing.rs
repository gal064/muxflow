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
    time::{Duration, SystemTime, UNIX_EPOCH},
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

pub(crate) struct DesktopInputTiming(Option<Marks>);

impl DesktopInputTiming {
    pub(crate) fn begin(
        pane_id: &str,
        bytes: usize,
        messages: usize,
        oldest_queue: Duration,
        newest_queue: Duration,
    ) -> Self {
        if configured_path().is_none() {
            return Self(None);
        }
        Self(Some(Marks {
            pane_id: pane_id.to_owned(),
            bytes,
            messages,
            oldest_queue,
            newest_queue,
        }))
    }

    pub(crate) fn finish(
        self,
        request_id: u64,
        connection_epoch: u64,
        control_queue: Duration,
        bridge_write: Duration,
        control_queue_depth: usize,
    ) {
        let Some(marks) = self.0 else { return };
        enqueue(serde_json::json!({
            "atUnixMillis": unix_millis(),
            "subsystem": "desktop_native",
            "event": "terminalInput",
            "requestId": request_id,
            "connectionEpoch": connection_epoch,
            "paneId": marks.pane_id,
            "bytes": marks.bytes,
            "messageCount": marks.messages,
            "oldestQueueMs": millis(marks.oldest_queue),
            "newestQueueMs": millis(marks.newest_queue),
            "controlQueueMs": millis(control_queue),
            "bridgeWriteMs": millis(bridge_write),
            "controlQueueDepth": control_queue_depth,
        }));
    }
}

/// Records the instant one terminal-output frame has been decoded from the
/// host stream, before it crosses the native-to-WebView channel. The host log
/// carries the same connection epoch, sequence, pane and generation.
pub(crate) fn record_terminal_output_received(
    connection_epoch: u64,
    sequence: u64,
    pane_id: &str,
    generation: u64,
    bytes: usize,
) {
    if configured_path().is_none() {
        return;
    }
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
