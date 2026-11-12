//! Motion for indicators that can stay on for a while: the pending-tab pulse.
//!
//! GPUI's `with_animation` asks for another frame on every frame, and a frame
//! re-renders the view that drew the element and every view above it: here the
//! whole chrome. These indicators read one clock instead, which steps every
//! [`STEP`] and wakes only the views that drew one at the last frame. A window
//! that is not shown gets no steps: it is woken when it is shown again.
//! Reduced motion stops the clock, as it stops GPUI's animations.
//!
//! The working-agent mark is [`WorkingIcon`], which does not move at all: a
//! spinner stepped on this clock still cost about 5 % of a core and 3.7 % GPU
//! busy with six agents working and nothing else changing, against the same
//! rows drawn still.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use gpui_kit::{
    App, EntityId, Global, Hsla, IntoElement, ParentElement, RenderOnce, Styled, Subscription,
    Window, WindowId, div, svg,
};

use crate::theme::metrics::ui;

/// One step of the shared clock: 13.3 frames a second.
pub const STEP: Duration = Duration::from_millis(75);

#[derive(Default)]
struct Clock {
    epoch: Option<Instant>,
    windows: HashMap<WindowId, Waiting>,
}

impl Global for Clock {}

/// The views of one window that drew a moving indicator since its last step.
#[derive(Default)]
struct Waiting {
    views: Vec<EntityId>,
    /// A step's timer, or the wait for a hidden window to be shown, is running.
    armed: bool,
    /// While hidden: wakes the views once the window is shown again.
    shown: Option<Subscription>,
}

/// Time on the shared clock, for an indicator drawn now. Wakes the view
/// drawing it at the next step while its window is shown. None under reduced
/// motion: the indicator holds still.
pub fn frame(window: &mut Window, cx: &mut App) -> Option<Duration> {
    if cx.reduce_motion() {
        return None;
    }
    let now = cx.background_executor().now();
    let view = window.current_view();
    let id = window.window_handle().window_id();
    let clock = cx.default_global::<Clock>();
    let elapsed = now.duration_since(*clock.epoch.get_or_insert(now));
    let waiting = clock.windows.entry(id).or_default();
    if !waiting.views.contains(&view) {
        waiting.views.push(view);
    }
    if waiting.armed {
        return Some(elapsed);
    }
    waiting.armed = true;
    if window.is_visible() {
        let into_step = Duration::from_nanos((elapsed.as_nanos() % STEP.as_nanos()) as u64);
        let delay = STEP - into_step;
        cx.spawn(async move |cx| {
            cx.background_executor().timer(delay).await;
            cx.update(|cx| wake(id, cx));
        })
        .detach();
    } else {
        let shown = window.observe_window_visibility(move |visibility, _, cx| {
            if visibility.is_visible() {
                wake(id, cx);
            }
        });
        if let Some(waiting) = cx.global_mut::<Clock>().windows.get_mut(&id) {
            waiting.shown = Some(shown);
        }
    }
    Some(elapsed)
}

fn wake(id: WindowId, cx: &mut App) {
    let Some(waiting) = cx.global_mut::<Clock>().windows.remove(&id) else {
        return;
    };
    for view in waiting.views {
        cx.notify(view);
    }
}

/// A working agent: Tauri's spinner ring, a ring at 35 % with its top quarter
/// in full colour, held still. Nothing about it asks for a frame.
#[derive(IntoElement)]
pub struct WorkingIcon {
    size: f32,
    color: Hsla,
}

impl WorkingIcon {
    /// `size` in CSS pixels, as Tauri's `.spinner` box; the ring is 1.5 px.
    pub fn new(size: f32, color: impl Into<Hsla>) -> Self {
        Self {
            size,
            color: color.into(),
        }
    }
}

impl RenderOnce for WorkingIcon {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let (ring, arc) = ring_svgs(self.size);
        div()
            .relative()
            .flex_none()
            .size(ui(self.size))
            .child(
                svg()
                    .data(ring.as_bytes())
                    .absolute()
                    .size_full()
                    .text_color(self.color.opacity(0.35)),
            )
            .child(
                svg()
                    .data(arc.as_bytes())
                    .absolute()
                    .size_full()
                    .text_color(self.color),
            )
    }
}

