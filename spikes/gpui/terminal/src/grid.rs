//! Pure grid logic: turning `Term` cells into paint spans, and the per-row
//! shaped-text cache keyed by damage and content. No GPUI types, so it is
//! tested without a display; `element.rs` plugs in the real shaper.

use alacritty_terminal::Term;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

/// RGB as a `u32` (`0xRRGGBB`), the only color type the renderer needs.
pub type Rgb24 = u32;

#[derive(Clone, Copy)]
pub struct Palette {
    pub ansi: [Rgb24; 16],
    pub fg: Rgb24,
    pub bg: Rgb24,
    pub cursor: Rgb24,
    pub cursor_text: Rgb24,
}

/// The app's dark palette, from `packages/client-core/palette.css`.
pub const DARK: Palette = Palette {
    ansi: [
        0x1d1f21, 0xcc6566, 0xb6bd68, 0xf0c674, 0x82a2be, 0xb294bb, 0x8abeb7, 0xc4c8c6, 0x666666,
        0xd54e53, 0xb9ca4b, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea,
    ],
    fg: 0xffffff,
    bg: 0x282c34,
    cursor: 0xffffff,
    cursor_text: 0x353a44,
};

fn rgb_u32(rgb: Rgb) -> Rgb24 {
    ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | (rgb.b as u32)
}

fn dim(color: Rgb24) -> Rgb24 {
    let f = |v: u32| (v * 2 / 3) & 0xff;
    (f((color >> 16) & 0xff) << 16) | (f((color >> 8) & 0xff) << 8) | f(color & 0xff)
}

fn indexed(index: u8, palette: &Palette) -> Rgb24 {
    match index {
        0..=15 => palette.ansi[index as usize],
        16..=231 => {
            let idx = index - 16;
            let (r, g, b) = (idx / 36, (idx / 6) % 6, idx % 6);
            let level = |v: u8| if v == 0 { 0u32 } else { 55 + v as u32 * 40 };
            (level(r) << 16) | (level(g) << 8) | level(b)
        }
        232..=255 => {
            let v = 8 + (index as u32 - 232) * 10;
            (v << 16) | (v << 8) | v
        }
    }
}

fn named(color: NamedColor, palette: &Palette) -> Rgb24 {
    use NamedColor::*;
    match color {
        Foreground | BrightForeground | DimForeground => palette.fg,
        Background => palette.bg,
        Cursor => palette.cursor,
        Black => palette.ansi[0],
        Red => palette.ansi[1],
        Green => palette.ansi[2],
        Yellow => palette.ansi[3],
        Blue => palette.ansi[4],
        Magenta => palette.ansi[5],
        Cyan => palette.ansi[6],
        White => palette.ansi[7],
        BrightBlack => palette.ansi[8],
        BrightRed => palette.ansi[9],
        BrightGreen => palette.ansi[10],
        BrightYellow => palette.ansi[11],
        BrightBlue => palette.ansi[12],
        BrightMagenta => palette.ansi[13],
        BrightCyan => palette.ansi[14],
        BrightWhite => palette.ansi[15],
        DimBlack => dim(palette.ansi[0]),
        DimRed => dim(palette.ansi[1]),
        DimGreen => dim(palette.ansi[2]),
        DimYellow => dim(palette.ansi[3]),
        DimBlue => dim(palette.ansi[4]),
        DimMagenta => dim(palette.ansi[5]),
        DimCyan => dim(palette.ansi[6]),
        DimWhite => dim(palette.ansi[7]),
    }
}

fn resolve(color: Color, colors: &Colors, palette: &Palette) -> Rgb24 {
    match color {
        Color::Spec(rgb) => rgb_u32(rgb),
        Color::Indexed(index) => colors[index as usize].map(rgb_u32).unwrap_or_else(|| indexed(index, palette)),
        Color::Named(name) => colors[name].map(rgb_u32).unwrap_or_else(|| named(name, palette)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Decoration {
    None,
    Single,
    Double,
    Curl,
    Dotted,
    Dashed,
}

/// Style for one run of text. Two adjacent cells with an equal key shape together.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextStyle {
    pub bold: bool,
    pub italic: bool,
    pub fg: Rgb24,
    pub underline: Decoration,
    pub underline_color: Option<Rgb24>,
    pub strikeout: bool,
}

/// A horizontal run of glyphs starting at `col`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Segment {
    pub col: usize,
    pub text: String,
    pub style: TextStyle,
    /// Cells each glyph advances by: 1, or 2 for a wide character (which gets
    /// its own segment so `force_width` stays uniform within a run).
    pub cell_span: usize,
}

/// A background span in columns `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BgSpan {
    pub start: usize,
    pub end: usize,
    pub color: Rgb24,
}

