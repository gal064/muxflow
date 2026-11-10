//! T3.4: keyboard, IME, paste and mouse reporting, and the input latency
//! probes (`echoLagProbe`, `inputLatencyStats`), which need the input path.

use gpui_kit::{App, Bounds, Div, Entity, Pixels, SharedString, Stateful, Window};

use super::pane::TerminalPane;

pub fn init(_cx: &mut App) {}

/// This feature's state on a pane.
#[derive(Default)]
pub struct PaneInput {
    /// Text an IME is composing. The element draws it at the cursor.
    pub preedit: Option<SharedString>,
}

impl PaneInput {
    /// A `%output` reached this pane's screen, in stream order. For the echo
    /// probe's round trips.
    pub fn note_output(&mut self, _sequence: u64, _generation: u64) {}
}

/// Adds key and mouse listeners to a pane's root.
pub fn attach(
    element: Stateful<Div>,
    _pane: &Entity<TerminalPane>,
    _window: &mut Window,
    _cx: &mut App,
) -> Stateful<Div> {
    element
}

/// Runs during the pane's paint, with the element's bounds: where IME input
/// registers with `window.handle_input`.
pub fn paint(
    _pane: &Entity<TerminalPane>,
    _bounds: Bounds<Pixels>,
    _window: &mut Window,
    _cx: &mut App,
) {
}
