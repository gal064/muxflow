//! Shortcut strings and what a keystroke means. Port of the keyboard half of
//! `apps/desktop/src/commands/registry.ts`.
//!
//! Shortcuts are stored in the portable form the shortcut editor round-trips
//! (`Meta+Shift+P`). A key press is turned into that form and compared, rather
//! than registered with GPUI's keymap, so an override takes effect at once and a
//! hand-edited override can never be an unparsable GPUI binding.

use std::collections::HashMap;

use gpui_kit::Keystroke;

use super::registry::{COMMANDS, CommandDefinition, CommandId, Platform, selection_index};

/// Command id to binding; `None` is a deliberately removed binding.
pub type ShortcutOverrides = HashMap<String, Option<String>>;

pub fn shortcut_for<'a>(
    command: &CommandDefinition,
    platform: Platform,
    overrides: &'a ShortcutOverrides,
) -> Option<&'a str> {
    match overrides.get(command.id) {
        Some(None) => None,
        Some(Some(shortcut)) => is_safe_shortcut(shortcut).then_some(shortcut.as_str()),
        None => command.default_shortcut(platform),
    }
}

const MODIFIER_ORDER: [&str; 4] = ["Ctrl", "Alt", "Shift", "Meta"];

pub fn normalize_shortcut(shortcut: &str) -> String {
    let parts: Vec<&str> = shortcut
        .split('+')
        .filter(|part| !part.is_empty())
        .collect();
    let mut normalized: Vec<String> = MODIFIER_ORDER
        .iter()
        .filter(|modifier| parts.contains(modifier))
        .map(|modifier| (*modifier).to_owned())
        .collect();
    if let Some(key) = parts.iter().find(|part| !MODIFIER_ORDER.contains(part)) {
        normalized.push(match *key {
            " " => "Space".to_owned(),
            key if key.chars().count() == 1 => key.to_uppercase(),
            key => key.to_owned(),
        });
    }
    normalized.join("+")
}

pub fn is_safe_shortcut(shortcut: &str) -> bool {
    let normalized = normalize_shortcut(shortcut);
    let parts: Vec<&str> = normalized.split('+').collect();
    let key = parts.last().copied().unwrap_or("");
    let has_command_modifier = ["Ctrl", "Alt", "Meta"]
        .iter()
        .any(|modifier| parts.contains(modifier));
    has_command_modifier && !key.is_empty() && !["Control", "Alt", "Shift", "Meta"].contains(&key)
}

/// Unsafe, or naming a key GPUI has no name for, such as `Meta+Shift+BMeta+B`:
/// that one would never match a press and would drop out of the menus.
pub fn unsafe_shortcut_bindings(overrides: &ShortcutOverrides) -> Vec<CommandId> {
    COMMANDS
        .iter()
        .filter(|command| {
            matches!(overrides.get(command.id), Some(Some(value))
                if !is_safe_shortcut(value) || crate::menus::gpui_keystroke(value).is_none())
        })
        .map(|command| command.id)
        .collect()
}

/// GPUI reports the character a Shift chord typed, and clears Shift for it, so
/// ⌘⇧[ arrives as `cmd-{`. A binding is written against the unshifted key.
fn unshifted(key: &str) -> Option<&'static str> {
    Some(match key {
        "{" => "[",
        "}" => "]",
        ":" => ";",
        "\"" => "'",
        "~" => "`",
        "<" => ",",
        ">" => ".",
        "?" => "/",
        "|" => "\\",
        "_" => "-",
        "+" => "=",
        "!" => "1",
        "@" => "2",
        "#" => "3",
        "$" => "4",
        "%" => "5",
        "^" => "6",
        "&" => "7",
        "*" => "8",
        "(" => "9",
        ")" => "0",
        _ => return None,
    })
}

/// A GPUI key name in the stored form.
fn stored_key(key: &str) -> String {
    match key {
        "left" => "ArrowLeft".to_owned(),
        "right" => "ArrowRight".to_owned(),
        "up" => "ArrowUp".to_owned(),
        "down" => "ArrowDown".to_owned(),
        "enter" => "Enter".to_owned(),
        "escape" => "Escape".to_owned(),
        "backspace" => "Backspace".to_owned(),
        "delete" => "Delete".to_owned(),
        "tab" => "Tab".to_owned(),
        "space" => "Space".to_owned(),
        "pageup" => "PageUp".to_owned(),
        "pagedown" => "PageDown".to_owned(),
        "home" => "Home".to_owned(),
        "end" => "End".to_owned(),
        "insert" => "Insert".to_owned(),
        key if key.len() > 1 && key.starts_with('f') && key[1..].parse::<u8>().is_ok() => {
            key.to_uppercase()
        }
        key if key.chars().count() == 1 => key.to_uppercase(),
        key => key.to_owned(),
    }
}

