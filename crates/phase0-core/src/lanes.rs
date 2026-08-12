use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneProbeReport {
    pub bulk_bytes: u64,
    pub bulk_chunk_bytes: usize,
    pub bulk_queue_capacity: usize,
    pub control_messages: usize,
    pub max_control_latency_micros: u64,
}

/// Exercises the Phase 0 lane contract using independent bounded queues.
///
/// The probe is intentionally transport-neutral. SSH multiplexing plugs into
/// these lanes later; this gate proves that bulk backpressure cannot occupy the
/// control queue or require file-sized memory.
pub async fn run_lane_probe(bulk_bytes: u64, bulk_chunk_bytes: usize) -> LaneProbeReport {
    const BULK_CAPACITY: usize = 2;
    const CONTROL_MESSAGES: usize = 128;

    let (bulk_tx, mut bulk_rx) = mpsc::channel::<Vec<u8>>(BULK_CAPACITY);
    let (control_tx, mut control_rx) = mpsc::channel::<Instant>(32);

    let bulk_producer = tokio::spawn(async move {
        let mut remaining = bulk_bytes;
        while remaining > 0 {
            let length = remaining.min(bulk_chunk_bytes as u64) as usize;
            if bulk_tx.send(vec![0x5a; length]).await.is_err() {
                break;
            }
            remaining -= length as u64;
        }
    });

    let control_producer = tokio::spawn(async move {
        for _ in 0..CONTROL_MESSAGES {
            control_tx.send(Instant::now()).await.unwrap();
            tokio::task::yield_now().await;
        }
    });

    let mut received_bulk = 0_u64;
    let mut received_control = 0;
    let mut max_control_latency = Duration::ZERO;
    while received_bulk < bulk_bytes || received_control < CONTROL_MESSAGES {
        tokio::select! {
            biased;
            Some(sent_at) = control_rx.recv(), if received_control < CONTROL_MESSAGES => {
                max_control_latency = max_control_latency.max(sent_at.elapsed());
                received_control += 1;
            }
            Some(chunk) = bulk_rx.recv(), if received_bulk < bulk_bytes => {
                received_bulk += chunk.len() as u64;
                tokio::task::yield_now().await;
            }
            else => break,
        }
    }

    bulk_producer.await.unwrap();
    control_producer.await.unwrap();

    LaneProbeReport {
        bulk_bytes: received_bulk,
        bulk_chunk_bytes,
        bulk_queue_capacity: BULK_CAPACITY,
        control_messages: received_control,
        max_control_latency_micros: max_control_latency.as_micros() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bulk_backpressure_does_not_starve_control() {
        let report = run_lane_probe(8 * 1024 * 1024, 64 * 1024).await;
        assert_eq!(report.bulk_bytes, 8 * 1024 * 1024);
        assert_eq!(report.control_messages, 128);
        assert_eq!(report.bulk_queue_capacity, 2);
        assert!(report.max_control_latency_micros < 250_000, "{report:?}");
    }
}
