//! One file-type mapping, read by the Explorer tree and the tab strip. Port of
//! `fileIcons.ts`, with the shapes from `ui/Icon.tsx`.
//!
//! VS Code's Seti icons are recognisable mostly by color: a handful of shape
//! archetypes tinted per language. This follows that with the terminal's own
//! ANSI palette, so the Explorer cannot drift from the theme.

use gpui_kit::Rgba;

use crate::theme::Palette;

use super::wire::{FileKind, TargetKind};

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

pub const CHEVRON_RIGHT: &[u8] = glyph!(r#"<path d="M6.25 3.75 10.5 8l-4.25 4.25"/>"#);
pub const CHEVRON_DOWN: &[u8] = glyph!(r#"<path d="M3.75 6.25 8 10.5l4.25-4.25"/>"#);
pub const EYE: &[u8] = glyph!(
    r#"<path d="M1.3 8s2.4-4 6.7-4 6.7 4 6.7 4-2.4 4-6.7 4-6.7-4-6.7-4Z"/><circle cx="8" cy="8" r="2"/>"#
);
pub const EYE_OFF: &[u8] = glyph!(
    r#"<path d="M6.1 4.3A7.2 7.2 0 0 1 8 4c4.3 0 6.7 4 6.7 4a12 12 0 0 1-1.9 2.3M10.2 11.6a7 7 0 0 1-2.2.4c-4.3 0-6.7-4-6.7-4a12 12 0 0 1 2.4-2.7M2 2l12 12"/>"#
);
const FILE: &[u8] = glyph!(r#"<path d="M4 2.5h4.5L12 6v7.5H4Z"/><path d="M8.25 2.6V6H11.9"/>"#);
const MARKDOWN: &[u8] = glyph!(
    r#"<rect height="9.5" rx="1.5" width="13" x="1.5" y="3.25"/><path d="M4 10.25v-4l1.75 2 1.75-2v4"/><path d="M10.5 6.25v4m0 0-1.25-1.4m1.25 1.4 1.25-1.4"/>"#
);
const FOLDER: &[u8] = glyph!(
    r#"<path d="M1.75 12.25v-8A1.25 1.25 0 0 1 3 3h2.6l1.4 1.75h6A1.25 1.25 0 0 1 14.25 6v6.25a1.25 1.25 0 0 1-1.25 1.25H3a1.25 1.25 0 0 1-1.25-1.25Z"/>"#
);
// The same folder with its front panel swung forward: only the lid moves.
const FOLDER_OPEN: &[u8] = glyph!(
    r#"<path d="M1.75 12.25v-8A1.25 1.25 0 0 1 3 3h2.6l1.4 1.75h6A1.25 1.25 0 0 1 14.25 6v1.5"/><path d="M1.9 12.9 3.7 7.75h10.5l-1.8 5.15a1.25 1.25 0 0 1-1.18.85H3a1.25 1.25 0 0 1-1.1-.85Z"/>"#
);
const FILE_CODE: &[u8] =
    glyph!(r#"<path d="m6 4.75-3.25 3.3L6 11.25M10 4.75l3.25 3.3L10 11.25"/>"#);
const FILE_TEXT: &[u8] = glyph!(
    r#"<path d="M4 2.5h4.5L12 6v7.5H4Z"/><path d="M8.25 2.6V6H11.9"/><path d="M5.9 8.6h4.2M5.9 10.8h3"/>"#
);
// Six teeth on a visible hub: at 14px an eight-spoke gear reads as an asterisk.
const FILE_CONFIG: &[u8] = glyph!(
    r#"<circle cx="8" cy="8" r="2.4"/><path d="M11.5 8h1.9M4.5 8H2.6M9.75 11.03l.95 1.65M6.25 11.03l-.95 1.65M6.25 4.97l-.95-1.65M9.75 4.97l.95-1.65"/>"#
);
const FILE_IMAGE: &[u8] = glyph!(
    r#"<rect height="9.5" rx="1.5" width="11" x="2.5" y="3.25"/><circle cx="6" cy="6.6" r="1"/><path d="m2.9 11.9 3.35-3.3 2.2 2.15 2.3-2.25 2.7 2.65"/>"#
);
const FILE_LOCK: &[u8] = glyph!(
    r#"<rect height="6.25" rx="1.4" width="9" x="3.5" y="7"/><path d="M5.75 7V5.4a2.25 2.25 0 0 1 4.5 0V7"/>"#
);
const FILE_SHELL: &[u8] = glyph!(r#"<path d="m3.25 4.9 3.1 3.1-3.1 3.1M8.4 11.6h4.35"/>"#);
const FILE_DATA: &[u8] = glyph!(
    r#"<path d="M6.9 2.9c-1.3 0-1.9.6-1.9 1.9v1.3c0 1.1-.55 1.9-1.5 1.9.95 0 1.5.8 1.5 1.9v1.3c0 1.3.6 1.9 1.9 1.9"/><path d="M9.1 2.9c1.3 0 1.9.6 1.9 1.9v1.3c0 1.1.55 1.9 1.5 1.9-.95 0-1.5.8-1.5 1.9v1.3c0 1.3-.6 1.9-1.9 1.9"/>"#
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    File,
    Markdown,
    Folder,
    FolderOpen,
    Code,
    Text,
    Config,
    Image,
    Lock,
    Shell,
    Data,
}

impl Shape {
    pub fn svg(self) -> &'static [u8] {
        match self {
            Self::File => FILE,
            Self::Markdown => MARKDOWN,
            Self::Folder => FOLDER,
            Self::FolderOpen => FOLDER_OPEN,
            Self::Code => FILE_CODE,
            Self::Text => FILE_TEXT,
            Self::Config => FILE_CONFIG,
            Self::Image => FILE_IMAGE,
            Self::Lock => FILE_LOCK,
            Self::Shell => FILE_SHELL,
            Self::Data => FILE_DATA,
        }
    }
}

/// A palette slot rather than a color, so a theme change repaints it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tint {
    Term(u8),
    Dim,
    Faint,
}

impl Tint {
    pub fn color(self, palette: &Palette) -> Rgba {
        match self {
            Self::Term(1) => palette.term_1,
            Self::Term(2) => palette.term_2,
            Self::Term(3) => palette.term_3,
            Self::Term(4) => palette.term_4,
            Self::Term(5) => palette.term_5,
            Self::Term(6) => palette.term_6,
            Self::Term(_) => palette.term_7,
            Self::Dim => palette.chrome_dim,
            Self::Faint => palette.chrome_faint,
        }
    }
}

const DIRECTORY_TINT: Tint = Tint::Term(4);
/// A symlink keeps its target's shape and takes the tint the `l` glyph had.
const SYMLINK_TINT: Tint = Tint::Term(6);
const FALLBACK: (Shape, Tint) = (Shape::File, Tint::Dim);
const CONFIG: (Shape, Tint) = (Shape::Config, Tint::Dim);

fn by_extension(extension: &str) -> Option<(Shape, Tint)> {
    use Shape::*;
    Some(match extension {
        "ts" | "tsx" | "mts" | "cts" => (Code, Tint::Term(4)),
        "js" | "jsx" | "mjs" | "cjs" => (Code, Tint::Term(3)),
        "rs" => (Code, Tint::Term(1)),
        "py" => (Code, Tint::Term(2)),
        "json" | "jsonc" | "toml" | "yml" | "yaml" => (Data, Tint::Term(3)),
        "md" | "mdown" | "markdown" => (Markdown, Tint::Term(4)),
        "css" | "scss" | "sass" | "less" => (Code, Tint::Term(5)),
        "html" | "htm" => (Code, Tint::Term(1)),
        "sh" | "zsh" | "bash" | "fish" => (Shell, Tint::Term(2)),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "bmp" | "ico" | "svg" => {
            (Image, Tint::Term(5))
        }
        "lock" => (Lock, Tint::Faint),
        "txt" | "log" => (Text, Tint::Dim),
        _ => return None,
    })
}

fn shape_for_name(name: &str) -> (Shape, Tint) {
    let lower = name.to_lowercase();
    // A dotfile is configuration first and a language second, and `*.config.*`
    // is the same claim spelled the other way round.
    let config_suffix = lower
        .rsplit_once(".config.")
        .is_some_and(|(_, tail)| !tail.is_empty() && !tail.contains('.'));
    if lower.starts_with('.') || config_suffix {
        return CONFIG;
    }
    lower
        .rsplit_once('.')
        .and_then(|(_, extension)| by_extension(extension))
        .unwrap_or(FALLBACK)
}

pub fn file_icon(
    name: &str,
    kind: FileKind,
    target_kind: Option<TargetKind>,
    expanded: bool,
) -> (Shape, Tint) {
    let symlink = kind == FileKind::Symlink;
    if kind == FileKind::Directory || (symlink && target_kind == Some(TargetKind::Directory)) {
        let shape = if expanded {
            Shape::FolderOpen
        } else {
            Shape::Folder
        };
        return (
            shape,
            if symlink {
                SYMLINK_TINT
            } else {
                DIRECTORY_TINT
            },
        );
    }
    let (shape, tint) = shape_for_name(name);
    (shape, if symlink { SYMLINK_TINT } else { tint })
}
