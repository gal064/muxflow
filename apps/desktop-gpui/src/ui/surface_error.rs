//! The one shape every rejection takes: a sentence, then the host's diagnostic
//! behind a disclosure. Port of `ui/SurfaceError.tsx`.
//!
//! The disclosure is real rather than a truncation, so the exact text a bug
//! report needs is one click away and still selectable. It opens closed on
//! every new error: the open state remembers which detail was opened, so a
//! fresh rejection never inherits the previous one's.

use std::rc::Rc;

use gpui_kit::{
    App, InteractiveElement, IntoElement, ParentElement, RenderOnce, Role, SharedString,
    StatefulInteractiveElement, Styled, Window,
    base::TestSupportExt,
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
    },
    div,
    prelude::FluentBuilder,
};

use crate::{
    fonts,
    theme::{
        self,
        metrics::{RADIUS_CONTROL, TYPE_META, TYPE_SECONDARY, ui},
    },
    ui::error_summary::{self, Summary},
};

type Dismiss = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct SurfaceError {
    id: SharedString,
    raw: String,
    summary: Option<String>,
    role: Option<Role>,
    dismiss: Option<Dismiss>,
    bare: bool,
}

impl SurfaceError {
    /// `raw` is whatever the surface caught, verbatim.
    pub fn new(id: impl Into<SharedString>, raw: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            raw: raw.into(),
            summary: None,
            role: Some(Role::Alert),
            dismiss: None,
            bare: false,
        }
    }

    /// A sentence the surface knows to be better than any derived from the
    /// text, such as what state a helper rollback left the host in. The raw
    /// text still goes behind the disclosure.
    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    /// For a container that already announces itself, such as a problem
    /// notice: an alert inside an alert would be read twice.
    pub fn no_role(mut self) -> Self {
        self.role = None;
        self
    }

    /// Without the padding, wash and size of its own, for a card that is already the
    /// container: `.toast-body`.
    pub fn bare(mut self) -> Self {
        self.bare = true;
        self
    }

    /// Makes the banner closeable, for a surface where nothing else clears it.
    pub fn on_dismiss(mut self, dismiss: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.dismiss = Some(Rc::new(dismiss));
        self
    }
}

/// The sentence and the disclosed text, as `SurfaceError.tsx` derives them.
pub fn headline(raw: &str, summary: Option<&str>) -> Summary {
    match summary {
        Some(summary) => Summary {
            summary: summary.to_owned(),
            detail: (raw.trim() != summary).then(|| raw.trim().to_owned()),
        },
        None => error_summary::summarize(raw),
    }
}

/// GPUI breaks a line after any hyphen, so `git push -u` could wrap between the
/// dash and its letter, where a browser keeps a flag whole. A non-breaking
/// hyphen keeps it together; the accessible name keeps the plain text.
fn keep_flags_whole(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut before: Option<char> = None;
    while let Some(c) = chars.next() {
        // A dash that opens a flag: after a space or its own first dash, before
        // a letter or the second dash. `well-known` is left alone.
        let flag = c == '-'
            && before.is_some_and(|b| b.is_whitespace() || b == '-')
            && chars
                .peek()
                .is_some_and(|next| next.is_alphanumeric() || *next == '-');
        kept.push(if flag { '\u{2011}' } else { c });
        before = Some(c);
    }
    kept
}

