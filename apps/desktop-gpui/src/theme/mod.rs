//! Light and dark, UI scale, and the Kit theme derived from them.
//!
//! Colors come from the generated [`palette`] (`packages/client-core`).
//! Features read [`palette(cx)`](palette()) for their own surfaces and
//! [`metrics`] for lengths; Kit components read the Kit theme set here.

pub mod metrics;
mod palette;

use std::rc::Rc;

use desktop_core::app_state::ThemePreference;
use gpui_kit::{
    App, Global, Rgba, WindowAppearance,
    component::{Theme, ThemeConfig},
};
pub use palette::{Palette, dark, light};
use serde_json::json;

use crate::fonts;

/// The theme on screen. A global, so any view can read it.
pub struct Appearance {
    pub preference: ThemePreference,
    pub is_dark: bool,
    pub scale: f32,
    pub palette: Palette,
}

impl Global for Appearance {}

/// The active palette.
pub fn palette(cx: &App) -> Palette {
    cx.global::<Appearance>().palette
}

/// The theme and UI scale to show, from the saved preferences.
pub fn apply(preference: ThemePreference, scale: f64, system: WindowAppearance, cx: &mut App) {
    let is_dark = resolve_dark(preference, system);
    let palette = if is_dark { dark() } else { light() };
    let scale = scale as f32;
    cx.set_global(Appearance {
        preference,
        is_dark,
        scale,
        palette,
    });
    let config = Rc::new(kit_config(&palette, is_dark, scale));
    Theme::update(cx, |theme| theme.apply_config(&config));
}

/// Re-applies when the preference follows the OS and the OS changed.
pub fn follow_system(system: WindowAppearance, cx: &mut App) {
    let appearance = cx.global::<Appearance>();
    let (preference, scale, was_dark) =
        (appearance.preference, appearance.scale, appearance.is_dark);
    if resolve_dark(preference, system) != was_dark {
        apply(preference, f64::from(scale), system, cx);
    }
}

fn resolve_dark(preference: ThemePreference, system: WindowAppearance) -> bool {
    match preference {
        ThemePreference::Dark => true,
        ThemePreference::Light => false,
        ThemePreference::System => {
            matches!(
                system,
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            )
        }
    }
}

/// The modal backdrop of `styles.css`: `--shadow` at 35%, over whatever the dialog
/// covers. Kit's own overlay is a flat 5% black, which left a dialog barely
/// lifted off the window in the light theme.
fn scrim(shadow: Rgba) -> Rgba {
    Rgba {
        a: shadow.a * 0.35,
        ..shadow
    }
}

fn hex(color: Rgba) -> String {
    let channel = |value: f32| (value * 255.).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        channel(color.r),
        channel(color.g),
        channel(color.b),
        channel(color.a)
    )
}

/// Kit derives every other component color from these. The UI scale drives the
/// rem size through `font.size`, which Kit's root applies each frame.
fn kit_config(palette: &Palette, is_dark: bool, scale: f32) -> ThemeConfig {
    let p = palette;
    let colors: serde_json::Map<String, serde_json::Value> = [
        ("background", p.chrome_bg),
        ("foreground", p.chrome_ink),
        ("border", p.chrome_border),
        ("input.border", p.chrome_border),
        ("ring", p.accent),
        ("selection.background", p.accent_wash),
        ("caret", p.chrome_ink_strong),
        ("link", p.accent),
        ("primary.background", p.accent),
        ("primary.foreground", p.accent_ink),
        ("secondary.background", p.chrome_raised),
        ("secondary.foreground", p.chrome_ink),
        ("secondary.hover.background", p.chrome_hover),
        ("muted.background", p.chrome_raised),
        ("muted.foreground", p.chrome_dim),
        ("accent.background", p.chrome_hover),
        ("accent.foreground", p.chrome_ink_strong),
        ("danger.background", p.danger),
        ("danger.foreground", p.accent_ink),
        ("success.background", p.ok),
        ("warning.background", p.warn),
        ("info.background", p.accent),
        ("popover.background", p.chrome_raised),
        ("popover.foreground", p.chrome_ink),
        ("list.background", p.chrome_bg),
        ("list.hover.background", p.chrome_hover),
        ("list.active.background", p.chrome_selected),
        ("list.active.border", p.chrome_selected_edge),
        ("sidebar.background", p.chrome_bg),
        ("sidebar.foreground", p.chrome_ink),
        ("sidebar.border", p.chrome_hairline),
        ("sidebar.accent.background", p.chrome_selected),
        ("sidebar.accent.foreground", p.chrome_ink_strong),
        ("sidebar.primary.background", p.accent),
        ("sidebar.primary.foreground", p.accent_ink),
        ("tab_bar.background", p.chrome_bg),
        ("tab.background", p.chrome_bg),
        ("tab.foreground", p.chrome_dim),
        ("tab.active.background", p.chrome_selected),
        ("tab.active.foreground", p.chrome_ink_strong),
        ("title_bar.background", p.chrome_bg),
        ("title_bar.border", p.chrome_hairline),
        ("drop_target.background", p.accent_wash),
        ("window.border", p.chrome_border),
        ("overlay", scrim(p.shadow)),
    ]
    .into_iter()
    .map(|(key, color)| (key.to_owned(), hex(color).into()))
    .collect();
    serde_json::from_value(json!({
        "name": if is_dark { "Muxflow Dark" } else { "Muxflow Light" },
        "mode": if is_dark { "dark" } else { "light" },
        "font.family": fonts::SANS,
        "font.size": 16. * scale,
        "mono_font.family": fonts::MONO,
        "mono_font.size": metrics::TERM_FONT_SIZE * scale,
        "radius": metrics::RADIUS_CONTROL as usize,
        "radius.lg": metrics::RADIUS_PANEL as usize,
        "colors": colors,
        "highlight": syntax(p),
    }))
    .expect("the Muxflow theme is valid Kit config")
}

