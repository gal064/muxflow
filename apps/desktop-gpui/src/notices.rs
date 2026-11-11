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
    Anchor, AnyElement, App, Window,
    component::{
        ActiveTheme, Theme, WindowExt, button::Button, h_flex, notification::Notification, v_flex,
    },
    div,
    prelude::*,
    relative,
};
use regex::Regex;

use crate::{
    theme::{
        self, Palette,
        metrics::{NARROW_DIALOGS, narrower_than, ui},
    },
    ui::{surface_error::SurfaceError, toast},
};

/// Matches the height of the Tauri notice's error details.
const PROBLEM_MAX_HEIGHT: f32 = 132.;

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

/// A notice as `.toast` draws it: the text, then bordered buttons ending in Dismiss. A
/// problem has the danger border. `actions` are the buttons before Dismiss.
pub fn card(
    problem: bool,
    narrow: bool,
    palette: &Palette,
    text: impl Fn() -> AnyElement + 'static,
    actions: impl Fn(&App) -> Vec<Button> + 'static,
) -> Notification {
    let palette = *palette;
    // Kit stretches a notice to the stack's width; a `.toast` is as wide as its text,
    // except in a narrow window, which spans it.
    toast::card(Notification::new(), problem, &palette)
        .when(!narrow, |card| card.w_auto().self_end())
        // A problem stays until dismissed: it is the user's to acknowledge.
        .autohide(!problem)
        .content(move |_, _, cx| {
            let notice = cx.entity();
            let dismiss = toast::button(
                Button::new("dismiss-notice")
                    .label("Dismiss")
                    .on_click(move |_, window, cx| {
                        notice.update(cx, |notice, cx| notice.dismiss(window, cx))
                    }),
                &palette,
            );
            v_flex()
                .gap(ui(5.))
                .line_height(relative(toast::LINE))
                .child(text())
                .child(
                    h_flex()
                        .gap(ui(6.))
                        .children(
                            actions(cx)
                                .into_iter()
                                .map(|button| toast::button(button, &palette)),
                        )
                        .child(dismiss),
                )
                .into_any_element()
        })
}

/// A problem as a sentence with the host's text behind Details, as Tauri's
/// toast drew `SurfaceError`. An error text can be any length, so its card
/// scrolls instead of growing past the window and taking its Dismiss button
/// with it.
fn problem_card(message: String, narrow: bool, palette: &Palette) -> Notification {
    card(
        true,
        narrow,
        palette,
        move || {
            div()
                .id("problem-notice-text")
                .max_h(ui(PROBLEM_MAX_HEIGHT))
                .overflow_y_scroll()
                // The card itself is the alert, so the text inside it is not one.
                .child(
                    SurfaceError::new("problem-notice-error", message.clone())
                        .no_role()
                        .bare(),
                )
                .into_any_element()
        },
        |_| Vec::new(),
    )
}

/// Kit's notices sit top right at one width; `styles.css` puts a `.toast` at the bottom
/// right, and a 600 CSS px or narrower window spans it 8 px in from each side. Called
/// each frame from the shell, and writes only on a change: the theme is shared and
/// observed.
pub fn fit_to(window: &Window, cx: &mut App) {
    let (width, margin) = if narrower_than(NARROW_DIALOGS, window) {
        let margin = ui(toast::NARROW_MARGIN).to_pixels(window.rem_size());
        (window.viewport_size().width - margin * 2., margin)
    } else {
        let margin = ui(toast::MARGIN).to_pixels(window.rem_size());
        (ui(toast::WIDTH).to_pixels(window.rem_size()), margin)
    };
    let current = &cx.theme().notification;
    if current.placement != Anchor::BottomRight
        || current.width != width
        || current.margins.right != margin
        || current.margins.bottom != margin
    {
        let settings = &mut Theme::global_mut(cx).notification;
        settings.placement = Anchor::BottomRight;
        settings.width = width;
        settings.margins.right = margin;
        settings.margins.bottom = margin;
    }
}

/// The status notice's identity: a new one replaces the one showing.
struct StatusNoticeId;

/// Shows `message` if it is worth showing. Returns whether it was.
pub fn notify(message: &str, window: &mut Window, cx: &mut App) -> bool {
    let Some(notice) = notice_for_status(message) else {
        return false;
    };
    let palette = theme::palette(cx);
    let narrow = narrower_than(NARROW_DIALOGS, window);
    let notification = match notice.severity {
        // A finished download offers to open what it wrote.
        Severity::Info
            if let Some(download) = crate::files::download_notice(&notice.message, narrow, cx) =>
        {
            download
        }
        Severity::Info => {
            let message = notice.message;
            card(
                false,
                narrow,
                &palette,
                move || {
                    div()
                        .text_color(palette.chrome_dim)
                        .child(message.clone())
                        .into_any_element()
                },
                |_| Vec::new(),
            )
        }
        Severity::Problem => problem_card(notice.message, narrow, &palette),
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
            assert_eq!(severity(message), Some(Severity::Problem), "{message}");
        }
    }

    #[test]
    fn times_out_a_bulk_closes_receipt_but_not_its_partial_failure() {
        let receipt = notice_for_status("Closed 3 tabs.").unwrap();
        assert_eq!(receipt.severity, Severity::Info);
        for partial in [
            "Closed 1 of 2 tabs; 1 could not be closed.",
            "Closed 1 of 2 tabs; 1 still had an agent and was left open.",
        ] {
            assert_eq!(severity(partial), Some(Severity::Problem), "{partial}");
        }
    }

    #[test]
    fn treats_a_hosts_coded_rejection_as_a_problem_even_without_a_word_boundary() {
        assert_eq!(
            severity(
                "tmux_action_rejected: workspace start directory /x does not exist or is not a directory"
            ),
            Some(Severity::Problem)
        );
    }
}
