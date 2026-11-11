//! Dragging a tab, a document or a pane onto a tab, the tab strip, a workspace
//! row or a pane edge. Port of `features/dragDrop`.
//!
//! One pointer gesture of ours rather than GPUI's drag and drop: a pane grab
//! starts inside the terminal, which must not see the press, and a drag has to
//! keep every other element from reacting to it. A press arms the drag; it
//! starts once the pointer has travelled [`DRAG_THRESHOLD_PX`], so clicks keep
//! working; the release drops it where the pointer last was.
//!
//! The window-level listeners ([`listeners`]) follow the pointer. Each target
//! offers itself while the pointer is over it ([`offer`]); the first offer in a
//! move wins, so an inner target beats the one around it. [`rules`] decides what
//! a drop on a target would do, and the shell carries it out.

pub mod rules;

use std::time::{Duration, Instant};

use gpui_kit::{
    AnyElement, App, DispatchPhase, FocusHandle, Global, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point,
    Styled, Window, anchored, canvas, deferred, div, point, prelude::FluentBuilder, px,
};

use crate::{
    terminal::integration::frames::DRAG_THRESHOLD_PX,
    theme::{
        self,
        metrics::{TYPE_MENU, ui},
    },
    workspaces,
};

pub use rules::{DragPayload, DropPlan, DropTarget, WorkspaceRef};

/// A press that may become a drag, and the drag it became.
struct Active {
    payload: DragPayload,
    position: Point<Pixels>,
    target: Option<DropTarget>,
    /// Whether releasing on `target` would do something, decided when first
    /// asked: markers are drawn only then.
    allowed: std::cell::Cell<Option<bool>>,
}

#[derive(Default)]
pub struct PointerDrag {
    armed: Option<(DragPayload, Point<Pixels>)>,
    active: Option<Active>,
    /// A target was offered during this move.
    offered: bool,
    /// Escape ended a drag with the button still down: the release that
    /// follows is not a click on whatever is under it.
    swallow_release: bool,
    ended_at: Option<Instant>,
    /// What had the keyboard when the button went down: a drag is not a visit.
    focus_at_press: Option<FocusHandle>,
}

impl Global for PointerDrag {}

/// What the pointer's movement did to the gesture.
#[derive(Debug, PartialEq, Eq)]
pub enum Moved {
    Nothing,
    /// A drag is under way, and what it shows has to follow the pointer.
    Dragging,
    /// A release this gesture never saw: the drag is over.
    Ended,
}

/// What the pointer's release did to the gesture.
#[derive(Debug, PartialEq)]
pub enum Release {
    /// Not a drag's release: the click it is goes ahead.
    Passes,
    /// The release after an Escape, which is no click.
    Swallowed,
    Drop {
        payload: DragPayload,
        target: Option<DropTarget>,
    },
}

impl PointerDrag {
    /// Arms a drag from a primary-button press. Nothing happens until the
    /// pointer travels [`DRAG_THRESHOLD_PX`], so the press stays an ordinary
    /// click (and a Shift-click stays a pin) when it does not.
    pub fn press(&mut self, payload: Option<DragPayload>, position: Point<Pixels>) {
        // A new press: the release an Escape was waiting for is not coming.
        self.swallow_release = false;
        if self.active.is_none()
            && let Some(payload) = payload
        {
            self.armed = Some((payload, position));
        }
    }

    pub fn moved(&mut self, position: Point<Pixels>, primary_held: bool) -> Moved {
        self.offered = false;
        if !primary_held {
            // A release this gesture never saw: a later plain move is no drag,
            // and the release an Escape was waiting for is not coming.
            self.armed = None;
            self.swallow_release = false;
            return if self.finish() {
                Moved::Ended
            } else {
                Moved::Nothing
            };
        }
        if let Some((payload, origin)) = &self.armed
            && (position - *origin).magnitude() >= f64::from(DRAG_THRESHOLD_PX)
        {
            self.active = Some(Active {
                payload: payload.clone(),
                position,
                target: None,
                allowed: Default::default(),
            });
            self.armed = None;
        }
        match &mut self.active {
            Some(active) => {
                active.position = position;
                // Offered again by whatever the pointer is over now.
                active.target = None;
                active.allowed.set(None);
                Moved::Dragging
            }
            None => Moved::Nothing,
        }
    }

    /// A target the pointer is over. The first of a move wins, so the innermost
    /// target beats the one around it; a dock zone beats a pane edge it covers.
    pub fn offer(&mut self, target: DropTarget) {
        let Some(active) = &mut self.active else {
            return;
        };
        if self.offered && !matches!(target, DropTarget::Dock(_)) {
            return;
        }
        self.offered = true;
        active.target = Some(target);
        active.allowed.set(None);
    }

    pub fn release(&mut self) -> Release {
        self.armed = None;
        let Some(active) = self.active.take() else {
            return if std::mem::take(&mut self.swallow_release) {
                self.ended_at = Some(Instant::now());
                Release::Swallowed
            } else {
                Release::Passes
            };
        };
        self.ended_at = Some(Instant::now());
        Release::Drop {
            payload: active.payload,
            target: active.target,
        }
    }

    /// Escape: ends a drag under way, and says whether it did. The button is
    /// still down, so the release to come is swallowed.
    pub fn cancel(&mut self) -> bool {
        self.armed = None;
        if self.finish() {
            self.swallow_release = true;
            true
        } else {
            false
        }
    }