/// The ring and its top quarter (the CSS top border, corner to corner), in a
/// box of `size` CSS pixels with a 1.5 px stroke.
fn ring_svgs(size: f32) -> (String, String) {
    let c = size / 2.;
    let r = c - 0.75;
    let d = r * std::f32::consts::FRAC_1_SQRT_2;
    let head = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {size} {size}" fill="none" stroke="#000" stroke-width="1.5">"##
    );
    let ring = format!(r#"{head}<circle cx="{c}" cy="{c}" r="{r}"/></svg>"#);
    let arc = format!(
        r#"{head}<path d="M{x0} {y}A{r} {r} 0 0 1 {x1} {y}"/></svg>"#,
        x0 = c - d,
        x1 = c + d,
        y = c - d,
    );
    (ring, arc)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui_kit::{
        AnyElement, Context, Render, TestAppContext, WindowHandle, WindowVisibility, px, size,
    };

    use super::*;

    /// A view that draws `element` and counts its renders.
    pub(crate) struct Host {
        renders: Rc<Cell<usize>>,
        element: fn() -> AnyElement,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.renders.set(self.renders.get() + 1);
            div().size_full().child((self.element)())
        }
    }

    pub(crate) fn open(
        element: fn() -> AnyElement,
        cx: &mut TestAppContext,
    ) -> (WindowHandle<Host>, Rc<Cell<usize>>) {
        let renders = Rc::new(Cell::new(0));
        let counter = renders.clone();
        let window = cx.open_window(size(px(40.), px(40.)), move |_, _| Host {
            renders: counter,
            element,
        });
        cx.run_until_parked();
        (window, renders)
    }

    /// Renders over one second of the executor's clock, after the first.
    pub(crate) fn renders_in_a_second(renders: &Rc<Cell<usize>>, cx: &mut TestAppContext) -> usize {
        let start = renders.get();
        for _ in 0..100 {
            cx.executor().advance_clock(Duration::from_millis(10));
            cx.run_until_parked();
        }
        renders.get() - start
    }

    /// Next-frame requests: what GPUI's own animations make on every frame.
    pub(crate) fn frames_requested(window: &WindowHandle<Host>, cx: &mut TestAppContext) -> usize {
        let count = window
            .update(cx, |_, window, cx| window.simulate_next_frame(cx))
            .unwrap();
        cx.run_until_parked();
        count
    }

    /// An element on the clock, as the pending-tab pulse is.
    #[derive(IntoElement)]
    struct Ticker;

    impl RenderOnce for Ticker {
        fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
            frame(window, cx);
            div().size(px(4.))
        }
    }

    fn ticker() -> AnyElement {
        Ticker.into_any_element()
    }

    #[gpui_kit::test]
    fn the_clock_asks_for_at_most_fifteen_frames_a_second(cx: &mut TestAppContext) {
        let (window, renders) = open(ticker, cx);
        let per_second = renders_in_a_second(&renders, cx);
        assert!(
            (10..=15).contains(&per_second),
            "{per_second} frames a second"
        );
        assert_eq!(frames_requested(&window, cx), 0, "no every-frame animation");
    }

    #[gpui_kit::test]
    fn a_hidden_window_asks_for_no_frames_until_shown(cx: &mut TestAppContext) {
        let (window, renders) = open(ticker, cx);
        cx.simulate_window_visibility_change(window.into(), WindowVisibility::Hidden);
        // The step already armed may still land once.
        cx.executor().advance_clock(STEP * 2);
        cx.run_until_parked();
        assert_eq!(renders_in_a_second(&renders, cx), 0, "hidden: no frames");

        cx.simulate_window_visibility_change(window.into(), WindowVisibility::Visible);
        cx.run_until_parked();
        assert!(
            renders_in_a_second(&renders, cx) >= 10,
            "shown: it steps again"
        );
    }

    #[gpui_kit::test]
    fn reduced_motion_stops_the_clock(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, renders) = open(ticker, cx);
        assert_eq!(renders_in_a_second(&renders, cx), 0);
        assert_eq!(frames_requested(&window, cx), 0);
    }

    #[gpui_kit::test]
    fn the_working_icon_asks_for_no_frames(cx: &mut TestAppContext) {
        fn working() -> AnyElement {
            WorkingIcon::new(9., gpui_kit::rgba(0x808080ff)).into_any_element()
        }
        let (window, renders) = open(working, cx);
        assert_eq!(renders_in_a_second(&renders, cx), 0);
        assert_eq!(frames_requested(&window, cx), 0);
    }

    #[gpui_kit::test]
    fn the_clock_stops_when_nothing_on_it_is_drawn(cx: &mut TestAppContext) {
        fn nothing() -> AnyElement {
            div().into_any_element()
        }
        let (_window, renders) = open(nothing, cx);
        assert_eq!(renders_in_a_second(&renders, cx), 0);
    }

    #[test]
    fn the_quarter_arc_is_the_top_border() {
        let (ring, arc) = ring_svgs(9.);
        assert!(
            ring.contains(r#"<circle cx="4.5" cy="4.5" r="3.75"/>"#),
            "{ring}"
        );
        // From the top-left diagonal to the top-right one, clockwise.
        assert!(arc.contains("A3.75 3.75 0 0 1"), "{arc}");
        assert!(arc.contains(r#"viewBox="0 0 9 9""#), "{arc}");
    }
}
