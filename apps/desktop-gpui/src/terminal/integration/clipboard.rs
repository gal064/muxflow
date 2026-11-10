//! A terminal program's clipboard write: OSC 52 in a pane's output (port of
//! `osc52Clipboard.ts`, decoded by the engine), or a copy tmux kept in a paste
//! buffer, which the host forwards. Both reach the system clipboard only while
//! the `terminalApplicationClipboard` setting allows it.

use desktop_core::app_state::load_app_state;
use gpui_kit::{App, ClipboardItem};

use crate::app::Core;

/// One program may replace at most 1 MiB of clipboard text.
pub const MAX_APPLICATION_CLIPBOARD_BYTES: usize = 1024 * 1024;

/// Whether a program's write may replace the clipboard. An empty write never
/// does: it would only clear what the user copied.
pub fn admitted(enabled: bool, text: &str) -> bool {
    enabled && !text.is_empty() && text.len() <= MAX_APPLICATION_CLIPBOARD_BYTES
}

/// On unless the user turned it off, as in the Tauri app.
fn enabled(cx: &App) -> bool {
    cx.try_global::<Core>()
        .and_then(|core| load_app_state(&core.app_state).ok())
        .and_then(|state| state.shell.terminal_application_clipboard)
        .unwrap_or(true)
}

pub fn write_from_application(text: String, cx: &mut App) {
    if admitted(enabled(cx), &text) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_only_a_bounded_nonempty_write_while_the_setting_is_on() {
        assert!(admitted(true, "copied"));
        assert!(!admitted(false, "copied"));
        assert!(!admitted(true, ""));
        let limit = "x".repeat(MAX_APPLICATION_CLIPBOARD_BYTES);
        assert!(admitted(true, &limit));
        assert!(!admitted(true, &format!("{limit}x")));
    }
}
