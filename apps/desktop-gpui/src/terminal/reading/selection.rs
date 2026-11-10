//! What a selection copies. The selection itself is alacritty's, kept on the
//! grid so output that scrolls the screen carries it along; the text is read
//! with xterm's rules, because that is what copy cleanup was written against.

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Boundary, Point};
use alacritty_terminal::selection::SelectionRange;
use alacritty_terminal::term::cell::Flags;
use terminal_interactions::{
    Buffer, BufferRange, Position, TerminalSelectionSnapshot, capture_terminal_selection,
    clean_wrapped_command_selection, translate_to_string,
};

use super::buffer::{GridBuffer, buffer_row};

/// xterm's default word separators, which double-click stops at.
const WORD_SEPARATORS: &str = " ()[]{}',\"`";

pub fn range<T>(term: &Term<T>) -> Option<SelectionRange> {
    term.selection.as_ref()?.to_range(term)
}

/// One past the last selected cell, counting a wide glyph's second half.
fn end_column<T>(term: &Term<T>, end: Point) -> usize {
    let wide = term.grid()[end].flags.contains(Flags::WIDE_CHAR);
    (end.column.0 + if wide { 2 } else { 1 }).min(term.columns())
}

/// The selected text and its row evidence, read in one go so output cannot
/// change the evidence under the copy. Port of `captureTerminalSelection`'s
/// caller: the text is xterm's `getSelection`.
pub fn snapshot<T>(term: &Term<T>) -> Option<TerminalSelectionSnapshot> {
    let range = range(term)?;
    let columns = term.columns();
    let (first, last) = (
        buffer_row(term, range.start.line),
        buffer_row(term, range.end.line),
    );
    let (start_x, end_x) = (range.start.column.0, end_column(term, range.end));
    let buffer = GridBuffer::new(term, first..=last);
    let row_text = |row: usize, start: usize, end: usize| {
        buffer
            .line(row)
            .map(|line| translate_to_string(line, true, start, end))
            .unwrap_or_default()
    };
    let mut rows: Vec<String> = Vec::new();
    if range.is_block {
        rows.extend((first..=last).map(|row| row_text(row, start_x, end_x)));
    } else {
        for row in first..=last {
            let start = if row == first { start_x } else { 0 };
            let end = if row == last { end_x } else { columns };
            let text = row_text(row, start, end);
            let continues = row != first && buffer.line(row).is_some_and(|line| line.is_wrapped());
            match rows.last_mut() {
                Some(previous) if continues => previous.push_str(&text),
                _ => rows.push(text),
            }
        }
    }
    let text = rows.join("\n").replace('\u{a0}', " ");
    if text.is_empty() {
        return None;
    }
    let position = (!range.is_block).then_some(BufferRange {
        start: Position {
            x: start_x,
            y: first,
        },
        end: Position { x: end_x, y: last },
    });
    Some(capture_terminal_selection(
        &text, position, columns, &buffer,
    ))
}

/// What a copy writes. Port of `copyCompletedTerminalSelection`.
pub fn copy_text<T>(term: &Term<T>, clean_wrapped_commands: bool) -> Option<String> {
    let snapshot = snapshot(term)?;
    Some(if clean_wrapped_commands {
        clean_wrapped_command_selection(&snapshot)
    } else {
        snapshot.text
    })
}

fn is_word_cell<T>(term: &Term<T>, point: Point) -> bool {
    let cell = &term.grid()[point];
    if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
        return true;
    }
    !(cell.c == '\0' || cell.c.is_whitespace() || WORD_SEPARATORS.contains(cell.c))
}

