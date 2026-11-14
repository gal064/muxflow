//! The keyboard encoding xterm.js gave the terminal (`evaluateKeyboardEvent` in
//! xterm.js 6, MIT), for the keys that become bytes on key down.
//!
//! Text is not here. A key that types a character reaches the pane through the
//! platform's text input (see [`super::ime`]), which is what lets an IME, a dead
//! key or macOS Option compose it first. What is here is every key that has no
//! text of its own: editing and cursor keys, function keys, Control chords, and
//! Alt as an Escape prefix where Alt is not a third-level shift.

use gpui_kit::Keystroke;

use crate::commands::registry::Platform;

const ESC: u8 = 0x1b;

/// The bytes for a key press, or `None` when it is text for the platform's text
/// input, an app's, or nothing at all.
pub fn encode_key(
    keystroke: &Keystroke,
    application_cursor: bool,
    platform: Platform,
) -> Option<Vec<u8>> {
    let m = keystroke.modifiers;
    // xterm's modifier parameter, less one.
    let code =
        m.shift as u8 | (m.alt as u8) << 1 | (m.control as u8) << 2 | (m.platform as u8) << 3;
    let csi_mod = |number: &str, last: char| -> Vec<u8> {
        if code == 0 {
            format!("\x1b[{number}{last}").into_bytes()
        } else {
            format!("\x1b[{number};{}{last}", code + 1).into_bytes()
        }
    };
    // Cursor-like keys: CSI 1;m X when modified, else SS3 in application cursor
    // mode, else CSI.
    let cursor = |last: char| -> Vec<u8> {
        if code != 0 {
            format!("\x1b[1;{}{last}", code + 1).into_bytes()
        } else if application_cursor {
            vec![ESC, b'O', last as u8]
        } else {
            vec![ESC, b'[', last as u8]
        }
    };
    let escaped = |bytes: &[u8]| -> Vec<u8> {
        if m.alt {
            [&[ESC], bytes].concat()
        } else {
            bytes.to_vec()
        }
    };
    Some(match keystroke.key.as_str() {
        "backspace" => escaped(if m.control { b"\x08" } else { b"\x7f" }),
        "tab" if m.shift => b"\x1b[Z".to_vec(),
        "tab" => b"\t".to_vec(),
        "enter" => escaped(b"\r"),
        "escape" => escaped(&[ESC]),
        // Command-Arrow belongs to the policy, or to nothing.
        "left" | "right" | "up" | "down" if m.platform => return None,
        "left" => cursor('D'),
        "right" => cursor('C'),
        "up" => cursor('A'),
        "down" => cursor('B'),
        // Unmodified Home/End are tmux's (`khome`/`kend` in tmux-256color), not
        // xterm's: the host writes these bytes with `send-keys -H`, so tmux
        // never re-encodes them, and vim reads xterm's SS3 Home as Escape, `O`,
        // `H`. Modified ones already match tmux's `kHOM*`/`kEND*`.
        "home" if code == 0 => b"\x1b[1~".to_vec(),
        "end" if code == 0 => b"\x1b[4~".to_vec(),
        "home" => cursor('H'),
        "end" => cursor('F'),
        // Ctrl- and Shift-Insert are copy and paste on some systems.
        "insert" if m.shift || m.control => return None,
        "insert" => b"\x1b[2~".to_vec(),
        "delete" => csi_mod("3", '~'),
        // Shift-Page scrolls the scrollback, which is the reading side's.
        "pageup" | "pagedown" if m.shift => return None,
        "pageup" if m.control => csi_mod("5", '~'),
        "pagedown" if m.control => csi_mod("6", '~'),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        "f1" => function_ss3(code, 'P'),
        "f2" => function_ss3(code, 'Q'),
        "f3" => function_ss3(code, 'R'),
        "f4" => function_ss3(code, 'S'),
        "f5" => csi_mod("15", '~'),
        "f6" => csi_mod("17", '~'),
        "f7" => csi_mod("18", '~'),
        "f8" => csi_mod("19", '~'),
        "f9" => csi_mod("20", '~'),
        "f10" => csi_mod("21", '~'),
        "f11" => csi_mod("23", '~'),
        "f12" => csi_mod("24", '~'),
        _ => return chord(keystroke, platform),
    })
}

fn function_ss3(code: u8, last: char) -> Vec<u8> {
    if code == 0 {
        vec![ESC, b'O', last as u8]
    } else {
        format!("\x1b[1;{}{last}", code + 1).into_bytes()
    }
}

