//! Port of `terminalInputPolicy.ts`: the terminal shortcuts Muxflow owns, which
//! win over the keyboard encoding in [`super::keys`].
//!
//! Copying a finished selection, which the TypeScript file also held, is
//! selection and belongs to `terminal/reading`.

use gpui_kit::Keystroke;

use crate::commands::registry::Platform;

/// Super+C in the kitty keyboard encoding, which is how kitty and Ghostty
/// report Cmd+C to an application. Both agent composers read it as "copy the
/// selection" and ignore it with nothing selected. Ctrl+C is deliberately not
/// used for this: without a selection it is their interrupt.
pub const KITTY_SUPER_C: &[u8] = b"\x1b[99;9u";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyContext {
    pub alternate_screen: bool,
    pub application_cursor_keys: bool,
    pub platform: Platform,
    /// An IME composition is open in the pane.
    pub composing: bool,
}

/// Returns bytes for the terminal shortcuts Muxflow owns, or nothing.
pub fn translate_terminal_key(keystroke: &Keystroke, context: KeyContext) -> Option<&'static [u8]> {
    if context.composing {
        return None;
    }
    let modifiers = keystroke.modifiers;
    let key = keystroke.key.as_str();
    // Legacy terminals alias Ctrl+/ and Ctrl+_ to 0x1f (US); xterm.js 6 mapped
    // only the second. GPUI names the key as the layout has it, so a QWERTZ
    // Ctrl+-, which sits where a US `/` does, reports `-` and is left alone.
    if modifiers.control && !modifiers.platform && !modifiers.alt && !modifiers.shift && key == "/"
    {
        return Some(b"\x1f");
    }
    // Shift-Enter is what Ghostty sends for it: the CSI-u encoding, for every
    // app. Both agent composers read it as "insert a newline"; a bare carriage
    // return reads as submit. A special case keyed on tmux's foreground command
    // used to cover one composer and missed the other, whose process name
    // depends on how it was installed.
    if key == "enter"
        && modifiers.shift
        && !modifiers.platform
        && !modifiers.control
        && !modifiers.alt
    {
        return Some(b"\x1b[13;2u");
    }
    // Command-Arrow always means line start/end; the screen only picks the
    // encoding. Normal-screen shells and wrappers read the readline Control-A
    // and Control-E, while full-screen programs, including the agent composers
    // on the alternate screen, read Home/End. Those are tmux's own Home/End
    // (`khome`/`kend` in tmux-256color), whatever DECCKM says: the host writes
    // these bytes with `send-keys -H`, so tmux never re-encodes them, and vim
    // reads xterm's SS3 Home as Escape, `O`, `H`. A process-name allowlist made
    // an ordinary wrapper silently lose the shortcut.
    if context.platform != Platform::Mac
        || !modifiers.platform
        || modifiers.shift
        || modifiers.control
        || modifiers.alt
    {
        return None;
    }
    match (key, context.alternate_screen) {
        ("left", false) => Some(b"\x01"),
        ("right", false) => Some(b"\x05"),
        ("left", true) => Some(b"\x1b[1~"),
        ("right", true) => Some(b"\x1b[4~"),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardAlias {
    Copy,
    Paste,
}

/// Ctrl+Shift+C and Ctrl+Shift+V, the Linux terminal's traditional copy and
/// paste. The keymap puts those commands on Ctrl+C / Ctrl+V; these stay as
/// fixed aliases so a Ghostty or GNOME Terminal habit keeps working.
pub fn terminal_clipboard_alias(
    keystroke: &Keystroke,
    platform: Platform,
) -> Option<ClipboardAlias> {
    let modifiers = keystroke.modifiers;
    if platform != Platform::Linux
        || !modifiers.control
        || !modifiers.shift
        || modifiers.alt
        || modifiers.platform
    {
        return None;
    }
    match keystroke.key.to_ascii_lowercase().as_str() {
        "c" => Some(ClipboardAlias::Copy),
        "v" => Some(ClipboardAlias::Paste),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::Modifiers;

    fn key(key: &str, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: None,
        }
    }

    fn mods(control: bool, alt: bool, shift: bool, platform: bool) -> Modifiers {
        Modifiers {
            control,
            alt,
            shift,
            platform,
            function: false,
        }
    }

    const CTRL: Modifiers = Modifiers {
        control: true,
        alt: false,
        shift: false,
        platform: false,
        function: false,
    };
    const CMD: Modifiers = Modifiers {
        control: false,
        alt: false,
        shift: false,
        platform: true,
        function: false,
    };

    fn context() -> KeyContext {
        KeyContext {
            alternate_screen: false,
            application_cursor_keys: false,
            platform: Platform::Mac,
            composing: false,
        }
    }

    fn translate(keystroke: &Keystroke, context: KeyContext) -> Option<&'static [u8]> {
        translate_terminal_key(keystroke, context)
    }

    #[test]
    fn control_slash_sends_0x1f_which_xterm_js_6_does_not_map() {
        let slash = key("/", CTRL);
        assert_eq!(translate(&slash, context()), Some(&b"\x1f"[..]));
        let linux = KeyContext {
            platform: Platform::Linux,
            ..context()
        };
        assert_eq!(translate(&slash, linux), Some(&b"\x1f"[..]));
        // Inside a TUI too: this is the one binding the shortcut exists for.
        let tui = KeyContext {
            alternate_screen: true,
            ..context()
        };
        assert_eq!(translate(&slash, tui), Some(&b"\x1f"[..]));
        // A QWERTZ Ctrl+- sits where a US `/` does and reports its own key.
        assert_eq!(translate(&key("-", CTRL), context()), None);

        // Everything with another modifier stays the encoder's.
        for modifiers in [
            mods(true, false, true, false),
            mods(true, true, false, false),
            mods(true, false, false, true),
        ] {
            assert_eq!(translate(&key("/", modifiers), context()), None);
        }
        assert_eq!(
            translate(&key("_", mods(true, false, true, false)), context()),
            None
        );
        // Command-/ is an app shortcut, not terminal bytes.
        assert_eq!(translate(&key("/", CMD), context()), None);
        // A composing IME still wins over every translation.
        let composing = KeyContext {
            composing: true,
            ..context()
        };
        assert_eq!(translate(&slash, composing), None);
    }

    #[test]
    fn shift_enter_is_csi_u_for_every_app_the_way_ghostty_sends_it() {
        let shifted = key("enter", mods(false, false, true, false));
        for alternate_screen in [false, true] {
            let context = KeyContext {
                alternate_screen,
                ..context()
            };
            assert_eq!(translate(&shifted, context), Some(&b"\x1b[13;2u"[..]));
        }
        assert_eq!(translate(&key("enter", Modifiers::none()), context()), None);
        assert_eq!(
            translate(&key("enter", mods(true, false, true, false)), context()),
            None
        );
    }

    #[test]
    fn command_arrows_send_the_readline_line_controls_on_the_normal_screen() {
        assert_eq!(translate(&key("left", CMD), context()), Some(&b"\x01"[..]));
        assert_eq!(translate(&key("right", CMD), context()), Some(&b"\x05"[..]));
    }

    #[test]
    fn command_arrows_send_tmux_home_end_on_the_alternate_screen_in_either_cursor_key_mode() {
        let alternate = KeyContext {
            alternate_screen: true,
            ..context()
        };
        let application = KeyContext {
            application_cursor_keys: true,
            ..alternate
        };
        for context in [alternate, application] {
            assert_eq!(translate(&key("left", CMD), context), Some(&b"\x1b[1~"[..]));
            assert_eq!(
                translate(&key("right", CMD), context),
                Some(&b"\x1b[4~"[..])
            );
        }
    }

    #[test]
    fn other_platforms_and_modified_command_arrows_are_left_alone() {
        let linux = KeyContext {
            platform: Platform::Linux,
            ..context()
        };
        assert_eq!(translate(&key("left", CMD), linux), None);
        let linux_tui = KeyContext {
            alternate_screen: true,
            ..linux
        };
        assert_eq!(translate(&key("left", CMD), linux_tui), None);
        assert_eq!(translate(&key("left", Modifiers::none()), context()), None);
        for extra in [
            mods(false, true, false, true),
            mods(true, false, false, true),
            mods(false, false, true, true),
        ] {
            assert_eq!(translate(&key("left", extra), context()), None);
            let tui = KeyContext {
                alternate_screen: true,
                ..context()
            };
            assert_eq!(translate(&key("left", extra), tui), None);
        }
    }

    #[test]
    fn composition_is_never_translated() {
        let composing = KeyContext {
            composing: true,
            ..context()
        };
        assert_eq!(
            translate(&key("enter", mods(false, false, true, false)), composing),
            None
        );
        assert_eq!(translate(&key("left", CMD), composing), None);
        let tui = KeyContext {
            alternate_screen: true,
            ..composing
        };
        assert_eq!(translate(&key("left", CMD), tui), None);
    }

    #[test]
    fn ctrl_shift_c_and_v_are_copy_and_paste_on_linux_only() {
        let chord = mods(true, false, true, false);
        assert_eq!(
            terminal_clipboard_alias(&key("c", chord), Platform::Linux),
            Some(ClipboardAlias::Copy)
        );
        assert_eq!(
            terminal_clipboard_alias(&key("v", chord), Platform::Linux),
            Some(ClipboardAlias::Paste)
        );
        assert_eq!(
            terminal_clipboard_alias(&key("c", chord), Platform::Mac),
            None
        );
    }

    #[test]
    fn unshifted_and_extra_modifier_chords_stay_with_the_keymap_and_the_shell() {
        assert_eq!(
            terminal_clipboard_alias(&key("c", CTRL), Platform::Linux),
            None
        );
        assert_eq!(
            terminal_clipboard_alias(&key("c", mods(true, true, true, false)), Platform::Linux),
            None
        );
        assert_eq!(
            terminal_clipboard_alias(&key("x", mods(true, false, true, false)), Platform::Linux),
            None
        );
    }
}