/// The word under `point`, following soft wraps, as xterm's double-click
/// finds it. A separator is a word of its own; a blank is no word.
pub fn word_at<T>(term: &Term<T>, mut point: Point) -> Option<(Point, Point)> {
    // A wide glyph's second cell is a blank spacer: the glyph is the cell
    // before it.
    if point.column.0 > 0 && term.grid()[point].flags.contains(Flags::WIDE_CHAR_SPACER) {
        point.column -= 1;
    }
    let cell = &term.grid()[point];
    if cell.c == ' ' || cell.c == '\0' {
        return None;
    }
    if !is_word_cell(term, point) {
        return Some((point, point));
    }
    let wraps_into = |point: Point| {
        term.grid()[point.line][term.last_column()]
            .flags
            .contains(Flags::WRAPLINE)
    };
    let mut start = point;
    loop {
        let previous = start.sub(term, Boundary::Grid, 1);
        let crosses = previous.line != start.line && !wraps_into(previous);
        if previous == start || crosses || !is_word_cell(term, previous) {
            break;
        }
        start = previous;
    }
    let mut end = point;
    loop {
        let next = end.add(term, Boundary::Grid, 1);
        let crosses = next.line != end.line && !wraps_into(end);
        if next == end || crosses || !is_word_cell(term, next) {
            break;
        }
        end = next;
    }
    Some((start, end))
}

/// The cells `range` covers on one grid line, as `[start, end)`.
pub fn columns_on_line(
    range: &SelectionRange,
    line: i32,
    columns: usize,
) -> Option<(usize, usize)> {
    let (start, end) = (range.start, range.end);
    if line < start.line.0 || line > end.line.0 {
        return None;
    }
    if range.is_block {
        return Some((start.column.0, end.column.0 + 1));
    }
    let from = if line == start.line.0 {
        start.column.0
    } else {
        0
    };
    let to = if line == end.line.0 {
        end.column.0 + 1
    } else {
        columns
    };
    (from < to).then_some((from, to.min(columns)))
}

