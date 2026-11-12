//! A Markdown tab's rendered views: the preview, for its Preview and Split
//! views, and the Live view. Port of `features/shell/MarkdownPreview`,
//! `features/files/markdown*` and `features/files/LiveMarkdownEditor.tsx`.
//!
//! Kit's Markdown view draws the preview natively, so nothing is rendered as
//! HTML: the sanitizer, its debounce and the CSP have no counterpart here. What
//! Tauri's sanitizer allowed is kept where Kit would otherwise be more
//! permissive: an image is drawn only from an embedded raster `data:` URL, and a
//! link goes only where [`link::target`] sends it.
//!
//! The Live view's editing lives in the `live-markdown` crate; [`live`] joins
//! it to the tab's document.

mod link;
mod live;
mod view;

use std::rc::Rc;

use gpui_kit::App;

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    crate::editor::set_markdown_preview(Rc::new(view::surface), cx);
    live_markdown::view::init(cx);
    crate::editor::set_live_markdown(live::live_markdown(), cx);
}
