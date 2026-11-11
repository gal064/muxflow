//! The app's own icon set, port of `ui/Icon.tsx`: 16-unit stroke paths at
//! SF-Symbols weight, embedded as SVG and drawn in the current text color, so a
//! control's color decides how its icon looks. The Explorer's file-type shapes
//! from the same set live with their mapping in `files/icons.rs`.
//!
//! Icons are decorative: the control around one carries its accessible name.

use gpui_kit::component::Icon;

/// The SVG for a 16-unit glyph drawn in the TypeScript set's 1.4 stroke.
macro_rules! glyph {
    ($body:literal) => {
        concat!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-linecap="round" stroke-linejoin="round" stroke-width="1.4">"##,
            $body,
            "</svg>"
        )
        .as_bytes()
    };
}
pub(crate) use glyph;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    SidebarLeft,
    PanelRight,
    Bell,
    Plus,
    Minus,
    Settings,
    Discard,
    Refresh,
    Close,
    CloseOthers,
    CloseNonAgent,
    More,
    ArrowLeft,
    ArrowRight,
    Diff,
    Zoom,
    Search,
    Folder,
    WindowMaximize,
    WindowRestore,
}

impl Glyph {
    pub fn svg(self) -> &'static [u8] {
        match self {
            Self::SidebarLeft => glyph!(
                r#"<rect height="11" rx="2" width="13" x="1.5" y="2.5"/><path d="M6 2.5v11"/><path d="M3.4 5.6h1.2M3.4 8h1.2"/>"#
            ),
            Self::PanelRight => glyph!(
                r#"<rect height="11" rx="2" width="13" x="1.5" y="2.5"/><path d="M10 2.5v11"/><path d="M11.4 5.6h1.2M11.4 8h1.2"/>"#
            ),
            Self::Bell => glyph!(
                r#"<path d="M4 6.75a4 4 0 0 1 8 0c0 3 .9 4 1.4 4.5H2.6C3.1 10.75 4 9.75 4 6.75Z"/><path d="M6.6 13.2a1.6 1.6 0 0 0 2.8 0"/>"#
            ),
            Self::Plus => glyph!(r#"<path d="M8 3.75v8.5M3.75 8h8.5"/>"#),
            // `plus` with one stroke taken away, so stage and unstage read as a pair.
            Self::Minus => glyph!(r#"<path d="M3.75 8h8.5"/>"#),
            // Six teeth around an open hub, so it reads as a gear rather than
            // as the spoked config-file shape.
            Self::Settings => glyph!(
                r#"<path d="M6.16 3.46 L6.78 1.72 L9.22 1.72 L9.84 3.46 A4.9 4.9 0 0 1 11.02 4.14 L12.83 3.80 L14.05 5.92 L12.85 7.32 A4.9 4.9 0 0 1 12.85 8.68 L14.05 10.08 L12.83 12.20 L11.02 11.86 A4.9 4.9 0 0 1 9.84 12.54 L9.22 14.28 L6.78 14.28 L6.16 12.54 A4.9 4.9 0 0 1 4.98 11.86 L3.17 12.20 L1.95 10.08 L3.15 8.68 A4.9 4.9 0 0 1 3.15 7.32 L1.95 5.92 L3.17 3.80 L4.98 4.14 A4.9 4.9 0 0 1 6.16 3.46Z"/><circle cx="8" cy="8" r="2"/>"#
            ),
            Self::Discard => glyph!(
                r#"<path d="M5.6 3.35 2.6 6.35l3 3"/><path d="M2.6 6.35h6.15a3.6 3.6 0 1 1 0 7.2H6.2"/>"#
            ),
            Self::Refresh => {
                glyph!(r#"<path d="M12.6 5.9A5 5 0 1 0 13 8"/><path d="M9.7 5.9h2.9V3"/>"#)
            }
            Self::Close => glyph!(r#"<path d="m4.25 4.25 7.5 7.5M11.75 4.25l-7.5 7.5"/>"#),
            // Lucide (ISC) `copy-x` on its 24 grid, scaled to this one with the
            // stroke widened to keep the set's weight.
            Self::CloseOthers => glyph!(
                r#"<g stroke-width="2.1" transform="scale(.6667)"><line x1="12" x2="18" y1="12" y2="18"/><line x1="12" x2="18" y1="18" y2="12"/><rect height="14" rx="2" ry="2" width="14" x="8" y="8"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></g>"#
            ),
            // Lucide (ISC) `bot` with an × badge knocked out of its corner. Tauri
            // painted the knockout in the surface color; a mask cuts it instead,
            // since this SVG is drawn in one color.
            Self::CloseNonAgent => glyph!(
                r##"<mask id="knockout"><rect width="16" height="16" fill="#fff" stroke="none"/><circle cx="12.4" cy="12.4" r="3.6" fill="#000" stroke="none"/></mask><g mask="url(#knockout)"><g stroke-width="2.3" transform="translate(-1 -1) scale(.6)"><path d="M12 8V4H8"/><rect height="12" rx="2" width="16" x="4" y="8"/><path d="M2 14h2"/><path d="M20 14h2"/><path d="M15 13v2"/><path d="M9 13v2"/></g></g><path d="m10.7 10.7 3.4 3.4M14.1 10.7l-3.4 3.4" stroke-width="1.5"/>"##
            ),
            Self::More => glyph!(
                r#"<circle cx="3.6" cy="8" r=".9"/><circle cx="8" cy="8" r=".9"/><circle cx="12.4" cy="8" r=".9"/>"#
            ),
            Self::ArrowLeft => glyph!(r#"<path d="M12.25 8h-8.5M7.5 4.25 3.75 8l3.75 3.75"/>"#),
            Self::ArrowRight => glyph!(r#"<path d="M3.75 8h8.5M8.5 4.25 12.25 8 8.5 11.75"/>"#),
            Self::Diff => glyph!(r#"<path d="M4.5 3v6.5M2.5 5h4"/><path d="M9.5 11h4"/>"#),
            Self::Zoom => glyph!(
                r#"<path d="M2.75 6V2.75H6M10 2.75h3.25V6M13.25 10v3.25H10M6 13.25H2.75V10"/>"#
            ),
            Self::Search => glyph!(r#"<circle cx="7" cy="7" r="4.25"/><path d="m10.2 10.2 3 3"/>"#),
            Self::Folder => crate::files::icons::Shape::Folder.svg(),
            Self::WindowMaximize => {
                glyph!(r#"<rect height="8.5" rx="1" width="8.5" x="3.75" y="3.75"/>"#)
            }
            Self::WindowRestore => glyph!(
                r#"<rect height="7" rx="1" width="7" x="3.25" y="5.75"/><path d="M5.75 5.75V4.25a1 1 0 0 1 1-1h5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1h-1.5"/>"#
            ),
        }
    }
}

impl From<Glyph> for Icon {
    fn from(glyph: Glyph) -> Self {
        Icon::empty().data(glyph.svg())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui_kit::SvgRenderer;

    use super::*;

    const ALL: [Glyph; 20] = [
        Glyph::SidebarLeft,
        Glyph::PanelRight,
        Glyph::Bell,
        Glyph::Plus,
        Glyph::Minus,
        Glyph::Settings,
        Glyph::Discard,
        Glyph::Refresh,
        Glyph::Close,
        Glyph::CloseOthers,
        Glyph::CloseNonAgent,
        Glyph::More,
        Glyph::ArrowLeft,
        Glyph::ArrowRight,
        Glyph::Diff,
        Glyph::Zoom,
        Glyph::Search,
        Glyph::Folder,
        Glyph::WindowMaximize,
        Glyph::WindowRestore,
    ];

    /// Every glyph parses and paints something: a typo in a path would
    /// otherwise draw an empty button. The bot's knockout really is cut out.
    #[test]
    fn every_glyph_draws() {
        let renderer = SvgRenderer::new(Arc::new(()));
        // Alpha at a point on the 16-unit grid.
        let alpha_at = |glyph: Glyph, x: f32, y: f32| {
            let image = renderer
                .render_single_frame(glyph.svg(), 4.)
                .unwrap_or_else(|error| panic!("{glyph:?}: {error}"));
            let size = image.size(0);
            let scale = size.width.0 as f32 / 16.;
            let (x, y) = ((x * scale) as usize, (y * scale) as usize);
            image.as_bytes(0).unwrap()[(y * size.width.0 as usize + x) * 4 + 3]
        };
        for glyph in ALL {
            let image = renderer.render_single_frame(glyph.svg(), 4.).unwrap();
            let painted = image
                .as_bytes(0)
                .unwrap()
                .chunks(4)
                .any(|pixel| pixel[3] > 0);
            assert!(painted, "{glyph:?} is empty");
        }
        // The bot's right edge, above the badge and inside it.
        assert!(alpha_at(Glyph::CloseNonAgent, 11., 6.) > 128);
        assert_eq!(alpha_at(Glyph::CloseNonAgent, 11., 9.5), 0);
    }
}