/// Everything needed to paint a row, and the identity the shaped-text cache
/// compares so unchanged content is never reshaped.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct RowSpec {
    pub bg: Vec<BgSpan>,
    pub segments: Vec<Segment>,
}

/// Build the paint description for one visible row. `palette` resolves colors;
/// default background is not painted (the pane background covers it).
pub fn build_row<L: EventListener>(term: &Term<L>, line: i32, palette: &Palette) -> RowSpec {
    let cols = term.columns();
    let colors = term.colors();
    let row = &term.grid()[Line(line)];
    let mut spec = RowSpec::default();
    let mut current: Option<Segment> = None;
    let flush = |spec: &mut RowSpec, current: &mut Option<Segment>| {
        if let Some(mut segment) = current.take() {
            let trimmed = segment.text.trim_end_matches(' ').len();
            segment.text.truncate(trimmed);
            if !segment.text.is_empty() {
                spec.segments.push(segment);
            }
        }
    };
    for col in 0..cols {
        let cell = &row[Column(col)];
        let mut fg = resolve(cell.fg, colors, palette);
        let mut bg = resolve(cell.bg, colors, palette);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.flags.contains(Flags::DIM) {
            fg = dim(fg);
        }
        let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
        if bg != palette.bg {
            let end = (col + width).min(cols);
            match spec.bg.last_mut() {
                Some(last) if last.end == col && last.color == bg => last.end = end,
                _ => spec.bg.push(BgSpan { start: col, end, color: bg }),
            }
        }
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let zerowidth = cell.zerowidth();
        let is_blank = (cell.c == ' ' || cell.c == '\0') && zerowidth.is_none();
        if is_blank || cell.flags.contains(Flags::HIDDEN) {
            if let Some(segment) = current.as_mut() {
                segment.text.push(' ');
            }
            continue;
        }
        let underline = decoration(cell.flags);
        let style = TextStyle {
            bold: cell.flags.contains(Flags::BOLD),
            italic: cell.flags.contains(Flags::ITALIC),
            fg,
            underline,
            underline_color: cell
                .underline_color()
                .map(|color| resolve(color, colors, palette)),
            strikeout: cell.flags.contains(Flags::STRIKEOUT),
        };
        let continues = current
            .as_ref()
            .is_some_and(|s| s.style == style && s.cell_span == 1 && width == 1);
        if !continues {
            flush(&mut spec, &mut current);
            current = Some(Segment {
                col,
                text: String::new(),
                style,
                cell_span: width,
            });
        }
        let segment = current.as_mut().expect("current segment set above");
        segment.text.push(cell.c);
        if let Some(chars) = zerowidth {
            segment.text.extend(chars);
        }
        if width == 2 {
            // A wide cell is always a segment of its own.
            flush(&mut spec, &mut current);
        }
    }
    flush(&mut spec, &mut current);
    spec
}

fn decoration(flags: Flags) -> Decoration {
    if flags.contains(Flags::UNDERCURL) {
        Decoration::Curl
    } else if flags.contains(Flags::DOUBLE_UNDERLINE) {
        Decoration::Double
    } else if flags.contains(Flags::DOTTED_UNDERLINE) {
        Decoration::Dotted
    } else if flags.contains(Flags::DASHED_UNDERLINE) {
        Decoration::Dashed
    } else if flags.contains(Flags::UNDERLINE) {
        Decoration::Single
    } else {
        Decoration::None
    }
}

/// A row in the shaped-text cache.
pub struct CachedRow<S> {
    pub spec: RowSpec,
    pub shaped: Vec<S>,
}