/// The document surfaces' colors, port of `features/files/monaco.ts`. Syntax
/// is the terminal's ANSI palette, so an editor and a terminal showing the
/// same file agree about what a string looks like. Plain code is the chrome
/// ink, and punctuation, operators and comments the app's quiet ink: the ANSI
/// dim grey is too faint on this ground for the most common quiet token.
fn syntax(p: &Palette) -> serde_json::Value {
    let style = |color: Rgba| json!({ "color": hex(color) });
    let italic = |color: Rgba| json!({ "color": hex(color), "font_style": "italic" });
    let bold = |color: Rgba| json!({ "color": hex(color), "font_weight": 700 });
    json!({
        "editor.background": hex(p.chrome_bg),
        "editor.foreground": hex(p.chrome_ink),
        "editor.gutter.background": hex(p.chrome_bg),
        "editor.line_number": hex(p.chrome_faint),
        "editor.active_line_number": hex(p.chrome_ink),
        "editor.active_line.background": hex(p.chrome_hover),
        "syntax": {
            "comment": italic(p.chrome_dim),
            "keyword": style(p.term_13),
            "string": style(p.term_2),
            "string.escape": style(p.term_3),
            "string.regex": style(p.term_1),
            "number": style(p.term_3),
            "boolean": style(p.term_3),
            "constant": style(p.term_3),
            "attribute": style(p.term_3),
            "type": style(p.term_6),
            "constructor": style(p.term_6),
            "function": style(p.term_12),
            "tag": style(p.term_12),
            "title": bold(p.term_12),
            "variable.special": italic(p.term_1),
            "punctuation": style(p.chrome_dim),
            "operator": style(p.chrome_dim),
            "text.literal": style(p.term_2),
            "link_uri": style(p.term_4),
            "emphasis": italic(p.chrome_ink),
            "emphasis.strong": bold(p.chrome_ink),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preference_picks_the_palette() {
        use WindowAppearance::{Dark, Light};
        assert!(resolve_dark(ThemePreference::Dark, Light));
        assert!(!resolve_dark(ThemePreference::Light, Dark));
        assert!(resolve_dark(ThemePreference::System, Dark));
        assert!(!resolve_dark(ThemePreference::System, Light));
    }

    /// Port of `theme.test.ts`: the dark terminal is Ghostty's Default Style
    /// Dark, so it matches the user's own terminal, with an inverted selection.
    #[test]
    fn the_dark_terminal_is_ghostty_default_style_dark() {
        let palette = dark();
        assert_eq!(hex(palette.term_bg), "#282c34ff");
        assert_eq!(hex(palette.term_fg), "#ffffffff");
        assert_eq!(hex(palette.term_cursor), "#ffffffff");
        assert_eq!(hex(palette.term_selection_bg), "#ffffffff");
        assert_eq!(hex(palette.term_selection_fg), "#282c34ff");
    }

    #[test]
    fn the_backdrop_is_the_shadow_at_35_percent() {
        assert_eq!(hex(scrim(light().shadow)), "#18181b16");
        assert_eq!(hex(scrim(dark().shadow)), "#00000059");
    }

    #[test]
    fn hex_keeps_alpha() {
        assert_eq!(hex(dark().accent_wash), "#7aa6da1f");
        assert_eq!(hex(dark().chrome_bg), "#282c34ff");
    }
}
