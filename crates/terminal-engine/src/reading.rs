//! Whether a pane is following live output or showing history the user is
//! reading. Port of `TerminalReadingState.ts`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalReadingState {
    Live,
    Reading,
    /// Live bytes arrived after the user left the bottom. They deliberately did
    /// not touch the displayed screen.
    Outdated,
    /// The one authoritative seed requested on the way back to live.
    Refreshing,
}

use TerminalReadingState::{Live, Outdated, Reading, Refreshing};

#[derive(Debug, PartialEq, Eq)]
pub struct ReturnToLiveDecision {
    pub state: TerminalReadingState,
    pub request_seed: bool,
    pub scroll_locally: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct OutputDecision {
    pub state: TerminalReadingState,
    pub render: bool,
    pub became_outdated: bool,
}

pub fn reading_state_for_viewport(
    state: TerminalReadingState,
    at_bottom: bool,
) -> TerminalReadingState {
    if !at_bottom && state == Live {
        Reading
    } else {
        state
    }
}

pub fn reading_state_for_output(state: TerminalReadingState) -> OutputDecision {
    match state {
        Reading => OutputDecision {
            state: Outdated,
            render: false,
            became_outdated: true,
        },
        Outdated => OutputDecision {
            state,
            render: false,
            became_outdated: false,
        },
        _ => OutputDecision {
            state,
            render: state == Live,
            became_outdated: false,
        },
    }
}

pub fn return_to_live(state: TerminalReadingState) -> ReturnToLiveDecision {
    match state {
        Outdated => ReturnToLiveDecision {
            state: Refreshing,
            request_seed: true,
            scroll_locally: false,
        },
        Reading => ReturnToLiveDecision {
            state: Live,
            request_seed: false,
            scroll_locally: true,
        },
        _ => ReturnToLiveDecision {
            state,
            request_seed: false,
            scroll_locally: false,
        },
    }
}

pub fn reading_state_for_authoritative_screen() -> TerminalReadingState {
    Live
}

/// Only live and still-current reading screens may be kept for a resume.
pub fn reading_screen_is_current(state: TerminalReadingState) -> bool {
    matches!(state, Live | Reading)
}

pub fn reading_may_page_history(state: TerminalReadingState) -> bool {
    matches!(state, Live | Reading)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_reading_when_the_viewport_leaves_the_live_bottom() {
        assert_eq!(reading_state_for_viewport(Live, false), Reading);
        assert_eq!(reading_state_for_viewport(Live, true), Live);
    }

    #[test]
    fn keeps_the_first_and_later_live_bytes_out_of_the_reading_screen() {
        assert_eq!(
            reading_state_for_output(Reading),
            OutputDecision {
                state: Outdated,
                render: false,
                became_outdated: true
            }
        );
        assert_eq!(
            reading_state_for_output(Outdated),
            OutputDecision {
                state: Outdated,
                render: false,
                became_outdated: false
            }
        );
        assert_eq!(
            reading_state_for_output(Live),
            OutputDecision {
                state: Live,
                render: true,
                became_outdated: false
            }
        );
    }

    #[test]
    fn returns_immediately_when_nothing_changed_and_requests_one_seed_otherwise() {
        assert_eq!(
            return_to_live(Reading),
            ReturnToLiveDecision {
                state: Live,
                request_seed: false,
                scroll_locally: true
            }
        );
        assert_eq!(
            return_to_live(Outdated),
            ReturnToLiveDecision {
                state: Refreshing,
                request_seed: true,
                scroll_locally: false
            }
        );
        assert_eq!(
            return_to_live(Refreshing),
            ReturnToLiveDecision {
                state: Refreshing,
                request_seed: false,
                scroll_locally: false
            }
        );
    }

    #[test]
    fn accepts_an_authoritative_screen_from_every_state() {
        for state in [Live, Reading, Outdated, Refreshing] {
            assert_eq!(reading_state_for_authoritative_screen(), Live);
            assert_eq!(
                reading_state_for_viewport(state, false),
                if state == Live { Reading } else { state }
            );
        }
    }

    #[test]
    fn never_caches_or_extends_history_after_the_reading_screen_became_stale() {
        for state in [Outdated, Refreshing] {
            assert!(!reading_screen_is_current(state));
            assert!(!reading_may_page_history(state));
        }
        for state in [Live, Reading] {
            assert!(reading_screen_is_current(state));
            assert!(reading_may_page_history(state));
        }
    }
}
