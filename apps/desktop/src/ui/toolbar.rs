//! The small buttons on the editor, Markdown and Git diff bars: `styles.css`'s
//! `.editor-toolbar > button`, with `.danger` and `aria-pressed`. Kit's default
//! button is white with full-strength ink, which read as louder than the bar.

use gpui_kit::{
    Styled,
    component::{Sizable, button::Button},
};

use crate::theme::{
    Palette,
    metrics::{TYPE_META, ui},
};

/// `button:disabled { opacity: .42 }`.
const DISABLED_OPACITY: f32 = 0.42;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Plain,
    /// The shown view of a group, `aria-pressed="true"`.
    Pressed,
    /// A filled `.danger`, for an action that removes something.
    Danger,
}

/// `disabled` dims it as `button:disabled` does: Kit re-applies the colors set here
/// over its own disabled style, so the dimming has to be restated.
pub fn toolbar_button(button: Button, tone: Tone, disabled: bool, palette: &Palette) -> Button {
    let button = button
        .xsmall()
        .px(ui(8.))
        .py(ui(3.))
        .border_1()
        .text_size(ui(TYPE_META));
    let button = if disabled {
        button.opacity(DISABLED_OPACITY)
    } else {
        button
    };
    match tone {
        Tone::Plain => button
            .bg(palette.chrome_raised)
            .border_color(palette.chrome_border)
            .text_color(palette.chrome_dim),
        Tone::Pressed => button
            .bg(palette.accent_wash)
            .border_color(palette.accent)
            .text_color(palette.chrome_ink_strong),
        Tone::Danger => button
            .bg(palette.danger)
            .border_color(palette.danger)
            .text_color(palette.accent_ink),
    }
}
