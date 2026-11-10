//! Turning a pane's cells into what paints them, and the per-row cache that
//! keeps unchanged rows from being shaped again. No GPUI types, so it is tested
//! without a display.
//!
//! Damage says which rows might have changed; comparing the rebuilt spec says
//! whether they did. The second check is what makes a full damage, a scroll or
//! a seed, cheap when most rows are unchanged.

use alacritty_terminal::Term;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{RenderableCursor, TermDamage, point_to_viewport};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

/// `0xRRGGBB`, the one color type this module needs.
pub type Rgb24 = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub ansi: [Rgb24; 16],
    pub fg: Rgb24,
    pub bg: Rgb24,
    pub cursor: Rgb24,
    pub cursor_text: Rgb24,
}

fn rgb24(rgb: Rgb) -> Rgb24 {
    (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b)
}

fn cube(index: u8) -> Rgb24 {
    let level = |value: u8| {
        if value == 0 {
            0
        } else {
            55 + u32::from(value) * 40
        }
    };
    let index = index - 16;
    (level(index / 36) << 16) | (level((index / 6) % 6) << 8) | level(index % 6)
}

fn indexed(index: u8, palette: &Palette) -> Rgb24 {
    match index {
        0..=15 => palette.ansi[usize::from(index)],
        16..=231 => cube(index),
        232..=255 => {
            let value = 8 + (u32::from(index) - 232) * 10;
            (value << 16) | (value << 8) | value
        }
    }
}

fn named(color: NamedColor, palette: &Palette) -> Rgb24 {
    use NamedColor::*;
    let dim = match color {
        DimBlack | DimRed | DimGreen | DimYellow | DimBlue | DimMagenta | DimCyan | DimWhite
        | DimForeground => 2,
        _ => 3,
    };
    let scale = |value: Rgb24| {
        let part = |shift: u32| (((value >> shift) & 0xff) * dim / 3) << shift;
        part(16) | part(8) | part(0)
    };
    let base = match color {
        Foreground | BrightForeground | DimForeground => palette.fg,
        Background => palette.bg,
        Cursor => palette.cursor,
        Black | DimBlack => palette.ansi[0],
        Red | DimRed => palette.ansi[1],
        Green | DimGreen => palette.ansi[2],
        Yellow | DimYellow => palette.ansi[3],
        Blue | DimBlue => palette.ansi[4],
        Magenta | DimMagenta => palette.ansi[5],
        Cyan | DimCyan => palette.ansi[6],
        White | DimWhite => palette.ansi[7],
        BrightBlack => palette.ansi[8],
        BrightRed => palette.ansi[9],
        BrightGreen => palette.ansi[10],
        BrightYellow => palette.ansi[11],
        BrightBlue => palette.ansi[12],
        BrightMagenta => palette.ansi[13],
        BrightCyan => palette.ansi[14],
        BrightWhite => palette.ansi[15],
    };
    scale(base)
}

fn resolve(color: Color, colors: &Colors, palette: &Palette) -> Rgb24 {
    match color {
        Color::Spec(rgb) => rgb24(rgb),
        Color::Indexed(index) => colors[usize::from(index)]
            .map(rgb24)
            .unwrap_or_else(|| indexed(index, palette)),
        Color::Named(name) => colors[name]
            .map(rgb24)
            .unwrap_or_else(|| named(name, palette)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoration {
    None,
    Single,
    Curl,
}

/// A run's style. Adjacent cells with an equal style shape together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextStyle {
    pub bold: bool,
    pub italic: bool,
    pub dim: bool,
    pub fg: Rgb24,
    pub underline: Decoration,
    pub underline_color: Option<Rgb24>,
    pub strikeout: bool,
}

impl TextStyle {
    fn decorated(&self) -> bool {
        self.underline != Decoration::None || self.strikeout
    }
}

/// A run of glyphs starting at `column`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub column: usize,
    pub text: String,
    pub style: TextStyle,
    /// Cells one glyph takes: 1, or 2 for a wide character, which is always a
    /// segment of its own so a single forced advance fits the whole run.
    pub cell_span: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BgSpan {
    pub start: usize,
    pub end: usize,
    pub color: Rgb24,
}

/// A box-drawing or block cell, painted as geometry so it tiles exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoxCell {
    pub column: usize,
    pub glyph: char,
    pub color: Rgb24,
}

/// Cells to draw differently: selection, a search hit, a link under the
/// pointer. Viewport row, columns `[start, end)`. They enter the row spec, so a
/// highlight that changes repaints that row, and nothing else needs to know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellHighlight {
    pub row: usize,
    pub start: usize,
    pub end: usize,
    pub bg: Option<Rgb24>,
    pub fg: Option<Rgb24>,
    pub underline: bool,
}

