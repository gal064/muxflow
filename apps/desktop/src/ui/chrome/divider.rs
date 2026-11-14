//! The draggable and arrow-key dividers: both rails, the agents section and the
//! docked document. A drag previews without saving and commits once on release,
//! as `transientDrag.ts` does; the release handler lives on the [`Chrome`] view.
//!
//! The band is Kit's base `resize_handle`, the one Kit offers for edges a
//! `ResizablePanelGroup` does not own. It paints nothing itself: the rail's own
//! hairline is the divider, and the cursor is the affordance, as in Tauri.

use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, AppContext, Axis, Context, ElementId, InteractiveElement, IntoElement,
    KeyDownEvent, MouseUpEvent, Render, Role, StatefulInteractiveElement, Styled, WeakEntity,
    Window, accesskit::Orientation, base::HandleEdge, base::resize_handle, canvas, div,
};

use crate::{theme, ui::chrome::Chrome};

/// Which divider a drag carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Divider {
    Sidebar,
    Panel,
    Agents,
    Dock,
}

impl Divider {
    pub fn id(self) -> &'static str {
        match self {
            Divider::Sidebar => "sidebar-resize",
            Divider::Panel => "panel-resize",
            Divider::Agents => "agents-resize",
            Divider::Dock => "dock-resize",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Divider::Sidebar => "Resize the sidebar",
            Divider::Panel => "Resize the panel",
            Divider::Agents => "Resize the agents section",
            Divider::Dock => "Resize the docked document",
        }
    }

    fn axis(self) -> Axis {
        match self {
            Divider::Agents => Axis::Vertical,
            Divider::Sidebar | Divider::Panel | Divider::Dock => Axis::Horizontal,
        }
    }

    /// One arrow key's change to the value, for the keys the divider moves on.
    /// The dock answers in fractions and its sign is the toward-the-side rule.
    pub fn key_delta(self, key: &str, dock_on_right: bool) -> Option<f32> {
        let step = match self {
            Divider::Sidebar | Divider::Panel => 16.,
            Divider::Agents => 0.04,
            Divider::Dock => 0.02,
        };
        match (self, key) {
            (Divider::Sidebar, "left") | (Divider::Panel, "right") => Some(-step),
            (Divider::Sidebar, "right") | (Divider::Panel, "left") => Some(step),
            (Divider::Agents, "down") => Some(-step),
            (Divider::Agents, "up") => Some(step),
            (Divider::Dock, "left") => Some(if dock_on_right { step } else { -step }),
            (Divider::Dock, "right") => Some(if dock_on_right { -step } else { step }),
            _ => None,
        }
    }
}

/// The value `resize_handle` carries, read back by `on_drag_move`.
#[derive(Clone, Copy)]
pub struct DividerDrag(pub Divider);

/// The drag preview is the window itself, so the floating view draws nothing.
struct NoPreview;

impl Render for NoPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_0()
    }
}

/// A divider's band, for a `relative` parent, hugging `edge`.
pub fn divider(
    chrome: WeakEntity<Chrome>,
    divider: Divider,
    edge: HandleEdge,
    dock_on_right: bool,
    value: f32,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let accent = theme::palette(cx).accent_wash;
    let focus = window
        .use_keyed_state(divider.id(), cx, |_, cx| cx.focus_handle().tab_stop(true))
        .read(cx)
        .clone();
    let orientation = match divider.axis() {
        // Kit's axis is the drag's; a side-by-side divider is a vertical line.
        Axis::Horizontal => Orientation::Vertical,
        Axis::Vertical => Orientation::Horizontal,
    };
    resize_handle::<DividerDrag, NoPreview>(ElementId::from(divider.id()), divider.axis())
        .with_appearance(Rc::new(|_, _, _| Some(div().into_any_element())))
        .inside(edge)
        .role(Role::Splitter)
        .aria_label(divider.label())
        .aria_orientation(orientation)
        .aria_numeric_value(f64::from(value))
        .track_focus(&focus)
        // Nothing else marks which divider has the keyboard.
        .focus_visible(move |style| style.bg(accent))
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            let Some(delta) = divider.key_delta(&event.keystroke.key, dock_on_right) else {
                return;
            };
            chrome
                .update(cx, |chrome, cx| chrome.nudge(divider, delta, window, cx))
                .ok();
            cx.stop_propagation();
        })
        .on_drag(DividerDrag(divider), |_, _, _, cx| cx.new(|_| NoPreview))
        .into_any_element()
}

/// Ends a drag on the next mouse up anywhere in the window. Painted only while
/// a drag runs.
pub fn drag_release(chrome: WeakEntity<Chrome>) -> AnyElement {
    canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
                if phase.bubble() {
                    chrome
                        .update(cx, |chrome, cx| chrome.end_drag(window, cx))
                        .ok();
                }
            });
        },
    )
    .absolute()
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrow_keys_grow_and_shrink_like_the_handles_in_tauri() {
        assert_eq!(Divider::Sidebar.key_delta("right", false), Some(16.));
        assert_eq!(Divider::Sidebar.key_delta("up", false), None);
        // The panel's handle is on its left edge, so left widens it.
        assert_eq!(Divider::Panel.key_delta("left", false), Some(16.));
        assert_eq!(Divider::Agents.key_delta("up", false), Some(0.04));
        // A divider moved toward the document's own edge narrows the document.
        assert_eq!(Divider::Dock.key_delta("right", true), Some(-0.02));
        assert_eq!(Divider::Dock.key_delta("left", true), Some(0.02));
    }
}
