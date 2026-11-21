//! Embedded fonts: JetBrains Mono and Inter, the same faces the Tauri app
//! ships.
//!
//! GPUI's Linux text system (cosmic-text / fontdb) cannot read `.woff2`; it
//! drops the bytes without an error. The files here are TTFs converted
//! losslessly from `apps/desktop/src/assets/fonts/*.woff2` (see
//! `assets/fonts/convert.sh`).
//!
//! The Inter file's family name is "Inter Variable", not "Inter" as the CSS
//! `@font-face` calls it. [`SANS`] uses the real name, which also means a
//! system-installed "Inter" can never be picked up by accident.

use std::borrow::Cow;

use gpui_kit::App;

/// Family name for chrome text.
pub const SANS: &str = "Inter Variable";
/// Family name for terminal and code text.
pub const MONO: &str = "JetBrains Mono";

const FILES: [&[u8]; 6] = [
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/InterVariable.ttf"),
    include_bytes!("../assets/fonts/InterVariable-Italic.ttf"),
];

/// Register the embedded fonts with GPUI's text system.
///
/// Panics if the families are missing afterwards, so a bad font file fails
/// loudly instead of silently falling back to a system font.
pub fn register(cx: &App) {
    let text_system = cx.text_system();
    text_system
        .add_fonts(FILES.iter().map(|b| Cow::Borrowed(*b)).collect())
        .expect("failed to add embedded fonts");
    let names = text_system.all_font_names();
    for family in [SANS, MONO] {
        assert!(
            names.iter().any(|n| n == family),
            "embedded font family {family:?} was not registered"
        );
    }
}
