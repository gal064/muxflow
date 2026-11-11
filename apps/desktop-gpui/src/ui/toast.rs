//! `styles.css`'s `.toast`, shared by Kit's status notices and the recovery
//! offers: a small raised card at the bottom right with 11 px text, and
//! bordered buttons under it.

use gpui_kit::{
    BoxShadow, Hsla, Styled,
    component::{Sizable, button::Button},
    px,
};

use crate::theme::{
    Palette,
    metrics::{TYPE_META, ui},
};

/// `right: 14px; bottom: 14px`, and `max-width: 420px`.
pub const MARGIN: f32 = 14.;
pub const WIDTH: f32 = 420.;
/// A window 600 CSS px or narrower: `right: 8px; bottom: 8px; left: 8px`.
pub const NARROW_MARGIN: f32 = 8.;
/// `line-height: normal` for the sans face.
pub const LINE: f32 = 1.21;
const PADDING: f32 = 12.;
const RADIUS: f32 = 8.;

/// `box-shadow: 0 12px 32px color-mix(in srgb, var(--shadow) 70%, transparent)`, with
/// GPUI's blur radius half of CSS's.
pub fn shadow(palette: &Palette) -> Vec<BoxShadow> {
    let mut color = Hsla::from(palette.shadow);
    color.a *= 0.7;
    vec![BoxShadow::new(px(0.), px(12.), color).blur_radius(px(16.))]
}

/// The look of the card. A problem has the danger border.
pub fn card<S: Styled>(card: S, problem: bool, palette: &Palette) -> S {
    card.p(ui(PADDING))
        .gap(ui(5.))
        .rounded(ui(RADIUS))
        .border_1()
        .border_color(if problem {
            palette.danger
        } else {
            palette.chrome_border
        })
        .bg(palette.chrome_raised)
        .text_size(ui(TYPE_META))
        .shadow(shadow(palette))
}

/// `.toast button`: padding 5px 9px, a 1px border, 4px radius, on the window ground.
pub fn button(button: Button, palette: &Palette) -> Button {
    button
        .xsmall()
        .h_auto()
        .px(ui(9.))
        .py(ui(5.))
        .border_1()
        .border_color(palette.chrome_border)
        .bg(palette.chrome_bg)
        .text_color(palette.chrome_ink)
        .text_size(ui(TYPE_META))
}
