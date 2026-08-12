//! Shared bounds for terminal-input transport and queue coalescing.

/// Maximum bytes in one addressed, atomically committed input request.
pub const MAX_INPUT_REQUEST_BYTES: usize = 1024 * 1024;

/// Small same-pane renderer calls may share one correlated protocol request.
pub const DESKTOP_INPUT_COALESCE_BYTES: usize = 256 * 1024;

/// Small same-pane protocol requests may share one tmux buffer commit.
pub const HOST_INPUT_COALESCE_BYTES: usize = 4096;

const _: () = assert!(MAX_INPUT_REQUEST_BYTES > DESKTOP_INPUT_COALESCE_BYTES);
const _: () = assert!(DESKTOP_INPUT_COALESCE_BYTES > HOST_INPUT_COALESCE_BYTES);
