//! tmux discovery and control-mode primitives.
//!
//! This crate deliberately does not decode terminal output as UTF-8. tmux control
//! mode is a byte protocol and the renderer is responsible for terminal decoding.

mod control;
mod discovery;
mod input;
mod layout;
mod replay;

pub use control::{
    CommandTag, ControlDispatcher, ControlParseError, ControlParser, ControlRecord, DispatchEvent,
    unescape_output,
};
pub use discovery::{
    BatchedDiscovery, Pane, Session, TmuxSnapshot, Window, batched_discovery_args, discover,
    discover_with, discover_with_socket_name, parse_batched_discovery,
};
pub use input::{DESKTOP_INPUT_COALESCE_BYTES, HOST_INPUT_COALESCE_BYTES, MAX_INPUT_REQUEST_BYTES};
pub use layout::{LayoutAxis, LayoutGeneration, LayoutNode, LayoutParseError, parse_layout};
pub use replay::{
    BufferedOutput, OutputDisposition, PaneResource, PaneResourceState, PaneResourceStore,
    ReplayBatch, ScreenSeeder, VisibilityCheckpoint,
};