/// Everything needed to paint a row, and what the cache compares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowSpec {
    pub bg: Vec<BgSpan>,
    pub segments: Vec<Segment>,
    pub boxes: Vec<BoxCell>,
}

fn decoration(flags: Flags) -> Decoration {
    if flags.contains(Flags::UNDERCURL) {
        Decoration::Curl
    } else if flags.intersects(
        Flags::UNDERLINE
            | Flags::DOUBLE_UNDERLINE
            | Flags::DOTTED_UNDERLINE
            | Flags::DASHED_UNDERLINE,
    ) {
        Decoration::Single
    } else {
        Decoration::None
    }
}

/// Whether `glyph` is painted as geometry rather than from the font.
pub fn is_box_glyph(glyph: char) -> bool {
    crate::terminal::box_glyphs::covers(glyph)
}

/// Builds one visible row. `viewport_row` counts from the top of what is on
/// screen, so a pane scrolled into its history paints those rows.
pub fn build_row<L: EventListener>(
    term: &Term<L>,
    viewport_row: usize,
    palette: &Palette,
    highlights: &[CellHighlight],
) -> RowSpec {
    let columns = term.columns();
    let colors = term.colors();
    let display_offset = term.grid().display_offset() as i32;
    let row = &term.grid()[Line(viewport_row as i32 - display_offset)];
    let mut spec = RowSpec::default();
    let mut current: Option<Segment> = None;
    let flush = |spec: &mut RowSpec, current: &mut Option<Segment>| {
        if let Some(mut segment) = current.take() {
            // A decorated blank is drawn: its spaces carry the line.
            if !segment.style.decorated() {
                let trimmed = segment.text.trim_end_matches(' ').len();
                segment.text.truncate(trimmed);
            }
            if !segment.text.is_empty() {
                spec.segments.push(segment);
            }
        }
    };
    for column in 0..columns {
        let cell = &row[Column(column)];
        let mut fg = resolve(cell.fg, colors, palette);
        let mut bg = resolve(cell.bg, colors, palette);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        let highlight = highlights.iter().rfind(|highlight| {
            highlight.row == viewport_row && (highlight.start..highlight.end).contains(&column)
        });
        if let Some(highlight) = highlight {
            bg = highlight.bg.unwrap_or(bg);
            fg = highlight.fg.unwrap_or(fg);
        }
        let width = if cell.flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        };
        if bg != palette.bg {
            let end = (column + width).min(columns);
            match spec.bg.last_mut() {
                Some(last) if last.end == column && last.color == bg => last.end = end,
                _ => spec.bg.push(BgSpan {
                    start: column,
                    end,
                    color: bg,
                }),
            }
        }
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        let zerowidth = cell.zerowidth().filter(|chars| !chars.is_empty());
        let blank = (cell.c == ' ' || cell.c == '\0') && zerowidth.is_none();
        let underline = if highlight.is_some_and(|highlight| highlight.underline) {
            Decoration::Single
        } else {
            decoration(cell.flags)
        };
        let strikeout = cell.flags.contains(Flags::STRIKEOUT);
        let hidden = cell.flags.contains(Flags::HIDDEN);
        if hidden || (blank && underline == Decoration::None && !strikeout) {
            // A run's decoration is drawn under all of it, so a plain gap
            // cannot stay inside a decorated run.
            if current.as_ref().is_some_and(|run| run.style.decorated()) {
                flush(&mut spec, &mut current);
            } else if let Some(segment) = current.as_mut() {
                segment.text.push(' ');
            }
            continue;
        }
        if zerowidth.is_none() && is_box_glyph(cell.c) {
            flush(&mut spec, &mut current);
            spec.boxes.push(BoxCell {
                column,
                glyph: cell.c,
                color: fg,
            });
            continue;
        }
        let style = TextStyle {
            bold: cell.flags.contains(Flags::BOLD),
            italic: cell.flags.contains(Flags::ITALIC),
            dim: cell.flags.contains(Flags::DIM),
            fg,
            underline,
            underline_color: cell
                .underline_color()
                .map(|color| resolve(color, colors, palette)),
            strikeout,
        };
        let continues = current
            .as_ref()
            .is_some_and(|segment| segment.style == style && segment.cell_span == 1 && width == 1);
        if !continues {
            flush(&mut spec, &mut current);
            current = Some(Segment {
                column,
                text: String::new(),
                style,
                cell_span: width,
            });
        }
        let segment = current.as_mut().expect("current segment set above");
        segment.text.push(if blank { ' ' } else { cell.c });
        if let Some(chars) = zerowidth {
            segment.text.extend(chars);
        }
        if width == 2 {
            flush(&mut spec, &mut current);
        }
    }
    flush(&mut spec, &mut current);
    spec
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Hollow,
    Underline,
    Beam,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorSpec {
    pub row: usize,
    pub column: usize,
    pub cell_span: usize,
    pub shape: CursorShape,
    /// The glyph under a block cursor, redrawn in the cursor's text color.
    pub glyph: Option<(String, TextStyle)>,
    pub blinking: bool,
}

