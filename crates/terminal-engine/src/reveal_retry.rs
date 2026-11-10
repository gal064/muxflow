//! What a failed pane reveal means, and whether it is worth trying again. Port
//! of `revealRetry.ts`.
//!
//! Matched as substrings because the native side returns these as plain
//! strings. Each is a literal copy of a sentence in `desktop-core`'s
//! `connection.rs`, or its structured code.

pub const TRANSIENT_REVEAL_ERRORS: [&str; 3] = [
    "host bridge is disconnected",
    "terminal client is no longer attached",
    "connection_unavailable",
];

const LATE_REVEAL_ERROR: &str = "host request timed out";

/// The one refusal a retry can never satisfy: nothing between attempts can
/// re-stamp the checkpoint, because only the frame still in flight carries the
/// new epoch.
pub const STALE_REVEAL_EPOCH_CODE: &str = "terminal_visibility_epoch_rejected";

pub(crate) const REVEAL_RETRY_DELAY_MS: u64 = 250;
pub const REVEAL_RETRY_LIMIT: u32 = 8;

pub fn is_transient_reveal_error(error: &str) -> bool {
    TRANSIENT_REVEAL_ERRORS
        .iter()
        .any(|phrase| error.contains(phrase))
}

pub fn is_stale_epoch_reveal_error(error: &str) -> bool {
    error.contains(STALE_REVEAL_EPOCH_CODE)
}

/// Failures that are the link's state, not news for the user.
pub fn is_quiet_reveal_error(error: &str) -> bool {
    is_transient_reveal_error(error) || error.contains(LATE_REVEAL_ERROR)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevealFailureAction {
    Ignore,
    Retry,
    Rebuild,
    Degrade,
}

pub fn reveal_failure_action(error: &str, current: bool, retries_used: u32) -> RevealFailureAction {
    if !current {
        return RevealFailureAction::Ignore;
    }
    if is_stale_epoch_reveal_error(error) {
        return RevealFailureAction::Rebuild;
    }
    if !is_transient_reveal_error(error) || retries_used >= REVEAL_RETRY_LIMIT {
        return RevealFailureAction::Degrade;
    }
    RevealFailureAction::Retry
}

#[cfg(test)]
mod tests {
    use super::*;
    use RevealFailureAction::{Degrade, Ignore, Rebuild, Retry};

    fn stale_epoch() -> String {
        format!(
            "{STALE_REVEAL_EPOCH_CODE}: terminal visibility checkpoint belongs to a stale connection epoch"
        )
    }

    #[test]
    fn recognizes_every_native_refusal_that_means_the_transport_is_not_ready() {
        assert!(is_transient_reveal_error("host bridge is disconnected"));
        assert!(is_transient_reveal_error(
            "terminal client is no longer attached"
        ));
        assert!(is_transient_reveal_error(
            "connection_unavailable: host connection is disconnected or reconciling"
        ));
    }

    #[test]
    fn treats_anything_else_as_a_real_failure() {
        assert!(!is_transient_reveal_error("visibility conflict"));
        assert!(!is_transient_reveal_error(
            "pane %1 is unknown to this session"
        ));
    }

    #[test]
    fn no_longer_counts_a_stale_checkpoint_epoch_as_transient() {
        assert!(!is_transient_reveal_error(&stale_epoch()));
        assert!(is_stale_epoch_reveal_error(&stale_epoch()));
        assert!(!is_stale_epoch_reveal_error("host bridge is disconnected"));
    }

    #[test]
    fn ignores_a_failure_whose_attempt_has_been_superseded_transient_or_not() {
        assert_eq!(
            reveal_failure_action("host bridge is disconnected", false, 0),
            Ignore
        );
        assert_eq!(reveal_failure_action(&stale_epoch(), false, 0), Ignore);
        assert_eq!(
            reveal_failure_action("visibility conflict", false, 0),
            Ignore
        );
    }

    #[test]
    fn retries_a_transient_failure_while_the_attempt_has_budget_left() {
        let error = "host bridge is disconnected";
        assert_eq!(reveal_failure_action(error, true, 0), Retry);
        assert_eq!(
            reveal_failure_action(error, true, REVEAL_RETRY_LIMIT - 1),
            Retry
        );
        assert_eq!(
            reveal_failure_action(error, true, REVEAL_RETRY_LIMIT),
            Degrade
        );
    }

    #[test]
    fn rebuilds_a_stale_epoch_refusal_on_its_first_failure_whatever_the_budget() {
        assert_eq!(reveal_failure_action(&stale_epoch(), true, 0), Rebuild);
        assert_eq!(reveal_failure_action(&stale_epoch(), true, 3), Rebuild);
    }

    #[test]
    fn degrades_a_real_conflict_on_its_first_failure() {
        assert_eq!(
            reveal_failure_action("visibility conflict", true, 0),
            Degrade
        );
    }

    #[test]
    fn retries_the_not_ready_refusal_instead_of_degrading_on_it() {
        let error = "connection_unavailable: host connection is disconnected or reconciling";
        assert_eq!(reveal_failure_action(error, true, 0), Retry);
        assert_eq!(
            reveal_failure_action(error, true, REVEAL_RETRY_LIMIT),
            Degrade
        );
    }

    #[test]
    fn late_answers_are_quiet_but_not_resent_on_the_fast_ladder() {
        let late = "host request timed out; the link was kept, but commit outcome is unknown and the request will not be replayed";
        assert!(is_quiet_reveal_error(late));
        assert!(is_quiet_reveal_error("host bridge is disconnected"));
        assert!(!is_quiet_reveal_error("visibility conflict"));
        assert_eq!(reveal_failure_action(late, true, 0), Degrade);
    }
}
