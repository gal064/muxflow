//! Detecting that the machine slept. Port of
//! `features/shell/useDesktopResumeRecovery.ts`.

use std::{future::Future, pin::pin};

use futures::future::{Either, select};

pub const RESUME_GAP_MS: u64 = 15_000;
pub const CHECK_INTERVAL_MS: u64 = 5_000;
/// How long a resume gives the existing link to answer before rebuilding it.
pub const RESUME_PROBE_TIMEOUT_MS: u64 = 3_000;
const DEDUPE_MS: u64 = 1_000;

/// Which detector asked for a recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeTrigger {
    Native,
    TimerGap,
    Online,
}

impl ResumeTrigger {
    pub fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::TimerGap => "timerGap",
            Self::Online => "online",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeProbeOutcome {
    Alive,
    Dead,
}

impl ResumeProbeOutcome {
    pub fn name(self) -> &'static str {
        match self {
            Self::Alive => "alive",
            Self::Dead => "dead",
        }
    }
}

/// One correlated request settles whether a resume needs a rebuild: an answer
/// means the link is live, an error or the timeout means it is not.
pub async fn probe_resumed_link(
    probe: impl Future<Output = Result<(), String>>,
    timeout: impl Future<Output = ()>,
) -> ResumeProbeOutcome {
    match select(pin!(probe), pin!(timeout)).await {
        Either::Left((Ok(()), _)) => ResumeProbeOutcome::Alive,
        Either::Left((Err(_), _)) | Either::Right(_) => ResumeProbeOutcome::Dead,
    }
}

/// A monotonic timer gap, without repeated recoveries for one wake.
#[derive(Debug)]
pub struct ResumeGapDetector {
    last_observed_at: u64,
    last_recovery_at: Option<u64>,
}

impl ResumeGapDetector {
    pub fn new(started_at: u64) -> Self {
        Self {
            last_observed_at: started_at,
            last_recovery_at: None,
        }
    }

    pub fn observe(&mut self, now: u64) -> bool {
        let elapsed = now.saturating_sub(self.last_observed_at);
        self.last_observed_at = now;
        let recent = self
            .last_recovery_at
            .is_some_and(|at| now.saturating_sub(at) < RESUME_GAP_MS);
        if elapsed < RESUME_GAP_MS || recent {
            return false;
        }
        self.last_recovery_at = Some(now);
        true
    }

    /// An inactive window's timers are not evidence of a machine suspend.
    pub fn reset(&mut self, now: u64) {
        self.last_observed_at = now;
    }
}

#[derive(Debug)]
pub struct ResumeTransitionDetector {
    offline: bool,
}

impl ResumeTransitionDetector {
    pub fn new(offline: bool) -> Self {
        Self { offline }
    }

    pub fn network(&mut self, online: bool) -> bool {
        let resumed = self.offline && online;
        self.offline = !online;
        resumed
    }
}

/// Several detectors can see one wake; a recovery a second is enough.
#[derive(Debug, Default)]
pub struct ResumeDedupe {
    last_recovery_at: Option<u64>,
}

impl ResumeDedupe {
    pub fn admit(&mut self, now: u64) -> bool {
        if self
            .last_recovery_at
            .is_some_and(|at| now.saturating_sub(at) < DEDUPE_MS)
        {
            return false;
        }
        self.last_recovery_at = Some(now);
        true
    }
}

/// Milliseconds on a clock that keeps counting through a suspend, which
/// `Instant` does not on Linux.
pub fn suspend_aware_now_ms() -> u64 {
    #[cfg(target_os = "linux")]
    const CLOCK: libc::clockid_t = libc::CLOCK_BOOTTIME;
    #[cfg(not(target_os = "linux"))]
    const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `time` is a valid, writable timespec for the duration of the call.
    let status = unsafe { libc::clock_gettime(CLOCK, &mut time) };
    if status != 0 {
        return 0;
    }
    (time.tv_sec as u64) * 1_000 + (time.tv_nsec as u64) / 1_000_000
}

