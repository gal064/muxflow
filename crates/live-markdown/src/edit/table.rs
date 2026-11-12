//! Editing inside a rendered table: every cell edit goes through today's
//! whole-cell normalization, Tab and Enter move by cell, Up and Down keep the
//! column. Full `liveTable` behaviour is T7.3's.

use std::ops::Range;

use super::LiveDoc;
use crate::doc::{EditKind, Selection};
use crate::projection::Projection;
use crate::table::{cell_source, row_insertion};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellDir {
    Next,
    Prev,
    Down,
}

/// The point after the first padding space of empty cell `col` in an inserted
/// row: each empty cell is written `  |`.
fn col_offset(col: usize) -> usize {
    1 + col * 3
}

impl LiveDoc {
    /// Typing that starts and ends in one cell writes that cell. False when
    /// `range` is not inside a single cell.
    pub(super) fn type_in_cell(&mut self, range: Range<usize>, text: &str, kind: EditKind) -> bool {
        let p = self.near(Selection::caret(range.start), range.clone());
        let cell = p.cell_at(range.start).map(|c| (c.2.from, c.2.limit));
        let Some((from, limit)) = cell else {
            return false;
        };
        if p.cell_at(range.end).map(|c| (c.2.from, c.2.limit)) != cell {
            return false;
        }
        let src = self.doc.text();
        let new = format!(
            "{}{}{}",
            &src[from..range.start],
            text,
            &src[range.end..limit]
        );
        let caret_in_new = range.start - from + text.len();
        let caret = self.write_cell(from, limit, &new, caret_in_new, kind);
        self.cell_typed_end = text.ends_with(' ').then_some(caret);
        true
    }

    /// Rewrite a cell through today's whole-cell normalization (`cellSource`),
    /// so no edit can leave an unescaped pipe or a backslash against the closing
    /// pipe. Returns the caret.
    fn write_cell(
        &mut self,
        from: usize,
        limit: usize,
        new: &str,
        caret_in_new: usize,
        kind: EditKind,
    ) -> usize {
        let normalized = cell_source(new);
        let caret = from + cell_source(&new[..caret_in_new]).len();
        let slashes = normalized.bytes().rev().take_while(|b| *b == b'\\').count();
        let next = self.doc.text().as_bytes().get(limit).copied();
        let suffix = if slashes % 2 == 1 && next == Some(b'|') {
            " "
        } else {
            ""
        };
        self.doc
            .replace(from..limit, &format!("{normalized}{suffix}"), caret, kind);
        caret
    }

    /// Backspace in a cell: within the cell's text, or removing an empty row
    /// from its first cell. True when the caret is in a cell.
    pub(super) fn backspace_in_cell(&mut self, p: &Projection, caret: usize) -> bool {
        let Some((t, _, cell)) = p.cell_at(caret) else {
            return false;
        };
        if caret > cell.from {
            let prev = self
                .prev_boundary(caret)
                .unwrap_or(cell.from)
                .max(cell.from);
            let src = self.doc.text();
            let new = format!("{}{}", &src[cell.from..prev], &src[caret..cell.limit]);
            let at = self.write_cell(
                cell.from,
                cell.limit,
                &new,
                prev - cell.from,
                EditKind::Deletion,
            );
            self.cell_typed_end = self.cell_typed_end.map(|_| at);
            self.settle();
        } else if let Some(line) = empty_row(p, t, cell.row) {
            let (up, col) = (cell.row - 1, cell.col);
            let new_caret = p.tables[t]
                .cells
                .iter()
                .find(|c| c.row == up && c.col == col)
                .map_or(line.start, |c| c.to);
            self.doc.replace(line, "", new_caret, EditKind::Other);
            self.settle();
        }
        true
    }

    /// Delete in a cell stays in the cell. True when the caret is in a cell.
    pub(super) fn delete_in_cell(&mut self, p: &Projection, caret: usize) -> bool {
        let Some((_, _, cell)) = p.cell_at(caret) else {
            return false;
        };
        if caret < cell.limit {
            let next = self
                .next_boundary(caret)
                .unwrap_or(cell.limit)
                .min(cell.limit);
            let src = self.doc.text();
            let new = format!("{}{}", &src[cell.from..caret], &src[next..cell.limit]);
            self.write_cell(
                cell.from,
                cell.limit,
                &new,
                caret - cell.from,
                EditKind::Deletion,
            );
            self.settle();
        }
        true
    }

    /// Enter in a cell moves down a row, adding one at the end; never a
    /// newline, which would split the row. True when the caret is in a cell.
    pub(super) fn enter_in_cell(&mut self) -> bool {
        if self.cell_ops().is_none() {
            return false;
        }
        self.cell_navigate(CellDir::Down);
        true
    }

    /// The caret's table, row, column and the table's last row.
    fn cell_ops(&mut self) -> Option<(usize, usize, usize, usize)> {
        let caret = self.doc.selection().head;
        let p = self.here();
        let (t, _c, cell) = p.cell_at(caret)?;
        Some((t, cell.row, cell.col, p.tables[t].layout.rows.len() - 1))
    }

    /// Up or Down inside a table goes by cell, not by pixels: a wrapped neighbour
    /// must not pull the caret out of its column. False at the table's edge.
    pub fn cell_vertical(&mut self, down: bool) -> bool {
        let Some((t, row, col, last_row)) = self.cell_ops() else {
            return false;
        };
        if (down && row == last_row) || (!down && row == 0) {
            return false;
        }
        let to = if down { row + 1 } else { row - 1 };
        let p = self.here();
        let cells = &p.tables[t].cells;
        let target = cells
            .iter()
            .find(|c| c.row == to && c.col == col)
            .or_else(|| cells.iter().rfind(|c| c.row == to))
            .map(|c| c.to);
        match target {
            Some(at) => {
                self.set_caret(at);
                true
            }
            None => false,
        }
    }

    /// Tab, Shift-Tab and Enter inside a table.
    pub fn cell_navigate(&mut self, dir: CellDir) -> bool {
        let Some((t, row, col, last_row)) = self.cell_ops() else {
            return false;
        };
        let p = self.here();
        let table = &p.tables[t];
        let cell = |r: usize, c: usize| table.cells.iter().find(|x| x.row == r && x.col == c);
        let last_in = |r: usize| table.cells.iter().rfind(|x| x.row == r);
        let target = match dir {
            CellDir::Next => cell(row, col + 1).or_else(|| cell(row + 1, 0)),
            CellDir::Prev if col > 0 => cell(row, col - 1),
            CellDir::Prev if row > 0 => last_in(row - 1),
            CellDir::Prev => None,
            CellDir::Down => cell(row + 1, col).or_else(|| last_in(row + 1)),
        };
        if let Some(cell) = target {
            let at = if dir == CellDir::Prev {
                cell.to
            } else {
                cell.from
            };
            self.set_caret(at);
            return true;
        }
        if matches!(dir, CellDir::Next | CellDir::Down) && row == last_row {
            let (at, insert) = row_insertion(&table.layout, row);
            let new_caret = at + 2 + col_offset(if dir == CellDir::Down { col } else { 0 });
            self.doc
                .replace(at..at, &insert, new_caret, EditKind::Other);
            self.settle();
            return true;
        }
        false
    }
}

/// The source line of an empty body row, with the line break before it.
fn empty_row(p: &Projection, table: usize, row: usize) -> Option<Range<usize>> {
    let span = p.tables[table].layout.rows.get(row)?;
    (row > 0 && span.cells.iter().all(|c| c.to == c.from)).then(|| span.from - 1..span.to)
}
