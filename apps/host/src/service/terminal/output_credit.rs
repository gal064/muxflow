//! Connection-scoped terminal delivery credit.
//!
//! The ordered event queue is a record bound, not a byte bound. This window
//! limits terminal payload admitted ahead of a response while keeping every
//! event and response in the existing single FIFO.

use std::sync::{
    Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};

use tmux_agent_protocol::MAX_FRAME_BYTES;

pub(crate) const OUTPUT_WINDOW_BYTES: u64 = 2 * 1024 * 1024;
pub(crate) const OUTPUT_WINDOW_RECORDS: u64 = 1_024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct OutputCharge {
    pub bytes: u64,
    pub records: u64,
}

impl OutputCharge {
    pub(crate) fn terminal(bytes: usize) -> Self {
        Self {
            bytes: bytes as u64,
            records: 1,
        }
    }
}

#[derive(Debug)]
struct OpenState {
    reserved: OutputCharge,
    acknowledged: OutputCharge,
}

#[derive(Debug)]
enum State {
    Legacy,
    Open(OpenState),
    Closed,
}

#[derive(Debug)]
pub(crate) struct OutputCredit {
    state: Mutex<State>,
    released: Condvar,
}

impl OutputCredit {
    pub(crate) fn negotiated(enabled: bool) -> Self {
        Self {
            state: Mutex::new(if enabled {
                State::Open(OpenState {
                    reserved: OutputCharge::default(),
                    acknowledged: OutputCharge::default(),
                })
            } else {
                State::Legacy
            }),
            released: Condvar::new(),
        }
    }

    /// Reserves before queue admission. One protocol-sized oversize record may
    /// occupy an otherwise-empty window so a valid frame cannot deadlock.
    ///
    /// `stopped` is the reserving attachment's stop flag. The credit is shared
    /// by every attachment on the connection, so closure cannot be the only way
    /// out of the wait: stopping one attachment must unpark its own waiters
    /// without ending delivery for the others. The flag is checked under the
    /// state lock; pair a store to it with [`Self::wake_waiters`].
    pub(crate) fn reserve(
        &self,
        charge: OutputCharge,
        stopped: &AtomicBool,
    ) -> Result<Reservation<'_>, &'static str> {
        if charge.bytes > MAX_FRAME_BYTES as u64 || charge.records > 1 {
            return Err("terminal delivery charge exceeds the protocol frame limit");
        }
        let mut state = self.state.lock().unwrap();
        loop {
            if stopped.load(Ordering::Acquire) {
                return Err("terminal delivery attachment is stopped");
            }
            match &mut *state {
                State::Legacy => return Ok(Reservation::legacy(self)),
                State::Closed => return Err("terminal delivery credit is closed"),
                State::Open(open) => {
                    let outstanding_bytes =
                        open.reserved.bytes.saturating_sub(open.acknowledged.bytes);
                    let outstanding_records = open
                        .reserved
                        .records
                        .saturating_sub(open.acknowledged.records);
                    let empty = outstanding_bytes == 0 && outstanding_records == 0;
                    let bytes_fit = outstanding_bytes.saturating_add(charge.bytes)
                        <= OUTPUT_WINDOW_BYTES
                        || (empty && charge.bytes > OUTPUT_WINDOW_BYTES);
                    let records_fit =
                        outstanding_records.saturating_add(charge.records) <= OUTPUT_WINDOW_RECORDS;
                    if bytes_fit && records_fit {
                        open.reserved.bytes = open.reserved.bytes.saturating_add(charge.bytes);
                        open.reserved.records =
                            open.reserved.records.saturating_add(charge.records);
                        return Ok(Reservation {
                            credit: self,
                            charge,
                            committed: false,
                            legacy: false,
                        });
                    }
                }
            }
            state = self.released.wait(state).unwrap();
        }
    }

    pub(crate) fn acknowledge(&self, cumulative: OutputCharge) -> Result<(), &'static str> {
        let mut state = self.state.lock().unwrap();
        match &mut *state {
            State::Legacy => Ok(()),
            State::Closed => Err("terminal delivery credit is closed"),
            State::Open(open) => {
                if cumulative.bytes < open.acknowledged.bytes
                    || cumulative.records < open.acknowledged.records
                    || cumulative.bytes > open.reserved.bytes
                    || cumulative.records > open.reserved.records
                {
                    return Err("terminal delivery acknowledgement is outside admitted credit");
                }
                open.acknowledged = cumulative;
                drop(state);
                self.released.notify_all();
                Ok(())
            }
        }
    }

    pub(crate) fn close(&self) {
        *self.state.lock().unwrap() = State::Closed;
        self.released.notify_all();
    }

    /// Wakes every [`Self::reserve`] waiter so it re-checks its stop flag.
    ///
    /// Call after storing the flag. Taking and releasing the state lock first
    /// is what closes the lost-wakeup race: a waiter that read the flag as
    /// clear holds that lock until it parks, so it cannot miss this notify.
    pub(crate) fn wake_waiters(&self) {
        drop(self.state.lock().unwrap());
        self.released.notify_all();
    }

    fn rollback(&self, charge: OutputCharge) {
        let mut state = self.state.lock().unwrap();
        if let State::Open(open) = &mut *state {
            open.reserved.bytes = open.reserved.bytes.saturating_sub(charge.bytes);
            open.reserved.records = open.reserved.records.saturating_sub(charge.records);
        }
        drop(state);
        self.released.notify_all();
    }
}

