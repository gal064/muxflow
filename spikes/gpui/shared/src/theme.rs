//! The desktop app's dark palette.
//!
//! Values are copied from `packages/client-core/palette.css` (`:root` /
//! `[data-palette="dark"]`), which is generated from `palette.ts`. Only the
//! entries the spikes need are listed; keep the names identical to the CSS.

use gpui_kit::{Rgba, rgb, rgba};

/// Dark palette. Field names mirror the CSS custom properties.
pub struct Palette {
    pub chrome_bg: Rgba,
    pub chrome_border: Rgba,
    pub chrome_ink: Rgba,
    pub chrome_ink_strong: Rgba,
    pub chrome_dim: Rgba,
    pub accent: Rgba,
    pub accent_wash: Rgba,
    pub term_bg: Rgba,
    pub term_fg: Rgba,
}

pub fn dark() -> Palette {
    Palette {
        chrome_bg: rgb(0x282c34),
        chrome_border: rgb(0x3e4451),
        chrome_ink: rgb(0xc4c8c6),
        chrome_ink_strong: rgb(0xffffff),
        chrome_dim: rgb(0xa4abb6),
        accent: rgb(0x7aa6da),
        accent_wash: rgba(0x7aa6da1f),
        term_bg: rgb(0x282c34),
        term_fg: rgb(0xffffff),
    }
}
