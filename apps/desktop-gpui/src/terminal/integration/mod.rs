//! T3.6: OSC 52, screenshot and file paste, drag and drop, the transfer
//! surface, pane routing, hover focus, agent state, and the `TerminalColors`
//! report to the host.

use gpui_kit::{AnyElement, App, Div, Entity, Stateful, Window};

use super::pane::TerminalPane;

pub fn init(_cx: &mut App) {}

/// This feature's state on a pane.
#[derive(Default)]
pub struct PaneIntegration {}

/// Adds drop and paste listeners to a pane's root.
pub fn attach(
    element: Stateful<Div>,
    _pane: &Entity<TerminalPane>,
    _window: &mut Window,
    _cx: &mut App,
) -> Stateful<Div> {
    element
}

/// Drawn over the grid: drop hints and transfer status.
pub fn children(
    _pane: &Entity<TerminalPane>,
    _window: &mut Window,
    _cx: &mut App,
) -> Vec<AnyElement> {
    Vec::new()
}
