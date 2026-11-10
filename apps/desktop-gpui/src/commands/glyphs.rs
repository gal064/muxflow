//! Renders a stored shortcut in the notation the platform's own menus use. Port
//! of `apps/desktop/src/commands/shortcutGlyphs.ts`.

use super::registry::Platform;

/// Display order on both platforms, the approved mock's ⌃⌥⌘⇧.
const MODIFIER_ORDER: [&str; 4] = ["Ctrl", "Alt", "Meta", "Shift"];

fn modifier_symbol(modifier: &str, platform: Platform) -> &'static str {
    match (modifier, platform) {
        ("Ctrl", Platform::Mac) => "⌃",
        ("Alt", Platform::Mac) => "⌥",
        ("Shift", Platform::Mac) => "⇧",
        ("Meta", Platform::Mac) => "⌘",
        ("Ctrl", Platform::Linux) => "Ctrl",
        ("Alt", Platform::Linux) => "Alt",
        ("Shift", Platform::Linux) => "Shift",
        _ => "Super",
    }
}

fn mac_key_glyph(key: &str) -> Option<&'static str> {
    Some(match key {
        "ArrowLeft" => "←",
        "ArrowRight" => "→",
        "ArrowUp" => "↑",
        "ArrowDown" => "↓",
        "Enter" | "Return" => "↩",
        "Escape" => "⎋",
        "Backspace" => "⌫",
        "Delete" => "⌦",
        "Tab" => "⇥",
        " " | "Space" => "Space",
        "PageUp" => "⇞",
        "PageDown" => "⇟",
        "Home" => "↖",
        "End" => "↘",
        _ => return None,
    })
}

fn linux_key_name(key: &str) -> Option<&'static str> {
    Some(match key {
        "ArrowLeft" => "Left",
        "ArrowRight" => "Right",
        "ArrowUp" => "Up",
        "ArrowDown" => "Down",
        " " => "Space",
        _ => return None,
    })
}

fn split(shortcut: &str) -> (Vec<&'static str>, Option<&str>) {
    let parts: Vec<&str> = shortcut
        .split('+')
        .filter(|part| !part.is_empty())
        .collect();
    let modifiers = MODIFIER_ORDER
        .iter()
        .copied()
        .filter(|modifier| parts.contains(modifier))
        .collect();
    let key = parts
        .iter()
        .copied()
        .find(|part| !MODIFIER_ORDER.contains(part));
    (modifiers, key)
}

fn upper_if_single(key: &str) -> String {
    if key.chars().count() == 1 {
        key.to_uppercase()
    } else {
        key.to_owned()
    }
}

/// Nothing for an unbound command, so it draws no empty lozenge.
pub fn shortcut_glyphs(shortcut: Option<&str>, platform: Platform) -> Option<String> {
    let (modifiers, key) = split(shortcut?);
    let mut pieces: Vec<String> = modifiers
        .iter()
        .map(|modifier| modifier_symbol(modifier, platform).to_owned())
        .collect();
    if let Some(key) = key {
        let named = match platform {
            Platform::Mac => mac_key_glyph(key),
            Platform::Linux => linux_key_name(key),
        };
        pieces.push(named.map_or_else(|| upper_if_single(key), str::to_owned));
    }
    if pieces.is_empty() {
        return None;
    }
    Some(match platform {
        Platform::Mac => pieces.concat(),
        Platform::Linux => pieces.join("+"),
    })
}

/// The same shortcut, spoken: every chip carries this as its accessible name,
/// because `⌘⇧P` announces nothing useful.
pub fn shortcut_spoken(shortcut: Option<&str>, platform: Platform) -> Option<String> {
    let (modifiers, key) = split(shortcut?);
    let mut pieces: Vec<String> = modifiers
        .iter()
        .map(|modifier| {
            match (*modifier, platform) {
                ("Ctrl", _) => "Control",
                ("Alt", Platform::Mac) => "Option",
                ("Alt", Platform::Linux) => "Alt",
                ("Shift", _) => "Shift",
                ("Meta", Platform::Mac) => "Command",
                _ => "Super",
            }
            .to_owned()
        })
        .collect();
    if let Some(key) = key {
        pieces.push(if key.chars().count() == 1 {
            key.to_uppercase()
        } else {
            split_camel_case(key)
        });
    }
    (!pieces.is_empty()).then(|| pieces.join(" "))
}

fn split_camel_case(key: &str) -> String {
    let mut spoken = String::new();
    let mut previous_lower = false;
    for character in key.chars() {
        if previous_lower && character.is_uppercase() {
            spoken.push(' ');
        }
        previous_lower = character.is_lowercase();
        spoken.push(character);
    }
    spoken
}

#[cfg(test)]
mod tests {
    use super::{
        super::{
            keymap::{ShortcutOverrides, shortcut_for},
            registry::COMMANDS,
        },
        *,
    };
    use Platform::{Linux, Mac};

    #[test]
    fn mac_shortcuts_are_unseparated_glyphs_in_the_mock_order() {
        for (shortcut, expected) in [
            ("Meta+Shift+P", "⌘⇧P"),
            ("Meta+K", "⌘K"),
            ("Ctrl+Meta+Shift+=", "⌃⌘⇧="),
            ("Meta+Shift+Enter", "⌘⇧↩"),
            ("Meta+Alt+B", "⌥⌘B"),
            ("Meta+Alt+ArrowLeft", "⌥⌘←"),
        ] {
            assert_eq!(
                shortcut_glyphs(Some(shortcut), Mac).as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn linux_shortcuts_are_words() {
        for (shortcut, expected) in [
            ("Ctrl+Shift+P", "Ctrl+Shift+P"),
            ("Alt+ArrowLeft", "Alt+Left"),
            ("Meta+B", "Super+B"),
        ] {
            assert_eq!(
                shortcut_glyphs(Some(shortcut), Linux).as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn an_unbound_command_renders_nothing() {
        assert_eq!(shortcut_glyphs(None, Mac), None);
        assert_eq!(shortcut_glyphs(Some(""), Mac), None);
        assert_eq!(shortcut_glyphs(Some("Meta+"), Mac).as_deref(), Some("⌘"));
    }

    #[test]
    fn speaks_what_it_draws() {
        for (shortcut, platform, expected) in [
            ("Meta+Shift+P", Mac, "Command Shift P"),
            ("Meta+Alt+ArrowLeft", Mac, "Option Command Arrow Left"),
            ("Ctrl+Meta+Shift+=", Mac, "Control Command Shift ="),
            ("Ctrl+Shift+P", Linux, "Control Shift P"),
        ] {
            assert_eq!(
                shortcut_spoken(Some(shortcut), platform).as_deref(),
                Some(expected)
            );
        }
        assert_eq!(shortcut_spoken(None, Mac), None);
    }

    #[test]
    fn no_default_renders_as_a_raw_stored_binding() {
        let none = ShortcutOverrides::new();
        for platform in [Mac, Linux] {
            for command in &COMMANDS {
                let Some(rendered) =
                    shortcut_glyphs(shortcut_for(command, platform, &none), platform)
                else {
                    continue;
                };
                assert!(!rendered.contains("Meta"), "{}", command.id);
                if platform == Mac {
                    assert!(!rendered.contains('+'), "{}", command.id);
                }
            }
        }
    }
}