pub fn shortcut_from_keystroke(keystroke: &Keystroke) -> String {
    let modifiers = keystroke.modifiers;
    let unshifted = unshifted(&keystroke.key);
    let key = stored_key(unshifted.unwrap_or(&keystroke.key));
    let mut parts: Vec<&str> = Vec::new();
    if modifiers.control {
        parts.push("Ctrl");
    }
    if modifiers.alt {
        parts.push("Alt");
    }
    if modifiers.shift || unshifted.is_some() {
        parts.push("Shift");
    }
    if modifiers.platform {
        parts.push("Meta");
    }
    parts.push(&key);
    normalize_shortcut(&parts.join("+"))
}

pub fn command_for_shortcut(
    pressed: &str,
    platform: Platform,
    overrides: &ShortcutOverrides,
) -> Option<&'static CommandDefinition> {
    let mut matches = COMMANDS.iter().filter(|command| {
        shortcut_for(command, platform, overrides)
            .is_some_and(|shortcut| normalize_shortcut(shortcut) == pressed)
    });
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

/// What the app does with a keystroke, decided before anything acts on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutDisposition {
    /// Not the app's: it continues to the focused element and the platform.
    Ignore,
    /// The app's, but its command cannot run. Consumed anyway: ⌘W handed back
    /// to macOS closes the only window, which quits mid-session.
    Swallow,
    /// A claimed chord left for the terminal: Linux Ctrl+C with nothing to copy
    /// is the shell's interrupt. Acts like `Ignore`.
    Yield(CommandId),
    Run(CommandId),
}

/// `available` is the caller's answer for one command. `text_field` is a focused
/// Kit input that keeps this chord for itself; the terminal surface is not one,
/// so it keeps its shortcuts.
pub fn shortcut_disposition(
    pressed: &str,
    overlay_open: bool,
    text_field: bool,
    platform: Platform,
    overrides: &ShortcutOverrides,
    has_terminal_selection: bool,
    available: impl Fn(&CommandDefinition) -> bool,
) -> ShortcutDisposition {
    let Some(command) = command_for_shortcut(pressed, platform, overrides) else {
        return ShortcutDisposition::Ignore;
    };
    if overlay_open || (text_field && !is_positional_selector(pressed, command)) {
        return ShortcutDisposition::Ignore;
    }
    if !available(command) {
        return if command.native_fallback {
            ShortcutDisposition::Ignore
        } else {
            ShortcutDisposition::Swallow
        };
    }
    if platform == Platform::Linux
        && command.id == "terminal.copy"
        && pressed == "Ctrl+C"
        && !has_terminal_selection
    {
        return ShortcutDisposition::Yield(command.id);
    }
    ShortcutDisposition::Run(command.id)
}

/// Positional navigation is global by design: switching a workspace or tab must
/// work while an editor owns focus. Overlays still win.
fn is_positional_selector(pressed: &str, command: &CommandDefinition) -> bool {
    pressed
        .rsplit('+')
        .next()
        .is_some_and(|key| key.len() == 1 && key.as_bytes()[0].is_ascii_digit())
        && (selection_index(command.id, "workspace.select").is_some()
            || selection_index(command.id, "tab.select").is_some())
}

#[derive(Debug, PartialEq, Eq)]
pub struct ShortcutCollision {
    pub shortcut: String,
    pub command_ids: Vec<CommandId>,
}

pub fn shortcut_collisions(
    platform: Platform,
    overrides: &ShortcutOverrides,
) -> Vec<ShortcutCollision> {
    let mut bindings: Vec<(String, Vec<CommandId>)> = Vec::new();
    for command in &COMMANDS {
        let Some(shortcut) = shortcut_for(command, platform, overrides) else {
            continue;
        };
        let normalized = normalize_shortcut(shortcut);
        match bindings.iter_mut().find(|(known, _)| *known == normalized) {
            Some((_, ids)) => ids.push(command.id),
            None => bindings.push((normalized, vec![command.id])),
        }
    }
    bindings
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(shortcut, command_ids)| ShortcutCollision {
            shortcut,
            command_ids,
        })
        .collect()
}

