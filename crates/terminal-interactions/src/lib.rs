//! Pure terminal interaction logic: link detection, wrapped-command copy
//! cleanup, and layout notification decoding.
//!
//! This ports `packages/terminal-interactions`, which the mobile app keeps
//! using. `packages/terminal-interactions/fixtures` holds the cases that both
//! suites run, so the two implementations cannot drift apart.

mod buffer;
mod file_paths;
mod links;
mod selection;
mod text;
mod topology;
mod web_links;

pub use buffer::{Buffer, BufferLine, BufferRange, Cell, Position, translate_to_string};
pub use file_paths::{
    TerminalFileLink, is_explicit_terminal_file_path, resolve_terminal_file_path,
    terminal_file_link_cell_range, terminal_file_links,
};
pub use links::{LinkKind, TerminalDetectedLink, terminal_links_for_buffer_line};
pub use selection::{
    TerminalSelectionRow, TerminalSelectionSnapshot, capture_terminal_selection,
    clean_wrapped_command_selection,
};
pub use topology::{PaneGrid, TopologyError, TopologyNotification, decode_topology_notification};
pub use web_links::{TerminalWebLink, terminal_web_links};
