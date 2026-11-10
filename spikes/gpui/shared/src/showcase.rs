//! A small view used by every spike binary until the spike has real content:
//! heading in Inter, a monospace sample in JetBrains Mono, and one focusable,
//! labeled button that GPUI Kit exposes through AccessKit (role `Button`).

use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::*;

use crate::fonts::{MONO, SANS};
use crate::theme;

/// Accessible name of the button. The S10 check looks for exactly this.
pub const BUTTON_LABEL: &str = "Press me";

pub struct Showcase {
    heading: SharedString,
    note: SharedString,
    presses: usize,
}

impl Showcase {
    pub fn new(heading: &str, note: &str, cx: &mut App) -> Entity<Self> {
        let (heading, note) = (heading.to_owned().into(), note.to_owned().into());
        cx.new(|_| Self {
            heading,
            note,
            presses: 0,
        })
    }
}

impl Render for Showcase {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme::dark();
        let variant = ButtonCustomVariant::new(cx)
            .color(p.accent.into())
            .foreground(p.accent.into())
            .hover(p.accent_wash.into())
            .active(p.accent_wash.into());

        div()
            .size_full()
            .bg(p.chrome_bg)
            .text_color(p.chrome_ink)
            .font_family(SANS)
            .p_8()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .text_size(px(28.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.chrome_ink_strong)
                    .child(self.heading.clone()),
            )
            .child(
                div()
                    .text_size(px(14.))
                    .text_color(p.chrome_dim)
                    .child(self.note.clone()),
            )
            .child(
                div()
                    .font_family(MONO)
                    .text_size(px(13.))
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(p.chrome_border)
                    .bg(p.term_bg)
                    .text_color(p.term_fg)
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child("$ muxflow --version   # JetBrains Mono")
                    .child("fn main() { println!(\"0O 1lI {} -> => != ==\"); }")
                    .child(format!("button presses: {}", self.presses)),
            )
            .child(
                div().flex().child(
                    Button::new("spike-button")
                        .custom(variant)
                        .label(BUTTON_LABEL)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.presses += 1;
                            cx.notify();
                        })),
                ),
            )
    }
}
