//! The gesture without a window. Port of `pointerDrag.test.ts`.

use gpui_kit::{Pixels, Point, point, px};

use crate::workspaces::actions::PaneEdge;

use super::{DragPayload, DropTarget, Moved, PointerDrag, Release, WorkspaceRef};

fn at(x: f32, y: f32) -> Point<Pixels> {
    point(px(x), px(y))
}

fn payload() -> DragPayload {
    DragPayload::TerminalTab {
        host: "local".into(),
        session_id: "$1".into(),
        window_id: "@2".into(),
        label: "two".into(),
    }
}

fn row() -> DropTarget {
    DropTarget::Workspace(WorkspaceRef {
        host: "local".into(),
        session_id: "$2".into(),
    })
}

/// Pressed at (10, 10) and moved far enough to be a drag.
fn dragging() -> PointerDrag {
    let mut drag = PointerDrag::default();
    drag.press(Some(payload()), at(10., 10.));
    assert_eq!(drag.moved(at(40., 40.), true), Moved::Dragging);
    drag
}

#[test]
fn stays_a_click_until_the_pointer_passes_the_threshold() {
    let mut drag = PointerDrag::default();
    drag.press(Some(payload()), at(10., 10.));
    assert_eq!(drag.moved(at(12., 11.), true), Moved::Nothing);
    assert!(!drag.is_active());
    assert_eq!(drag.release(), Release::Passes);
}

#[test]
fn drops_where_the_pointer_was_offered_a_target() {
    let mut drag = dragging();
    drag.offer(row());
    assert_eq!(
        drag.release(),
        Release::Drop {
            payload: payload(),
            target: Some(row())
        }
    );
    assert!(!drag.is_active());
    assert!(drag.ended_at.is_some());
}

#[test]
fn a_release_over_no_target_drops_nothing() {
    let mut drag = dragging();
    // Offered on one move, and left behind on the next.
    drag.offer(row());
    assert_eq!(drag.moved(at(80., 80.), true), Moved::Dragging);
    assert_eq!(
        drag.release(),
        Release::Drop {
            payload: payload(),
            target: None
        }
    );
}

#[test]
fn the_first_target_of_a_move_wins_and_a_dock_zone_beats_a_pane_edge() {
    let mut drag = dragging();
    let tab = DropTarget::Tab {
        key: "terminal:@2".into(),
        side: crate::workspaces::actions::RelativePosition::After,
        workspace: None,
    };
    drag.offer(tab.clone());
    drag.offer(row());
    assert_eq!(drag.active.as_ref().unwrap().target, Some(tab));

    assert_eq!(drag.moved(at(50., 50.), true), Moved::Dragging);
    drag.offer(DropTarget::Pane {
        pane_id: "%1".into(),
        edge: PaneEdge::Left,
    });
    let dock = DropTarget::Dock(desktop_core::app_state::DockSide::Left);
    drag.offer(dock.clone());
    assert_eq!(drag.active.as_ref().unwrap().target, Some(dock));
}

#[test]
fn drops_nothing_when_escape_cancels_the_drag_and_swallows_the_release() {
    let mut drag = dragging();
    drag.offer(row());
    assert!(drag.cancel());
    assert!(!drag.is_active());
    // The button is still down: the release that follows is no click.
    assert_eq!(drag.release(), Release::Swallowed);
    assert_eq!(drag.release(), Release::Passes);
    // Nothing to cancel the second time.
    assert!(!drag.cancel());
}

#[test]
fn disarms_when_a_move_arrives_with_the_button_already_up() {
    let mut drag = PointerDrag::default();
    drag.press(Some(payload()), at(10., 10.));
    assert_eq!(drag.moved(at(40., 40.), false), Moved::Nothing);
    assert_eq!(drag.moved(at(80., 80.), true), Moved::Nothing);
    assert!(!drag.is_active());

    // A drag under way whose release was never seen ends on the next move.
    let mut drag = dragging();
    assert_eq!(drag.moved(at(60., 60.), false), Moved::Ended);
    assert!(!drag.is_active());
}

#[test]
fn ignores_a_press_with_nothing_to_carry() {
    let mut drag = PointerDrag::default();
    drag.press(None, at(10., 10.));
    assert_eq!(drag.moved(at(40., 40.), true), Moved::Nothing);
    assert!(!drag.is_active());
}

#[test]
fn a_press_while_a_drag_runs_does_not_restart_it() {
    let mut drag = dragging();
    drag.press(
        Some(DragPayload::AppTab {
            host: "local".into(),
            session_id: "$1".into(),
            tab_id: "x".into(),
            label: "x".into(),
        }),
        at(0., 0.),
    );
    assert_eq!(drag.active.as_ref().unwrap().payload, payload());
}

#[test]
fn offers_outside_a_drag_are_ignored() {
    let mut drag = PointerDrag::default();
    drag.offer(row());
    assert!(drag.active.is_none());
}

#[test]
fn an_escape_whose_release_never_comes_swallows_no_later_click() {
    let mut drag = dragging();
    assert!(drag.cancel());
    // The button went up outside the window: the next move has none held.
    assert_eq!(drag.moved(at(50., 50.), false), Moved::Nothing);
    assert_eq!(drag.release(), Release::Passes);
    // Or the next press arrives first.
    let mut drag = dragging();
    assert!(drag.cancel());
    drag.press(None, at(5., 5.));
    assert_eq!(drag.release(), Release::Passes);
}
