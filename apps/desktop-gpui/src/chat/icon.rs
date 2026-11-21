//! The chat's icons: Lucide's, from the Kit's catalog, in place of text marks.
//! One that is not among the Kit's defaults must be listed in
//! [`crate::app::ExtraIcons`] to be drawn.
//!
//! Icons are decorative: the row or control around one carries its name.

use gpui_kit::{Hsla, Styled as _, component::Icon};

pub(super) use gpui_kit::assets::IconName;

use crate::theme::metrics::ui;

/// `name` drawn `size` CSS pixels square, in `ink`.
pub(super) fn icon(name: IconName, size: f32, ink: impl Into<Hsla>) -> Icon {
    Icon::new(name).size(ui(size)).text_color(ink)
}
