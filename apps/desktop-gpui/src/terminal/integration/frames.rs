//! The rules behind the gestures on a pane's frame: divider drag and keys,
//! pane grab, hover focus. `surface.rs` owns the frames and their state; what
//! can be decided without GPUI is here, so it is table-tested.

use std::time::Duration;

use serde_json::{Value, json};

/// How long the pointer rests on a pane before it takes focus: long enough
/// that sweeping across a split on the way somewhere else focuses nothing.
pub const HOVER_FOCUS_DELAY: Duration = Duration::from_millis(120);
/// Releasing a pane grab enters whatever pane is under the pointer. That is
/// the drop, not a hover: focusing that pane would take focus from the pane
/// the drop just placed beside it.
pub const AFTER_GRAB_QUIET: Duration = Duration::from_millis(300);
/// Cells an arrow key on a focused divider moves it.
pub const KEY_RESIZE_CELLS: u32 = 2;
/// Less than this is a click, not a drag, for a divider and a pane grab alike.
pub const DRAG_THRESHOLD_PX: f32 = 4.;
/// Share of a pane, from each edge, that counts as that edge. The middle is
/// no drop.
const PANE_EDGE_ZONE: f32 = 0.3;

/// Which way a divider moves the edge it sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// The divider on a pane's right edge.
    Horizontal,
    /// The divider on a pane's bottom edge.
    Vertical,
}

/// Port of `resizeCellsFromPixels`: at least one cell, so a drag always moves.
pub fn resize_cells_from_pixels(delta_pixels: f32, pane_pixels: f32, pane_cells: u16) -> u32 {
    if !delta_pixels.is_finite() || !pane_pixels.is_finite() || pane_pixels <= 0. || pane_cells == 0
    {
        return 1;
    }
    let cell_pixels = pane_pixels / f32::from(pane_cells);
    ((delta_pixels.abs() / cell_pixels).round() as u32).max(1)
}

/// The tmux action a divider move is: growing a pane toward the edge it sits
/// on, or shrinking it back.
pub fn resize_action(pane_id: &str, axis: Axis, forward: bool, cells: u32) -> Value {
    let kind = match (axis, forward) {
        (Axis::Horizontal, true) => "resizePaneRight",
        (Axis::Horizontal, false) => "resizePaneLeft",
        (Axis::Vertical, true) => "resizePaneDown",
        (Axis::Vertical, false) => "resizePaneUp",
    };
    json!({ "kind": kind, "pane_id": pane_id, "resize_cells": cells })
}

/// An arrow key along a focused divider's axis, as a direction to move it.
pub fn divider_key(axis: Axis, key: &str) -> Option<bool> {
    match (axis, key) {
        (Axis::Horizontal, "right") | (Axis::Vertical, "down") => Some(true),
        (Axis::Horizontal, "left") | (Axis::Vertical, "up") => Some(false),
        _ => None,
    }
}

/// The half of a pane a grabbed pane would join.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneEdge {
    Left,
    Right,
    Top,
    Bottom,
}

impl PaneEdge {
    fn wire(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }
}

/// Port of `paneEdgeAt`: the edge a point falls on, as fractions of the pane
/// from its left and top, or none in the pane's middle.
pub fn pane_edge_at(x: f32, y: f32) -> Option<PaneEdge> {
    let distances = [
        (PaneEdge::Left, x),
        (PaneEdge::Right, 1. - x),
        (PaneEdge::Top, y),
        (PaneEdge::Bottom, 1. - y),
    ];
    let (edge, distance) =
        distances.into_iter().fold(
            distances[0],
            |best, entry| if entry.1 < best.1 { entry } else { best },
        );
    (distance <= PANE_EDGE_ZONE).then_some(edge)
}

/// A grabbed pane dropped on another's edge: `planDrop`'s pane case. Both are
/// in the window on screen, so the join rearranges it and empties nothing.
pub fn join_action(pane_id: &str, target_pane_id: &str, edge: PaneEdge) -> Option<Value> {
    (pane_id != target_pane_id).then(|| {
        json!({
            "kind": "joinPane",
            "pane_id": pane_id,
            "target_pane_id": target_pane_id,
            "pane_edge": edge.wire(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_divider_drag_moves_at_least_one_cell() {
        assert_eq!(resize_cells_from_pixels(40., 800., 100), 5);
        assert_eq!(resize_cells_from_pixels(-40., 800., 100), 5);
        assert_eq!(resize_cells_from_pixels(1., 800., 100), 1);
        assert_eq!(resize_cells_from_pixels(f32::NAN, 800., 100), 1);
        assert_eq!(resize_cells_from_pixels(40., 0., 100), 1);
        assert_eq!(resize_cells_from_pixels(40., 800., 0), 1);
    }

    #[test]
    fn a_divider_grows_its_pane_toward_its_edge_or_shrinks_it_back() {
        assert_eq!(
            resize_action("%1", Axis::Horizontal, true, 3),
            json!({"kind": "resizePaneRight", "pane_id": "%1", "resize_cells": 3})
        );
        assert_eq!(
            resize_action("%1", Axis::Vertical, false, 2)["kind"],
            "resizePaneUp"
        );
        assert_eq!(divider_key(Axis::Horizontal, "right"), Some(true));
        assert_eq!(divider_key(Axis::Horizontal, "up"), None);
        assert_eq!(divider_key(Axis::Vertical, "up"), Some(false));
        assert_eq!(divider_key(Axis::Vertical, "left"), None);
    }

    #[test]
    fn a_pane_has_four_drop_edges_and_no_middle() {
        assert_eq!(pane_edge_at(0.1, 0.5), Some(PaneEdge::Left));
        assert_eq!(pane_edge_at(0.9, 0.5), Some(PaneEdge::Right));
        assert_eq!(pane_edge_at(0.5, 0.2), Some(PaneEdge::Top));
        assert_eq!(pane_edge_at(0.5, 0.75), Some(PaneEdge::Bottom));
        assert_eq!(pane_edge_at(0.5, 0.5), None);
        // The nearer edge wins in a corner.
        assert_eq!(pane_edge_at(0.05, 0.2), Some(PaneEdge::Left));
    }

    #[test]
    fn a_pane_joins_another_and_never_itself() {
        assert_eq!(
            join_action("%1", "%2", PaneEdge::Bottom),
            Some(json!({
                "kind": "joinPane",
                "pane_id": "%1",
                "target_pane_id": "%2",
                "pane_edge": "bottom",
            }))
        );
        assert_eq!(join_action("%1", "%1", PaneEdge::Left), None);
    }
}