/// Where the cursor is on screen, or `None` while it is hidden or scrolled off.
pub fn cursor_spec<L: EventListener>(
    term: &Term<L>,
    focused: bool,
    palette: &Palette,
) -> Option<CursorSpec> {
    use alacritty_terminal::vte::ansi::CursorShape as Shape;
    let RenderableCursor { point, shape } = term.renderable_content().cursor;
    if matches!(shape, Shape::Hidden) {
        return None;
    }
    let display_offset = term.grid().display_offset();
    let viewport = point_to_viewport(display_offset, point)?;
    if viewport.line >= term.screen_lines() {
        return None;
    }
    let cell = &term.grid()[point];
    let cell_span = if cell.flags.contains(Flags::WIDE_CHAR) {
        2
    } else {
        1
    };
    let shape = if focused {
        match shape {
            Shape::Underline => CursorShape::Underline,
            Shape::Beam => CursorShape::Beam,
            Shape::HollowBlock => CursorShape::Hollow,
            _ => CursorShape::Block,
        }
    } else {
        CursorShape::Hollow
    };
    let glyph = (matches!(shape, CursorShape::Block) && !cell.c.is_whitespace() && cell.c != '\0')
        .then(|| {
            let mut text = String::from(cell.c);
            text.extend(cell.zerowidth().into_iter().flatten());
            let style = TextStyle {
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
                dim: false,
                fg: palette.cursor_text,
                underline: Decoration::None,
                underline_color: None,
                strikeout: false,
            };
            (text, style)
        });
    Some(CursorSpec {
        row: viewport.line,
        column: viewport.column.0,
        cell_span,
        shape,
        glyph,
        blinking: term.cursor_style().blinking,
    })
}