/// A printable key with Control or Alt held.
fn chord(keystroke: &Keystroke, platform: Platform) -> Option<Vec<u8>> {
    let m = keystroke.modifiers;
    let key = keystroke.key.as_str();
    let letter = (key.len() == 1)
        .then(|| key.as_bytes()[0].to_ascii_lowercase())
        .filter(u8::is_ascii_lowercase);
    if m.control && !m.alt && !m.platform {
        // GPUI's Linux backend names a shifted symbol by the symbol and drops
        // Shift; macOS keeps Shift on the unshifted key. Both mean Ctrl+_ and
        // Ctrl+@ here.
        match (key, m.shift) {
            ("_", _) | ("-", true) => return Some(vec![0x1f]),
            ("@", _) | ("2", true) => return Some(vec![0]),
            _ => {}
        }
        if m.shift {
            return None;
        }
        let byte = match key {
            "space" | "2" => 0,
            "3" | "4" | "5" | "6" | "7" => key.as_bytes()[0] - b'3' + 0x1b,
            "8" => 0x7f,
            "[" => 0x1b,
            "\\" => 0x1c,
            "]" => 0x1d,
            _ => letter? - b'a' + 1,
        };
        return Some(vec![byte]);
    }
    // On macOS Option is a third-level shift: what it types is text.
    if m.alt && !m.platform && platform != Platform::Mac {
        if let Some(letter) = letter {
            let byte = if m.control {
                letter - b'a' + 1
            } else if m.shift {
                letter.to_ascii_uppercase()
            } else {
                letter
            };
            return Some(vec![ESC, byte]);
        }
        if key == "space" {
            return Some(vec![ESC, if m.control { 0 } else { b' ' }]);
        }
        // Digits and punctuation, already shifted by the layout.
        let symbol = keystroke
            .key_char
            .as_deref()
            .filter(|text| text.len() == 1 && text.as_bytes()[0].is_ascii_graphic())
            .unwrap_or(key);
        if symbol.len() == 1 && symbol.as_bytes()[0].is_ascii_graphic() {
            return Some([&[ESC], symbol.as_bytes()].concat());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::Modifiers;

    fn press(spec: &'static str) -> Keystroke {
        Keystroke::parse(spec).expect("a valid keystroke")
    }

    fn linux(spec: &'static str) -> Option<Vec<u8>> {
        encode_key(&press(spec), false, Platform::Linux)
    }

    fn mac(spec: &'static str) -> Option<Vec<u8>> {
        encode_key(&press(spec), false, Platform::Mac)
    }

    fn bytes(text: &str) -> Option<Vec<u8>> {
        Some(text.as_bytes().to_vec())
    }

    #[test]
    fn editing_keys() {
        assert_eq!(linux("backspace"), bytes("\x7f"));
        assert_eq!(linux("ctrl-backspace"), bytes("\x08"));
        assert_eq!(linux("alt-backspace"), bytes("\x1b\x7f"));
        assert_eq!(linux("tab"), bytes("\t"));
        assert_eq!(linux("shift-tab"), bytes("\x1b[Z"));
        assert_eq!(linux("enter"), bytes("\r"));
        assert_eq!(linux("alt-enter"), bytes("\x1b\r"));
        assert_eq!(linux("escape"), bytes("\x1b"));
        assert_eq!(linux("alt-escape"), bytes("\x1b\x1b"));
        assert_eq!(linux("insert"), bytes("\x1b[2~"));
        assert_eq!(linux("shift-insert"), None);
        assert_eq!(linux("delete"), bytes("\x1b[3~"));
        assert_eq!(linux("ctrl-delete"), bytes("\x1b[3;5~"));
    }

    #[test]
    fn cursor_keys_follow_the_cursor_key_mode_unless_modified() {
        assert_eq!(linux("up"), bytes("\x1b[A"));
        assert_eq!(linux("down"), bytes("\x1b[B"));
        assert_eq!(linux("right"), bytes("\x1b[C"));
        assert_eq!(linux("left"), bytes("\x1b[D"));
        let application = |spec| encode_key(&press(spec), true, Platform::Linux);
        assert_eq!(application("up"), bytes("\x1bOA"));
        // A modified key is the same in either mode.
        assert_eq!(application("ctrl-left"), bytes("\x1b[1;5D"));
        assert_eq!(linux("shift-up"), bytes("\x1b[1;2A"));
        assert_eq!(linux("alt-right"), bytes("\x1b[1;3C"));
        assert_eq!(linux("ctrl-shift-end"), bytes("\x1b[1;6F"));
        // Command-Arrow is the policy's, or an app shortcut's.
        assert_eq!(mac("cmd-left"), None);
    }

    #[test]
    fn home_and_end_are_tmux_keys_in_either_cursor_key_mode() {
        for application_cursor in [false, true] {
            let encode = |spec| encode_key(&press(spec), application_cursor, Platform::Linux);
            assert_eq!(encode("home"), bytes("\x1b[1~"));
            assert_eq!(encode("end"), bytes("\x1b[4~"));
        }
        assert_eq!(linux("shift-home"), bytes("\x1b[1;2H"));
    }

    #[test]
    fn page_keys_leave_shift_to_the_scrollback() {
        assert_eq!(linux("pageup"), bytes("\x1b[5~"));
        assert_eq!(linux("pagedown"), bytes("\x1b[6~"));
        assert_eq!(linux("ctrl-pageup"), bytes("\x1b[5;5~"));
        assert_eq!(linux("shift-pageup"), None);
        assert_eq!(linux("shift-pagedown"), None);
    }

    #[test]
    fn function_keys() {
        assert_eq!(linux("f1"), bytes("\x1bOP"));
        assert_eq!(linux("f4"), bytes("\x1bOS"));
        assert_eq!(linux("shift-f1"), bytes("\x1b[1;2P"));
        assert_eq!(linux("f5"), bytes("\x1b[15~"));
        assert_eq!(linux("f6"), bytes("\x1b[17~"));
        assert_eq!(linux("f11"), bytes("\x1b[23~"));
        assert_eq!(linux("f12"), bytes("\x1b[24~"));
        assert_eq!(linux("ctrl-f12"), bytes("\x1b[24;5~"));
    }

    #[test]
    fn control_chords_become_c0_controls() {
        assert_eq!(linux("ctrl-a"), bytes("\x01"));
        assert_eq!(linux("ctrl-c"), bytes("\x03"));
        assert_eq!(linux("ctrl-z"), bytes("\x1a"));
        assert_eq!(linux("ctrl-space"), bytes("\0"));
        assert_eq!(linux("ctrl-2"), bytes("\0"));
        assert_eq!(linux("ctrl-["), bytes("\x1b"));
        assert_eq!(linux("ctrl-\\"), bytes("\x1c"));
        assert_eq!(linux("ctrl-]"), bytes("\x1d"));
        assert_eq!(linux("ctrl-3"), bytes("\x1b"));
        assert_eq!(linux("ctrl-7"), bytes("\x1f"));
        assert_eq!(linux("ctrl-8"), bytes("\x7f"));
        assert_eq!(mac("ctrl-a"), bytes("\x01"));
        // Linux names the shifted symbol; macOS keeps Shift on the key.
        assert_eq!(linux("ctrl-_"), bytes("\x1f"));
        assert_eq!(mac("ctrl-shift--"), bytes("\x1f"));
        assert_eq!(linux("ctrl-@"), bytes("\0"));
        assert_eq!(mac("ctrl-shift-2"), bytes("\0"));
        // Ctrl+Shift+letter types nothing, as in xterm.js.
        assert_eq!(linux("ctrl-shift-a"), None);
    }

    #[test]
    fn alt_is_an_escape_prefix_except_where_it_is_macos_option() {
        assert_eq!(linux("alt-b"), bytes("\x1bb"));
        assert_eq!(linux("alt-shift-b"), bytes("\x1bB"));
        assert_eq!(linux("ctrl-alt-b"), bytes("\x1b\x02"));
        assert_eq!(linux("alt-space"), bytes("\x1b "));
        assert_eq!(linux("alt-."), bytes("\x1b."));
        let mut shifted_digit = press("alt-!");
        shifted_digit.key_char = Some("!".into());
        assert_eq!(
            encode_key(&shifted_digit, false, Platform::Linux),
            bytes("\x1b!")
        );
        // Option types its third-level character through text input.
        assert_eq!(mac("alt-b"), None);
        assert_eq!(mac("alt-left"), bytes("\x1b[1;3D"));
    }

    #[test]
    fn text_is_left_to_the_platform_text_input() {
        let mut typed = Keystroke {
            modifiers: Modifiers::none(),
            key: "a".into(),
            key_char: Some("a".into()),
        };
        assert_eq!(encode_key(&typed, false, Platform::Linux), None);
        typed.modifiers.shift = true;
        typed.key_char = Some("A".into());
        assert_eq!(encode_key(&typed, false, Platform::Linux), None);
        assert_eq!(linux("space"), None);
        assert_eq!(mac("cmd-a"), None);
        assert_eq!(linux("super-a"), None);
    }
}
