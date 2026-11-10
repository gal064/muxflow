//! Bounded retries for a pane stuck in a degraded state. Port of
//! `PaneDegradedWatchdog.ts`, driven by the caller's clock: a healthy pane holds
//! no deadline, and `note_healthy` on the output path is one emptiness check.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

/// Most actionable first, so the first reason in the set is the one reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PaneDegradedReason {
    RevealFailed,
    DeferredOverflow,
    RendererReseed,
    HubConflictReseed,
    HubAwaitingSeed,
    PaneAwaitingSeed,
    SeedRequested,
}

impl PaneDegradedReason {
    pub fn name(self) -> &'static str {
        match self {
            Self::RevealFailed => "revealFailed",
            Self::DeferredOverflow => "deferredOverflow",
            Self::RendererReseed => "rendererReseed",
            Self::HubConflictReseed => "hubConflictReseed",
            Self::HubAwaitingSeed => "hubAwaitingSeed",
            Self::PaneAwaitingSeed => "paneAwaitingSeed",
            Self::SeedRequested => "seedRequested",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::RevealFailed => "This pane is waiting for the host to show it",
            Self::DeferredOverflow => "Output outran this pane's recovery buffer",
            Self::RendererReseed => "The terminal renderer refused this pane's content",
            Self::HubConflictReseed => "A conflicting terminal handoff was rejected",
            Self::HubAwaitingSeed => "This pane's output is held back until a seed arrives",
            Self::PaneAwaitingSeed => "This pane is waiting for a fresh terminal seed",
            Self::SeedRequested => "This pane is still waiting for the terminal seed it asked for",
        }
    }
}

pub const PANE_WATCHDOG_BASE_DELAY: Duration = Duration::from_millis(2_000);
pub const PANE_WATCHDOG_MAX_DELAY: Duration = Duration::from_millis(30_000);

#[derive(Debug, PartialEq, Eq)]
pub struct WatchdogRetry {
    pub reason: PaneDegradedReason,
    /// Zero-based within one degraded episode.
    pub attempt: u32,
}

#[derive(Default)]
pub struct PaneDegradedWatchdog {
    reasons: BTreeSet<PaneDegradedReason>,
    attempts: u32,
    deadline: Option<Instant>,
    stopped: bool,
}

impl PaneDegradedWatchdog {
    pub fn note(&mut self, reason: PaneDegradedReason, now: Instant) {
        if self.stopped || !self.reasons.insert(reason) {
            return;
        }
        self.arm(now);
    }

    pub fn clear(&mut self, reason: PaneDegradedReason) {
        if self.reasons.remove(&reason) && self.reasons.is_empty() {
            self.recovered();
        }
    }

    pub fn note_healthy(&mut self) {
        if self.reasons.is_empty() && self.attempts == 0 {
            return;
        }
        self.reasons.clear();
        self.recovered();
    }

    pub fn stop(&mut self) {
        self.stopped = true;
        self.reasons.clear();
        self.deadline = None;
    }

    pub fn degraded(&self) -> bool {
        !self.reasons.is_empty()
    }

    pub fn primary_reason(&self) -> Option<PaneDegradedReason> {
        self.reasons.first().copied()
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Fires an expired bound. The caller asks for a seed in response, so the
    /// pane stays degraded on that request even if asking withdraws every other
    /// reason.
    pub fn poll(&mut self, now: Instant) -> Option<WatchdogRetry> {
        let deadline = self.deadline?;
        if now < deadline {
            return None;
        }
        self.deadline = None;
        let reason = self.primary_reason()?;
        let attempt = self.attempts;
        self.attempts += 1;
        self.reasons.insert(PaneDegradedReason::SeedRequested);
        self.arm(now);
        Some(WatchdogRetry { reason, attempt })
    }

    fn recovered(&mut self) {
        self.attempts = 0;
        self.deadline = None;
    }

    fn arm(&mut self, now: Instant) {
        if self.stopped || self.deadline.is_some() {
            return;
        }
        let doubled = PANE_WATCHDOG_BASE_DELAY.saturating_mul(1 << self.attempts.min(16));
        self.deadline = Some(now + doubled.min(PANE_WATCHDOG_MAX_DELAY));
    }
}

#[cfg(test)]
mod tests {
    use super::PaneDegradedReason::*;
    use super::*;

