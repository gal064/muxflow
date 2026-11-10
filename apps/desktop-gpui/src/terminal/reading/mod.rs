//! T3.5: selection, scrollback, history paging, search, and links.

use alacritty_terminal::Term;
use gpui_kit::{AnyElement, App, Div, Entity, Stateful, Window};
use terminal_engine::Listener;

use super::{grid::CellHighlight, pane::TerminalPane};

pub fn init(_cx: &mut App) {}

/// This feature's state on a pane.
#[derive(Default)]
pub struct PaneReading {}

/// Selection, search hits and the link under the pointer, in viewport rows.
/// Called once per repaint with the grid locked. Asking the pane to repaint
/// after a change is this feature's job: selection does not damage the grid.
pub fn highlights(_pane: &TerminalPane, _term: &Term<Listener>) -> Vec<CellHighlight> {
    Vec::new()
}

/// Adds selection and wheel listeners to a pane's root.
pub fn attach(
    element: Stateful<Div>,
    _pane: &Entity<TerminalPane>,
    _window: &mut Window,
    _cx: &mut App,
) -> Stateful<Div> {
    element
}

/// Drawn over the grid: the find bar, the scrollbar, the new-output button.
pub fn children(
    _pane: &Entity<TerminalPane>,
    _window: &mut Window,
    _cx: &mut App,
) -> Vec<AnyElement> {
    Vec::new()
}
