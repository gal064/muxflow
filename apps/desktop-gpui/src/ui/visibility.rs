//! Whether the window is shown, for polls that only feed the screen.
//!
//! A minimized, fully covered or off-Space window draws nothing: GPUI's
//! platforms stop asking it for frames. What still runs is the polls that hand
//! work to the screen, and those slow down while the window is hidden. One flag
//! for the app, because the app has one window.
//!
//! The animation clock in [`super::motion`] watches its own window instead,
//! because it is per window and waits for one.

use gpui_kit::{App, Context, Global, Subscription, Window};

#[derive(Default)]
struct Hidden(bool);

impl Global for Hidden {}

/// Follows `window`'s visibility for as long as the subscription lives. On
/// being shown it redraws in full: output the engine read since the link's
/// last hidden drain has notified no pane yet, and a full redraw makes every
/// pane read its terminal as it is now.
pub fn track(window: &mut Window, cx: &mut App) -> Subscription {
    cx.set_global(Hidden(!window.is_visible()));
    window.observe_window_visibility(|visibility, window, cx| {
        cx.set_global(Hidden(!visibility.is_visible()));
        if visibility.is_visible() {
            window.refresh();
        }
    })
}

/// Calls `shown_again` each time the window is shown, for a poll waiting at
/// its hidden pace to start over.
pub fn on_shown<T: 'static>(
    cx: &mut Context<T>,
    mut shown_again: impl FnMut(&mut T, &mut Context<T>) + 'static,
) -> Subscription {
    cx.observe_global::<Hidden>(move |this, cx| {
        if shown(cx) {
            shown_again(this, cx);
        }
    })
}

/// False while the tracked window is hidden. True when nothing tracks one, as
/// before the window opens.
pub fn shown(cx: &App) -> bool {
    !cx.try_global::<Hidden>().is_some_and(|hidden| hidden.0)
}

#[cfg(test)]
pub(crate) mod tests {
    use gpui_kit::{
        AnyWindowHandle, Context, IntoElement, Render, TestAppContext, WindowVisibility, div, px,
        size,
    };

    use super::*;

    struct Tracked {
        _visibility: Subscription,
    }

    impl Render for Tracked {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    /// A window the flag follows, as the app's own does.
    pub(crate) fn open_tracked_window(cx: &mut TestAppContext) -> AnyWindowHandle {
        let window = cx.open_window(size(px(40.), px(40.)), |window, cx| Tracked {
            _visibility: track(window, cx),
        });
        cx.run_until_parked();
        window.into()
    }

    #[gpui_kit::test]
    fn the_flag_follows_the_window(cx: &mut TestAppContext) {
        assert!(cx.update(|cx| shown(cx)), "no window yet: shown");
        let window = open_tracked_window(cx);
        assert!(cx.update(|cx| shown(cx)));

        cx.simulate_window_visibility_change(window, WindowVisibility::Hidden);
        cx.run_until_parked();
        assert!(!cx.update(|cx| shown(cx)), "minimized: hidden");

        cx.simulate_window_visibility_change(window, WindowVisibility::Visible);
        cx.run_until_parked();
        assert!(cx.update(|cx| shown(cx)), "restored: shown");
    }
}
