//! Tooltips. Kit shows one when the pointer rests on a control, also a disabled
//! one, so a disabled control can say why. `bar_button` gives every bar control
//! its name; this adds the shortcut, as Tauri's `title` did.

use gpui_kit::{App, SharedString};

use crate::commands::{
    Commands,
    glyphs::shortcut_glyphs,
    keymap::shortcut_for,
    registry::{CommandId, command_by_id},
};

/// "Settings (Ctrl+,)": a control's name with its command's current shortcut,
/// overrides included. The bare name when the command has none.
pub fn with_shortcut(label: &str, command: CommandId, cx: &App) -> SharedString {
    let commands = cx.global::<Commands>();
    let platform = commands.platform();
    let shortcut = command_by_id(command)
        .and_then(|command| shortcut_for(command, platform, commands.overrides()));
    match shortcut_glyphs(shortcut, platform) {
        Some(glyphs) => format!("{label} ({glyphs})").into(),
        None => label.to_owned().into(),
    }
}
