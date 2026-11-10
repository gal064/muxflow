//! A page of scrollback, from the rows tmux captured to the bytes the terminal
//! is given. Port of `historyPage.ts`.
//!
//! The unit is the physical row: the history capture runs without `-J`, so one
//! captured row is one row of the grid at this pane's width. A row that fills
//! the grid is written with no line break, so the terminal wraps it itself and
//! flags the continuation as if the pane had printed it live.

use unicode_segmentation::UnicodeSegmentation;

const ROW_SEPARATOR: &[u8] = b"\r\n";

/// `n` separators mean `n + 1` rows, including a blank last row.
pub fn split_history_rows(history: &[u8]) -> Vec<&[u8]> {
    let mut rows = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index + 1 < history.len() {
        if history[index] == 0x0d && history[index + 1] == 0x0a {
            rows.push(&history[start..index]);
            index += 2;
            start = index;
        } else {
            index += 1;
        }
    }
    rows.push(&history[start..]);
    rows
}

fn after_escape(text: &str, escape: usize) -> usize {
    let bytes = text.as_bytes();
    match bytes.get(escape + 1) {
        Some(b'[') => {
            let mut index = escape + 2;
            while index < bytes.len() {
                let code = bytes[index];
                index += 1;
                if (0x40..=0x7e).contains(&code) {
                    break;
                }
            }
            index
        }
        Some(b']') => {
            let mut index = escape + 2;
            while index < bytes.len() {
                if bytes[index] == 0x07 {
                    return index + 1;
                }
                if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
                    return index + 2;
                }
                index += 1;
            }
            index
        }
        // By character: the byte after the escape may begin a multibyte one.
        Some(_) => text[escape + 1..]
            .chars()
            .next()
            .map_or(escape + 1, |next| escape + 1 + next.len_utf8()),
        None => escape + 1,
    }
}

fn plain_text(row: &[u8]) -> String {
    let text = String::from_utf8_lossy(row);
    let bytes = text.as_bytes();
    let mut plain = String::with_capacity(text.len());
    let mut index = 0;
    let mut kept = 0;
    while index < bytes.len() {
        let code = bytes[index];
        if code == 0x1b {
            plain.push_str(&text[kept..index]);
            index = after_escape(&text, index).min(bytes.len());
            kept = index;
        } else if code < 0x20 || code == 0x7f {
            plain.push_str(&text[kept..index]);
            index += 1;
            kept = index;
        } else {
            index += 1;
        }
    }
    plain.push_str(&text[kept..]);
    plain
}

/// How many cells a captured row occupies, at most.
///
/// Deliberately a lower bound. An over-count makes a row that never filled the
/// grid look as though it did, and it is then joined to the row below, losing a
/// line break the user printed. A wide glyph counts as one cell, so a row of them
/// measures short and is treated as unwrapped, which costs only a reflow.
pub fn visible_width(row: &[u8]) -> usize {
    plain_text(row).graphemes(true).count()
}

