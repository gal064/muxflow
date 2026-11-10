//! Editing behaviour over the source-is-truth document, with no GPUI types:
//! caret motion across hidden ranges, typing, table cells, IME preedit, undo.
//! Everything here is unit-tested; the GPUI view only feeds it events.

use std::ops::Range;

use markdown::mdast::Node;
use unicode_segmentation::UnicodeSegmentation;

use crate::doc::{Document, EditKind, Selection};
use crate::projection::{Active, BlockKind, Projection, parse, project};
use crate::table::{cell_source, row_insertion};

pub struct LiveDoc {
    pub doc: Document,
    ast: Node,
    ast_revision: u64,
    pub focused: bool,
    marked: Option<Range<usize>>,
    cell_typed_end: Option<usize>,
}

impl LiveDoc {
    pub fn new(text: impl Into<String>) -> Self {
        let doc = Document::new(text);
        let ast = parse(doc.text());
        Self { ast_revision: doc.revision(), doc, ast, focused: true, marked: None, cell_typed_end: None }
    }

    pub fn text(&self) -> &str {
        self.doc.text()
    }

    pub fn selection(&self) -> Selection {
        self.doc.selection()
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub fn projection(&mut self) -> Projection {
        self.refresh();
        self.project_for(self.doc.selection())
    }

    fn refresh(&mut self) {
        if self.ast_revision != self.doc.revision() {
            self.ast = parse(self.doc.text());
            self.ast_revision = self.doc.revision();
        }
    }

    /// The projection a given caret/selection would produce.
    fn project_for(&self, sel: Selection) -> Projection {
        let active = Active {
            ranges: if self.focused { vec![sel.range()] } else { Vec::new() },
            marked: self.marked.clone(),
            cell_typed_end: self.cell_typed_end,
        };
        project(self.doc.text(), &self.ast, &active)
    }

    /// After an edit: re-parse and keep the caret off hidden or atomic markup.
    fn settle(&mut self) {
        self.refresh();
        let sel = self.doc.selection();
        if sel.is_empty() && !self.project_for(sel).is_stop(sel.head) {
            let pos = self.snap(sel.head);
            self.doc.set_selection_coalescing(Selection::caret(pos));
        }
    }

    fn next_boundary(&self, pos: usize) -> Option<usize> {
        self.doc.text()[pos..].graphemes(true).next().map(|g| pos + g.len())
    }

    fn prev_boundary(&self, pos: usize) -> Option<usize> {
        self.doc.text()[..pos].graphemes(true).next_back().map(|g| pos - g.len())
    }

    /// The next position the caret can stop at, walking past hidden and
    /// atomic source stretches. Reveal depends on the caret, so a candidate is
    /// judged under the projection it would itself produce.
    pub fn next_stop(&mut self, from: usize, forward: bool) -> Option<usize> {
        self.refresh();
        let step = |s: &Self, p| if forward { s.next_boundary(p) } else { s.prev_boundary(p) };
        let mut pos = step(self, from)?;
        loop {
            let p = self.project_for(Selection::caret(pos));
            if p.is_stop(pos) {
                return Some(pos);
            }
            if let Some(next) = p.jump_over(pos, forward) {
                pos = next;
                continue;
            }
            pos = step(self, pos)?;
        }
    }

    /// `pos` if it is a stop, else the nearest stop forward, else backward.
    pub fn snap(&mut self, pos: usize) -> usize {
        self.refresh();
        if self.project_for(Selection::caret(pos)).is_stop(pos) {
            return pos;
        }
        self.next_stop(pos, true).or_else(|| self.next_stop(pos, false)).unwrap_or(pos)
    }

    pub fn set_caret(&mut self, pos: usize) {
        self.cell_typed_end = None;
        let pos = self.snap(pos);
        self.commit_composition();
        self.doc.set_selection(Selection::caret(pos));
    }

    /// Extend the selection to `pos` (a pointer drag: direction follows the anchor).
    pub fn set_head(&mut self, pos: usize) {
        let forward = pos >= self.doc.selection().anchor;
        self.extend_to(pos, forward);
    }

    /// Extend the selection. It cannot cross a cell boundary from inside a cell.
    /// From outside a table it takes the table whole, as today's widget does,
    /// leaving the head on the stop just past the table in the travel direction.
    pub fn extend_to(&mut self, pos: usize, forward: bool) {
        self.cell_typed_end = None;
        let pos = self.snap(pos);
        let anchor = self.doc.selection().anchor;
        let p = self.project_for(Selection::caret(anchor));
        let pos = if let Some((_, _, cell)) = p.cell_at(anchor) {
            pos.clamp(cell.from, cell.limit)
        } else if let Some(table) = p.tables.iter().find(|t| t.src.contains(&pos)) {
            if forward {
                self.next_stop(table.src.end, true).unwrap_or(table.src.end)
            } else {
                self.next_stop(table.src.start, false).unwrap_or(table.src.start)
            }
        } else {
            pos
        };
        self.commit_composition();
        self.doc.set_selection(Selection { anchor, head: pos });
    }

    pub fn move_horizontal(&mut self, forward: bool, extend: bool) {
        let sel = self.doc.selection();
        if !extend && !sel.is_empty() {
            let range = sel.range();
            self.set_caret(if forward { range.end } else { range.start });
            return;
        }
        if let Some(next) = self.next_stop(sel.head, forward) {
            if extend { self.extend_to(next, forward) } else { self.set_caret(next) }
        }
    }

    pub fn move_line_edge(&mut self, end: bool, extend: bool) {
        let head = self.doc.selection().head;
        let text = self.doc.text();
        let line_start = text[..head].rfind('\n').map_or(0, |i| i + 1);
        let line_end = text[head..].find('\n').map_or(text.len(), |i| head + i);
        let target = if end { line_end } else { line_start };
        let p = self.projection();
        let cell = p.cell_at(head).map(|(_, _, c)| (c.from, c.limit));
        let target = match cell {
            Some((from, limit)) => if end { limit } else { from },
            None => self.snap(target),
        };
        if extend { self.set_head(target) } else { self.set_caret(target) }
    }

    pub fn select_all(&mut self) {
        self.cell_typed_end = None;
        let len = self.doc.text().len();
        self.commit_composition();
        self.doc.set_selection(Selection { anchor: 0, head: len });
    }

    pub fn commit_composition(&mut self) {
        if self.marked.take().is_some() {
            self.doc.end_undo_group();
        }
    }

    /// Replace the preedit (or selection) with `text` and mark it, the way a
    /// platform IME composes. `caret_in_text` is the composition caret as a
    /// byte offset within `text`.
    pub fn set_preedit(&mut self, text: &str, caret_in_text: Option<usize>) {
        self.refresh();
        let sel = self.doc.selection();
        let range = self.marked.clone().unwrap_or(sel.range());
        let start = range.start;
        // A pipe in a preedit must not split the cell it is composed in.
        let in_cell = self.project_for(Selection::caret(start)).cell_at(start).is_some();
        let escaped = in_cell.then(|| cell_source(text));
        let text = escaped.as_deref().unwrap_or(text);
        let caret = start + caret_in_text.unwrap_or(text.len()).min(text.len());
        self.doc.replace(range, text, caret, EditKind::Composition);
        self.marked = (!text.is_empty()).then(|| start..start + text.len());
        self.refresh();
    }

    /// Type text at the caret, through the same path as the platform's input
    /// handler. Inside a table cell it is written as the cell writes it.
    pub fn insert_text(&mut self, text: &str) {
        self.refresh();
        let composing = self.marked.is_some();
        let marked = self.marked.take();
        let sel = self.doc.selection();
        let range = marked.unwrap_or(sel.range());
        let kind = if composing { EditKind::Composition } else { EditKind::Typing };
        let p = self.project_for(Selection::caret(range.start));
        let cell = p.cell_at(range.start).map(|c| (c.2.from, c.2.limit));
        let same_cell = cell.is_some() && p.cell_at(range.end).map(|c| (c.2.from, c.2.limit)) == cell;
        if let (true, Some((from, limit))) = (same_cell, cell) {
            let src = self.doc.text();
            let new = format!("{}{}{}", &src[from..range.start], text, &src[range.end..limit]);
            let caret_in_new = range.start - from + text.len();
            let caret = self.write_cell(from, limit, &new, caret_in_new, kind);
            self.cell_typed_end = text.ends_with(' ').then_some(caret);
        } else {
            let caret = range.start + text.len();
            self.doc.replace(range, text, caret, kind);
        }
        if composing {
            self.doc.end_undo_group();
        }
        self.settle();
    }

    /// Rewrite a cell through today's whole-cell normalization (`cellSource`),
    /// so no edit can leave an unescaped pipe or a backslash against the closing
    /// pipe. Returns the caret.
    fn write_cell(&mut self, from: usize, limit: usize, new: &str, caret_in_new: usize, kind: EditKind) -> usize {
        let normalized = cell_source(new);
        let caret = from + cell_source(&new[..caret_in_new]).len();
        let slashes = normalized.bytes().rev().take_while(|b| *b == b'\\').count();
        let next = self.doc.text().as_bytes().get(limit).copied();
        let suffix = if slashes % 2 == 1 && next == Some(b'|') { " " } else { "" };
        self.doc.replace(from..limit, &format!("{normalized}{suffix}"), caret, kind);
        caret
    }

    fn cell_ops(&mut self) -> Option<(usize, usize, usize, usize)> {
        let caret = self.doc.selection().head;
        let p = self.projection();
        let (t, _c, cell) = p.cell_at(caret)?;
        let table = &p.tables[t];
        Some((t, cell.row, cell.col, table.layout.rows.len() - 1))
    }

    pub fn delete_selection(&mut self) {
        let sel = self.doc.selection();
        if !sel.is_empty() {
            let range = sel.range();
            let caret = range.start;
            self.doc.replace(range, "", caret, EditKind::Deletion);
            self.settle();
        }
    }

    pub fn backspace(&mut self) {
        self.commit_composition();
        self.refresh();
        let sel = self.doc.selection();
        if !sel.is_empty() {
            return self.delete_selection();
        }
        let caret = sel.head;
        if caret == 0 {
            return;
        }
        let p = self.projection();
        if let Some((t, _, cell)) = p.cell_at(caret) {
            if caret > cell.from {
                let prev = self.prev_boundary(caret).unwrap_or(cell.from).max(cell.from);
                let src = self.doc.text();
                let new = format!("{}{}", &src[cell.from..prev], &src[caret..cell.limit]);
                let at = self.write_cell(cell.from, cell.limit, &new, prev - cell.from, EditKind::Deletion);
                self.cell_typed_end = self.cell_typed_end.map(|_| at);
                self.settle();
            } else if let Some(row) = self.empty_row(&p, t, cell.row) {
                let line = row.clone();
                let up = cell.row - 1;
                let col = cell.col;
                let new_caret = p.tables[t].cells.iter().find(|c| c.row == up && c.col == col).map_or(line.start, |c| c.to);
                self.doc.replace(line, "", new_caret, EditKind::Other);
                self.settle();
            }
            return;
        }
        if let Some(atom) = p.atomic_before(caret) {
            let start = atom.start;
            self.doc.replace(atom, "", start, EditKind::Deletion);
            self.settle();
            return;
        }
        let prev = self.prev_boundary(caret).unwrap_or(0);
        self.doc.replace(prev..caret, "", prev, EditKind::Deletion);
        self.settle();
    }

    fn empty_row(&self, p: &Projection, table: usize, row: usize) -> Option<Range<usize>> {
        let t = &p.tables[table];
        let span = t.layout.rows.get(row)?;
        (row > 0 && span.cells.iter().all(|c| c.to == c.from)).then(|| span.from - 1..span.to)
    }

    pub fn delete_forward(&mut self) {
        self.commit_composition();
        self.refresh();
        let sel = self.doc.selection();
        if !sel.is_empty() {
            return self.delete_selection();
        }
        let caret = sel.head;
        let p = self.projection();
        if let Some((_, _, cell)) = p.cell_at(caret) {
            if caret < cell.limit {
                let next = self.next_boundary(caret).unwrap_or(cell.limit).min(cell.limit);
                let src = self.doc.text();
                let new = format!("{}{}", &src[cell.from..caret], &src[next..cell.limit]);
                self.write_cell(cell.from, cell.limit, &new, caret - cell.from, EditKind::Deletion);
                self.settle();
            }
            return;
        }
        if let Some(atom) = p.atomic_after(caret) {
            self.doc.replace(atom, "", caret, EditKind::Deletion);
            self.settle();
            return;
        }
        let Some(next) = self.next_boundary(caret) else { return };
        self.doc.replace(caret..next, "", caret, EditKind::Deletion);
        self.settle();
    }

    /// Up or Down inside a table goes by cell, not by pixels: a wrapped neighbour
    /// must not pull the caret out of its column. False at the table's edge.
    pub fn cell_vertical(&mut self, down: bool) -> bool {
        let Some((t, row, col, last_row)) = self.cell_ops() else { return false };
        if (down && row == last_row) || (!down && row == 0) {
            return false;
        }
        let to = if down { row + 1 } else { row - 1 };
        let p = self.projection();
        let cells = &p.tables[t].cells;
        let target = cells
            .iter()
            .find(|c| c.row == to && c.col == col)
            .or_else(|| cells.iter().filter(|c| c.row == to).next_back())
            .map(|c| c.to);
        match target {
            Some(at) => {
                self.set_caret(at);
                true
            }
            None => false,
        }
    }

    /// Tab, Enter and friends inside a table.
    pub fn cell_navigate(&mut self, dir: CellDir) -> bool {
        let Some((t, row, col, last_row)) = self.cell_ops() else { return false };
        let p = self.projection();
        let table = &p.tables[t];
        let target = match dir {
            CellDir::Next => {
                let same_row = table.cells.iter().find(|c| c.row == row && c.col == col + 1);
                same_row.or_else(|| table.cells.iter().find(|c| c.row == row + 1 && c.col == 0))
            }
            CellDir::Prev => {
                if col > 0 {
                    table.cells.iter().find(|c| c.row == row && c.col == col - 1)
                } else if row > 0 {
                    table.cells.iter().filter(|c| c.row == row - 1).next_back()
                } else {
                    None
                }
            }
            CellDir::Down => table
                .cells
                .iter()
                .find(|c| c.row == row + 1 && c.col == col)
                .or_else(|| table.cells.iter().filter(|c| c.row == row + 1).next_back()),
        };
        if let Some(cell) = target {
            let at = if matches!(dir, CellDir::Prev) { cell.to } else { cell.from };
            self.set_caret(at);
            return true;
        }
        if matches!(dir, CellDir::Next | CellDir::Down) && row == last_row {
            let (at, insert) = row_insertion(&table.layout, row);
            // The new row's first cell (or same column) is a point after the
            // first padding space of the inserted row.
            let new_caret = at + 2 + col_offset(if matches!(dir, CellDir::Down) { col } else { 0 });
            self.doc.replace(at..at, &insert, new_caret, EditKind::Other);
            self.settle();
            return true;
        }
        false
    }

    /// Enter: a new row in a table, a continued item in a list, a new paragraph.
    pub fn enter(&mut self) {
        self.refresh();
        self.commit_composition();
        if self.cell_ops().is_some() {
            // Never a newline: that would split the row.
            self.cell_navigate(CellDir::Down);
            return;
        }
        self.delete_selection();
        let caret = self.doc.selection().head;
        let p = self.projection();
        let block = p.locate(caret).and_then(|(leaf, _)| {
            p.blocks.iter().find(|b| b.leaves.contains(&leaf)).cloned()
        });
        match block.map(|b| (b.kind, b.quote, b.src)) {
            Some((BlockKind::ListItem { .. }, _, src)) => self.continue_list(src, caret),
            Some((BlockKind::Paragraph, quote, _)) if quote > 0 => {
                self.insert_plain(caret, "\n>\n> ");
            }
            Some((BlockKind::Paragraph, _, _)) => self.insert_plain(caret, "\n\n"),
            _ => self.insert_plain(caret, "\n"),
        }
    }

    fn insert_plain(&mut self, at: usize, text: &str) {
        self.doc.replace(at..at, text, at + text.len(), EditKind::Other);
        self.settle();
    }

    fn continue_list(&mut self, item: Range<usize>, caret: usize) {
        let text = self.doc.text();
        // The whole line, so indentation and quote markers carry over.
        let from = text[..item.start].rfind('\n').map_or(0, |i| i + 1);
        let line = &text[from..text[item.start..].find('\n').map_or(text.len(), |i| item.start + i)];
        let Some(marker) = list_marker(line) else {
            self.insert_plain(caret, "\n");
            return;
        };
        let content = &line[marker.len..];
        if content.trim().is_empty() {
            let from = item.start;
            self.doc.replace(from..caret, "", from, EditKind::Other);
            self.settle();
            return;
        }
        let next = format!("\n{}", marker.next());
        self.insert_plain(caret, &next);
    }

    pub fn undo(&mut self) {
        self.cell_typed_end = None;
        self.commit_composition();
        self.doc.undo();
        self.settle();
    }

    pub fn redo(&mut self) {
        self.cell_typed_end = None;
        self.commit_composition();
        self.doc.redo();
        self.settle();
    }

    /// The bytes to write on save: the source, unmodified.
    pub fn save_bytes(&self) -> Vec<u8> {
        self.doc.text().as_bytes().to_vec()
    }
}

fn col_offset(col: usize) -> usize {
    // Each empty inserted cell is "  |": the point after its first space.
    1 + col * 3
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellDir {
    Next,
    Prev,
    Down,
}

struct ListMarker<'a> {
    indent: &'a str,
    bullet: &'a str,
    number: Option<u32>,
    delim: &'a str,
    spacing: &'a str,
    task: bool,
    len: usize,
}

impl ListMarker<'_> {
    fn next(&self) -> String {
        let bullet = match self.number {
            Some(n) => format!("{}{}", n + 1, self.delim),
            None => self.bullet.to_string(),
        };
        format!("{}{}{}{}", self.indent, bullet, self.spacing, if self.task { "[ ] " } else { "" })
    }
}