/// Which rows changed since the last call, in viewport rows. Clears the damage,
/// so this pane's element must be the only caller.
pub fn damaged_rows<L: EventListener>(term: &mut Term<L>) -> Damage {
    let screen = term.screen_lines();
    let damage = match term.damage() {
        TermDamage::Full => Damage {
            full: true,
            rows: (0..screen).collect(),
        },
        TermDamage::Partial(lines) => {
            let mut rows: Vec<usize> = lines
                .map(|bounds| bounds.line)
                .filter(|line| *line < screen)
                .collect();
            rows.sort_unstable();
            rows.dedup();
            Damage { full: false, rows }
        }
    };
    term.reset_damage();
    damage
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Damage {
    pub full: bool,
    pub rows: Vec<usize>,
}

pub struct CachedRow<S> {
    pub spec: RowSpec,
    pub shaped: Vec<S>,
}

/// Shaped text, one entry per visible row.
pub struct RowCache<S> {
    pub rows: Vec<Option<CachedRow<S>>>,
    pub columns: usize,
}

impl<S> Default for RowCache<S> {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            columns: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub reshaped: usize,
    pub reused: usize,
}

impl<S> RowCache<S> {
    /// Returns whether a size change dropped every entry.
    pub fn ensure_size(&mut self, columns: usize, rows: usize) -> bool {
        if self.columns == columns && self.rows.len() == rows {
            return false;
        }
        self.columns = columns;
        self.rows.clear();
        self.rows.resize_with(rows, || None);
        true
    }

    /// Stores one row's spec, shaping it only if it changed.
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
        if slot.as_ref().is_some_and(|entry| entry.spec == spec) {
            stats.reused += 1;
            return;
        }
        let shaped = shape(&spec);
        *slot = Some(CachedRow { spec, shaped });
        stats.reshaped += 1;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;
    use terminal_engine::{GridSize, Listener};

    use super::*;

    const PALETTE: Palette = Palette {
        ansi: [
            0x101010, 0x200000, 0x002000, 0x202000, 0x000020, 0x200020, 0x002020, 0x202020,
            0x404040, 0x600000, 0x006000, 0x606000, 0x000060, 0x600060, 0x006060, 0x606060,
        ],
        fg: 0xffffff,
        bg: 0x282c34,
        cursor: 0xff0000,
        cursor_text: 0x000000,
    };

    fn term(columns: usize, rows: usize) -> Term<Listener> {
        Term::new(Config::default(), &GridSize { columns, rows }, Listener)
    }

    fn feed(term: &mut Term<Listener>, bytes: &[u8]) {
        let mut parser: Processor = Processor::new();
        parser.advance(term, bytes);
    }

    fn row_text(spec: &RowSpec) -> Vec<(usize, String)> {
        spec.segments
            .iter()
            .map(|segment| (segment.column, segment.text.clone()))
            .collect()
    }

    #[test]
    fn plain_text_is_one_trimmed_segment_with_no_background() {
        let mut term = term(20, 3);
        feed(&mut term, b"hello world   ");
        let spec = build_row(&term, 0, &PALETTE, &[]);
        assert_eq!(row_text(&spec), [(0, "hello world".to_owned())]);
        assert!(spec.bg.is_empty());
    }

    #[test]
    fn attributes_split_runs_and_keep_their_style() {
        let mut term = term(40, 3);
        feed(
            &mut term,
            b"\x1b[1mbold\x1b[0m \x1b[2mdim\x1b[0m \x1b[3mit\x1b[0m \x1b[4mun\x1b[0m \x1b[4:3mcu\x1b[0m \x1b[41mred\x1b[0m",
        );
        let spec = build_row(&term, 0, &PALETTE, &[]);
        let text: Vec<&str> = spec.segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, ["bold", "dim", "it", "un", "cu", "red"]);
        assert!(spec.segments[0].style.bold);
        assert!(spec.segments[1].style.dim);
        assert!(spec.segments[2].style.italic);
        assert_eq!(spec.segments[3].style.underline, Decoration::Single);
        assert_eq!(spec.segments[4].style.underline, Decoration::Curl);
        assert_eq!(
            spec.bg,
            [BgSpan {
                start: 18,
                end: 21,
                color: PALETTE.ansi[1],
            }]
        );
    }

    #[test]
    fn bold_keeps_its_color_as_the_desktop_sets_it() {
        let mut term = term(40, 3);
        feed(&mut term, b"\x1b[31mA\x1b[1;31mB");
        let spec = build_row(&term, 0, &PALETTE, &[]);
        let fg: Vec<Rgb24> = spec.segments.iter().map(|s| s.style.fg).collect();
        assert_eq!(fg, [PALETTE.ansi[1], PALETTE.ansi[1]]);
    }

    #[test]
    fn a_plain_gap_ends_an_underlined_run() {
        let mut term = term(40, 3);
        feed(&mut term, b"\x1b[4mfoo\x1b[0m   \x1b[4mbar");
        let spec = build_row(&term, 0, &PALETTE, &[]);
        let runs: Vec<(usize, &str)> = spec
            .segments
            .iter()
            .map(|s| (s.column, s.text.as_str()))
            .collect();
        assert_eq!(runs, [(0, "foo"), (6, "bar")]);
    }

    #[test]
    fn underlined_blanks_are_drawn() {
        let mut term = term(40, 3);
        feed(&mut term, b"\x1b[4m   \x1b[0m");
        let spec = build_row(&term, 0, &PALETTE, &[]);
        assert_eq!(spec.segments[0].text, "   ");
    }

    #[test]
    fn inverse_swaps_colors_and_paints_the_background() {
        let mut term = term(20, 3);
        feed(&mut term, b"\x1b[7mX");
        let spec = build_row(&term, 0, &PALETTE, &[]);
        assert_eq!(spec.segments[0].style.fg, PALETTE.bg);
        assert_eq!(
            spec.bg,
            [BgSpan {
                start: 0,
                end: 1,
                color: PALETTE.fg,
            }]
        );
    }

    #[test]
    fn wide_and_combining_characters_keep_their_cells() {
        let mut term = term(20, 3);
        feed(&mut term, "ab\u{4f60}c e\u{301}x".as_bytes());
        let spec = build_row(&term, 0, &PALETTE, &[]);
        let runs: Vec<(usize, &str, usize)> = spec
            .segments
            .iter()
            .map(|s| (s.column, s.text.as_str(), s.cell_span))
            .collect();
        assert_eq!(
            runs,
            [(0, "ab", 1), (2, "\u{4f60}", 2), (4, "c e\u{301}x", 1),]
        );
    }

    #[test]
    fn box_drawing_cells_leave_the_text_run() {
        let mut term = term(20, 3);
        feed(&mut term, "a\u{2502}b".as_bytes());
        let spec = build_row(&term, 0, &PALETTE, &[]);
        assert_eq!(row_text(&spec), [(0, "a".to_owned()), (2, "b".to_owned())]);
        assert_eq!(
            spec.boxes,
            [BoxCell {
                column: 1,
                glyph: '\u{2502}',
                color: PALETTE.fg,
            }]
        );
    }

    #[test]
    fn a_highlight_recolors_its_cells_and_underlines_them() {
        let mut term = term(20, 3);
        feed(&mut term, b"selected");
        let highlight = CellHighlight {
            row: 0,
            start: 0,
            end: 4,
            bg: Some(0x123456),
            fg: Some(0x654321),
            underline: true,
        };
        let spec = build_row(&term, 0, &PALETTE, &[highlight]);
        assert_eq!(
            row_text(&spec),
            [(0, "sele".to_owned()), (4, "cted".to_owned())]
        );
        assert_eq!(spec.segments[0].style.fg, 0x654321);
        assert_eq!(spec.segments[0].style.underline, Decoration::Single);
        assert_eq!(spec.bg[0].color, 0x123456);
    }

    #[test]
    fn a_scrolled_pane_builds_rows_from_its_history() {
        use alacritty_terminal::grid::Scroll;
        let mut term = term(20, 2);
        feed(&mut term, b"one\r\ntwo\r\nthree");
        assert_eq!(row_text(&build_row(&term, 0, &PALETTE, &[]))[0].1, "two");
        term.scroll_display(Scroll::Delta(1));
        assert_eq!(row_text(&build_row(&term, 0, &PALETTE, &[]))[0].1, "one");
        assert_eq!(row_text(&build_row(&term, 1, &PALETTE, &[]))[0].1, "two");
    }

    #[test]
    fn the_cursor_is_where_the_viewport_says_and_gone_when_scrolled_off() {
        use alacritty_terminal::grid::Scroll;
        let mut term = term(20, 3);
        feed(&mut term, b"one\r\ntwo\r\nthree\r\nfour");
        let cursor = cursor_spec(&term, true, &PALETTE).unwrap();
        assert_eq!(cursor.row, 2);
        assert_eq!(cursor.column, 4);
        assert_eq!(cursor.shape, CursorShape::Block);
        assert!(cursor.glyph.is_none(), "nothing under it to redraw");
        term.scroll_display(Scroll::Delta(1));
        assert!(cursor_spec(&term, true, &PALETTE).is_none());
    }

    #[test]
    fn an_unfocused_or_hidden_cursor_does_not_block() {
        let mut term = term(20, 3);
        feed(&mut term, b"x");
        assert_eq!(
            cursor_spec(&term, false, &PALETTE).unwrap().shape,
            CursorShape::Hollow
        );
        feed(&mut term, b"\x1b[?25l");
        assert!(cursor_spec(&term, true, &PALETTE).is_none());
    }

    #[test]
    fn the_row_cache_shapes_a_row_only_when_its_spec_changes() {
        let mut term = term(20, 3);
        feed(&mut term, b"same");
        let mut cache: RowCache<usize> = RowCache::default();
        cache.ensure_size(20, 3);
        let shapes = Cell::new(0);
        let mut stats = CacheStats::default();
        let shape = |_: &RowSpec| {
            shapes.set(shapes.get() + 1);
            vec![1]
        };
        cache.update(0, build_row(&term, 0, &PALETTE, &[]), &mut stats, shape);
        cache.update(0, build_row(&term, 0, &PALETTE, &[]), &mut stats, shape);
        assert_eq!(shapes.get(), 1);
        assert_eq!(
            stats,
            CacheStats {
                reshaped: 1,
                reused: 1,
            }
        );
        feed(&mut term, b"!");
        cache.update(0, build_row(&term, 0, &PALETTE, &[]), &mut stats, shape);
        assert_eq!(shapes.get(), 2);
    }

    #[test]
    fn a_size_change_drops_the_cache_and_a_same_size_does_not() {
        let mut cache: RowCache<usize> = RowCache::default();
        assert!(cache.ensure_size(20, 3));
        let mut stats = CacheStats::default();
        cache.update(0, RowSpec::default(), &mut stats, |_| vec![]);
        assert!(!cache.ensure_size(20, 3));
        assert!(cache.rows[0].is_some());
        assert!(cache.ensure_size(30, 3));
        assert!(cache.rows[0].is_none());
    }

    #[test]
    fn damage_reports_only_touched_rows_and_a_scroll_reports_all_of_them() {
        let mut term = term(20, 5);
        term.reset_damage();
        feed(&mut term, b"\x1b[2;1Hx");
        let first = damaged_rows(&mut term);
        assert!(!first.full);
        assert!(first.rows.contains(&1) && !first.rows.contains(&3));
        feed(&mut term, b"\r\n\r\n\r\n\r\n\r\n\r\n");
        let scrolled = damaged_rows(&mut term);
        assert!(scrolled.full);
        assert_eq!(scrolled.rows, [0, 1, 2, 3, 4]);
    }
}
