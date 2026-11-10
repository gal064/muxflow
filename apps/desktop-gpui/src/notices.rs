//! Status notices. Port of `apps/desktop/src/features/shell/statusNotice.ts`:
//! the app has one status channel that many places write to, and this decides
//! which of its messages a person needs to see.
//!
//! It is a denylist of routine progress rather than an allowlist, because a
//! message nobody classified should be shown, not swallowed. What is shown is a
//! Kit notification. There is one status notice at a time and a new one replaces
//! it. A problem stays until dismissed, because something the app refused to do
//! is the user's to acknowledge. Anything else goes after Kit's own delay.

use std::sync::LazyLock;

use gpui_kit::{
    App, Window,
    component::{WindowExt, notification::Notification},
};
use regex::Regex;

/// Progress the connection already reports through the host row, the
/// disconnected strip and the workspace list. Matched on the whole message.
const ROUTINE: &[&str] = &[
    "Live",
    "Discovering local tmux…",
    "Waiting for authoritative tmux state…",
    "Topology changed; reconciling…",
    "Terminal output is catching up…",
];

/// Ordinary progress with a variable tail.
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

/// Bookkeeping that fails as a consequence of something already reported, such
/// as marking a pane hidden after the link dropped. Matched on the whole shape.
static CONSEQUENTIAL: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        Regex::new(r"^Could not mark %\d+ (hidden|visible): ").expect("a valid pattern"),
        Regex::new(r"^Could not mark agent attention seen: ").expect("a valid pattern"),
    ]
});

/// Words that mean the app refused, failed, or could not do something.
static PROBLEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(\bcannot\b|\bcould not\b|\bunavailable\b|\bfailed\b|\bfailure\b|\berror\b|refus|rejected\b|\bno longer\b|\bnot available\b|\bfrozen\b|\bstopped\b|\btimed out\b|\bunknown\b|\bexceeds\b|\brequire[sd]?\b|\bno agent is waiting\b|\bleft open\b)",
    )
    .expect("a valid pattern")
});

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Info,
    Problem,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusNotice {
    pub message: String,
    pub severity: Severity,
}

pub fn notice_for_status(message: &str) -> Option<StatusNotice> {
    let message = message.trim();
    if message.is_empty()
        || ROUTINE.contains(&message)
        || ROUTINE_PREFIXES
            .iter()
            .any(|prefix| message.starts_with(prefix))
        || CONSEQUENTIAL
            .iter()
            .any(|pattern| pattern.is_match(message))
    {
        return None;
    }
    Some(StatusNotice {
        message: message.to_owned(),
        severity: if PROBLEM.is_match(message) {
            Severity::Problem
        } else {
            Severity::Info
        },
    })
}

/// The status notice's identity: a new one replaces the one showing.
struct StatusNoticeId;

/// Shows `message` if it is worth showing. Returns whether it was.
pub fn notify(message: &str, window: &mut Window, cx: &mut App) -> bool {
    let Some(notice) = notice_for_status(message) else {
        return false;
    };
    let notification = match notice.severity {
        Severity::Info => Notification::info(notice.message),
        Severity::Problem => Notification::error(notice.message).autohide(false),
    };
    window.push_notification(notification.id::<StatusNoticeId>(), cx);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn severity(message: &str) -> Option<Severity> {
        notice_for_status(message).map(|notice| notice.severity)
    }

    #[test]
    fn routine_progress_is_not_shown() {
        for message in [
            "Live",
            "  Live  ",
            "Discovering local tmux…",
            "Connecting to a host",
            "Connection lost, retrying",
            "Opened a file",
            "Renaming workspace…",
            "",
            "   ",
        ] {
            assert_eq!(severity(message), None, "{message:?}");
        }
    }

    #[test]
    fn a_message_that_only_starts_like_progress_still_shows() {
        assert_eq!(
            severity("Live view could not start"),
            Some(Severity::Problem)
        );
        assert_eq!(severity("Connect to the host first"), Some(Severity::Info));
    }

    #[test]
    fn disconnect_bookkeeping_is_not_shown_but_other_failures_are() {
        assert_eq!(
            severity("Could not mark %129 hidden: terminal client is no longer attached"),
            None
        );
        assert_eq!(severity("Could not mark agent attention seen: gone"), None);
        assert_eq!(
            severity("Could not mark the pane hidden: no such pane"),
            Some(Severity::Problem)
        );
    }

    #[test]
    fn refusals_are_problems_and_the_rest_is_information() {
        for message in [
            "Could not save shortcuts: disk full",
            "Close workspace is unavailable: nothing is selected in that panel any more.",
            "Request timed out",
            "Saved shell state is frozen",
            "UNKNOWN command",
        ] {
            assert_eq!(severity(message), Some(Severity::Problem), "{message:?}");
        }
        for message in [
            "Copied the path",
            "Disabled conflicting saved shortcuts: a (b).",
        ] {
            assert_eq!(severity(message), Some(Severity::Info), "{message:?}");
        }
    }

    #[test]
    fn the_message_is_trimmed() {
        assert_eq!(
            notice_for_status("  Copied  ").map(|notice| notice.message),
            Some("Copied".to_owned())
        );
    }
}
