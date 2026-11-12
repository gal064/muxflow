//! Live Markdown: the source text edited in its rendered form, with syntax
//! hidden until the caret reaches it. See `README.md`.
//!
//! Only [`view`] uses GPUI. The caret and every selection are source byte
//! offsets; what is revealed is a pure function of the selection.

pub mod doc;
pub mod edit;
pub mod link;
mod lists;
pub mod projection;
pub mod table;
pub mod view;

/// Files larger than this, in bytes as read, open as source (Tauri's
/// `LIVE_MARKDOWN_LIMIT_CHARS` is 1 MiB). See the README's size limit note.
// Temporary; T7.5 (on-screen-only drawing + block-scoped reparse) restores Tauri's 1 MiB.
pub const LIMIT_BYTES: u64 = 48 * 1024;
