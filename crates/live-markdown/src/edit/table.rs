//! Editing inside a rendered table, as today's table widget (`liveTable.ts`):
//! every cell edit goes through the whole-cell normalization; Tab, Enter and
//! the arrows move by cell and leave the table at its edges; a row is added
//! from the last row and an empty one removed with Backspace. A short row's
//! missing cells take the caret, and typing into one fills the row out to it.

use std::ops::Range;

use super::LiveDoc;
use super::commands::wrapped_by;
use crate::doc::{EditKind, Selection};
use crate::projection::{Cell, Projection};
use crate::table::{cell_change, cell_source, row_insertion, toggled_marker};

/// The keys today's table widget takes while a cell has the caret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableKey {
    Left,
    Right,
    Up,
    Down,
    Tab,
    ShiftTab,
    Enter,
    Escape,
}

/// A missing cell holding the caret: its row's end and its column. It holds
/// only while the caret stays there and the text is unchanged.
#[derive(Clone, Copy, Debug)]
pub(super) struct Missing {
    at: usize,
    col: usize,
    revision: u64,
}

/// The point after the first padding space of empty cell `col` in an inserted
/// row: each empty cell is written `  |`.
fn col_offset(col: usize) -> usize {
    1 + col * 3
}

fn odd_backslashes(text: &str) -> bool {
    text.bytes().rev().take_while(|b| *b == b'\\').count() % 2 == 1
}

impl LiveDoc {
    /// The missing cell holding the caret, as (row end, column).
    pub(super) fn missing(&self) -> Option<(usize, usize)> {
        let m = self.missing?;
        (m.revision == self.doc.revision() && self.doc.selection() == Selection::caret(m.at))
            .then_some((m.at, m.col))
    }

    /// Tables are drawn but take no caret and no edit, as in a read-only
    /// document today. A caret in a cell moves off the table.
    pub fn set_read_only(&mut self, read_only: bool) {
        if self.read_only == read_only {
            return;
        }
        self.read_only = read_only;
        let head = self.doc.selection().head;
        if read_only && self.here().cell_at(head).is_some() {
            self.set_caret(head);
        }
    }

    /// Puts the caret in a short row's missing cell (a click on it).
    pub fn set_caret_in_missing(&mut self, at: usize, col: usize) {
        if self.read_only {
            return self.set_caret(at);
        }
        self.cell_typed_end = None;
        self.commit_composition();
        self.doc.set_selection(Selection::caret(at));
        self.missing = Some(Missing {
            at,
            col,
            revision: self.doc.revision(),
        });
    }

    fn go(&mut self, cell: &Cell, end: bool) {
        if cell.missing {
            self.set_caret_in_missing(cell.from, cell.col);
        } else {
            self.set_caret(if end { cell.to } else { cell.from });
        }
    }

    /// Fills the caret's short row out to its missing cell, which becomes an
    /// empty cell holding the caret, as the first keystroke into it does
    /// today. Returns the caret.
    pub(super) fn fill_missing(&mut self, kind: EditKind) -> Option<usize> {
        let (at, col) = self.missing()?;
        let p = self.here();
        let (t, _, cell) = p.cell_at(at)?;
        let (range, insert) = cell_change(&p.tables[t].layout, cell.row, col, "")?;
        // Written `  |`: the caret goes after the first space, as in any empty cell.
        let caret = range.start + insert.len() - 2;
        self.doc.replace(range, &insert, caret, kind);
        Some(caret)
    }

    /// Typing that starts and ends in one cell writes that cell. False when
    /// `range` is not inside a single cell.
    pub(super) fn type_in_cell(&mut self, range: Range<usize>, text: &str, kind: EditKind) -> bool {
        let range = self.fill_missing(kind).map_or(range, |at| at..at);
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
        let src = self.doc.text();
        // The space that kept a trailing backslash off the pipe was written
        // with it, and goes with it.
        let mut end = limit;
        if odd_backslashes(&src[from..limit]) && src.as_bytes().get(limit) == Some(&b' ') {
            end += 1;
        }
        let next = src.as_bytes().get(end).copied();
        let suffix = if odd_backslashes(&normalized) && next == Some(b'|') {
            " "
        } else {
            ""
        };
        self.doc
            .replace(from..end, &format!("{normalized}{suffix}"), caret, kind);
        caret
    }

