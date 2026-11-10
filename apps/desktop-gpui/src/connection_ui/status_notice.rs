//! Which status messages a person needs to see, and for how long. Port of
//! `features/shell/statusNotice.ts`: a denylist of routine chatter, because a
//! message nobody classified should be shown, not swallowed.

use std::sync::LazyLock;

use regex::Regex;

const ROUTINE: &[&str] = &[
    "Live",
    "Discovering local tmux…",
    "Waiting for authoritative tmux state…",
    "Topology changed; reconciling…",
    "Terminal output is catching up…",
];

const ROUTINE_PREFIXES: &[&str] = &[
    "Connecting to ",
    "Connection ",
    "Opened ",
    "Viewing the last known ",
    "Renaming ",
    "Launching ",
    "Resuming ",
    "Branching ",
];

/// Internal bookkeeping that fails as a consequence of something already being
/// reported, matched on the whole message shape.
static CONSEQUENTIAL: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        Regex::new(r"^Could not mark %\d+ (hidden|visible): ").expect("static pattern"),
        Regex::new(r"^Could not mark agent attention seen: ").expect("static pattern"),
    ]
});

/// Words that mean the app refused, failed, or could not do something.
static PROBLEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(\bcannot\b|\bcould not\b|\bunavailable\b|\bfailed\b|\bfailure\b|\berror\b|refus|rejected\b|\bno longer\b|\bnot available\b|\bfrozen\b|\bstopped\b|\btimed out\b|\bunknown\b|\bexceeds\b|\brequire[sd]?\b|\bno agent is waiting\b|\bleft open\b)",
    )
    .expect("static pattern")
});

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeSeverity {
    Info,
    Problem,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusNotice {
    pub message: String,
    pub severity: NoticeSeverity,
}

pub fn notice_for_status(message: &str) -> Option<StatusNotice> {
    let trimmed = message.trim();
    if trimmed.is_empty()
        || ROUTINE.contains(&trimmed)
        || ROUTINE_PREFIXES
            .iter()
            .any(|prefix| trimmed.starts_with(prefix))
        || CONSEQUENTIAL
            .iter()
            .any(|pattern| pattern.is_match(trimmed))
    {
        return None;
    }
    let severity = if PROBLEM.is_match(trimmed) {
        NoticeSeverity::Problem
    } else {
        NoticeSeverity::Info
    };
    Some(StatusNotice {
        message: trimmed.to_owned(),
        severity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn severity(message: &str) -> Option<NoticeSeverity> {
        notice_for_status(message).map(|notice| notice.severity)
    }

    #[test]
    fn shows_anything_it_has_not_been_told_is_routine() {
        assert_eq!(
            notice_for_status("Something nobody thought about")
                .unwrap()
                .message,
            "Something nobody thought about"
        );
        assert_eq!(
            severity("Helper installed. Version 0.2.0."),
            Some(NoticeSeverity::Info)
        );
    }

    #[test]
    fn stays_quiet_for_the_progress_the_rest_of_the_shell_already_shows() {
        for routine in [
            "Live",
            "Discovering local tmux…",
            "Waiting for authoritative tmux state…",
            "Topology changed; reconciling…",
            "Connecting to remote-linux…",
            "Connection reconnecting…",
            "Opened README.md",
            "Branching Codex in a new tab…",
            "Viewing the last known workspace. Writes remain frozen.",
            "   ",
        ] {
            assert_eq!(notice_for_status(routine), None, "{routine}");
        }
    }

    #[test]
    fn classifies_refusals_as_problems_and_problems_do_not_time_out() {
        let refusal = notice_for_status(
            "This action is unavailable until the authoritative connection is live.",
        )
        .unwrap();
        assert_eq!(refusal.severity, NoticeSeverity::Problem);
        let info = notice_for_status("Helper installed. Version 0.2.0.").unwrap();
        assert_eq!(info.severity, NoticeSeverity::Info);
    }

    #[test]
    fn treats_the_messages_the_app_actually_emits_on_failure_as_problems() {
        for message in [
            "Agent Codex one has no exact pane match; navigation is unavailable.",
            "Agent destination Codex one is no longer available: pane closed.",
            "Could not save application tabs; changes will be retried: disk full",
            "A terminal grid of 812 columns exceeds the supported bound; the request was refused.",
            "Downloads require a live file host.",
            "No agent is waiting on you.",
        ] {
            assert_eq!(
                severity(message),
                Some(NoticeSeverity::Problem),
                "{message}"
            );
        }
    }

    #[test]
    fn times_out_a_bulk_closes_receipt_but_not_its_partial_failure() {
        let receipt = notice_for_status("Closed 3 tabs.").unwrap();
        assert_eq!(receipt.severity, NoticeSeverity::Info);
        for partial in [
            "Closed 1 of 2 tabs; 1 could not be closed.",
            "Closed 1 of 2 tabs; 1 still had an agent and was left open.",
        ] {
            assert_eq!(
                severity(partial),
                Some(NoticeSeverity::Problem),
                "{partial}"
            );
        }
    }

    #[test]
    fn does_not_turn_a_disconnects_pane_teardown_into_a_permanent_alert() {
        for message in [
            "Could not mark %129 hidden: terminal client is no longer attached",
            "Could not mark %7 visible: connection closed",
            "Could not mark agent attention seen: request timed out",
        ] {
            assert_eq!(notice_for_status(message), None, "{message}");
        }
        assert_eq!(
            severity("Could not mark the file read-only"),
            Some(NoticeSeverity::Problem)
        );
    }

    #[test]
    fn treats_a_hosts_coded_rejection_as_a_problem_even_without_a_word_boundary() {
        assert_eq!(
            severity(
                "tmux_action_rejected: workspace start directory /x does not exist or is not a directory"
            ),
            Some(NoticeSeverity::Problem)
        );
    }
}