/// Repairs a keymap written by a build that allowed conflicting bindings, and
/// returns what it took away. Only overrides are removed. Removing one can
/// reveal a default and with it another collision, so this runs until unique.
pub fn repair_shortcut_collisions(
    platform: Platform,
    overrides: &mut ShortcutOverrides,
) -> Vec<(CommandId, String)> {
    let mut displaced = Vec::new();
    while let Some(collision) = shortcut_collisions(platform, overrides).into_iter().next() {
        // One explicit customization wins over defaults introduced later; of two
        // old customizations the first registry binding is kept.
        let keep = collision
            .command_ids
            .iter()
            .copied()
            .find(|id| matches!(overrides.get(*id), Some(Some(_))));
        for id in collision.command_ids {
            if Some(id) == keep {
                continue;
            }
            let lost = match overrides.get(id) {
                Some(Some(shortcut)) => Some(shortcut.clone()),
                _ => super::registry::command_by_id(id)
                    .and_then(|command| command.default_shortcut(platform))
                    .map(str::to_owned),
            };
            if let Some(lost) = lost {
                displaced.push((id, lost));
            }
            overrides.insert(id.to_owned(), None);
        }
    }
    displaced
}

#[cfg(test)]
mod tests {
    use gpui_kit::Modifiers;

    use super::*;

    fn overrides(entries: &[(&str, Option<&str>)]) -> ShortcutOverrides {
        entries
            .iter()
            .map(|(id, binding)| ((*id).to_owned(), binding.map(str::to_owned)))
            .collect()
    }

    fn pressed(stroke: &str) -> String {
        // `Keystroke::parse` keeps the source for its error, so tests leak it.
        let stroke: &'static str = Box::leak(stroke.to_owned().into_boxed_str());
        shortcut_from_keystroke(&Keystroke::parse(stroke).unwrap())
    }

    fn id_on(platform: Platform, stroke: &str, keymap: &ShortcutOverrides) -> Option<CommandId> {
        command_for_shortcut(&pressed(stroke), platform, keymap).map(|command| command.id)
    }

    fn disposition(
        stroke: &str,
        overlay_open: bool,
        text_field: bool,
        available: bool,
    ) -> ShortcutDisposition {
        shortcut_disposition(
            &pressed(stroke),
            overlay_open,
            text_field,
            Platform::Mac,
            &ShortcutOverrides::new(),
            true,
            |_| available,
        )
    }

    /// The Linux defaults for the Ghostty fallbacks, from a real keystroke.
    #[test]
    fn linux_chords_resolve_with_ghostty_fallbacks() {
        let none = ShortcutOverrides::new();
        assert_eq!(
            id_on(Platform::Mac, "cmd-shift-w", &none),
            Some("window.closeNonAgent")
        );
        assert_eq!(
            id_on(Platform::Linux, "ctrl-shift-w", &none),
            Some("window.closeNonAgent")
        );
        assert_eq!(
            id_on(Platform::Linux, "ctrl-shift-o", &none),
            Some("pane.splitRight")
        );
        assert_eq!(
            id_on(Platform::Linux, "ctrl-shift-d", &none),
            Some("pane.splitDown")
        );
        assert_eq!(
            id_on(Platform::Linux, "ctrl-shift-enter", &none),
            Some("pane.zoom")
        );
        // The terminal keeps Ctrl+D (EOF) and Ctrl+E (end of line).
        assert_eq!(id_on(Platform::Linux, "ctrl-d", &none), None);
        assert_eq!(id_on(Platform::Linux, "ctrl-e", &none), None);
    }

    #[test]
    fn shifted_punctuation_reads_as_the_unshifted_key() {
        let none = ShortcutOverrides::new();
        // GPUI's Linux and macOS backends both report the shifted character.
        assert_eq!(
            id_on(Platform::Linux, "ctrl-{", &none),
            Some("tab.previous")
        );
        assert_eq!(id_on(Platform::Linux, "ctrl-}", &none), Some("tab.next"));
        assert_eq!(id_on(Platform::Mac, "cmd-{", &none), Some("tab.previous"));
    }

    #[test]
    fn arrows_and_digits_use_their_stored_names() {
        let none = ShortcutOverrides::new();
        assert_eq!(
            id_on(Platform::Mac, "cmd-alt-left", &none),
            Some("pane.focusLeft")
        );
        assert_eq!(
            id_on(Platform::Linux, "ctrl-shift-up", &none),
            Some("pane.resizeUp")
        );
        assert_eq!(id_on(Platform::Mac, "ctrl-4", &none), Some("tab.select4"));
        assert_eq!(id_on(Platform::Linux, "alt-7", &none), Some("tab.select7"));
        assert_eq!(
            id_on(Platform::Linux, "ctrl-1", &none),
            Some("workspace.select1")
        );
    }