    fn fire(watchdog: &mut PaneDegradedWatchdog) -> (Instant, Option<WatchdogRetry>) {
        let at = watchdog.deadline().expect("no watchdog bound is armed");
        (at, watchdog.poll(at))
    }

    #[test]
    fn arms_nothing_until_a_pane_is_degraded() {
        let mut watchdog = PaneDegradedWatchdog::default();
        watchdog.note_healthy();
        watchdog.clear(RevealFailed);
        assert_eq!(watchdog.deadline(), None);
        assert!(!watchdog.degraded());
    }

    #[test]
    fn backs_off_from_two_seconds_to_a_thirty_second_ceiling() {
        let start = Instant::now();
        let mut watchdog = PaneDegradedWatchdog::default();
        watchdog.note(PaneAwaitingSeed, start);
        let mut delays = Vec::new();
        let mut last = start;
        let mut attempts = Vec::new();
        for _ in 0..6 {
            let (at, retry) = fire(&mut watchdog);
            delays.push(at - last);
            last = at;
            let retry = retry.unwrap();
            assert_eq!(retry.reason, PaneAwaitingSeed);
            attempts.push(retry.attempt);
        }
        delays.push(watchdog.deadline().unwrap() - last);
        let seconds: Vec<u64> = delays.iter().map(Duration::as_secs).collect();
        assert_eq!(seconds, [2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(attempts, [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn keeps_waiting_on_the_seed_it_asked_for_even_after_every_other_reason_is_withdrawn() {
        let mut watchdog = PaneDegradedWatchdog::default();
        watchdog.note(HubConflictReseed, Instant::now());
        fire(&mut watchdog);
        watchdog.clear(HubConflictReseed);
        assert!(watchdog.degraded());
        assert_eq!(watchdog.primary_reason(), Some(SeedRequested));
        assert!(watchdog.deadline().is_some());
    }

    #[test]
    fn disarms_and_restarts_the_backoff_when_the_pane_proves_it_is_working() {
        let start = Instant::now();
        let mut watchdog = PaneDegradedWatchdog::default();
        watchdog.note(DeferredOverflow, start);
        fire(&mut watchdog);
        fire(&mut watchdog);
        watchdog.note_healthy();
        assert_eq!(watchdog.deadline(), None);
        assert_eq!(watchdog.attempts(), 0);
        let later = start + Duration::from_secs(100);
        watchdog.note(RevealFailed, later);
        assert_eq!(watchdog.deadline().unwrap() - later, Duration::from_secs(2));
    }

    #[test]
    fn never_retries_once_stopped() {
        let mut watchdog = PaneDegradedWatchdog::default();
        watchdog.note(RendererReseed, Instant::now());
        fire(&mut watchdog);
        assert!(watchdog.deadline().is_some());
        watchdog.stop();
        assert_eq!(watchdog.deadline(), None);
        watchdog.note(RendererReseed, Instant::now());
        assert_eq!(watchdog.deadline(), None);
    }

    #[test]
    fn reports_the_most_actionable_reason_first() {
        let mut watchdog = PaneDegradedWatchdog::default();
        let now = Instant::now();
        watchdog.note(SeedRequested, now);
        watchdog.note(HubAwaitingSeed, now);
        watchdog.note(RevealFailed, now);
        assert_eq!(watchdog.primary_reason(), Some(RevealFailed));
    }

    #[test]
    fn a_premature_poll_changes_nothing() {
        let start = Instant::now();
        let mut watchdog = PaneDegradedWatchdog::default();
        watchdog.note(RendererReseed, start);
        assert_eq!(watchdog.poll(start + Duration::from_millis(1)), None);
        assert_eq!(watchdog.attempts(), 0);
    }
}
