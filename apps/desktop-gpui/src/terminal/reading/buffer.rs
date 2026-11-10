//! The engine grid read through `terminal-interactions`' buffer traits, which
//! follow xterm's numbering: row `0` is the oldest history row, and a row is
//! "wrapped" when it continues the row above it.

use std::cell::OnceCell;
use std::ops::{Range, RangeInclusive};

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color, NamedColor};
use terminal_interactions::{Buffer, BufferLine, Cell};

/// Flags that describe layout rather than how a cell is drawn.
const LAYOUT_FLAGS: Flags = Flags::WRAPLINE
    .union(Flags::WIDE_CHAR)
    .union(Flags::WIDE_CHAR_SPACER)
    .union(Flags::LEADING_WIDE_CHAR_SPACER);

pub fn buffer_row<T>(term: &Term<T>, line: Line) -> usize {
    (line.0 + term.grid().history_size() as i32).max(0) as usize
}

pub fn grid_line<T>(term: &Term<T>, row: usize) -> Line {
    Line(row as i32 - term.grid().history_size() as i32)
}

struct CellSpan {
    text: Range<usize>,
    width: usize,
    attribute_default: bool,
}

pub struct GridLine {
    wrapped: bool,
    text: String,
    cells: Vec<CellSpan>,
}

impl BufferLine for GridLine {
    fn is_wrapped(&self) -> bool {
        self.wrapped
    }

    fn columns(&self) -> usize {
        self.cells.len()
    }

    fn cell(&self, index: usize) -> Option<Cell<'_>> {
        let cell = self.cells.get(index)?;
        Some(Cell {
            chars: &self.text[cell.text.clone()],
            width: cell.width,
            attribute_default: cell.attribute_default,
        })
    }
}

fn read_line<T>(term: &Term<T>, row: usize) -> GridLine {
    let grid = term.grid();
    let line = grid_line(term, row);
    let last = term.last_column();
    let wrapped = row > 0 && grid[Line(line.0 - 1)][last].flags.contains(Flags::WRAPLINE);
    let mut text = String::new();
    let mut cells = Vec::with_capacity(term.columns());
    for column in 0..term.columns() {
        let cell = &grid[line][Column(column)];
        let start = text.len();
        let zerowidth = cell.zerowidth().filter(|chars| !chars.is_empty());
        let spacer = cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
        // alacritty does not tell a written space from an untouched cell, and
        // xterm reads both as blank: an empty cell is what trims a row.
        let blank = spacer || ((cell.c == ' ' || cell.c == '\0') && zerowidth.is_none());
        if !blank {
            text.push(cell.c);
            text.extend(zerowidth.into_iter().flatten());
        }
        let width = if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            0
        } else if cell.flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        };
        cells.push(CellSpan {
            text: start..text.len(),
            width,
            attribute_default: cell.fg == Color::Named(NamedColor::Foreground)
                && cell.bg == Color::Named(NamedColor::Background)
                && (cell.flags - LAYOUT_FLAGS).is_empty()
                && cell.underline_color().is_none(),
        });
    }
    GridLine {
        wrapped,
        text,
        cells,
    }
}

/// Rows of the grid, read on first use. Rows outside `window` read as missing,
/// which bounds the work a hover over a pathological line can do.
pub struct GridBuffer<'a, T> {
    term: &'a Term<T>,
    first: usize,
    rows: Vec<OnceCell<GridLine>>,
}

impl<'a, T> GridBuffer<'a, T> {
    pub fn new(term: &'a Term<T>, window: RangeInclusive<usize>) -> Self {
        let last = (*window.end()).min(term.total_lines().saturating_sub(1));
        let first = *window.start();
        Self {
            term,
            first,
            rows: (first..=last).map(|_| OnceCell::new()).collect(),
        }
    }
}

impl<T> Buffer for GridBuffer<'_, T> {
    fn line_count(&self) -> usize {
        self.term.total_lines()
    }

    fn line(&self, row: usize) -> Option<&dyn BufferLine> {
        let slot = self.rows.get(row.checked_sub(self.first)?)?;
        Some(slot.get_or_init(|| read_line(self.term, row)))
    }
}

#[cfg(test)]
pub(super) mod tests {
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;
    use terminal_engine::{GridSize, Listener};
    use terminal_interactions::translate_to_string;

    use super::*;

    pub fn term(columns: usize, rows: usize, bytes: &[u8]) -> Term<Listener> {
        let config = Config {
            scrolling_history: 100,
            ..Config::default()
        };
        let mut term = Term::new(config, &GridSize { columns, rows }, Listener);
        Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new().advance(&mut term, bytes);
        term
    }

    #[test]
    fn rows_count_from_the_oldest_history_row_and_mark_their_continuations() {
        let term = term(5, 2, b"one\r\nabcdefgh\r\nlast");
        let buffer = GridBuffer::new(&term, 0..=10);
        assert_eq!(buffer.line_count(), 4);
        let text = |row: usize| {
            let line = buffer.line(row).unwrap();
            translate_to_string(line, true, 0, line.columns())
        };
        assert_eq!(
            [text(0), text(1), text(2), text(3)],
            ["one", "abcde", "fgh", "last"]
        );
        assert!(!buffer.line(1).unwrap().is_wrapped());
        assert!(buffer.line(2).unwrap().is_wrapped());
        assert_eq!(buffer_row(&term, Line(0)), 2);
        assert_eq!(grid_line(&term, 0), Line(-2));
    }

    #[test]
    fn a_wide_glyph_spans_two_cells_and_styled_cells_are_not_default() {
        let term = term(10, 2, "a\u{4f60}\x1b[1mb".as_bytes());
        let buffer = GridBuffer::new(&term, 0..=1);
        let line = buffer.line(0).unwrap();
        let cell = |index| line.cell(index).unwrap();
        assert_eq!((cell(1).chars, cell(1).width), ("\u{4f60}", 2));
        assert_eq!((cell(2).chars, cell(2).width), ("", 0));
        assert!(cell(0).attribute_default);
        assert!(!cell(3).attribute_default);
        assert_eq!(cell(4).chars, "", "an untouched cell is blank");
    }

    #[test]
    fn rows_outside_the_window_are_missing() {
        let term = term(5, 3, b"a\r\nb\r\nc");
        let buffer = GridBuffer::new(&term, 1..=1);
        assert!(buffer.line(0).is_none());
        assert!(buffer.line(1).is_some());
        assert!(buffer.line(2).is_none());
    }
}