fn list_marker(line: &str) -> Option<ListMarker<'_>> {
    let indent_len = line.len() - line.trim_start_matches([' ', '\t', '>']).len();
    let (indent, rest) = line.split_at(indent_len);
    let (bullet, after) = if rest.starts_with(['-', '*', '+']) {
        rest.split_at(1)
    } else {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || !rest[digits..].starts_with(['.', ')']) {
            return None;
        }
        rest.split_at(digits + 1)
    };
    let number = bullet.strip_suffix(['.', ')']).and_then(|n| n.parse().ok());
    let delim = if number.is_some() { &bullet[bullet.len() - 1..] } else { "" };
    let spacing_len = after.len() - after.trim_start_matches([' ', '\t']).len();
    if spacing_len == 0 && !after.is_empty() {
        return None;
    }
    let (spacing, after) = after.split_at(spacing_len);
    let task = after.starts_with("[ ] ") || after.starts_with("[x] ") || after.starts_with("[X] ");
    let len = indent.len() + bullet.len() + spacing.len() + if task { 4 } else { 0 };
    Some(ListMarker { indent, bullet, number, delim, spacing, task, len })
}


#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str, caret: usize) -> LiveDoc {
        let mut d = LiveDoc::new(text);
        d.doc.set_selection(Selection::caret(caret));
        d.refresh();
        d
    }

    fn table_cells(d: &mut LiveDoc) -> usize {
        d.projection().tables[0].cells.len()
    }

    fn head(d: &LiveDoc) -> usize {
        d.selection().head
    }

    #[test]
    fn right_walks_into_and_out_of_revealed_markers_without_skipping_text() {
        let text = "ab **cd** ef";
        let mut d = doc(text, 2);
        let mut seen = vec![head(&d)];
        for _ in 0..12 {
            d.move_horizontal(true, false);
            seen.push(head(&d));
        }
        // Every source offset is reached: when the caret touches the construct
        // its markers are revealed and are ordinary characters.
        assert_eq!(seen, vec![2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 12, 12]);
        let mut d = doc(text, text.len());
        for _ in 0..10 {
            d.move_horizontal(false, false);
        }
        assert_eq!(head(&d), 2);
    }

    #[test]
    fn caret_never_rests_inside_a_hidden_range_it_does_not_reveal() {
        // A bullet and a hidden heading prefix on inactive lines are atomic.
        let text = "- one\n- two";
        let mut d = doc(text, 2);
        // Start of the second item's text, then left: over the bullet.
        d.set_caret(8);
        assert_eq!(head(&d), 8);
        d.move_horizontal(false, false);
        assert_eq!(head(&d), 5, "left from item text goes to the previous line's end");
        // A click on the bullet lands on the text.
        d.set_caret(6);
        assert_eq!(head(&d), 8);
    }

    #[test]
    fn soft_break_is_one_caret_stop_on_each_side() {
        let text = "one\ntwo";
        let mut d = doc(text, 3);
        d.move_horizontal(true, false);
        assert_eq!(head(&d), 4);
        d.move_horizontal(false, false);
        assert_eq!(head(&d), 3);
    }

    #[test]
    fn typing_at_a_revealed_marker_edits_the_source() {
        let mut d = doc("**bold**", 8);
        d.insert_text("x");
        assert_eq!(d.text(), "**bold**x");
    }

    #[test]
    fn backspace_deletes_atomic_markers_whole() {
        // Mirrors liveMarkdown.test.ts "deletes a block marker whole".
        let mut d = doc("- [ ] task", 6);
        d.backspace();
        assert_eq!(d.text(), "- task");
        d.backspace();
        assert_eq!(d.text(), "task");
    }

    #[test]
    fn backspace_across_a_soft_break_joins_lines_in_the_file() {
        let text = "one two\nthree four\n\nnext";
        let mut d = doc(text, 8);
        d.backspace();
        assert_eq!(d.text(), "one twothree four\n\nnext");
    }

    #[test]
    fn enter_continues_lists_and_starts_paragraphs() {
        let mut d = doc("- one", 5);
        d.enter();
        assert_eq!(d.text(), "- one\n- ");
        let mut d = doc("  9) nine", 9);
        d.enter();
        assert_eq!(d.text(), "  9) nine\n  10) ");
        let mut d = doc("- [x] done", 10);
        d.enter();
        assert_eq!(d.text(), "- [x] done\n- [ ] ");
        let mut d = doc("Hello", 5);
        d.enter();
        assert_eq!(d.text(), "Hello\n\n");
        let mut d = doc("> quoted", 8);
        d.enter();
        assert_eq!(d.text(), "> quoted\n>\n> ");
        let mut d = doc("- one\n- ", 8);
        d.enter();
        assert_eq!(d.text(), "- one\n");
    }

    #[test]
    fn enter_keeps_nesting_and_quote_prefix() {
        let mut d = doc("- one\n  - nested", 16);
        d.enter();
        assert_eq!(d.text(), "- one\n  - nested\n  - ");
        let mut d = doc("> - a", 5);
        d.enter();
        assert_eq!(d.text(), "> - a\n> - ");
    }

    #[test]
    fn caret_is_kept_off_markup_an_edit_creates() {
        let mut d = doc("see - note", 4);
        d.enter();
        assert_eq!(d.text(), "see \n\n- note");
        let head = d.selection().head;
        assert!(d.projection().is_stop(head));
    }

    #[test]
    fn delete_takes_a_soft_break_whole() {
        let mut d = doc("> one\n> two", 5);
        d.delete_forward();
        assert_eq!(d.text(), "> onetwo");
    }

    #[test]
    fn extending_selects_across_hidden_markers_in_both_directions() {
        let text = "aa **bb** cc";
        let mut d = doc(text, 1);
        for _ in 0..4 {
            d.move_horizontal(true, true);
        }
        assert_eq!(d.selection().range(), 1..5);
        d.move_horizontal(false, false);
        assert_eq!(head(&d), 1, "Left collapses to the selection start");
        let mut d = doc(text, text.len());
        d.move_horizontal(false, true);
        assert_eq!(d.selection().range(), text.len() - 1..text.len());
    }

    #[test]
    fn a_selection_from_a_cell_stays_in_the_cell() {
        let text = "| ab | cd |\n|---|---|\n";
        let mut d = doc(text, text.find("ab").unwrap() + 1);
        for _ in 0..4 {
            d.move_horizontal(true, true);
        }
        let cd = text.find("ab").unwrap() + 2;
        assert_eq!(head(&d), cd);
        d.insert_text("x");
        assert_eq!(d.text(), "| ax | cd |\n|---|---|\n");
    }

    #[test]
    fn a_selection_from_outside_takes_the_table_whole() {
        let text = "before\n\n| ab | cd |\n|---|---|\n\nafter";
        let mut d = doc(text, 0);
        let inside = text.find("cd").unwrap();
        d.set_head(inside);
        let end = text.find("\n\nafter").unwrap();
        assert_eq!(d.selection().range(), 0..end + 1, "the head rests on the stop past the table");
        assert!(d.projection().is_stop(head(&d)));
    }

    #[test]
    fn a_selection_over_a_table_retracts_with_the_keyboard() {
        let text = "b\n\n| a |\n|---|\n\nafter";
        let mut d = doc(text, 0);
        d.set_head(text.find('a').unwrap());
        let past = head(&d);
        d.move_horizontal(false, true);
        assert!(head(&d) < past);
        assert_eq!(head(&d), 2, "stops just before the table");
    }

    #[test]
    fn enter_in_a_cell_above_a_short_row_never_splits_it() {
        let text = "| a | b |\n|---|---|\n| 1 |\n| 3 | 4 |\n";
        let mut d = doc(text, text.find('b').unwrap());
        d.enter();
        assert_eq!(d.text(), text);
        assert_eq!(head(&d), text.find('1').unwrap());
    }

    #[test]
    fn cell_writes_keep_pipes_escaped_on_every_edit() {
        let text = "| 1 | 2 |\n|---|---|\n";
        let mut d = doc(text, text.find('1').unwrap() + 1);
        d.insert_text("\\");
        d.insert_text("|");
        assert_eq!(table_cells(&mut d), 2, "typed backslash then pipe stays one cell: {}", d.text());
        let text = "| x\\|y | 2 |\n|---|---|\n";
        let mut d = doc(text, text.find('|').unwrap() + 3 + 1);
        d.backspace();
        assert_eq!(table_cells(&mut d), 2, "deleting the escape re-escapes: {}", d.text());
        assert!(d.text().contains("x\\|y"));
    }

    #[test]
    fn a_pipe_in_a_preedit_stays_in_its_cell() {
        let text = "| 1 | 2 |\n|---|---|\n";
        let mut d = doc(text, text.find('1').unwrap() + 1);
        d.set_preedit("x|y", None);
        assert_eq!(table_cells(&mut d), 2);
        d.insert_text("x|y");
        assert_eq!(table_cells(&mut d), 2);
        assert_eq!(d.text(), "| 1x\\|y | 2 |\n|---|---|\n");
    }

    #[test]
    fn home_and_end_stay_in_the_cell() {
        let text = "| ab     | cd |\n|---|---|\n| 1 | 2 |\n";
        let a = text.find("ab").unwrap();
        let mut d = doc(text, a + 1);
        d.move_line_edge(true, false);
        assert_eq!(head(&d), a + 2);
        d.move_line_edge(false, false);
        assert_eq!(head(&d), a);
    }

    #[test]
    fn padding_is_skipped_but_typed_spaces_are_caret_stops() {
        let text = "| ab     | cd |\n|---|---|\n";
        let a = text.find("ab").unwrap();
        let mut d = doc(text, a + 2);
        d.move_horizontal(true, false);
        assert_eq!(head(&d), text.find("cd").unwrap());
        let mut d = doc(text, a + 2);
        d.insert_text(" ");
        d.insert_text("x");
        assert_eq!(d.text(), "| ab x     | cd |\n|---|---|\n");
    }

    #[test]
    fn up_and_down_in_a_table_keep_the_column_and_stop_at_its_edges() {
        let text = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
        let mut d = doc(text, text.find('4').unwrap());
        assert!(d.cell_vertical(false));
        assert_eq!(head(&d), text.find('2').unwrap() + 1);
        assert!(d.cell_vertical(false));
        assert_eq!(head(&d), text.find('b').unwrap() + 1);
        assert!(!d.cell_vertical(false), "the top row falls through to pixel motion");
        let mut d = doc(text, text.find('4').unwrap());
        assert!(!d.cell_vertical(true));
    }

    #[test]
    fn cell_navigation_prev_and_down() {
        let text = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let two = text.find('2').unwrap();
        let mut d = doc(text, two);
        assert!(d.cell_navigate(CellDir::Prev));
        assert_eq!(head(&d), text.find('1').unwrap() + 1, "Shift-Tab lands at the end of the previous cell");
        let mut d = doc(text, text.find('b').unwrap());
        assert!(d.cell_navigate(CellDir::Down));
        assert_eq!(head(&d), two);
    }

    #[test]
    fn backspace_ends_a_composition_before_deleting() {
        let mut d = doc("abcdef", 1);
        d.set_preedit("x", None);
        d.backspace();
        assert_eq!(d.marked(), None);
        d.set_preedit("Q", None);
        assert_eq!(d.text(), "aQbcdef");
    }

    #[test]
    fn a_cell_edit_writes_only_that_cell_and_undoes() {
        let text = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let mut d = doc(text, text.find('2').unwrap());
        d.insert_text("x");
        assert_eq!(d.text(), "| a | b |\n|---|---|\n| 1 | x2 |\n");
        d.undo();
        assert_eq!(d.text(), text);
    }

    #[test]
    fn typing_a_pipe_in_a_cell_escapes_it() {
        let text = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let mut d = doc(text, text.find('1').unwrap() + 1);
        d.insert_text("|");
        assert_eq!(d.text(), "| a | b |\n|---|---|\n| 1\\| | 2 |\n");
    }

    #[test]
    fn a_trailing_backslash_stays_off_the_closing_pipe() {
        let text = "|a|\n|-|\n|b|\n";
        let mut d = doc(text, text.find('b').unwrap() + 1);
        d.insert_text("\\");
        assert_eq!(d.text(), "|a|\n|-|\n|b\\ |\n");
    }

    #[test]
    fn tab_moves_between_cells_and_adds_a_row_at_the_end() {
        let text = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let mut d = doc(text, text.find('2').unwrap());
        assert!(d.cell_navigate(CellDir::Next));
        assert_eq!(d.text(), "| a | b |\n|---|---|\n| 1 | 2 |\n|  |  |\n");
        d.insert_text("z");
        assert_eq!(d.text(), "| a | b |\n|---|---|\n| 1 | 2 |\n| z |  |\n");
    }

    #[test]
    fn backspace_in_an_empty_row_removes_it() {
        let text = "| a | b |\n|---|---|\n| 1 | 2 |\n|  |  |\n";
        let row = text.rfind("|  |").unwrap() + 1;
        let mut d = doc(text, row + 1);
        d.backspace();
        assert_eq!(d.text(), "| a | b |\n|---|---|\n| 1 | 2 |\n");
    }

    #[test]
    fn right_across_table_cells_skips_pipes_and_padding() {
        let text = "| ab | cd |\n|---|---|\n| 1 | 2 |\n";
        let mut d = doc(text, text.find("ab").unwrap());
        let cd = text.find("cd").unwrap();
        for _ in 0..2 {
            d.move_horizontal(true, false);
        }
        assert_eq!(head(&d), text.find("ab").unwrap() + 2);
        d.move_horizontal(true, false);
        assert_eq!(head(&d), cd, "from the end of one cell to the start of the next");
    }

    #[test]
    fn preedit_replaces_itself_commits_as_one_undo_and_can_be_cancelled() {
        let mut d = doc("ab", 1);
        d.set_preedit("に", None);
        assert_eq!(d.text(), "aにb");
        assert_eq!(d.marked(), Some(1..4));
        d.set_preedit("日本", None);
        assert_eq!(d.text(), "a日本b");
        d.insert_text("日本語");
        assert_eq!(d.text(), "a日本語b");
        assert_eq!(d.marked(), None);
        d.undo();
        assert_eq!(d.text(), "ab");
        // Cancelling removes the preedit.
        let mut d = doc("ab", 1);
        d.set_preedit("x", None);
        d.set_preedit("", None);
        assert_eq!(d.text(), "ab");
    }

    #[test]
    fn saved_bytes_are_the_source_and_edits_only_touch_their_range() {
        let text = "# T\n\n- *a*\n\n| x |\n|---|\n| y |\n";
        let mut d = doc(text, 0);
        assert_eq!(d.save_bytes(), text.as_bytes());
        // Move all around without editing: bytes untouched.
        d.set_caret(text.find('x').unwrap());
        d.move_horizontal(true, false);
        d.projection();
        assert_eq!(d.save_bytes(), text.as_bytes());
        d.insert_text("!");
        assert_eq!(String::from_utf8(d.save_bytes()).unwrap(), "# T\n\n- *a*\n\n| x! |\n|---|\n| y |\n");
    }

    #[test]
    fn selection_across_hidden_markers_replaces_source_including_them() {
        let text = "aa **bb** cc";
        let mut d = doc(text, 0);
        d.doc.set_selection(Selection { anchor: 3, head: 9 });
        d.insert_text("x");
        assert_eq!(d.text(), "aa x cc");
    }

    #[test]
    fn undo_restores_the_source_across_reveal_state() {
        // Edit while the caret reveals markers, then move away; undo goes back
        // to bytes, whatever is currently rendered.
        let mut d = doc("**bold** rest", 2);
        d.insert_text("x");
        assert_eq!(d.text(), "**xbold** rest");
        d.set_caret(13);
        d.undo();
        assert_eq!(d.text(), "**bold** rest");
        assert_eq!(head(&d), 2);
    }
}
