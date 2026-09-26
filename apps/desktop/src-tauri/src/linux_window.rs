//! Who draws the window's title bar on Linux.
//!
//! Tauri's Linux window is a GTK window, and with decorations on, GTK draws its
//! own client-side title bar — a second, thicker bar above Muxflow's, and one
//! tiling compositors such as Hyprland never ask for. So Muxflow turns GTK's
//! off and its own 28px bar takes over: with compact window buttons on a
//! floating-window desktop, and bare on a tiling one, where the compositor
//! owns placement and a close button is noise. tao still gives an undecorated
//! window a resize border, so edge resizing survives.
//!
//! Two environment variables override the guess:
//! `MUXFLOW_NATIVE_DECORATIONS=1` restores GTK's bar (the kill switch), and
//! `MUXFLOW_WINDOW_CONTROLS=show|hide` forces the buttons on or off.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChromeMode {
    /// GTK draws the title bar; Muxflow's bar carries no window buttons.
    Native,
    /// Muxflow's bar carries minimize, maximize and close.
    Buttons,
    /// No window buttons anywhere: the compositor manages the window.
    Bare,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WindowChrome {
    pub mode: ChromeMode,
    /// `XDG_CURRENT_DESKTOP` as seen, for the incident journal.
    pub desktop: String,
    pub reason: &'static str,
}

/// Desktops whose compositor tiles windows, matched against each
/// colon-separated entry of `XDG_CURRENT_DESKTOP`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const TILING_DESKTOPS: &[&str] = &[
    "hyprland", "sway", "niri", "river", "i3", "qtile", "bspwm", "dwm",
];

/// The decision, from the three variables it reads. Pure, so it can be tested.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn classify(
    native_decorations: Option<&str>,
    window_controls: Option<&str>,
    desktop: Option<&str>,
) -> WindowChrome {
    let desktop_name = desktop.unwrap_or_default().to_owned();
    let chrome = |mode, reason| WindowChrome {
        mode,
        desktop: desktop_name.clone(),
        reason,
    };
    if native_decorations.is_some_and(|value| value.trim() == "1") {
        return chrome(ChromeMode::Native, "MUXFLOW_NATIVE_DECORATIONS");
    }
    match window_controls
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("show") => return chrome(ChromeMode::Buttons, "MUXFLOW_WINDOW_CONTROLS"),
        Some("hide") => return chrome(ChromeMode::Bare, "MUXFLOW_WINDOW_CONTROLS"),
        _ => {}
    }
    let tiling = desktop_name.split(':').any(|entry| {
        let entry = entry.trim().to_ascii_lowercase();
        TILING_DESKTOPS.contains(&entry.as_str())
    });
    if tiling {
        chrome(ChromeMode::Bare, "tiling desktop")
    } else {
        chrome(ChromeMode::Buttons, "floating desktop")
    }
}

#[cfg(target_os = "linux")]
pub fn current() -> WindowChrome {
    let var = |name| std::env::var(name).ok();
    classify(
        var("MUXFLOW_NATIVE_DECORATIONS").as_deref(),
        var("MUXFLOW_WINDOW_CONTROLS").as_deref(),
        var("XDG_CURRENT_DESKTOP").as_deref(),
    )
}

#[cfg(not(target_os = "linux"))]
pub fn current() -> WindowChrome {
    WindowChrome {
        mode: ChromeMode::Native,
        desktop: String::new(),
        reason: "not Linux",
    }
}

/// Turn GTK's title bar off unless the native one was asked for.
pub fn apply(window: &tauri::WebviewWindow) -> Result<(), String> {
    if current().mode == ChromeMode::Native {
        return Ok(());
    }
    window
        .set_decorations(false)
        .map_err(|error| format!("turn off native window decorations: {error}"))
}

#[tauri::command]
pub fn window_chrome() -> WindowChrome {
    current()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(native: Option<&str>, controls: Option<&str>, desktop: Option<&str>) -> ChromeMode {
        classify(native, controls, desktop).mode
    }

    #[test]
    fn tiling_desktops_get_a_bare_bar() {
        assert_eq!(mode(None, None, Some("Hyprland")), ChromeMode::Bare);
        assert_eq!(mode(None, None, Some("sway")), ChromeMode::Bare);
        assert_eq!(mode(None, None, Some("niri")), ChromeMode::Bare);
        assert_eq!(mode(None, None, Some("i3")), ChromeMode::Bare);
    }

    #[test]
    fn floating_and_unknown_desktops_get_buttons() {
        assert_eq!(mode(None, None, Some("ubuntu:GNOME")), ChromeMode::Buttons);
        assert_eq!(mode(None, None, Some("KDE")), ChromeMode::Buttons);
        assert_eq!(mode(None, None, Some("")), ChromeMode::Buttons);
        assert_eq!(mode(None, None, None), ChromeMode::Buttons);
    }

    #[test]
    fn any_colon_separated_entry_can_name_the_tiling_desktop() {
        assert_eq!(mode(None, None, Some("wlroots:sway")), ChromeMode::Bare);
        // A substring is not a match: "i3" inside another name is not i3.
        assert_eq!(mode(None, None, Some("xi3fake")), ChromeMode::Buttons);
    }

    #[test]
    fn the_kill_switch_wins_over_everything() {
        assert_eq!(
            mode(Some("1"), Some("show"), Some("GNOME")),
            ChromeMode::Native
        );
        assert_eq!(mode(Some("1"), None, Some("Hyprland")), ChromeMode::Native);
        assert_eq!(mode(Some("0"), None, Some("Hyprland")), ChromeMode::Bare);
    }

    #[test]
    fn window_controls_force_the_buttons_either_way() {
        assert_eq!(
            mode(None, Some("show"), Some("Hyprland")),
            ChromeMode::Buttons
        );
        assert_eq!(mode(None, Some("HIDE"), Some("GNOME")), ChromeMode::Bare);
        assert_eq!(
            mode(None, Some("maybe"), Some("GNOME")),
            ChromeMode::Buttons
        );
    }

    #[test]
    fn the_reason_names_what_decided() {
        assert_eq!(
            classify(None, None, Some("Hyprland")).reason,
            "tiling desktop"
        );
        assert_eq!(
            classify(Some("1"), None, None).reason,
            "MUXFLOW_NATIVE_DECORATIONS"
        );
    }
}