/// Shaped-text cache, one entry per visible row.
///
/// Damage says which rows might have changed; comparing the rebuilt spec says
/// whether they actually did. The second check is what makes a full-damage
/// event (scroll, attach, alternate screen) cheap when most rows are unchanged.
pub struct RowCache<S> {
    pub rows: Vec<Option<CachedRow<S>>>,
    pub cols: usize,
}

/// What an update did, for the frame counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub reshaped: usize,
    pub reused: usize,
}

impl<S> Default for RowCache<S> {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            cols: 0,
        }
    }
}

impl<S> RowCache<S> {
    /// Size the cache; a change drops every entry (cell positions moved).
    pub fn ensure_size(&mut self, cols: usize, rows: usize) {
        if self.cols != cols || self.rows.len() != rows {
            self.cols = cols;
            self.rows.clear();
            self.rows.resize_with(rows, || None);
        }
    }

    /// Update one row with its freshly built spec, shaping only if it changed.
    pub fn update(
        &mut self,
        row: usize,
        spec: RowSpec,
        stats: &mut CacheStats,
        shape: impl FnOnce(&RowSpec) -> Vec<S>,
    ) {
        let Some(slot) = self.rows.get_mut(row) else {
            return;
        };
        if let Some(entry) = slot
            && entry.spec == spec
        {
            stats.reused += 1;
            return;
        }
        let shaped = shape(&spec);
        *slot = Some(CachedRow { spec, shaped });
        stats.reshaped += 1;
    }
}

