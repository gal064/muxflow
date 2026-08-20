//! Bounded native delivery and exact host-credit translation.
//!
//! JavaScript acknowledges wire frames only after synchronous decode and hub
//! admission. This ledger maps those cumulative frame boundaries back to the
//! exact terminal payload charge reserved by the host protocol connection.

use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
};

pub(super) const NATIVE_DELIVERY_WINDOW_BYTES: u64 = 2 * 1024 * 1024;
pub(super) const NATIVE_DELIVERY_WINDOW_RECORDS: u64 = 4_096;
pub(super) const NATIVE_DELIVERY_MAX_FRAME_BYTES: u64 = tmux_agent_protocol::MAX_FRAME_BYTES as u64;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct HostCharge {
    pub bytes: u64,
    pub records: u64,
}

impl HostCharge {
    #[cfg(test)]
    pub(super) fn terminal(bytes: usize) -> Self {
        Self {
            bytes: bytes as u64,
            records: 1,
        }
    }

    pub(super) fn accumulate(&mut self, other: Self) {
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.records = self.records.saturating_add(other.records);
    }
}

#[derive(Debug)]
struct FrameDebit {
    wire_bytes: u64,
    host: HostCharge,
    committed: bool,
}

#[derive(Debug)]
struct OpenState {
    epoch: u64,
    frames: VecDeque<FrameDebit>,
    retained_wire_bytes: u64,
    reserved_frames_total: u64,
    committed_frames_total: u64,
    acknowledged_frames_total: u64,
    acknowledged_wire_bytes_total: u64,
    acknowledged_host: HostCharge,
}

#[derive(Debug)]
enum State {
    Open(OpenState),
    Closed,
}

#[derive(Debug)]
pub(super) struct DeliveryWindow {
    state: Mutex<State>,
    released: Condvar,
}