/// Replaces the browser's `navigator.onLine`: an interface that is up, running,
/// not loopback and has an address. A failed query reads as online, like the
/// browser's own default.
pub fn network_online() -> bool {
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `head` is a valid out pointer; a zero return hands us a list we
    // free below.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return true;
    }
    let mut online = false;
    let mut cursor = head;
    while !cursor.is_null() {
        // SAFETY: `cursor` walks the list getifaddrs returned, not yet freed.
        let entry = unsafe { &*cursor };
        let flags = entry.ifa_flags as libc::c_int;
        let usable = flags & libc::IFF_UP != 0
            && flags & libc::IFF_RUNNING != 0
            && flags & libc::IFF_LOOPBACK == 0;
        // SAFETY: a non-null `ifa_addr` points at a sockaddr owned by the list.
        let family = (!entry.ifa_addr.is_null())
            .then(|| unsafe { (*entry.ifa_addr).sa_family } as libc::c_int);
        if usable && matches!(family, Some(libc::AF_INET | libc::AF_INET6)) {
            online = true;
            break;
        }
        cursor = entry.ifa_next;
    }
    // SAFETY: `head` came from a successful getifaddrs and is freed once.
    unsafe { libc::freeifaddrs(head) };
    online
}

#[cfg(test)]
mod tests {
    use futures::{executor::block_on, future};

    use super::*;

    #[test]
    fn ignores_ordinary_timer_and_focus_activity() {
        let mut detector = ResumeGapDetector::new(1_000);
        assert!(!detector.observe(6_000));
        assert!(!detector.observe(11_000));
    }

    #[test]
    fn requests_exactly_one_recovery_after_a_suspended_timer_gap() {
        let mut detector = ResumeGapDetector::new(1_000);
        assert!(detector.observe(1_000 + RESUME_GAP_MS));
        assert!(!detector.observe(1_000 + RESUME_GAP_MS + 1));
        assert!(!detector.observe(1_000 + RESUME_GAP_MS * 2));
        assert!(detector.observe(1_000 + RESUME_GAP_MS * 3 + 1));
    }

    #[test]
    fn ignores_clock_corrections_because_samples_are_monotonic_durations() {
        let mut detector = ResumeGapDetector::new(10_000);
        assert!(!detector.observe(9_000));
        assert!(!detector.observe(10_000));
    }

    #[test]
    fn resets_a_background_timer_gap_instead_of_treating_it_as_a_resume() {
        let mut detector = ResumeGapDetector::new(1_000);
        detector.reset(1_000 + RESUME_GAP_MS * 4);
        assert!(!detector.observe(1_000 + RESUME_GAP_MS * 4 + 1));
    }

    #[test]
    fn recovers_only_after_an_offline_to_online_transition() {
        let mut detector = ResumeTransitionDetector::new(false);
        assert!(!detector.network(true));
        assert!(!detector.network(false));
        assert!(detector.network(true));
        assert!(!detector.network(true));
        let mut started_offline = ResumeTransitionDetector::new(true);
        assert!(started_offline.network(true));
    }

    #[test]
    fn collapses_a_second_trigger_for_the_same_wake() {
        let mut dedupe = ResumeDedupe::default();
        assert!(dedupe.admit(5_000));
        assert!(!dedupe.admit(5_999));
        assert!(dedupe.admit(6_000));
    }

    #[test]
    fn keeps_a_link_that_answers() {
        let outcome = block_on(probe_resumed_link(future::ready(Ok(())), future::pending()));
        assert_eq!(outcome, ResumeProbeOutcome::Alive);
    }

    #[test]
    fn rebuilds_a_link_that_refuses() {
        let outcome = block_on(probe_resumed_link(
            future::ready(Err("refused".to_owned())),
            future::pending(),
        ));
        assert_eq!(outcome, ResumeProbeOutcome::Dead);
    }

    #[test]
    fn rebuilds_a_link_that_never_answers_at_the_timeout() {
        let outcome = block_on(probe_resumed_link(future::pending(), future::ready(())));
        assert_eq!(outcome, ResumeProbeOutcome::Dead);
    }

    #[test]
    fn the_suspend_aware_clock_moves_forward() {
        let first = suspend_aware_now_ms();
        assert!(first > 0);
        assert!(suspend_aware_now_ms() >= first);
    }
}
