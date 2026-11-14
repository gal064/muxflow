//! Whether the network is the thing that is wrong, said in those words. Port of
//! `app/linkQuality.ts`: a pure tally over injected time.

use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkQualityState {
    Unstable,
    Slow,
}

impl LinkQualityState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Unstable => "unstable",
            Self::Slow => "slow",
        }
    }
}

pub const UNSTABLE_WINDOW_MS: u64 = 300_000;
pub const UNSTABLE_LOSS_COUNT: usize = 2;
pub const SLOW_WINDOW_MS: u64 = 60_000;
pub const SLOW_LAG_COUNT: usize = 2;
pub const SLOW_LAG_MIN_MS: u64 = 750;
pub const SLOW_LAG_ALONE_MS: u64 = 1_500;
pub const LINK_QUALITY_CLEAR_MS: u64 = 120_000;
pub const LINK_QUALITY_POLL_MS: u64 = 10_000;
pub const LINK_STATS_POLL_MS: u64 = 5_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkQualityChange {
    Degraded {
        state: LinkQualityState,
        losses: usize,
        lag_events: usize,
        late_requests: usize,
    },
    Cleared {
        after_ms: u64,
    },
}

/// The sentence the user reads, deliberately about *their* connection.
pub fn describe_link_quality(state: LinkQualityState, host: &str) -> String {
    match state {
        LinkQualityState::Unstable => format!(
            "Your connection to {host} is unstable; Muxflow keeps losing the link and reconnecting."
        ),
        LinkQualityState::Slow => {
            format!("Your connection to {host} is slow; the host is answering, but late.")
        }
    }
}

fn prune(times: &mut VecDeque<u64>, at: u64, window_ms: u64) {
    while times
        .front()
        .is_some_and(|time| at.saturating_sub(*time) > window_ms)
    {
        times.pop_front();
    }
}

#[derive(Debug, Default)]
pub struct LinkQualityMonitor {
    losses: VecDeque<u64>,
    lags: VecDeque<u64>,
    lates: VecDeque<u64>,
    state: Option<LinkQualityState>,
    /// Set by a signal that means "slow" on its own; consumed by `evaluate`.
    slow_alone: bool,
    episode_started_at: u64,
    last_trigger_at: Option<u64>,
}

impl LinkQualityMonitor {
    pub fn state(&self) -> Option<LinkQualityState> {
        self.state
    }

    /// The link dropped: one degraded episode, not one reconnect attempt.
    pub fn note_link_lost(&mut self, at: u64) -> Option<LinkQualityChange> {
        self.losses.push_back(at);
        self.last_trigger_at = Some(at);
        self.evaluate(at)
    }

    /// One echo-lag record; only lags of at least `SLOW_LAG_MIN_MS` count.
    pub fn note_echo_lag(&mut self, at: u64, lag_ms: u64) -> Option<LinkQualityChange> {
        if lag_ms < SLOW_LAG_MIN_MS {
            return None;
        }
        if lag_ms >= SLOW_LAG_ALONE_MS {
            self.slow_alone = true;
        }
        self.lags.push_back(at);
        self.last_trigger_at = Some(at);
        self.evaluate(at)
    }

    /// A host answer the native side gave up waiting on: one is enough.
    pub fn note_late_request(&mut self, at: u64) -> Option<LinkQualityChange> {
        self.slow_alone = true;
        self.lates.push_back(at);
        self.last_trigger_at = Some(at);
        self.evaluate(at)
    }

    /// Time passing is the only thing that ends an episode.
    pub fn poll(&mut self, at: u64) -> Option<LinkQualityChange> {
        self.state?;
        let last = self.last_trigger_at?;
        if at.saturating_sub(last) < LINK_QUALITY_CLEAR_MS {
            return None;
        }
        let after_ms = at.saturating_sub(self.episode_started_at);
        self.state = None;
        self.last_trigger_at = None;
        self.losses.clear();
        self.lags.clear();
        self.lates.clear();
        Some(LinkQualityChange::Cleared { after_ms })
    }