/// Which rows a frame must rebuild, given the terminal's damage report.
pub fn damaged_rows<L: EventListener>(term: &mut Term<L>) -> DamageResult {
    use alacritty_terminal::term::TermDamage;
    let screen = term.screen_lines();
    let result = match term.damage() {
        TermDamage::Full => DamageResult {
            full: true,
            rows: (0..screen).collect(),
        },
        TermDamage::Partial(iter) => {
            let mut rows: Vec<usize> = iter.map(|bounds| bounds.line).filter(|l| *l < screen).collect();
            rows.sort_unstable();
            rows.dedup();
            DamageResult { full: false, rows }
        }
    };
    term.reset_damage();
    result
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DamageResult {
    pub full: bool,
    pub rows: Vec<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::{Listener, PaneSize};
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;
    use std::cell::Cell;

    fn term(cols: usize, rows: usize) -> Term<Listener> {
        Term::new(Config::default(), &PaneSize { cols, rows }, Listener)
    }

    fn feed(term: &mut Term<Listener>, bytes: &[u8]) {
        let mut parser: Processor = Processor::new();
        parser.advance(term, bytes);
    }

    #[test]
    fn plain_text_is_one_segment_trimmed() {
        let mut t = term(20, 3);
        feed(&mut t, b"hello world   ");
        let spec = build_row(&t, 0, &DARK);
        assert_eq!(spec.segments.len(), 1);
        assert_eq!(spec.segments[0].text, "hello world");
        assert_eq!(spec.segments[0].col, 0);
        assert!(spec.bg.is_empty());
    }

    #[test]
    fn sgr_attributes_split_segments_and_keep_background() {
        let mut t = term(40, 3);
        feed(&mut t, b"\x1b[1mbold\x1b[0m \x1b[2mdim\x1b[0m \x1b[3mit\x1b[0m \x1b[4mun\x1b[0m \x1b[41mred\x1b[0m");
        let spec = build_row(&t, 0, &DARK);
        let texts: Vec<_> = spec.segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["bold", " dim".trim(), "it", "un", "red"].iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(spec.segments[0].style.bold);
        assert_ne!(spec.segments[1].style.fg, DARK.fg, "dim scales fg");
        assert!(spec.segments[2].style.italic);
        assert_eq!(spec.segments[3].style.underline, Decoration::Single);
        assert_eq!(spec.bg.len(), 1);
        assert_eq!(spec.bg[0].color, DARK.ansi[1]);
    }

    #[test]
    fn wide_chars_get_their_own_segment_and_skip_spacer() {
        let mut t = term(20, 3);
        feed(&mut t, "ab你c".as_bytes());
        let spec = build_row(&t, 0, &DARK);
        let segs: Vec<(usize, &str, usize)> = spec
            .segments
            .iter()
            .map(|s| (s.col, s.text.as_str(), s.cell_span))
            .collect();
        assert_eq!(segs, vec![(0, "ab", 1), (2, "你", 2), (4, "c", 1)]);
    }

    #[test]
    fn combining_mark_stays_with_its_base() {
        let mut t = term(20, 3);
        feed(&mut t, "e\u{301}x".as_bytes());
        let spec = build_row(&t, 0, &DARK);
        assert_eq!(spec.segments[0].text, "e\u{301}x");
    }

    #[test]
    fn indexed_cube_and_truecolor_resolve() {
        let mut t = term(20, 3);
        feed(&mut t, b"\x1b[38;5;196mA\x1b[38;2;1;2;3mB");
        let spec = build_row(&t, 0, &DARK);
        assert_eq!(spec.segments[0].style.fg, 0xff0000);
        assert_eq!(spec.segments[1].style.fg, 0x010203);
    }

    #[test]
    fn inverse_swaps_and_paints_background() {
        let mut t = term(20, 3);
        feed(&mut t, b"\x1b[7mX");
        let spec = build_row(&t, 0, &DARK);
        assert_eq!(spec.segments[0].style.fg, DARK.bg);
        assert_eq!(spec.bg[0], BgSpan { start: 0, end: 1, color: DARK.fg });
    }

    #[test]
    fn row_cache_reuses_unchanged_specs_and_reshapes_changes() {
        let mut t = term(20, 3);
        feed(&mut t, b"same");
        let mut cache: RowCache<usize> = RowCache::default();
        cache.ensure_size(20, 3);
        let shapes = Cell::new(0);
        let mut stats = CacheStats::default();
        let shape = |_: &RowSpec| {
            shapes.set(shapes.get() + 1);
            vec![1]
        };
        cache.update(0, build_row(&t, 0, &DARK), &mut stats, shape);
        cache.update(0, build_row(&t, 0, &DARK), &mut stats, shape);
        assert_eq!(shapes.get(), 1, "identical spec must not reshape");
        assert_eq!(stats, CacheStats { reshaped: 1, reused: 1 });
        feed(&mut t, b"!");
        cache.update(0, build_row(&t, 0, &DARK), &mut stats, shape);
        assert_eq!(shapes.get(), 2);
    }

    #[test]
    fn resizing_the_cache_drops_entries() {
        let mut cache: RowCache<usize> = RowCache::default();
        cache.ensure_size(20, 3);
        let mut stats = CacheStats::default();
        cache.update(0, RowSpec::default(), &mut stats, |_| vec![]);
        assert!(cache.rows[0].is_some());
        cache.ensure_size(30, 3);
        assert!(cache.rows[0].is_none());
    }

    #[test]
    fn damage_reports_only_touched_rows_then_resets() {
        let mut t = term(20, 5);
        t.reset_damage(); // A new Term starts fully damaged.
        feed(&mut t, b"\x1b[2;1Hx");
        let first = damaged_rows(&mut t);
        assert!(!first.full);
        assert!(first.rows.contains(&1));
        feed(&mut t, b"\x1b[4;1Hy");
        let second = damaged_rows(&mut t);
        assert!(second.rows.contains(&3));
        assert!(second.rows.contains(&1), "the previous cursor row is repainted");
        assert!(!second.rows.contains(&0) && !second.rows.contains(&2));
        // Reset: nothing new without further output, apart from the cursor line.
        let third = damaged_rows(&mut t);
        assert_eq!(third.rows.len(), 1, "only the cursor line stays damaged");
    }

    #[test]
    fn scroll_reports_full_damage() {
        let mut t = term(20, 3);
        feed(&mut t, b"a\r\nb");
        // A new Term starts fully damaged: clear it so only the scroll sets it.
        t.reset_damage();
        feed(&mut t, b"\r\nc\r\nd\r\ne\r\n");
        let result = damaged_rows(&mut t);
        assert!(result.full);
        assert_eq!(result.rows, vec![0, 1, 2]);
    }
}
