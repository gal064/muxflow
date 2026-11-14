//! Embedded fonts: JetBrains Mono and Inter, the faces the Tauri app ships.
//!
//! `.woff2` is unreadable to GPUI's Linux text system (it drops the bytes with
//! no error), and variable Inter's weights are not honored there, so these are
//! TTFs and Inter is static instances. `assets/fonts/convert.sh` makes them.

use std::borrow::Cow;

use gpui_kit::App;

/// Family name for chrome text.
pub const SANS: &str = "Inter Variable";
/// Family name for terminal and code text.
pub const MONO: &str = "JetBrains Mono";

const FILES: [&[u8]; 10] = [
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Inter-Bold.ttf"),
    include_bytes!("../assets/fonts/Inter-Italic.ttf"),
    include_bytes!("../assets/fonts/Inter-BoldItalic.ttf"),
];

/// Registers the embedded fonts. Panics if a family is missing afterwards, so
/// a bad file fails loudly instead of falling back to a system font.
pub fn register(cx: &App) {
    let text_system = cx.text_system();
    text_system
        .add_fonts(FILES.iter().map(|bytes| Cow::Borrowed(*bytes)).collect())
        .expect("add the embedded fonts");
    let names = text_system.all_font_names();
    for family in [SANS, MONO] {
        assert!(
            names.iter().any(|name| name == family),
            "embedded font family {family:?} was not registered"
        );
    }
}
