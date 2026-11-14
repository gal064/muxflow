//! Lengths and type steps from the Tauri app's `tokens.css`, in CSS pixels at
//! 100% UI scale. Size chrome with [`ui`], not `px()`, so it follows UI scale.

use gpui_kit::{Rems, Window, rems};

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

/// `styles.css`'s narrow-window breakpoints, in CSS pixels. Tauri zoomed the
/// webview for UI scale, so they move with it: compare with [`narrower_than`].
pub const NARROW_DIALOGS: f32 = 600.;
pub const NARROW_TERMINAL: f32 = 880.;
pub const NARROW_QUICK_OPEN: f32 = 480.;

/// Whether the window is at most `breakpoint` CSS pixels wide at this scale.
pub fn narrower_than(breakpoint: f32, window: &Window) -> bool {
    window.viewport_size().width <= ui(breakpoint).to_pixels(window.rem_size())
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Context, IntoElement, Render, TestAppContext, div, px, size};

    use super::*;

    struct Blank;

    impl Render for Blank {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[gpui_kit::test]
    fn a_breakpoint_follows_ui_scale(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(|_, _| Blank);
        let mut at = |width: f32, rem: f32| {
            cx.simulate_resize(size(px(width), px(600.)));
            cx.update(|window, _| {
                window.set_rem_size(px(rem));
                narrower_than(NARROW_TERMINAL, window)
            })
        };
        assert!(!at(1000., 16.));
        // Inclusive, as `max-width` is.
        assert!(at(880., 16.));
        // The breakpoints follow UI scale, as Tauri's zoomed webview did.
        assert!(!at(1200., 20.));
        assert!(at(1100., 20.));
    }
}
