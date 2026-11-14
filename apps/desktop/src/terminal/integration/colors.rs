//! Port of `hostColors.ts`: tells every host which colors the terminal is drawn
//! with, now and whenever the theme changes.
//!
//! tmux answers a program's "what is your background?" (OSC 10/11) for the
//! terminal, from what the host reports, because a control-mode client has no
//! TTY to ask. Without this a light terminal is described as dark, and Codex,
//! Neovim and bat pick colors for a background that is not there.

use std::sync::Arc;

use gpui_kit::{App, Global, Rgba};

use crate::connection_ui::backend::TerminalBackend;
use crate::theme;

/// `#rrggbb`, the form the host accepts.
pub fn hex(color: Rgba) -> String {
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(color.r),
        channel(color.g),
        channel(color.b)
    )
}

struct HostColors {
    backend: Arc<dyn TerminalBackend>,
    sent: Option<(String, String)>,
}

impl Global for HostColors {}

/// Reports the colors on screen and follows the theme. Called once, when the
/// connections start.
pub fn start(backend: Arc<dyn TerminalBackend>, cx: &mut App) {
    cx.set_global(HostColors {
        backend,
        sent: None,
    });
    report(cx);
    cx.observe_global::<theme::Appearance>(report).detach();
}

fn report(cx: &mut App) {
    let palette = theme::palette(cx);
    let colors = (hex(palette.term_fg), hex(palette.term_bg));
    let state = cx.global_mut::<HostColors>();
    // The appearance also changes with the UI scale, which says nothing new.
    if state.sent.as_ref() == Some(&colors) {
        return;
    }
    // A failure leaves the host on its dark default, a hint and not worth an
    // error the user cannot act on, so it is only logged.
    if let Err(error) = state
        .backend
        .set_terminal_colors(colors.0.clone(), colors.1.clone())
    {
        eprintln!("muxflow: could not report terminal colors to the host: {error}");
    }
    state.sent = Some(colors);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::rgba;

    #[test]
    fn writes_a_color_the_way_the_host_reads_it() {
        assert_eq!(hex(rgba(0x1e1f22ff)), "#1e1f22");
        assert_eq!(hex(rgba(0xffffff80)), "#ffffff");
        assert_eq!(hex(theme::light().term_bg).len(), 7);
    }
}