    #[test]
    fn every_default_resolves_from_the_keystroke_that_produces_it() {
        let none = ShortcutOverrides::new();
        for (platform, primary) in [(Platform::Mac, "cmd"), (Platform::Linux, "ctrl")] {
            for command in &COMMANDS {
                let Some(shortcut) = command.default_shortcut(platform) else {
                    continue;
                };
                let stroke = shortcut
                    .split('+')
                    .map(|part| match part {
                        "Meta" => primary.to_owned(),
                        "Ctrl" => "ctrl".to_owned(),
                        "Alt" => "alt".to_owned(),
                        "Shift" => "shift".to_owned(),
                        "ArrowLeft" => "left".to_owned(),
                        "ArrowRight" => "right".to_owned(),
                        "ArrowUp" => "up".to_owned(),
                        "ArrowDown" => "down".to_owned(),
                        "Enter" => "enter".to_owned(),
                        key => key.to_lowercase(),
                    })
                    .collect::<Vec<_>>()
                    .join("-");
                assert_eq!(
                    id_on(platform, &stroke, &none),
                    Some(command.id),
                    "{stroke}"
                );
            }
        }
    }

    #[test]
    fn normalizes_modifier_order_and_honors_overrides_and_removals() {
        assert_eq!(normalize_shortcut("Shift+Ctrl+d"), "Ctrl+Shift+D");
        let new = super::super::registry::command_by_id("session.new").unwrap();
        let rebound = overrides(&[("session.new", Some("Alt+N"))]);
        assert_eq!(shortcut_for(new, Platform::Linux, &rebound), Some("Alt+N"));
        let removed = overrides(&[("session.new", None)]);
        assert_eq!(shortcut_for(new, Platform::Linux, &removed), None);
    }

    #[test]
    fn rejects_bare_printable_bindings() {
        assert!(!is_safe_shortcut("A"));
        assert!(!is_safe_shortcut("Shift+A"));
        assert!(is_safe_shortcut("Ctrl+A"));
        let bad = overrides(&[("session.new", Some("A"))]);
        assert_eq!(unsafe_shortcut_bindings(&bad), vec!["session.new"]);
        // An unsafe override never fires as its literal text.
        assert!(command_for_shortcut("A", Platform::Linux, &bad).is_none());
    }

    #[test]
    fn rejects_bindings_to_keys_without_a_name() {
        let typed = overrides(&[("session.new", Some("Meta+Shift+BMeta+B"))]);
        assert_eq!(unsafe_shortcut_bindings(&typed), vec!["session.new"]);
        let captured = overrides(&[
            ("session.new", Some("Ctrl+Insert")),
            ("commands.show", Some("Meta+Shift+ArrowLeft")),
        ]);
        assert_eq!(unsafe_shortcut_bindings(&captured), Vec::<CommandId>::new());
    }

    #[test]
    fn defaults_are_collision_free_and_overrides_collisions_are_detected() {
        let none = ShortcutOverrides::new();
        assert_eq!(shortcut_collisions(Platform::Linux, &none), vec![]);
        assert_eq!(shortcut_collisions(Platform::Mac, &none), vec![]);
        let clash = overrides(&[("session.new", Some("Ctrl+T"))]);
        assert_eq!(
            shortcut_collisions(Platform::Linux, &clash),
            vec![ShortcutCollision {
                shortcut: "Ctrl+T".into(),
                command_ids: vec!["session.new", "window.new"],
            }]
        );
        // A duplicate binding disables both commands.
        assert_eq!(id_on(Platform::Linux, "ctrl-t", &clash), None);
    }

    #[test]
    fn repair_keeps_the_customization_and_reports_the_lost_default() {
        let mut keymap = overrides(&[("window.new", Some("Meta+1"))]);
        let displaced = repair_shortcut_collisions(Platform::Mac, &mut keymap);
        assert_eq!(
            keymap,
            overrides(&[("window.new", Some("Meta+1")), ("workspace.select1", None)])
        );
        assert_eq!(displaced, vec![("workspace.select1", "Meta+1".to_owned())]);
        assert_eq!(shortcut_collisions(Platform::Mac, &keymap), vec![]);
    }