pub(crate) struct Reservation<'a> {
    credit: &'a OutputCredit,
    charge: OutputCharge,
    committed: bool,
    legacy: bool,
}

impl Reservation<'_> {
    fn legacy(credit: &OutputCredit) -> Reservation<'_> {
        Reservation {
            credit,
            charge: OutputCharge::default(),
            committed: true,
            legacy: true,
        }
    }

    pub(crate) fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if !self.committed && !self.legacy {
            self.credit.rollback(self.charge);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use super::*;

    fn running() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn exact_window_blocks_until_cumulative_credit_is_released() {
        let credit = Arc::new(OutputCredit::negotiated(true));
        let mut reservations = Vec::new();
        for _ in 0..32 {
            let reservation = credit
                .reserve(OutputCharge::terminal(64 * 1024), &running())
                .unwrap();
            reservation.commit();
            reservations.push(());
        }
        let waiting = Arc::clone(&credit);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let reservation = waiting
                .reserve(OutputCharge::terminal(64 * 1024), &running())
                .unwrap();
            reservation.commit();
            sender.send(()).unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
        credit
            .acknowledge(OutputCharge {
                bytes: 512 * 1024,
                records: 8,
            })
            .unwrap();
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn rollback_and_close_release_waiters_without_forging_credit() {
        let credit = OutputCredit::negotiated(true);
        drop(
            credit
                .reserve(OutputCharge::terminal(128), &running())
                .unwrap(),
        );
        assert!(credit.acknowledge(OutputCharge::terminal(1)).is_err());
        credit.close();
        assert!(
            credit
                .reserve(OutputCharge::terminal(1), &running())
                .is_err()
        );
    }

    #[test]
    fn one_protocol_sized_oversize_record_uses_an_empty_window() {
        let credit = OutputCredit::negotiated(true);
        let reservation = credit
            .reserve(
                OutputCharge::terminal(OUTPUT_WINDOW_BYTES as usize + 1),
                &running(),
            )
            .unwrap();
        reservation.commit();
    }

    #[test]
    fn tiny_records_release_by_record_credit_without_a_timer() {
        let credit = Arc::new(OutputCredit::negotiated(true));
        for _ in 0..OUTPUT_WINDOW_RECORDS {
            credit
                .reserve(OutputCharge::terminal(1), &running())
                .unwrap()
                .commit();
        }
        let waiting = Arc::clone(&credit);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            waiting
                .reserve(OutputCharge::terminal(1), &running())
                .unwrap()
                .commit();
            sender.send(()).unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
        credit
            .acknowledge(OutputCharge {
                bytes: OUTPUT_WINDOW_RECORDS,
                records: OUTPUT_WINDOW_RECORDS,
            })
            .unwrap();
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn stop_releases_a_writer_blocked_on_full_credit() {
        let credit = Arc::new(OutputCredit::negotiated(true));
        credit
            .reserve(OutputCharge::terminal(OUTPUT_WINDOW_BYTES as usize), &running())
            .unwrap()
            .commit();
        let waiting = Arc::clone(&credit);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            sender
                .send(waiting.reserve(OutputCharge::terminal(1), &running()).is_err())
                .unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
        credit.close();
        assert!(receiver.recv_timeout(Duration::from_secs(1)).unwrap());
    }

    /// Without the stop flag this waiter parks forever: the desktop has stopped
    /// acking, and only the detached attachment — never the connection — is
    /// being torn down, so nothing else ever notifies the condvar.
    #[test]
    fn attachment_stop_releases_its_waiter_without_closing_the_shared_credit() {
        let credit = Arc::new(OutputCredit::negotiated(true));
        credit
            .reserve(OutputCharge::terminal(OUTPUT_WINDOW_BYTES as usize), &running())
            .unwrap()
            .commit();
        let stopped = Arc::new(AtomicBool::new(false));
        let waiting = Arc::clone(&credit);
        let waiting_stopped = Arc::clone(&stopped);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            sender
                .send(
                    waiting
                        .reserve(OutputCharge::terminal(1), &waiting_stopped)
                        .is_err(),
                )
                .unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
        stopped.store(true, Ordering::Release);
        credit.wake_waiters();
        assert!(receiver.recv_timeout(Duration::from_secs(1)).unwrap());
        // Other attachments on the same connection are undisturbed: the window
        // still accepts acknowledgements and hands out credit.
        credit
            .acknowledge(OutputCharge {
                bytes: 1,
                records: 1,
            })
            .unwrap();
        credit
            .reserve(OutputCharge::terminal(1), &running())
            .unwrap()
            .commit();
    }
}
