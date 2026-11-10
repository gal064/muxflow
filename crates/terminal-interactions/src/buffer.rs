use serde::Serialize;

/// One terminal cell, as xterm's buffer API reports it.
#[derive(Clone, Copy, Debug)]
pub struct Cell<'a> {
    /// Empty for a cell that was never written.
    pub chars: &'a str,
    /// 0 for the trailing half of a wide glyph, 1 or 2 otherwise.
    pub width: usize,
    pub attribute_default: bool,
}

pub trait BufferLine {
    /// The terminal's own soft-wrap marker. A hard line break leaves this false.
    fn is_wrapped(&self) -> bool;
    fn columns(&self) -> usize;
    fn cell(&self, index: usize) -> Option<Cell<'_>>;
}

pub trait Buffer {
    fn line_count(&self) -> usize;
    fn line(&self, row: usize) -> Option<&dyn BufferLine>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Position {
    pub x: usize,
    pub y: usize,
}

/// Link ranges are one-based and inclusive. Selection ranges are zero-based
/// and end-exclusive, as xterm reports them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct BufferRange {
    pub start: Position,
    pub end: Position,
}

fn trimmed_length(line: &dyn BufferLine) -> usize {
    (0..line.columns())
        .rev()
        .find_map(|index| {
            let cell = line.cell(index)?;
            (!cell.chars.is_empty()).then_some(index + cell.width)
        })
        .unwrap_or(0)
}

/// Reads columns `start..end` as text with xterm's rules: a wide glyph's
/// trailing half is skipped and a cell that was never written reads as a space.
pub fn translate_to_string(
    line: &dyn BufferLine,
    trim_right: bool,
    start: usize,
    end: usize,
) -> String {
    let mut end = end.min(line.columns());
    if trim_right {
        end = end.min(trimmed_length(line));
    }
    let mut text = String::new();
    let mut column = start;
    while column < end {
        let Some(cell) = line.cell(column) else {
            text.push(' ');
            column += 1;
            continue;
        };
        text.push_str(if cell.chars.is_empty() {
            " "
        } else {
            cell.chars
        });
        column += cell.width.max(1);
    }
    text
}