impl RenderOnce for SurfaceError {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = theme::palette(cx);
        let id = self.id;
        let Summary { summary, detail } = headline(&self.raw, self.summary.as_deref());
        let opened =
            window.use_keyed_state(SharedString::from(format!("{id}-details")), cx, |_, _| {
                None::<String>
            });
        // A different detail closes it, so coming back to an earlier one
        // never finds it open.
        if opened.read(cx).is_some() && opened.read(cx).as_deref() != detail.as_deref() {
            opened.update(cx, |held, _| *held = None);
        }
        let open = detail.is_some() && opened.read(cx).as_deref() == detail.as_deref();
        let banner = div()
            .id(id.clone())
            .test_support()
            .when_some(self.role, |this, role| this.role(role))
            .aria_label(SharedString::from(summary.clone()))
            .flex()
            .flex_col()
            .items_start()
            .gap(ui(4.))
            .text_color(palette.danger_ink)
            .when(!self.bare, |this| {
                this.px(ui(8.))
                    .py(ui(6.))
                    .rounded(ui(RADIUS_CONTROL))
                    .bg(palette.danger_wash)
                    .text_size(ui(TYPE_SECONDARY))
            });
        let toggle = detail.clone().map(|detail| {
            Button::new(SharedString::from(format!("{id}-toggle")))
                .ghost()
                .xsmall()
                .label(if open { "▾ Details" } else { "▸ Details" })
                .on_click(move |_, _, cx| {
                    opened.update(cx, |held, cx| {
                        *held = (!open).then(|| detail.clone());
                        cx.notify();
                    })
                })
        });
        let dismiss = self.dismiss.map(|dismiss| {
            Button::new(SharedString::from(format!("{id}-dismiss")))
                .ghost()
                .xsmall()
                .label("Dismiss")
                .accessibility_label("Dismiss the error")
                .on_click(move |_, window, cx| dismiss(window, cx))
        });
        banner
            .child(keep_flags_whole(&summary))
            .when(toggle.is_some() || dismiss.is_some(), |this| {
                this.child(h_flex().gap(ui(4.)).children(toggle).children(dismiss))
            })
            .when_some(detail.filter(|_| open), |this, detail| {
                this.child(
                    div()
                        .id(SharedString::from(format!("{id}-detail")))
                        .test_support()
                        .role(Role::Paragraph)
                        .aria_label(SharedString::from(detail.clone()))
                        .max_h(ui(132.))
                        .overflow_y_scroll()
                        .text_color(palette.chrome_ink)
                        .font_family(fonts::MONO)
                        .text_size(ui(TYPE_META))
                        .child(detail),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_is_not_broken_after_its_dash() {
        assert_eq!(
            keep_flags_whole("git push --set-upstream"),
            "git push \u{2011}\u{2011}set-upstream"
        );
        assert_eq!(
            keep_flags_whole("run git push -u once, a well-known step"),
            "run git push \u{2011}u once, a well-known step"
        );
    }

    #[test]
    fn a_given_sentence_leads_and_the_raw_text_is_disclosed() {
        let shown = headline(
            "  helper rollback failed: ENOENT ",
            Some("Upgrade and rollback both failed."),
        );
        assert_eq!(shown.summary, "Upgrade and rollback both failed.");
        assert_eq!(
            shown.detail.as_deref(),
            Some("helper rollback failed: ENOENT")
        );
    }

    #[test]
    fn a_given_sentence_equal_to_the_text_discloses_nothing() {
        let shown = headline(
            "The helper check failed. ",
            Some("The helper check failed."),
        );
        assert_eq!(shown.detail, None);
    }

    #[test]
    fn without_a_sentence_the_summary_is_derived() {
        let shown = headline("git_rejected: no upstream", None);
        assert_eq!(shown, error_summary::summarize("git_rejected: no upstream"));
    }

    use std::{cell::Cell, path::Path};

    use gpui_kit::{
        Context, Render, TestAppContext,
        test::{TestQueryExt, TestWindowExt},
    };

    struct Host {
        raw: String,
        dismissed: Rc<Cell<bool>>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let dismissed = self.dismissed.clone();
            div().size_full().child(
                SurfaceError::new("push-error", self.raw.clone())
                    .on_dismiss(move |_, _| dismissed.set(true)),
            )
        }
    }

    const NO_UPSTREAM: &str =
        "git_no_upstream: fatal: The current branch main has no upstream branch.";

    #[gpui_kit::test]
    fn a_push_without_upstream_reads_as_a_sentence_with_details(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::app::init(crate::app::Core::at(Path::new("/nonexistent/s.json")), cx);
        });
        let dismissed = Rc::new(Cell::new(false));
        let (host, cx) = cx.add_window_view(|_, _| Host {
            raw: NO_UPSTREAM.into(),
            dismissed: dismissed.clone(),
        });
        let sentence =
            "This branch has no upstream yet; run git push -u in a terminal once, then retry.";
        cx.update(|window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find_by_label(sentence).role(), Some(Role::Alert));
            assert!(window.try_find_by_label(NO_UPSTREAM).is_none(), "closed");
            window.click("push-error-toggle", cx);
            window.render_frame(cx);
            assert_eq!(
                window.find_by_label(NO_UPSTREAM).role(),
                Some(Role::Paragraph)
            );
            window.click("push-error-dismiss", cx);
        });
        assert!(dismissed.get());
        // A new rejection opens closed.
        host.update(cx, |host, cx| {
            host.raw = "git_push_rejected: ! [rejected] main -> main".into();
            cx.notify();
        });
        cx.update(|window, cx| {
            window.render_frame(cx);
            let rejected = "git_push_rejected: ! [rejected] main -> main";
            assert!(window.try_find_by_label(rejected).is_none());
            assert!(window.try_find_by_label(NO_UPSTREAM).is_none());
            // It has Details of its own, which open on request.
            window.click("push-error-toggle", cx);
            window.render_frame(cx);
            assert!(window.try_find_by_label(rejected).is_some());
            window.click("push-error-toggle", cx);
        });
        // Nor does coming back to the one that was open.
        host.update(cx, |host, cx| {
            host.raw = NO_UPSTREAM.into();
            cx.notify();
        });
        cx.update(|window, cx| {
            window.render_frame(cx);
            assert!(window.try_find_by_label(NO_UPSTREAM).is_none());
        });
    }
}