    #[test]
    fn repair_of_two_customizations_keeps_the_first_registry_binding() {
        let mut keymap = overrides(&[
            ("session.new", Some("Meta+J")),
            ("window.new", Some("Meta+J")),
        ]);
        let displaced = repair_shortcut_collisions(Platform::Mac, &mut keymap);
        assert_eq!(
            keymap,
            overrides(&[("session.new", Some("Meta+J")), ("window.new", None)])
        );
        assert_eq!(displaced, vec![("window.new", "Meta+J".to_owned())]);
        let mut empty = ShortcutOverrides::new();
        assert_eq!(
            repair_shortcut_collisions(Platform::Mac, &mut empty),
            vec![]
        );
    }

    #[test]
    fn runs_a_bound_shortcut_whose_command_can_act() {
        assert_eq!(
            disposition("cmd-w", false, false, true),
            ShortcutDisposition::Run("window.close")
        );
    }

    #[test]
    fn swallows_a_bound_shortcut_whose_command_cannot_act() {
        assert_eq!(
            disposition("cmd-w", false, false, false),
            ShortcutDisposition::Swallow
        );
    }

    #[test]
    fn leaves_chords_the_keymap_never_claimed_alone() {
        assert_eq!(
            disposition("cmd-y", false, false, true),
            ShortcutDisposition::Ignore
        );
    }

    #[test]
    fn yields_every_chord_while_an_overlay_owns_the_screen() {
        assert_eq!(
            disposition("cmd-w", true, false, true),
            ShortcutDisposition::Ignore
        );
        assert_eq!(
            disposition("cmd-1", true, false, true),
            ShortcutDisposition::Ignore
        );
    }

    #[test]
    fn a_text_field_keeps_editing_chords_but_positional_selectors_stay_global() {
        assert_eq!(
            disposition("cmd-c", false, true, true),
            ShortcutDisposition::Ignore
        );
        assert_eq!(
            disposition("cmd-3", false, true, true),
            ShortcutDisposition::Run("workspace.select3")
        );
    }

    #[test]
    fn a_selector_rebound_off_the_digits_is_not_positional() {
        let rebound = overrides(&[("workspace.select1", Some("Meta+F1"))]);
        let disposition = shortcut_disposition(
            "Meta+F1",
            false,
            true,
            Platform::Mac,
            &rebound,
            true,
            |_| true,
        );
        assert_eq!(disposition, ShortcutDisposition::Ignore);
    }

    #[test]
    fn a_native_fallback_chord_is_not_swallowed() {
        assert_eq!(
            disposition("cmd-c", false, false, false),
            ShortcutDisposition::Ignore
        );
        assert_eq!(
            disposition("cmd-v", false, false, false),
            ShortcutDisposition::Ignore
        );
        let flagged: Vec<_> = COMMANDS
            .iter()
            .filter(|command| command.native_fallback)
            .map(|command| command.id)
            .collect();
        assert_eq!(flagged, ["terminal.copy", "terminal.paste"]);
    }

    fn linux_copy(selected: bool, keymap: &ShortcutOverrides, stroke: &str) -> ShortcutDisposition {
        shortcut_disposition(
            &pressed(stroke),
            false,
            false,
            Platform::Linux,
            keymap,
            selected,
            |_| true,
        )
    }

    #[test]
    fn linux_ctrl_c_copies_only_with_a_selection() {
        let none = ShortcutOverrides::new();
        assert_eq!(
            linux_copy(true, &none, "ctrl-c"),
            ShortcutDisposition::Run("terminal.copy")
        );
        assert_eq!(
            linux_copy(false, &none, "ctrl-c"),
            ShortcutDisposition::Yield("terminal.copy")
        );
        assert_eq!(
            linux_copy(false, &none, "ctrl-v"),
            ShortcutDisposition::Run("terminal.paste")
        );
        let rebound = overrides(&[("terminal.copy", Some("Ctrl+Shift+Y"))]);
        assert_eq!(
            linux_copy(false, &rebound, "ctrl-shift-y"),
            ShortcutDisposition::Run("terminal.copy")
        );
    }

    #[test]
    fn modifier_only_keystrokes_claim_nothing() {
        let shift = Keystroke {
            modifiers: Modifiers::default(),
            key: "shift".into(),
            key_char: None,
        };
        let none = ShortcutOverrides::new();
        let stored = shortcut_from_keystroke(&shift);
        assert_eq!(
            command_for_shortcut(&stored, Platform::Linux, &none).map(|c| c.id),
            None
        );
    }
}
