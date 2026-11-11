//! The rendered Markdown preview of a Markdown tab, for its Preview and Split
//! views. Port of `features/shell/MarkdownPreview` and `features/files/markdown*`.
//!
//! Kit's Markdown view draws the document natively, so nothing is rendered as
//! HTML: the sanitizer, its debounce and the CSP have no counterpart here. What
//! Tauri's sanitizer allowed is kept where Kit would otherwise be more
//! permissive: an image is drawn only from an embedded raster `data:` URL, and a
//! link goes only where [`link::target`] sends it.
//!
//! The Live view is not here: it is Phase 7.

mod link;
mod view;

use std::rc::Rc;

use gpui_kit::App;

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    crate::editor::set_markdown_preview(Rc::new(view::surface), cx);
}