/// The bytes for a page of captured rows, wrapped the way tmux had them.
///
/// A row followed by a blank one is never joined: the blank row is real output
/// and the carriage return that would carry a join cancels the pending wrap and
/// swallows it. Blank means no cells, not no bytes, because `capture-pane -e`
/// writes a blank row with a background as its SGR alone.
pub fn compose_history_page(rows: &[&[u8]], columns: usize) -> Vec<u8> {
    let widths: Vec<usize> = rows.iter().map(|row| visible_width(row)).collect();
    let mut composed = Vec::with_capacity(rows.iter().map(|row| row.len() + 2).sum());
    for (index, row) in rows.iter().enumerate() {
        composed.extend_from_slice(row);
        if index + 1 == rows.len() {
            break;
        }
        if !(columns > 0 && widths[index] >= columns && widths[index + 1] > 0) {
            composed.extend_from_slice(ROW_SEPARATOR);
        }
    }
    composed
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::cell::Flags;

    use super::*;
    use crate::terminal::scratch_terminal;

    fn compose(rows: &[&str], columns: usize) -> Vec<u8> {
        let rows: Vec<&[u8]> = rows.iter().map(|row| row.as_bytes()).collect();
        compose_history_page(&rows, columns)
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn counts_n_separators_as_n_plus_one_rows() {
        assert_eq!(
            split_history_rows(b"a\r\nb\r\nc"),
            vec![b"a".as_ref(), b"b", b"c"]
        );
        assert_eq!(split_history_rows(b"only"), vec![b"only".as_ref()]);
    }

    #[test]
    fn keeps_a_blank_last_row() {
        assert_eq!(
            split_history_rows(b"a\r\nb\r\n"),
            vec![b"a".as_ref(), b"b", b""]
        );
    }

    #[test]
    fn counts_grapheme_clusters_so_combining_sequences_and_wide_glyphs_never_measure_long() {
        assert_eq!(visible_width("e\u{301}".repeat(6).as_bytes()), 6);
        assert_eq!(visible_width("\u{1F469}\u{200D}\u{1F4BB}".as_bytes()), 1);
        assert_eq!(visible_width("\u{6F22}".repeat(4).as_bytes()), 4);
        assert!(compose(&[&"e\u{301}".repeat(5), "next"], 8).contains(&13));
    }

    #[test]
    fn counts_cells_not_the_sgr_that_colours_them() {
        assert_eq!(visible_width(b"plain"), 5);
        assert_eq!(visible_width(b"\x1b[1;31mred\x1b[0m"), 3);
        assert_eq!(visible_width(b"\x1b]8;;http://x\x07link\x1b]8;;\x07"), 4);
    }

    #[test]
    fn breaks_rows_narrower_than_the_grid_and_joins_rows_that_fill_it() {
        let composed = compose(&[&"x".repeat(8), "tail", "short"], 8);
        assert_eq!(text(&composed), format!("{}tail\r\nshort", "x".repeat(8)));
    }

    #[test]
    fn never_joins_a_row_into_a_blank_one() {
        let composed = compose(&[&"x".repeat(8), "", "after"], 8);
        assert_eq!(text(&composed), format!("{}\r\n\r\nafter", "x".repeat(8)));
    }

    #[test]
    fn treats_an_escapes_only_row_as_blank_not_as_a_continuation() {
        let composed = compose(&[&"x".repeat(8), "\x1b[41m", "after"], 8);
        assert_eq!(
            text(&composed),
            format!("{}\r\n\x1b[41m\r\nafter", "x".repeat(8))
        );
    }

    #[test]
    fn leaves_the_terminal_holding_a_wrapped_row_not_two_hard_broken_ones() {
        let term = scratch_terminal(8, 4, &compose(&["abcdefgh", "ij", "next"], 8));
        let grid = term.grid();
        assert!(
            grid[Line(0)][Column(7)].flags.contains(Flags::WRAPLINE),
            "the row that filled the grid carries the wrap flag"
        );
        assert!(!grid[Line(1)][Column(7)].flags.contains(Flags::WRAPLINE));
        let copied = term.bounds_to_string(
            Point::new(Line(0), Column(0)),
            Point::new(Line(1), term.last_column()),
        );
        assert_eq!(copied.trim_end(), "abcdefghij");
    }

    #[test]
    fn measures_a_row_with_an_escape_before_a_multibyte_character() {
        assert_eq!(visible_width(b"\x1b\xc3\xa9abc"), 3);
        assert_eq!(visible_width(b"\x1b\xffabc"), 3);
    }

    #[test]
    fn occupies_one_terminal_row_per_captured_row() {
        let captured = ["abcdefgh", "ij", "", "klm", "abcdefgh", "nop"];
        let term = scratch_terminal(8, 3, &compose(&captured, 8));
        let total = term.grid().history_size() as i32 + term.grid().cursor.point.line.0;
        assert_eq!(total, captured.len() as i32 - 1);
    }
}