    fn finish(&mut self) -> bool {
        let was = self.active.take().is_some();
        if was {
            self.ended_at = Some(Instant::now());
        }
        was
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }
}

/// Arms a drag, from a press on a source.
pub fn press(payload: Option<DragPayload>, position: Point<Pixels>, cx: &mut App) {
    cx.default_global::<PointerDrag>().press(payload, position);
}

/// A target says the pointer is over it.
pub fn offer(target: DropTarget, cx: &mut App) {
    if let Some(drag) = cx.try_global::<PointerDrag>()
        && drag.is_active()
    {
        cx.global_mut::<PointerDrag>().offer(target);
    }
}

pub fn active(cx: &App) -> bool {
    cx.try_global::<PointerDrag>()
        .is_some_and(PointerDrag::is_active)
}

/// The payload being dragged, while a drag is under way.
pub fn payload(cx: &App) -> Option<&DragPayload> {
    cx.try_global::<PointerDrag>()?
        .active
        .as_ref()
        .map(|active| &active.payload)
}

/// When the last drag ended: the pane a drop lands beside is no hover.
pub fn ended_at(cx: &App) -> Option<Instant> {
    cx.try_global::<PointerDrag>()?.ended_at
}

/// Whether a drag ended in this very event: the click a source would get from
/// the release that dropped it is no click.
pub fn just_ended(cx: &App) -> bool {
    ended_at(cx).is_some_and(|ended| ended.elapsed() < Duration::from_millis(100))
}

/// The target a drop would do something on, and what is dragged: what the
/// markers draw. Only a drop that would act has a marker.
pub fn marker(cx: &App) -> Option<(&DragPayload, &DropTarget)> {
    let active = cx.try_global::<PointerDrag>()?.active.as_ref()?;
    let target = active.target.as_ref()?;
    let allowed = active.allowed.get().unwrap_or_else(|| {
        let allowed = workspaces::moves::plan_for(&active.payload, target, cx)
            .is_some_and(|plan| !matches!(plan, DropPlan::Refuse(_)));
        active.allowed.set(Some(allowed));
        allowed
    });
    allowed.then_some((&active.payload, target))
}

/// Every drag's keys: Escape ends one, and is spent on it.
pub fn init(cx: &mut App) {
    cx.set_global(PointerDrag::default());
    cx.intercept_keystrokes(|event, window, cx| {
        if event.keystroke.key == "escape" && cx.global_mut::<PointerDrag>().cancel() {
            restore_focus(window, cx);
            window.refresh();
            cx.stop_propagation();
        }
    })
    .detach();
}

/// A drag is not a visit: the press focused the tab or row it started on, and
/// left there the next Space or Enter would activate it and typing would not
/// reach the terminal. Focus goes back where it was.
fn restore_focus(window: &mut Window, cx: &mut App) {
    let saved = cx.global_mut::<PointerDrag>().focus_at_press.take();
    if let Some(handle) = saved
        && !handle.is_focused(window)
    {
        window.focus(&handle, cx);
    }
}

/// The window-level listeners that follow the pointer. Painted first in the
/// window, so that on a move they run before any target offers itself.
pub fn listeners() -> AnyElement {
    canvas(
        |_, _, _| (),
        |_, _, window, _| {
            window.on_mouse_event(|event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                    cx.global_mut::<PointerDrag>().focus_at_press = window.focused(cx);
                }
            });
            window.on_mouse_event(|event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                let held = event.pressed_button == Some(MouseButton::Left);
                match cx.global_mut::<PointerDrag>().moved(event.position, held) {
                    Moved::Nothing => {}
                    Moved::Dragging => window.refresh(),
                    Moved::Ended => {
                        restore_focus(window, cx);
                        window.refresh();
                    }
                }
            });
            window.on_mouse_event(|_: &MouseUpEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                match cx.global_mut::<PointerDrag>().release() {
                    Release::Passes => {}
                    // The gesture is the press and the release: no click follows
                    // it. Sources check [`just_ended`] rather than this stopping
                    // the release, which would skip every other element's own
                    // cleanup of its press.
                    Release::Swallowed => {}
                    Release::Drop { payload, target } => {
                        restore_focus(window, cx);
                        window.refresh();
                        if let Some(target) = target {
                            workspaces::moves::drop_on(&payload, &target, cx);
                        }
                    }
                }
            });
        },
    )
    .absolute()
    .into_any_element()
}

/// What is being dragged, following the pointer, for as long as a drag lasts;
/// dimmed over a refusal. Port of `DragGhost`.
pub fn ghost(cx: &App) -> Option<AnyElement> {
    let active = cx.try_global::<PointerDrag>()?.active.as_ref()?;
    let palette = theme::palette(cx);
    let refused = active.target.is_some() && marker(cx).is_none();
    let position = active.position + point(px(12.), px(12.));
    let ghost = div()
        .id("drag-ghost")
        .max_w(ui(220.))
        .px(ui(8.))
        .py(ui(3.))
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .rounded(ui(4.))
        .border_1()
        .border_color(if refused {
            palette.chrome_border
        } else {
            palette.accent
        })
        .bg(palette.chrome_raised)
        .text_color(palette.chrome_ink)
        .text_size(ui(TYPE_MENU))
        .when(refused, |ghost| ghost.opacity(0.55))
        .child(active.payload.label().to_owned());
    // Placed by window coordinates, which the pointer's are: inside the shell
    // it would sit offset by the title bar above it.
    Some(deferred(anchored().position(position).child(ghost)).into_any_element())
}

#[cfg(test)]
mod tests;
