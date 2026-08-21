//! Connection-scoped terminal delivery credit.
//!
//! The ordered event queue is a record bound, not a byte bound. This window
//! limits terminal payload admitted ahead of a response while keeping every
//! event and response in the existing single FIFO.
//!
//! # Who is allowed to wait
//!
//! Admission ([`OutputCredit::admit`]) never blocks. Waiting for the window to
//! drain ([`OutputCredit::await_window`]) is a separate step, and only a tmux
//! control-reader thread may take it. The split is not stylistic; both halves
//! of it are deadlocks that happened.
//!
//! * The window is released only by `TerminalOutputAck` frames, and those are
//!   read by the connection's frame loop in `service.rs`. That loop `await`s
//!   every `Scheduling::Inline` request to completion before it reads the next
//!   frame, and `SetTerminalVisibility` is inline. A request handler that waits
//!   for credit is therefore waiting for an acknowledgement that the only
//!   thread able to deliver it cannot read until the wait ends. That is not a
//!   race — it is a closed cycle, and it is the one captured in production: a
//!   `set_visibility` parked here while every request on the connection timed
//!   out for nine minutes. Request handlers admit; they never wait.
//! * A reader waits *after* admitting, with the emission-order fence released.
//!   Waiting under that fence stops output for every pane on the connection,
//!   and `set_visibility` takes the fence while holding the terminal mutex —
//!   so it also stops input, resize, attach and topology reconciliation. That
//!   is the "typing lag under heavy agent output" symptom.
//!
//! The window is consequently a soft bound: it can be over-subscribed by the
//! one record each reader admits before it waits, plus any visibility recovery
//! admitted meanwhile — each capped by [`MAX_FRAME_BYTES`]. The readers repay
//! the excess by waiting longer before their next admission.

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

    /// Charges the window for a record that is about to be queued. Never waits.
    ///
    /// The charge is recorded even when the window is already over-subscribed,
    /// so admission can never depend on a peer. Keeping the accounting exact is
    /// the point: a record on the wire that was not charged would make the
    /// client's cumulative acknowledgement exceed what was admitted, which
    /// [`Self::acknowledge`] rejects as a protocol violation.
    ///
    /// Flow control is [`Self::await_window`], and only a control reader calls
    /// it — see the module docs for the two deadlocks that split these apart.
    ///
    /// `stopped` is the admitting attachment's stop flag; a stopped attachment
    /// admits nothing.
    pub(crate) fn admit(
        &self,
        charge: OutputCharge,
        stopped: &AtomicBool,
    ) -> Result<Reservation<'_>, &'static str> {
        if charge.bytes > MAX_FRAME_BYTES as u64 || charge.records > 1 {
            return Err("terminal delivery charge exceeds the protocol frame limit");
        }
        let mut state = self.state.lock().unwrap();
        if stopped.load(Ordering::Acquire) {
            return Err("terminal delivery attachment is stopped");
        }
        match &mut *state {
            State::Legacy => Ok(Reservation::legacy(self)),
            State::Closed => Err("terminal delivery credit is closed"),
            State::Open(open) => {
                open.reserved.bytes = open.reserved.bytes.saturating_add(charge.bytes);
                open.reserved.records = open.reserved.records.saturating_add(charge.records);
                Ok(Reservation {
                    credit: self,
                    charge,
                    committed: false,
                    legacy: false,
                })
            }
        }
    }

    /// Blocks a control reader until the window has room for another record.
    ///
    /// Call this only from a tmux control-reader thread, holding no lock any
    /// peer needs — never under the emission-order fence, never from a request
    /// dispatch. The module docs say why: this is the only wait in the system
    /// that depends on the client, and every other thread that took it wedged
    /// the connection.
    ///
    /// Returns as soon as the window is under budget, the credit closes, or the
    /// caller's attachment stops. `stopped` is read under the state lock; pair a
    /// store to it with [`Self::wake_waiters`].
    pub(crate) fn await_window(&self, stopped: &AtomicBool) {
        let mut state = self.state.lock().unwrap();
        loop {
            if stopped.load(Ordering::Acquire) {
                return;
            }
            match &*state {
                State::Legacy | State::Closed => return,
                State::Open(open) => {
                    let outstanding_bytes =
                        open.reserved.bytes.saturating_sub(open.acknowledged.bytes);
                    let outstanding_records = open
                        .reserved
                        .records
                        .saturating_sub(open.acknowledged.records);
                    if outstanding_bytes < OUTPUT_WINDOW_BYTES
                        && outstanding_records < OUTPUT_WINDOW_RECORDS
                    {
                        return;
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

    /// Wakes every [`Self::await_window`] waiter so it re-checks its stop flag.
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

    fn fill_window(credit: &OutputCredit) {
        for _ in 0..32 {
            credit
                .admit(OutputCharge::terminal(64 * 1024), &running())
                .unwrap()
                .commit();
        }
    }

    #[test]
    fn exact_window_holds_a_reader_until_cumulative_credit_is_released() {
        let credit = Arc::new(OutputCredit::negotiated(true));
        fill_window(&credit);
        let waiting = Arc::clone(&credit);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            waiting.await_window(&running());
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

    /// The deadlock this window caused, reduced to its one load-bearing fact:
    /// a thread that cannot wait for an acknowledgement — because it is the
    /// thread the acknowledgement has to be read by — can still admit. If
    /// `admit` ever waits again this test hangs, since nothing here ever acks.
    #[test]
    fn admission_never_waits_on_a_full_window() {
        let credit = OutputCredit::negotiated(true);
        fill_window(&credit);
        for _ in 0..4 {
            credit
                .admit(OutputCharge::terminal(64 * 1024), &running())
                .unwrap()
                .commit();
        }
        // Over-subscription is temporary and honest: the excess is charged, so
        // the client may acknowledge all of it, and the window then reopens.
        credit
            .acknowledge(OutputCharge {
                bytes: 36 * 64 * 1024,
                records: 36,
            })
            .unwrap();
        credit.await_window(&running());
    }

    #[test]
    fn rollback_and_close_release_waiters_without_forging_credit() {
        let credit = OutputCredit::negotiated(true);
        drop(
            credit
                .admit(OutputCharge::terminal(128), &running())
                .unwrap(),
        );
        assert!(credit.acknowledge(OutputCharge::terminal(1)).is_err());
        credit.close();
        assert!(credit.admit(OutputCharge::terminal(1), &running()).is_err());
    }

    #[test]
    fn an_oversize_record_is_refused_but_a_window_sized_one_is_admitted() {
        let credit = OutputCredit::negotiated(true);
        assert!(
            credit
                .admit(OutputCharge::terminal(MAX_FRAME_BYTES + 1), &running())
                .is_err()
        );
        credit
            .admit(
                OutputCharge::terminal(OUTPUT_WINDOW_BYTES as usize + 1),
                &running(),
            )
            .unwrap()
            .commit();
    }

    #[test]
    fn tiny_records_release_by_record_credit_without_a_timer() {
        let credit = Arc::new(OutputCredit::negotiated(true));
        for _ in 0..OUTPUT_WINDOW_RECORDS {
            credit
                .admit(OutputCharge::terminal(1), &running())
                .unwrap()
                .commit();
        }
        let waiting = Arc::clone(&credit);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            waiting.await_window(&running());
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
    fn close_releases_a_reader_waiting_on_a_full_window() {
        let credit = Arc::new(OutputCredit::negotiated(true));
        credit
            .admit(
                OutputCharge::terminal(OUTPUT_WINDOW_BYTES as usize),
                &running(),
            )
            .unwrap()
            .commit();
        let waiting = Arc::clone(&credit);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            waiting.await_window(&running());
            sender.send(()).unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
        credit.close();
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    /// Without the stop flag this waiter parks forever: the desktop has stopped
    /// acking, and only the detached attachment — never the connection — is
    /// being torn down, so nothing else ever notifies the condvar.
    #[test]
    fn attachment_stop_releases_its_waiter_without_closing_the_shared_credit() {
        let credit = Arc::new(OutputCredit::negotiated(true));
        credit
            .admit(
                OutputCharge::terminal(OUTPUT_WINDOW_BYTES as usize),
                &running(),
            )
            .unwrap()
            .commit();
        let stopped = Arc::new(AtomicBool::new(false));
        let waiting = Arc::clone(&credit);
        let waiting_stopped = Arc::clone(&stopped);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            waiting.await_window(&waiting_stopped);
            sender.send(()).unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
        stopped.store(true, Ordering::Release);
        credit.wake_waiters();
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        // Other attachments on the same connection are undisturbed: the window
        // still accepts acknowledgements and hands out credit.
        credit
            .acknowledge(OutputCharge {
                bytes: 1,
                records: 1,
            })
            .unwrap();
        credit
            .admit(OutputCharge::terminal(1), &running())
            .unwrap()
            .commit();
    }
}
