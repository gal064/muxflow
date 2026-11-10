//! The terminal: one [`TerminalLink`] per shown host, one [`TerminalPane`] per
//! tmux pane on screen, painted by [`TerminalElement`] and sized by the surface.
//!
//! Terminal data never crosses a serialization boundary. The host's frames go
//! to the engine thread ([`link`]), which parses and marks the pane dirty; a pane
//! repaints at most once per GPUI frame however much arrived, shaping only rows
//! that changed ([`grid`]). tmux owns each pane's grid: the box a pane measures
//! only decides what client size to ask for ([`client_size`]).
//!
//! # The three seams
//!
//! Input, reading and integration each own a submodule. They live apart because
//! nothing here calls one from another, and `pane.rs`, `element.rs` and
//! `surface.rs` are not edited to add a feature:
//!
//! | Seam | Reaches the pane through | Owner |
//! |---|---|---|
//! | [`input`] | `PaneInput`, `input::attach`, `input::paint`, `input::report_focus` (on focus and window activation), the pane's `send_input` | T3.4 |
//! | [`reading`] | `PaneReading`, `reading::attach`, `reading::children`, `reading::highlights` | T3.5 |
//! | [`integration`] | `PaneIntegration`, `integration::attach`, `integration::children` | T3.6 |
//!
//! Mouse and wheel events are claimed by mode, so `input` and `reading` never
//! fight over them: [`TerminalPane::mouse_reporting`] is true while the program
//! in the pane reads the mouse (`input` owns the event, except a Shift press, or
//! Option on macOS, which is forced local selection, and the Ctrl or Cmd left
//! press that grabs a pane or opens a link: [`input::claims_press`]), and false
//! otherwise (`reading` owns it).
//!
//! Dragging a pane, or a divider, is a gesture on the frame rather than the
//! pane, so T3.6 alone extends the frames in `surface.rs` for those.

pub mod box_glyphs;
pub mod cell_metrics;
pub mod client_size;
pub mod element;
pub mod grid;
pub mod input;
pub mod integration;
pub mod layout;
pub mod link;
pub mod paint_gate;
pub mod pane;
pub mod probes;
pub mod reading;
pub mod surface;

use gpui_kit::App;

pub use element::TerminalElement;
pub use link::TerminalLink;
pub use pane::TerminalPane;

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    input::init(cx);
    reading::init(cx);
    integration::init(cx);
}
