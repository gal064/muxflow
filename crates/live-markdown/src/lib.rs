//! Live Markdown: the source text edited in its rendered form, with syntax
//! hidden until the caret reaches it. See `README.md`.
//!
//! Only [`view`] uses GPUI. The caret and every selection are source byte
//! offsets; what is revealed is a pure function of the selection.

pub mod doc;
pub mod edit;
pub mod find;
pub mod link;
mod lists;
pub mod projection;
pub mod reparse;
pub mod table;
pub mod view;

/// Files larger than this, in bytes as read, open as source: Tauri's
/// `LIVE_MARKDOWN_LIMIT_CHARS`, which counts UTF-16 units. See the README's
/// size limit note.
pub const LIMIT_BYTES: u64 = 1024 * 1024;