    /// A different host is a different connection; nothing carries over.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn evaluate(&mut self, at: u64) -> Option<LinkQualityChange> {
        prune(&mut self.losses, at, UNSTABLE_WINDOW_MS);
        prune(&mut self.lags, at, SLOW_WINDOW_MS);
        prune(&mut self.lates, at, SLOW_WINDOW_MS);
        let next = if self.losses.len() >= UNSTABLE_LOSS_COUNT {
            Some(LinkQualityState::Unstable)
        } else if self.lags.len() >= SLOW_LAG_COUNT || self.slow_alone {
            Some(LinkQualityState::Slow)
        } else {
            None
        };
        self.slow_alone = false;
        let next = next.filter(|value| Some(*value) != self.state)?;
        // An episode may climb from slow to unstable, never back down inside
        // the same episode: that would be two sentences for one bad network.
        if self.state == Some(LinkQualityState::Unstable) {
            return None;
        }
        if self.state.is_none() {
            self.episode_started_at = at;
        }
        self.state = Some(next);
        Some(LinkQualityChange::Degraded {
            state: next,
            losses: self.losses.len(),
            lag_events: self.lags.len(),
            late_requests: self.lates.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn degraded(
        state: LinkQualityState,
        losses: usize,
        lag_events: usize,
    ) -> Option<LinkQualityChange> {
        Some(LinkQualityChange::Degraded {
            state,
            losses,
            lag_events,
            late_requests: 0,
        })
    }

    #[test]
    fn says_nothing_about_a_single_drop_or_two_an_hour_apart() {
        let mut monitor = LinkQualityMonitor::default();
        assert_eq!(monitor.note_link_lost(0), None);
        assert_eq!(monitor.note_link_lost(UNSTABLE_WINDOW_MS + 1), None);
        assert_eq!(monitor.state(), None);
    }

    #[test]
    fn calls_a_link_that_dropped_twice_inside_the_window_unstable_once() {
        let mut monitor = LinkQualityMonitor::default();
        assert_eq!(monitor.note_link_lost(1_000), None);
        assert_eq!(
            monitor.note_link_lost(20_000),
            degraded(LinkQualityState::Unstable, 2, 0)
        );
        assert_eq!(monitor.note_link_lost(40_000), None);
        assert_eq!(monitor.state(), Some(LinkQualityState::Unstable));
    }

    #[test]
    fn calls_two_late_echoes_inside_the_window_slow() {
        let mut monitor = LinkQualityMonitor::default();
        assert_eq!(monitor.note_echo_lag(0, 1_000), None);
        assert_eq!(
            monitor.note_echo_lag(SLOW_WINDOW_MS, 1_000),
            degraded(LinkQualityState::Slow, 0, 2)
        );
        assert_eq!(monitor.note_echo_lag(SLOW_WINDOW_MS + 1_000, 1_000), None);
    }

    #[test]
    fn climbs_from_slow_to_unstable_and_never_back_down_inside_one_episode() {
        let mut monitor = LinkQualityMonitor::default();
        monitor.note_echo_lag(0, 1_000);
        assert_eq!(
            monitor.note_echo_lag(1_000, 1_000),
            degraded(LinkQualityState::Slow, 0, 2)
        );
        monitor.note_link_lost(2_000);
        assert_eq!(
            monitor.note_link_lost(3_000),
            degraded(LinkQualityState::Unstable, 2, 2)
        );
        assert_eq!(monitor.note_link_lost(4_000 + SLOW_WINDOW_MS), None);
        assert_eq!(monitor.state(), Some(LinkQualityState::Unstable));
    }

    #[test]
    fn clears_only_after_a_quiet_window_and_reports_how_long_it_stood() {
        let mut monitor = LinkQualityMonitor::default();
        monitor.note_link_lost(1_000);
        monitor.note_link_lost(2_000);
        assert_eq!(monitor.poll(2_000 + LINK_QUALITY_CLEAR_MS - 1), None);
        monitor.note_link_lost(2_000 + LINK_QUALITY_CLEAR_MS - 1);
        assert_eq!(monitor.poll(2_000 + LINK_QUALITY_CLEAR_MS + 1), None);
        assert_eq!(
            monitor.poll(2_000 + 2 * LINK_QUALITY_CLEAR_MS),
            Some(LinkQualityChange::Cleared {
                after_ms: 2_000 + 2 * LINK_QUALITY_CLEAR_MS - 2_000,
            })
        );
        assert_eq!(monitor.state(), None);
        assert_eq!(monitor.poll(9_000_000), None);
        assert_eq!(monitor.note_link_lost(9_000_000), None);
    }

    #[test]
    fn carries_nothing_across_a_reset_to_a_different_host() {
        let mut monitor = LinkQualityMonitor::default();
        monitor.note_link_lost(0);
        monitor.reset();
        assert_eq!(monitor.note_link_lost(1_000), None);
        assert_eq!(monitor.state(), None);
    }

    #[test]
    fn names_the_host_and_the_symptom_in_the_users_own_terms() {
        assert_eq!(
            describe_link_quality(LinkQualityState::Unstable, "build-01"),
            "Your connection to build-01 is unstable; Muxflow keeps losing the link and reconnecting."
        );
        assert_eq!(
            describe_link_quality(LinkQualityState::Slow, "build-01"),
            "Your connection to build-01 is slow; the host is answering, but late."
        );
    }

    #[test]
    fn does_not_count_an_echo_late_by_less_than_a_networks_worth() {
        let mut monitor = LinkQualityMonitor::default();
        assert_eq!(monitor.note_echo_lag(0, SLOW_LAG_MIN_MS - 1), None);
        assert_eq!(monitor.note_echo_lag(1_000, SLOW_LAG_MIN_MS - 1), None);
        assert_eq!(monitor.state(), None);
        assert_eq!(monitor.note_echo_lag(2_000, SLOW_LAG_MIN_MS), None);
        assert_eq!(
            monitor.note_echo_lag(3_000, SLOW_LAG_MIN_MS),
            degraded(LinkQualityState::Slow, 0, 2)
        );
    }

    #[test]
    fn calls_the_link_slow_on_one_late_request_or_one_echo_late_by_a_networks_worth() {
        let mut late = LinkQualityMonitor::default();
        assert!(matches!(
            late.note_late_request(0),
            Some(LinkQualityChange::Degraded {
                state: LinkQualityState::Slow,
                ..
            })
        ));
        let mut echo = LinkQualityMonitor::default();
        assert!(matches!(
            echo.note_echo_lag(0, SLOW_LAG_ALONE_MS),
            Some(LinkQualityChange::Degraded {
                state: LinkQualityState::Slow,
                ..
            })
        ));
        let mut mild = LinkQualityMonitor::default();
        assert_eq!(mild.note_echo_lag(0, SLOW_LAG_ALONE_MS - 1), None);
    }
}