    /// Backspace in a cell: within the cell's text, or removing an empty row
    /// from an empty cell. True when the caret is in a cell.
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
        } else if cell.from == cell.limit
            && let Some(line) = empty_row(p, t, cell.row)
        {
            // Above the removed line, so its offsets hold.
            let above = p.tables[t]
                .cells
                .iter()
                .find(|c| c.row == cell.row - 1 && c.col == cell.col)
                .cloned();
            self.doc
                .replace(line.clone(), "", line.start, EditKind::Other);
            match above {
                Some(above) => self.go(&above, true),
                None => self.settle(),
            }
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
        self.table_key(TableKey::Enter)
    }

    /// A key with the caret in a cell, as today's widget takes it. Left and
    /// Right inside a cell's text, or with a selection, are the editor's;
    /// from outside, a step that lands in a table enters it as `enterTable`
    /// does. False when the key is not the table's.
    pub fn table_key(&mut self, key: TableKey) -> bool {
        if self.read_only {
            return false;
        }
        let sel = self.doc.selection();
        let p = self.here();
        let Some((t, _, cell)) = p.cell_at(sel.head) else {
            let forward = key == TableKey::Right;
            return matches!(key, TableKey::Left | TableKey::Right)
                && sel.is_empty()
                && self
                    .next_stop(sel.head, forward)
                    .is_some_and(|to| self.enter_table(to, forward));
        };
        let cell = cell.clone();
        let table = &p.tables[t];
        let src = table.src.clone();
        let (row, col) = (cell.row, cell.col);
        let last_row = table.layout.rows.len() - 1;
        let last_col = table.cells.iter().filter(|c| c.row == 0).count() - 1;
        let at = |r: usize, c: usize| table.cells.iter().find(|x| x.row == r && x.col == c);
        let edge = sel.is_empty()
            && sel.head
                == if key == TableKey::Left {
                    cell.from
                } else {
                    cell.limit
                };
        let (target, end) = match key {
            TableKey::Left | TableKey::Right if !edge => return false,
            TableKey::Tab | TableKey::Right if col < last_col => (at(row, col + 1), false),
            TableKey::Tab | TableKey::Right if row < last_row => (at(row + 1, 0), false),
            TableKey::ShiftTab | TableKey::Left if col > 0 => (at(row, col - 1), true),
            TableKey::ShiftTab | TableKey::Left if row > 0 => (at(row - 1, last_col), true),
            TableKey::Up if row > 0 => (at(row - 1, col), true),
            TableKey::Down if row < last_row => (at(row + 1, col), true),
            TableKey::Enter if row < last_row => (at(row + 1, col), false),
            TableKey::Tab | TableKey::Enter => {
                let new_col = if key == TableKey::Tab { 0 } else { col };
                let (at, insert) = row_insertion(&table.layout, row);
                self.cell_typed_end = None;
                self.doc.replace(
                    at..at,
                    &insert,
                    at + 2 + col_offset(new_col),
                    EditKind::Other,
                );
                self.settle();
                return true;
            }
            TableKey::Right | TableKey::Down | TableKey::Escape => {
                self.exit_table(src, true);
                return true;
            }
            TableKey::Left | TableKey::ShiftTab | TableKey::Up => {
                self.exit_table(src, false);
                return true;
            }
        };
        if let Some(target) = target.cloned() {
            self.go(&target, end);
        }
        true
    }

    /// The caret arriving at `pos` from outside a table enters it as today's
    /// `enterTable`: at the first cell's start going forward, at the last
    /// cell's end going back. A read-only table is crossed whole, as the
    /// caret passes any drawn block. False when `pos` is in no table.
    pub fn enter_table(&mut self, pos: usize, forward: bool) -> bool {
        let head = self.doc.selection().head;
        let p = self.near(Selection::caret(pos), pos..pos);
        let inside = |t: &Range<usize>, at: usize| t.start <= at && at <= t.end;
        let Some(table) = p.tables.iter().find(|t| inside(&t.src, pos)) else {
            return false;
        };
        if inside(&table.src, head) {
            return false;
        }
        if table.read_only {
            let edge = if forward {
                table.src.end
            } else {
                table.src.start
            };
            if let Some(to) = self.next_stop(edge, forward) {
                self.set_caret(to);
            }
            return true;
        }
        let cell = if forward {
            table.cells.first()
        } else {
            table.cells.last()
        };
        match cell.cloned() {
            Some(cell) => {
                self.go(&cell, !forward);
                true
            }
            None => false,
        }
    }

    /// Leaves the table for the text above or below it, as today's
    /// `exitTable`. A line of text directly under a table would be read as
    /// another row, so going down lands after a blank line, added if the
    /// document has none there.
    fn exit_table(&mut self, table: Range<usize>, forward: bool) {
        self.cell_typed_end = None;
        let (from, to) = (table.start, table.end);
        if !forward {
            if from == 0 {
                self.doc.replace(0..0, "\n", 0, EditKind::Other);
            } else {
                // The line above's end; never snapped forward into the table.
                let pos = if self.is_stop(from - 1) {
                    from - 1
                } else {
                    self.next_stop(from - 1, false).unwrap_or(from - 1)
                };
                self.set_caret(pos);
            }
            return;
        }
        let src = self.doc.text().as_bytes();
        let insert = match &src[to..(to + 2).min(src.len())] {
            b"\n\n" => None,
            // The document ends after the table's line break.
            b"\n" => Some((to + 1, "\n")),
            b"" => Some((to, "\n\n")),
            _ => Some((to + 1, "\n\n")),
        };
        match insert {
            None => self.set_caret(to + 2),
            Some((at, text)) => {
                self.doc.replace(at..at, text, to + 2, EditKind::Other);
                self.settle();
            }
        }
    }

    /// Ctrl+B and Ctrl+I in a cell: the selected text wrapped in `marker`, or
    /// unwrapped, and left selected, as today; with nothing
    /// selected the caret goes between the markers. False outside a cell.
    pub fn toggle_marker_in_cell(&mut self, marker: &str) -> bool {
        if self.read_only {
            return false;
        }
        let range = self.doc.selection().range();
        let p = self.here();
        let Some((_, _, cell)) = p.cell_at(range.start) else {
            return false;
        };
        let range = match self.fill_missing(EditKind::Typing) {
            Some(at) => at..at,
            None if range.end <= cell.limit => range,
            None => return false,
        };
        let p = self.here();
        let Some((_, _, cell)) = p.cell_at(range.start) else {
            return false;
        };
        let src = self.doc.text();
        // Markers just outside the selection, as prose's toggle: the same key
        // again after a wrap, with only the text selected, takes them off.
        let size = marker.len();
        let (range, result) = if wrapped_by(src, cell.from..cell.limit, &range, marker) {
            let text = src[range.clone()].to_string();
            (range.start - size..range.end + size, text)
        } else {
            let text = toggled_marker(&src[range.clone()], marker);
            (range, text)
        };
        let text = &src[range.clone()];
        let unwrapped = result.len() < text.len();
        let new = format!(
            "{}{}{}",
            &src[cell.from..range.start],
            result,
            &src[range.end..cell.limit]
        );
        // Where the text lands once normalized: a non-breaking space before it
        // is written as a plain one, a byte shorter.
        let at = range.start - cell.from;
        let (lead, kept) = if unwrapped {
            (at, result.as_str())
        } else {
            (at + marker.len(), text)
        };
        let start = cell.from + cell_source(&new[..lead]).len();
        let len = cell_source(kept).len();
        self.write_cell(
            cell.from,
            cell.limit,
            &new,
            at + result.len(),
            EditKind::Typing,
        );
        self.cell_typed_end = None;
        self.doc.set_selection(Selection {
            anchor: start,
            head: start + len,
        });
        true
    }
}

/// The source line of an empty body row, with the line break before it.
fn empty_row(p: &Projection, table: usize, row: usize) -> Option<Range<usize>> {
    let span = p.tables[table].layout.rows.get(row)?;
    (row > 0 && span.cells.iter().all(|c| c.to == c.from)).then(|| span.from - 1..span.to)
}
