//! Lengths and type steps from `apps/desktop/src/tokens.css`, in CSS pixels at
//! 100% UI scale. Size chrome with [`ui`], not `px()`, so it follows UI scale.

use gpui_kit::{Rems, rems};

/// A `tokens.css` length as rems. Kit sets the rem from the UI scale.
pub fn ui(css_pixels: f32) -> Rems {
    rems(css_pixels / 16.)
}

pub const TYPE_ROW: f32 = 13.5;
pub const TYPE_SECONDARY: f32 = 11.5;
pub const TYPE_META: f32 = 11.;
pub const TYPE_LABEL: f32 = 10.5;
pub const TYPE_MENU: f32 = 13.;
pub const TYPE_TITLEBAR: f32 = 12.5;
pub const TERM_FONT_SIZE: f32 = 13.;
pub const WEIGHT_ROW: f32 = 600.;
/// `--tracking-label`, in ems.
pub const TRACKING_LABEL: f32 = 0.12;

pub const BAR_HEIGHT: f32 = 34.;
pub const TITLEBAR_HEIGHT: f32 = 28.;
pub const SIDEBAR_WIDTH: f32 = 240.;
pub const PANEL_WIDTH: f32 = 300.;
pub const MENU_ROW_HEIGHT: f32 = 24.;
pub const PALETTE_WIDTH: f32 = 560.;
pub const PALETTE_ROW_HEIGHT: f32 = 28.;
pub const RADIUS_ROW: f32 = 7.;
pub const RADIUS_CONTROL: f32 = 5.;
pub const RADIUS_PANEL: f32 = 10.;
pub const FOCUS_RING: f32 = 2.;
pub const ATTENTION_RING: f32 = 2.5;
pub const ATTENTION_RADIUS: f32 = 6.;
pub const ATTENTION_INSET: f32 = 2.;