/// A point at the start of `column` on `line`, for building ranges in tests.
#[cfg(test)]
pub fn point(line: i32, column: usize) -> Point {
    use alacritty_terminal::index::{Column, Line};
    Point::new(Line(line), Column(column))
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::index::Side;
    use alacritty_terminal::selection::{Selection, SelectionType};

    use super::super::buffer::tests::term;
    use super::*;

    fn select<T>(term: &mut Term<T>, from: Point, to: Point) {
        let mut selection = Selection::new(SelectionType::Simple, from, Side::Left);
        selection.update(to, Side::Right);
        term.selection = Some(selection);
    }

    #[test]
    fn copies_rows_as_xterm_does_joining_only_soft_wraps() {
        let mut term = term(10, 4, b"first line\r\nabcdefghijKLM\r\nlast   ");
        // `first line` fills its row exactly, so the next row is not a wrap.
        select(&mut term, point(0, 6), point(3, 3));
        let copied = copy_text(&term, false).unwrap();
        assert_eq!(copied, "line\nabcdefghijKLM\nlast");
    }

    #[test]
    fn an_empty_selection_copies_nothing() {
        let mut term = term(10, 2, b"text");
        term.selection = Some(Selection::new(
            SelectionType::Simple,
            point(0, 1),
            Side::Left,
        ));
        assert_eq!(copy_text(&term, true), None);
        select(&mut term, point(1, 0), point(1, 4));
        assert_eq!(
            copy_text(&term, true),
            None,
            "a blank row is nothing to copy"
        );
    }

    #[test]
    fn a_wide_glyph_at_the_end_is_copied_whole() {
        let mut term = term(10, 2, "ab\u{4f60}c".as_bytes());
        select(&mut term, point(0, 0), point(0, 2));
        assert_eq!(copy_text(&term, false).unwrap(), "ab\u{4f60}");
    }

    #[test]
    fn cleanup_joins_a_command_an_application_wrapped_into_styled_rows() {
        // A composer's rendering: every glyph styled, a two-cell gutter on the
        // continuation, and each break where the next token would not fit.
        let screen = "\x1b[36m$ cargo test --workspace\x1b[0m\r\n  \x1b[36m--all-targets\x1b[0m";
        let mut term = term(26, 3, screen.as_bytes());
        select(&mut term, point(0, 0), point(1, 25));
        assert_eq!(
            copy_text(&term, false).unwrap(),
            "$ cargo test --workspace\n  --all-targets"
        );
        assert_eq!(
            copy_text(&term, true).unwrap(),
            "$ cargo test --workspace\n  --all-targets",
            "`$` is not a command token, so this stays verbatim"
        );
        let screen = "\x1b[36mcargo test --workspace\x1b[0m\r\n  \x1b[36m--all-targets\x1b[0m";
        let mut term = super::super::buffer::tests::term(24, 3, screen.as_bytes());
        select(&mut term, point(0, 0), point(1, 23));
        assert_eq!(
            copy_text(&term, true).unwrap(),
            "cargo test --workspace --all-targets"
        );
        assert_eq!(
            copy_text(&term, false).unwrap(),
            "cargo test --workspace\n  --all-targets"
        );
    }

    #[test]
    fn cleanup_leaves_plain_output_alone() {
        let screen = "cargo test --workspace\r\n  --all-targets";
        let mut term = term(24, 3, screen.as_bytes());
        select(&mut term, point(0, 0), point(1, 23));
        assert_eq!(
            copy_text(&term, true).unwrap(),
            "cargo test --workspace\n  --all-targets",
            "unstyled text is shell output, not a rendered command"
        );
    }

    #[test]
    fn a_block_selection_copies_its_columns_verbatim() {
        let mut term = term(10, 3, b"abcdef\r\nghijkl");
        let mut selection = Selection::new(SelectionType::Block, point(0, 1), Side::Left);
        selection.update(point(1, 2), Side::Right);
        term.selection = Some(selection);
        assert_eq!(copy_text(&term, true).unwrap(), "bc\nhi");
    }

    #[test]
    fn a_word_follows_soft_wraps_and_stops_at_separators() {
        let term = term(8, 3, b"x (path/to/file.rs) y");
        assert_eq!(
            word_at(&term, point(0, 5)),
            Some((point(0, 3), point(2, 1))),
            "path/to/file.rs crosses two wraps"
        );
        assert_eq!(
            word_at(&term, point(0, 2)),
            Some((point(0, 2), point(0, 2)))
        );
        assert_eq!(word_at(&term, point(0, 1)), None);
        let url = super::super::buffer::tests::term(40, 2, b"see http://host:80/a ok");
        assert_eq!(
            word_at(&url, point(0, 6)),
            Some((point(0, 4), point(0, 19))),
            "a colon does not end a word, as in xterm"
        );
    }

    #[test]
    fn a_double_click_on_either_half_of_a_wide_glyph_takes_its_word() {
        let term = term(10, 2, "a \u{4f60}\u{597d} b".as_bytes());
        let word = Some((point(0, 2), point(0, 5)));
        assert_eq!(word_at(&term, point(0, 2)), word);
        assert_eq!(word_at(&term, point(0, 3)), word);
        assert_eq!(word_at(&term, point(0, 5)), word);
    }

    #[test]
    fn a_word_does_not_cross_a_hard_line_end() {
        let term = term(8, 3, b"abcdefgh\r\nijk");
        assert_eq!(
            word_at(&term, point(1, 1)),
            Some((point(1, 0), point(1, 2)))
        );
        assert_eq!(
            word_at(&term, point(0, 7)),
            Some((point(0, 0), point(0, 7)))
        );
    }

    #[test]
    fn a_range_covers_whole_rows_between_its_ends() {
        let range = SelectionRange::new(point(-1, 3), point(1, 2), false);
        assert_eq!(columns_on_line(&range, -2, 10), None);
        assert_eq!(columns_on_line(&range, -1, 10), Some((3, 10)));
        assert_eq!(columns_on_line(&range, 0, 10), Some((0, 10)));
        assert_eq!(columns_on_line(&range, 1, 10), Some((0, 3)));
        let block = SelectionRange::new(point(0, 3), point(1, 5), true);
        assert_eq!(columns_on_line(&block, 1, 10), Some((3, 6)));
    }
}
