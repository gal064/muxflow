//! Publication-side admission state.
//!
//! A queued event is not a runnable transfer until its channel accepts that
//! event. Keeping this state outside the worker FIFO lets unrelated admitted
//! work dispatch while one renderer callback is slow, while cancellation can
//! still wait for the publication outcome without acknowledging a ghost job.

use std::{
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

use super::CancelState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AdmissionOutcome {
    Publishing,
    Committed,
    Cancelled,
    Rejected,
}

#[derive(Debug)]
struct State {
    cancel_requested: bool,
    outcome: AdmissionOutcome,
}

#[derive(Debug)]
pub(super) struct Admission {
    cancellation: Arc<CancelState>,
    state: Mutex<State>,
    changed: Condvar,
}

impl Admission {
    pub(super) fn new(cancellation: Arc<CancelState>) -> Arc<Self> {
        Arc::new(Self {
            cancellation,
            state: Mutex::new(State {
                cancel_requested: false,
                outcome: AdmissionOutcome::Publishing,
            }),
            changed: Condvar::new(),
        })
    }

    /// Linearizes publication with cancellation and returns the action the
    /// publishing caller owns. A successful cancelled publication still needs
    /// a queued-then-terminal transition; a rejected publication owns neither.
    pub(super) fn finish_publication(&self, published: bool) -> AdmissionOutcome {
        let mut state = self.state.lock().unwrap();
        state.outcome = if !published {
            AdmissionOutcome::Rejected
        } else if state.cancel_requested {
            AdmissionOutcome::Cancelled
        } else {
            AdmissionOutcome::Committed
        };
        let outcome = state.outcome;
        drop(state);
        self.changed.notify_all();
        outcome
    }

    pub(super) fn cancel_and_wait(&self, timeout: Duration) -> Result<AdmissionOutcome, String> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap();
        if state.outcome == AdmissionOutcome::Publishing {
            state.cancel_requested = true;
            self.cancellation.cancel();
            self.changed.notify_all();
        }
        while state.outcome == AdmissionOutcome::Publishing {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(
                    "bulk transfer queued publication is still pending; retry cancellation".into(),
                );
            };
            let (next, wait) = self.changed.wait_timeout(state, remaining).unwrap();
            state = next;
            if wait.timed_out() && state.outcome == AdmissionOutcome::Publishing {
                return Err(
                    "bulk transfer queued publication is still pending; retry cancellation".into(),
                );
            }
        }
        Ok(state.outcome)
    }

    pub(super) fn cancel_stale(&self) {
        let mut state = self.state.lock().unwrap();
        if state.outcome == AdmissionOutcome::Publishing {
            state.cancel_requested = true;
            self.cancellation.cancel_stale_binding();
            self.changed.notify_all();
        }
    }

    #[cfg(test)]
    pub(super) fn wait_until_cancelled(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap();
        while !state.cancel_requested {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("cancellation latched before test deadline");
            (state, _) = self.changed.wait_timeout(state, remaining).unwrap();
        }
    }
}
