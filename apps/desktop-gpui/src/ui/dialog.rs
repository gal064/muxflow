//! What every dialog shares with `styles.css`: the raised card with its heavy
//! shadow, the bordered footer buttons, and prose with inline `code`.

use gpui_kit::{
    AnyElement, App, BoxShadow, Hsla, IntoElement, RenderOnce, SharedString, Styled, StyledText,
    TextRun, Window,
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        dialog::Dialog,
    },
    px,
};

use crate::{
    fonts,
    theme::{
        Palette,
        metrics::{NARROW_DIALOGS, RADIUS_PANEL, TYPE_ROW, narrower_than, ui},
    },
};

/// `box-shadow: 0 24px 60px color-mix(in srgb, var(--shadow) 60%, transparent)`. GPUI's
/// blur radius is half of CSS's, as Kit's own shadows note.
fn shadow(palette: &Palette) -> Vec<BoxShadow> {
    let mut color = Hsla::from(palette.shadow);
    color.a *= 0.6;
    vec![BoxShadow::new(px(0.), px(24.), color).blur_radius(px(30.))]
}

/// Where a `.modal-backdrop` puts its dialog: 12vh from the top, and centered in a window
/// 600 CSS px or narrower.
pub fn place(dialog: Dialog, window: &Window) -> Dialog {
    if narrower_than(NARROW_DIALOGS, window) {
        dialog.centered()
    } else {
        dialog.margin_top(window.viewport_size().height * 0.12)
    }
}

/// The card of a dialog: raised ground, hairline border, the panel radius and shadow.
pub fn card<D: Styled>(dialog: D, palette: &Palette) -> D {
    dialog
        .text_size(ui(TYPE_ROW))
        .bg(palette.chrome_raised)
        .border_color(palette.chrome_border)
        .rounded(ui(RADIUS_PANEL))
        .shadow(shadow(palette))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Plain,
    Primary,
    Danger,
    /// `.danger-quiet`: danger ink on the plain button, for removing something.
    DangerQuiet,
}

/// A footer button: `:where(.modal-backdrop) button` is bordered on the window
/// ground and `.primary` / `.danger` fill it.
///
/// `disabled` dims it as `button:disabled` does (`opacity: .42`). Kit replays the colors
/// set here inside its own disabled state, so the dimming has to be restated; a filled
/// button keeps Kit's own wash.
pub fn button(button: Button, action: Action, disabled: bool, palette: &Palette) -> Button {
    let button = button
        .small()
        .h_auto()
        .px(ui(12.))
        .py(ui(6.))
        .border_1()
        .text_size(ui(TYPE_ROW));
    let button = if disabled && matches!(action, Action::Plain | Action::DangerQuiet) {
        button.opacity(0.42)
    } else {
        button
    };
    match action {
        Action::Plain => button
            .bg(palette.chrome_bg)
            .border_color(palette.chrome_border)
            .text_color(palette.chrome_ink),
        Action::Primary => button.primary().border_color(palette.accent),
        Action::Danger => button.danger().border_color(palette.danger),
        Action::DangerQuiet => button
            .bg(palette.chrome_bg)
            .border_color(palette.chrome_border)
            .text_color(palette.danger_ink),
    }
}

/// Splits `text` at backticks: odd pieces are `code`.
fn pieces(text: &str) -> Vec<(&str, bool)> {
    text.split('`')
        .enumerate()
        .filter(|(_, piece)| !piece.is_empty())
        .map(|(index, piece)| (piece, index % 2 == 1))
        .collect()
}

/// A paragraph whose `backticked` spans are set in the mono face, like `<code>`.
#[derive(IntoElement)]
pub struct Prose {
    text: SharedString,
}

impl Prose {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self { text: text.into() }
    }
}

impl RenderOnce for Prose {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let base = window.text_style();
        let pieces = pieces(&self.text);
        let plain: String = pieces.iter().map(|(piece, _)| *piece).collect();
        let runs = pieces
            .iter()
            .map(|(piece, code)| {
                let mut run = base.to_run(piece.len());
                if *code {
                    run.font.family = fonts::MONO.into();
                }
                run
            })
            .collect::<Vec<TextRun>>();
        StyledText::new(plain).with_runs(runs)
    }
}

impl From<Prose> for AnyElement {
    fn from(prose: Prose) -> Self {
        prose.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backticks_mark_code_and_are_dropped() {
        assert_eq!(
            pieces("an empty `A_B` and `c = false`, so"),
            vec![
                ("an empty ", false),
                ("A_B", true),
                (" and ", false),
                ("c = false", true),
                (", so", false)
            ]
        );
        assert_eq!(pieces("plain"), vec![("plain", false)]);
    }
}