impl DeliveryWindow {
    pub(super) fn new(epoch: u64) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::Open(OpenState {
                epoch,
                frames: VecDeque::new(),
                retained_wire_bytes: 0,
                reserved_frames_total: 0,
                committed_frames_total: 0,
                acknowledged_frames_total: 0,
                acknowledged_wire_bytes_total: 0,
                acknowledged_host: HostCharge::default(),
            })),
            released: Condvar::new(),
        })
    }

    pub(super) fn reserve(
        self: &Arc<Self>,
        wire_bytes: usize,
        host: HostCharge,
    ) -> Result<DeliveryReservation, String> {
        let wire_bytes = wire_bytes as u64;
        if wire_bytes > NATIVE_DELIVERY_MAX_FRAME_BYTES {
            return Err("one desktop event exceeds the protocol frame bound".into());
        }
        let mut state = self.state.lock().unwrap();
        loop {
            match &mut *state {
                State::Closed => return Err("desktop delivery window is closed".into()),
                State::Open(open)
                    if ((wire_bytes <= NATIVE_DELIVERY_WINDOW_BYTES
                        && open.retained_wire_bytes.saturating_add(wire_bytes)
                            <= NATIVE_DELIVERY_WINDOW_BYTES)
                        || (wire_bytes > NATIVE_DELIVERY_WINDOW_BYTES
                            && open.retained_wire_bytes == 0
                            && open.frames.is_empty()))
                        && (open.frames.len() as u64) < NATIVE_DELIVERY_WINDOW_RECORDS =>
                {
                    open.retained_wire_bytes = open.retained_wire_bytes.saturating_add(wire_bytes);
                    open.reserved_frames_total = open.reserved_frames_total.saturating_add(1);
                    let frame_number = open.reserved_frames_total;
                    open.frames.push_back(FrameDebit {
                        wire_bytes,
                        host,
                        committed: false,
                    });
                    return Ok(DeliveryReservation {
                        window: Arc::clone(self),
                        frame_number,
                        committed: false,
                    });
                }
                State::Open(_) => state = self.released.wait(state).unwrap(),
            }
        }
    }

    pub(super) fn acknowledge(
        &self,
        epoch: u64,
        cumulative_frames: u64,
        cumulative_wire_bytes: u64,
    ) -> Result<Option<HostCharge>, String> {
        let mut state = self.state.lock().unwrap();
        loop {
            let State::Open(open) = &mut *state else {
                return Ok(None);
            };
            if epoch != open.epoch {
                return Ok(None);
            }
            if cumulative_frames < open.acknowledged_frames_total
                || cumulative_wire_bytes < open.acknowledged_wire_bytes_total
                || cumulative_frames > open.reserved_frames_total
            {
                return Err("desktop delivery acknowledgement is outside admitted frames".into());
            }
            if cumulative_frames > open.committed_frames_total {
                state = self.released.wait(state).unwrap();
                continue;
            }
            let release_frames = cumulative_frames - open.acknowledged_frames_total;
            let mut released_wire = 0_u64;
            let mut released_host = HostCharge::default();
            let release_count = usize::try_from(release_frames)
                .map_err(|_| "desktop delivery acknowledgement frame count overflow")?;
            if release_count > open.frames.len() {
                return Err("desktop delivery ledger lost an admitted frame".into());
            }
            // Validate the complete cumulative prefix before mutating the
            // deque. A malformed/stale JS boundary must leave the exact ledger
            // retryable (or available for fail-closed reconnect), never pop a
            // debit and then discover that its byte boundary was wrong.
            for frame in open.frames.iter().take(release_count) {
                if !frame.committed {
                    return Err(
                        "desktop delivery acknowledgement crossed a provisional frame".into(),
                    );
                }
                released_wire = released_wire.saturating_add(frame.wire_bytes);
                released_host.bytes = released_host.bytes.saturating_add(frame.host.bytes);
                released_host.records = released_host.records.saturating_add(frame.host.records);
            }
            if open
                .acknowledged_wire_bytes_total
                .saturating_add(released_wire)
                != cumulative_wire_bytes
            {
                return Err(
                    "desktop delivery acknowledgement crossed a wire-frame boundary".into(),
                );
            }
            open.frames.drain(..release_count);
            open.acknowledged_frames_total = cumulative_frames;
            open.acknowledged_wire_bytes_total = cumulative_wire_bytes;
            open.retained_wire_bytes = open.retained_wire_bytes.saturating_sub(released_wire);
            open.acknowledged_host.bytes = open
                .acknowledged_host
                .bytes
                .saturating_add(released_host.bytes);
            open.acknowledged_host.records = open
                .acknowledged_host
                .records
                .saturating_add(released_host.records);
            let host = open.acknowledged_host;
            drop(state);
            self.released.notify_all();
            return Ok(Some(host));
        }
    }

    /// Releases host credit for terminal payload that never became a frame.
    ///
    /// The ledger above maps admitted frames to the credit they owe; this is the
    /// one charge that has no frame to map. Events quarantined between a
    /// detected sequence gap and its resync barrier are dropped before they
    /// reach JavaScript, so no acknowledgement will ever cover them — but the
    /// host already reserved their exact `terminal_delivery_*` charge and holds
    /// it until this connection's cumulative total catches up. Adding it here
    /// keeps that total monotonic and still bounded by what the host reserved,
    /// which is what stops a resync from permanently narrowing the window.
    pub(super) fn forfeit(&self, epoch: u64, host: HostCharge) -> Option<HostCharge> {
        let mut state = self.state.lock().unwrap();
        let State::Open(open) = &mut *state else {
            return None;
        };
        if epoch != open.epoch {
            return None;
        }
        open.acknowledged_host.accumulate(host);
        Some(open.acknowledged_host)
    }

    pub(super) fn close(&self) {
        *self.state.lock().unwrap() = State::Closed;
        self.released.notify_all();
    }

    fn commit(&self, frame_number: u64) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let State::Open(open) = &mut *state else {
            return Err("desktop delivery window closed during channel send".into());
        };
        if frame_number != open.committed_frames_total.saturating_add(1) {
            return Err("desktop delivery frames committed out of order".into());
        }
        let index = (frame_number - open.acknowledged_frames_total - 1) as usize;
        let frame = open
            .frames
            .get_mut(index)
            .ok_or("desktop delivery reservation disappeared before commit")?;
        frame.committed = true;
        open.committed_frames_total = frame_number;
        drop(state);
        self.released.notify_all();
        Ok(())
    }

    fn rollback(&self, frame_number: u64) {
        let mut state = self.state.lock().unwrap();
        if let State::Open(open) = &mut *state
            && frame_number == open.reserved_frames_total
            && frame_number > open.committed_frames_total
            && let Some(frame) = open.frames.pop_back()
        {
            open.reserved_frames_total -= 1;
            open.retained_wire_bytes = open.retained_wire_bytes.saturating_sub(frame.wire_bytes);
        }
        drop(state);
        self.released.notify_all();
    }
}

