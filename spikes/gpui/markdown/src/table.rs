//! Pure port of today's `liveTable.ts` source logic: cell spans and cell
//! writes. A table edit changes only that cell's bytes, so padding, alignment
//! rows and pipe style survive.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellSpan {
    pub from: usize,
    pub to: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowSpan {
    pub from: usize,
    pub to: usize,
    pub cells: Vec<CellSpan>,
    pub closed: bool,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableLayout {
    /// The header, then each body row (the delimiter row is structure).
    pub rows: Vec<RowSpan>,
    pub delimiter_end: usize,
}

/// The rows and cells of a GFM table's source, offset by `base`.
pub fn table_layout(source: &str, base: usize) -> TableLayout {
    let mut rows = Vec::new();
    let mut delimiter_end = base;
    let mut at = 0;
    for (index, line) in source.split('\n').enumerate() {
        if index == 1 {
            delimiter_end = base + at + line.len();
        } else {
            rows.push(row_span(line, base + at));
        }
        at += line.len() + 1;
    }
    TableLayout { rows, delimiter_end }
}

fn row_span(line: &str, offset: usize) -> RowSpan {
    let start = line.len() - line.trim_start().len();
    let end = line.trim_end().len();
    let bytes = line.as_bytes();
    let mut edges: Vec<isize> = Vec::new();
    let mut i = start;
    while i < end {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'|' {
            edges.push(i as isize);
        }
        i += 1;
    }
    let closed = edges.last().copied() == Some(end as isize - 1);
    if edges.first().copied() != Some(start as isize) {
        edges.insert(0, start as isize - 1);
    }
    if edges.len() < 2 || !closed {
        edges.push(end as isize);
    }
    let mut cells = Vec::new();
    for k in 0..edges.len() - 1 {
        let left = (edges[k] + 1) as usize;
        let right = edges[k + 1] as usize;
        let segment = &line[left..right];
        let trimmed = segment.trim();
        let from = left
            + if trimmed.is_empty() {
                1.min(segment.len())
            } else {
                segment.len() - segment.trim_start().len()
            };
        cells.push(CellSpan { from: offset + from, to: offset + from + trimmed.len() });
    }
    RowSpan { from: offset, to: offset + line.len(), cells, closed, end: offset + end }
}

/// Cell text as written to source: one line, with typed pipes escaped.
pub fn cell_source(text: &str) -> String {
    let flat = text.replace('\u{a0}', " ");
    let mut out = String::new();
    let mut pending_space = false;
    let mut chars = flat.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' || c == '\n' {
            while out.ends_with([' ', '\t']) {
                out.pop();
            }
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            pending_space = true;
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        if c == '|' {
            let slashes = out.chars().rev().take_while(|c| *c == '\\').count();
            if slashes % 2 == 0 {
                out.push('\\');
            }
        }
        out.push(c);
    }
    if pending_space {
        out.push(' ');
    }
    out
}

/// What a cell's text writes to the file: trimmed, with a trailing lone
/// backslash kept off the closing pipe by a space.
pub fn cell_write(shown: &str) -> String {
    let text = shown.trim();
    let slashes = text.chars().rev().take_while(|c| *c == '\\').count();
    if slashes % 2 == 1 {
        format!("{text} ")
    } else {
        text.to_string()
    }
}

/// Change setting a cell, padding a short row with empty cells.
pub fn cell_change(layout: &TableLayout, row: usize, col: usize, text: &str) -> Option<(std::ops::Range<usize>, String)> {
    let target = layout.rows.get(row)?;
    if let Some(cell) = target.cells.get(col) {
        return Some((cell.from..cell.to, text.to_string()));
    }
    let padding = "  |".repeat(col - target.cells.len());
    let close = if target.closed { "" } else { " |" };
    Some((target.end..target.end, format!("{close}{padding} {text} |")))
}

/// The change adding an empty row after `row` (after the header means after
/// the delimiter row).
pub fn row_insertion(layout: &TableLayout, row: usize) -> (usize, String) {
    let columns = layout.rows.first().map_or(1, |r| r.cells.len());
    let at = if row == 0 { layout.delimiter_end } else { layout.rows[row].to };
    (at, format!("\n|{}", "  |".repeat(columns)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(source: &str) -> Vec<Vec<String>> {
        table_layout(source, 0)
            .rows
            .iter()
            .map(|r| r.cells.iter().map(|c| source[c.from..c.to].to_string()).collect())
            .collect()
    }

    // The next tests mirror liveTable.test.ts "table layout".
    #[test]
    fn finds_each_cells_text_skipping_the_delimiter_row() {
        let source = "| a | bb |\n|---|---|\n| 1 |  |";
        let layout = table_layout(source, 0);
        assert_eq!(text(source), vec![vec!["a", "bb"], vec!["1", ""]]);
        assert_eq!(layout.delimiter_end, source.find("\n| 1").unwrap());
        assert_eq!(layout.rows[1].cells[1], CellSpan { from: 27, to: 27 });
    }

    #[test]
    fn splits_on_unescaped_pipes_only_with_or_without_outer_pipes() {
        let source = "a | b\\|c\n- | -\nx | `y`";
        assert_eq!(text(source), vec![vec!["a", "b\\|c"], vec!["x", "`y`"]]);
    }

    #[test]
    fn writes_cell_text_on_one_line_with_typed_pipes_escaped() {
        assert_eq!(cell_source("a|b"), "a\\|b");
        assert_eq!(cell_source("a\\|b"), "a\\|b");
        assert_eq!(cell_source("one\ntwo"), "one two");
    }

    #[test]
    fn writes_a_cell_trimmed_keeping_a_trailing_backslash_off_the_closing_pipe() {
        assert_eq!(cell_write(" a x "), "a x");
        assert_eq!(cell_write("a\\"), "a\\ ");
        assert_eq!(cell_write("a\\\\"), "a\\\\");
    }

    #[test]
    fn pads_a_short_row_out_to_the_cell_being_written() {
        let closed = table_layout("| a | b | c |\n|-|-|-|\n| 1 |", 0);
        assert_eq!(cell_change(&closed, 1, 2, "z"), Some((27..27, "  | z |".to_string())));
        let open = table_layout("a | b\n-|-\n1", 0);
        assert_eq!(cell_change(&open, 1, 1, "z"), Some((11..11, " | z |".to_string())));
    }

    #[test]
    fn adds_a_row_under_the_delimiter_after_the_header() {
        let layout = table_layout("| a | b |\n|-|-|\n| 1 | 2 |", 0);
        assert_eq!(row_insertion(&layout, 0), (15, "\n|  |  |".to_string()));
        assert_eq!(row_insertion(&layout, 1), (25, "\n|  |  |".to_string()));
    }
}