pub(super) struct DeliveryReservation {
    window: Arc<DeliveryWindow>,
    frame_number: u64,
    committed: bool,
}

impl DeliveryReservation {
    pub(super) fn commit(mut self) -> Result<(), String> {
        self.window.commit(self.frame_number)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for DeliveryReservation {
    fn drop(&mut self) {
        if !self.committed {
            self.window.rollback(self.frame_number);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn cumulative_wire_boundaries_release_exact_host_credit() {
        let window = DeliveryWindow::new(7);
        window
            .reserve(100, HostCharge::terminal(80))
            .unwrap()
            .commit()
            .unwrap();
        window
            .reserve(25, HostCharge::default())
            .unwrap()
            .commit()
            .unwrap();
        assert_eq!(
            window.acknowledge(7, 2, 125).unwrap(),
            Some(HostCharge {
                bytes: 80,
                records: 1
            })
        );
        assert!(window.acknowledge(7, 2, 124).is_err());
        assert_eq!(window.acknowledge(8, 2, 125).unwrap(), None);
    }

    #[test]
    fn invalid_boundary_does_not_consume_the_prefix_needed_by_the_correct_ack() {
        let window = DeliveryWindow::new(13);
        window
            .reserve(100, HostCharge::terminal(80))
            .unwrap()
            .commit()
            .unwrap();
        window
            .reserve(25, HostCharge::terminal(20))
            .unwrap()
            .commit()
            .unwrap();
        assert!(window.acknowledge(13, 1, 99).is_err());
        assert_eq!(
            window.acknowledge(13, 1, 100).unwrap(),
            Some(HostCharge {
                bytes: 80,
                records: 1,
            })
        );
        assert_eq!(
            window.acknowledge(13, 2, 125).unwrap(),
            Some(HostCharge {
                bytes: 100,
                records: 2,
            })
        );
    }

    #[test]
    fn full_window_blocks_and_close_releases_the_waiter() {
        let window = DeliveryWindow::new(9);
        window
            .reserve(NATIVE_DELIVERY_WINDOW_BYTES as usize, HostCharge::default())
            .unwrap()
            .commit()
            .unwrap();
        let waiting = Arc::clone(&window);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            sender
                .send(waiting.reserve(1, HostCharge::default()).is_err())
                .unwrap()
        });
        assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
        window.close();
        assert!(receiver.recv_timeout(Duration::from_secs(1)).unwrap());
    }

    #[test]
    fn one_protocol_bounded_oversize_frame_uses_an_empty_window() {
        let window = DeliveryWindow::new(1);
        let wire_bytes = NATIVE_DELIVERY_WINDOW_BYTES as usize + 1;
        window
            .reserve(wire_bytes, HostCharge::terminal(wire_bytes))
            .unwrap()
            .commit()
            .unwrap();
        let waiting = Arc::clone(&window);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            sender
                .send(waiting.reserve(1, HostCharge::default()).is_ok())
                .unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
        assert_eq!(
            window.acknowledge(1, 1, wire_bytes as u64).unwrap(),
            Some(HostCharge {
                bytes: wire_bytes as u64,
                records: 1,
            })
        );
        assert!(receiver.recv_timeout(Duration::from_secs(1)).unwrap());
        assert!(
            window
                .reserve(
                    NATIVE_DELIVERY_MAX_FRAME_BYTES as usize + 1,
                    HostCharge::default()
                )
                .is_err()
        );
    }

    #[test]
    fn one_coalesced_frame_releases_sixty_four_host_records_exactly() {
        let window = DeliveryWindow::new(11);
        window
            .reserve(
                256,
                HostCharge {
                    bytes: 64,
                    records: 64,
                },
            )
            .unwrap()
            .commit()
            .unwrap();
        assert_eq!(
            window.acknowledge(11, 1, 256).unwrap(),
            Some(HostCharge {
                bytes: 64,
                records: 64,
            })
        );
    }
}
