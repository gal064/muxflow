//! Editing behaviour over the source-is-truth document, with no GPUI types:
//! caret motion across hidden ranges, typing, Backspace and Delete across
//! markers, IME preedit, undo. Lists and tables have their own files.
//!
//! The tree is parsed again only around each change (`reparse`), and what is
//! drawn is projected again only where an edit or the selection reaches
//! (`drawn`). A question about one caret position (is it a stop, what marker
//! is before it) projects only the blocks around it.

mod changes;
mod commands;
mod drawn;
mod list;
mod motion;
mod table;

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use unicode_segmentation::UnicodeSegmentation;

use crate::doc::{Change, Document, EditKind, Selection};
use crate::projection::{Active, BlockKind, Projection, project, project_children, window};
use crate::reparse::Tree;

pub use drawn::Segment;

pub use table::TableKey;

pub struct LiveDoc {
    doc: Document,
    tree: Tree,
    /// The revision the tree was brought up to.
    parsed: u64,
    /// Markers reveal only while the editor has focus.
    pub focused: bool,
    /// The IME preedit, which lives in the source until it is committed.
    marked: Option<Range<usize>>,
    /// Just past whitespace typed at the end of a table cell.
    cell_typed_end: Option<usize>,
    /// The short row's missing cell the caret was put in.
    missing: Option<table::Missing>,
    /// Tables take no caret and no edit.
    read_only: bool,
    drawn: drawn::Drawn,
    /// For the next projected leaf: unique among everything drawn.
    next_key: u64,
    /// For the next segment an edit reaches.
    next_id: u64,
    /// Each wide table's sideways scroll in pixels, by where the table
    /// starts: the view's, kept here so it moves with its table on an edit.
    table_x: Rc<RefCell<HashMap<usize, f32>>>,
}

impl LiveDoc {
    pub fn new(text: impl Into<String>) -> Self {
        let doc = Document::new(text);
        Self {
            tree: Tree::new(doc.text()),
            parsed: doc.revision(),
            doc,
            focused: true,
            marked: None,
            cell_typed_end: None,
            missing: None,
            read_only: false,
            drawn: drawn::Drawn::default(),
            next_key: 1,
            next_id: 0,
            table_x: Rc::default(),
        }
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

    /// The text changed since the host last took it. Never while a
    /// composition is open: an uncommitted preedit is never the host's text.
    pub fn edited(&self) -> bool {
        self.marked.is_none() && self.doc.edited()
    }

    /// [`Self::edited`], and the host takes the text now.
    pub fn take_edited(&mut self) -> Option<Change> {
        if self.marked.is_some() {
            return None;
        }
        self.doc.take_edited()
    }

    /// The text changed outside the editor. An open composition is dropped
    /// with whatever it had typed.
    pub fn reset_text(&mut self, text: &str) {
        self.marked = None;
        self.cell_typed_end = None;
        self.doc.reset_text(text);
    }

    /// The whole document as it is drawn, projected afresh: for tests and
    /// tools. The view draws [`Self::segments`].
    pub fn projection(&mut self) -> Rc<Projection> {
        self.refresh();
        let active = self.active(self.doc.selection());
        Rc::new(project(self.doc.text(), self.tree.ast(), &active))
    }

    fn refresh(&mut self) {
        if self.parsed == self.doc.revision() {
            return;
        }
        self.parsed = self.doc.revision();
        if let Some(change) = self.doc.take_change() {
            let splice = self.tree.update(self.doc.text(), change);
            self.drawn.splice(&splice, change);
            let mut x = self.table_x.borrow_mut();
            if change.delta() != 0 && !x.is_empty() {
                *x = x.drain().map(|(at, x)| (change.map(at), x)).collect();
            }
        }
    }

    /// Each wide table's sideways scroll, by where the table starts, for the
    /// view to read and set. Edits move the keys with their tables.
    pub fn table_x(&self) -> Rc<RefCell<HashMap<usize, f32>>> {
        self.table_x.clone()
    }

    fn active(&self, sel: Selection) -> Active {
        Active {
            ranges: if self.focused {
                vec![sel.range()]
            } else {
                Vec::new()
            },
            marked: self.marked.clone(),
            cell_typed_end: self.cell_typed_end,
            missing: self.missing(),
            read_only: self.read_only,
        }
    }

    /// The blocks around `around`, as a selection `sel` would draw them.
    fn near(&mut self, sel: Selection, around: Range<usize>) -> Projection {
        self.refresh();
        let active = self.active(sel);
        let range = window(self.tree.root(), &around);
        let ast = self.tree.settled(range.clone());
        project_children(self.doc.text(), ast, &active, range)
    }

    /// The blocks around the caret, as they are drawn now.
    fn here(&mut self) -> Projection {
        let sel = self.doc.selection();
        self.near(sel, sel.range())
    }

    fn is_stop(&mut self, pos: usize) -> bool {
        self.near(Selection::caret(pos), pos..pos).is_stop(pos)
    }

    /// After an edit: keep the caret off hidden or atomic markup.
    fn settle(&mut self) {
        let sel = self.doc.selection();
        if sel.is_empty() && !self.is_stop(sel.head) {
            let pos = self.snap(sel.head);
            self.doc.set_selection_coalescing(Selection::caret(pos));
        }
    }

    fn next_boundary(&self, pos: usize) -> Option<usize> {
        self.doc.text()[pos..]
            .graphemes(true)
            .next()
            .map(|g| pos + g.len())
    }

    fn prev_boundary(&self, pos: usize) -> Option<usize> {
        self.doc.text()[..pos]
            .graphemes(true)
            .next_back()
            .map(|g| pos - g.len())
    }

    /// The next position the caret can stop at, walking past hidden and
    /// atomic source stretches. Reveal depends on the caret, so a candidate is
    /// judged under the projection it would itself produce.
    pub fn next_stop(&mut self, from: usize, forward: bool) -> Option<usize> {
        let step = |s: &Self, p| {
            if forward {
                s.next_boundary(p)
            } else {
                s.prev_boundary(p)
            }
        };
        let mut pos = step(self, from)?;
        loop {
            let p = self.near(Selection::caret(pos), pos..pos);
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
        if self.is_stop(pos) {
            return pos;
        }
        self.next_stop(pos, true)
            .or_else(|| self.next_stop(pos, false))
            .unwrap_or(pos)
    }

    pub fn set_caret(&mut self, pos: usize) {
        // Home or End in a missing cell keeps it: its row's end is no stop.
        if self.missing().is_some_and(|(at, _)| at == pos) {
            return;
        }
        self.cell_typed_end = None;
        self.missing = None;
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
        let from_cell = self
            .near(Selection::caret(anchor), anchor..anchor)
            .cell_at(anchor)
            .map(|(_, _, cell)| (cell.from, cell.limit));
        let table = self
            .near(Selection::caret(anchor), pos..pos)
            .tables
            .iter()
            .find(|t| t.src.contains(&pos))
            .map(|t| t.src.clone());
        let pos = if let Some((from, limit)) = from_cell {
            pos.clamp(from, limit)
        } else if let Some(table) = table {
            if forward {
                self.next_stop(table.end, true).unwrap_or(table.end)
            } else {
                self.next_stop(table.start, false).unwrap_or(table.start)
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
            if extend {
                self.extend_to(next, forward)
            } else {
                self.set_caret(next)
            }
        }
    }

    /// Home and End: the source line's edges, or the cell's inside a table.
    /// Visual-line edges are T7.2's.
    pub fn move_line_edge(&mut self, end: bool, extend: bool) {
        let head = self.doc.selection().head;
        let text = self.doc.text();
        let line_start = text[..head].rfind('\n').map_or(0, |i| i + 1);
        let line_end = text[head..].find('\n').map_or(text.len(), |i| head + i);
        let target = if end { line_end } else { line_start };
        let cell = self.here().cell_at(head).map(|(_, _, c)| (c.from, c.limit));
        let target = match cell {
            Some((from, limit)) => {
                if end {
                    limit
                } else {
                    from
                }
            }
            None => self.snap(target),
        };
        if extend {
            self.set_head(target)
        } else {
            self.set_caret(target)
        }
    }

    pub fn select_all(&mut self) {
        self.cell_typed_end = None;
        let len = self.doc.text().len();
        self.commit_composition();
        self.doc.set_selection(Selection {
            anchor: 0,
            head: len,
        });
    }

    /// The selected source, for copy and cut.
    pub fn selected_text(&self) -> &str {
        &self.doc.text()[self.doc.selection().range()]
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
        self.fill_missing(EditKind::Composition);
        let sel = self.doc.selection();
        let range = self.marked.clone().unwrap_or(sel.range());
        let start = range.start;
        // A pipe in a preedit must not split the cell it is composed in.
        let in_cell = self
            .near(Selection::caret(start), start..start)
            .cell_at(start)
            .is_some();
        let escaped = in_cell.then(|| crate::table::cell_source(text));
        let text = escaped.as_deref().unwrap_or(text);
        let caret = start + caret_in_text.unwrap_or(text.len()).min(text.len());
        self.doc.replace(range, text, caret, EditKind::Composition);
        self.marked = (!text.is_empty()).then(|| start..start + text.len());
    }

    /// Type text at the caret, through the same path as the platform's input
    /// handler. Inside a table cell it is written as the cell writes it.
    pub fn insert_text(&mut self, text: &str) {
        let marked = self.marked.take();
        let composing = marked.is_some();
        let range = marked.unwrap_or(self.doc.selection().range());
        let kind = if composing {
            EditKind::Composition
        } else {
            EditKind::Typing
        };
        if !self.type_in_cell(range.clone(), text, kind) {
            let caret = range.start + text.len();
            self.doc.replace(range, text, caret, kind);
        }
        if composing {
            self.doc.end_undo_group();
        }
        self.settle();
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
        let sel = self.doc.selection();
        if !sel.is_empty() {
            return self.delete_selection();
        }
        let caret = sel.head;
        if caret == 0 {
            return;
        }
        let p = self.here();
        if self.backspace_in_cell(&p, caret) {
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

    pub fn delete_forward(&mut self) {
        self.commit_composition();
        let sel = self.doc.selection();
        if !sel.is_empty() {
            return self.delete_selection();
        }
        let caret = sel.head;
        let p = self.here();
        if self.delete_in_cell(&p, caret) {
            return;
        }
        if let Some(atom) = p.atomic_after(caret) {
            self.doc.replace(atom, "", caret, EditKind::Deletion);
            self.settle();
            return;
        }
        let Some(next) = self.next_boundary(caret) else {
            return;
        };
        self.doc.replace(caret..next, "", caret, EditKind::Deletion);
        self.settle();
    }

    /// Enter: a new row in a table, a continued item in a list, a new paragraph.
    pub fn enter(&mut self) {
        self.commit_composition();
        if self.enter_in_cell() {
            return;
        }
        self.delete_selection();
        let caret = self.doc.selection().head;
        let p = self.here();
        let block = p
            .locate(caret)
            .and_then(|(leaf, _)| p.blocks.iter().find(|b| b.leaves.contains(&leaf)).cloned());
        let kind = block.as_ref().map(|b| &b.kind);
        if let (Some(BlockKind::Heading(_)), Some(b)) = (kind, &block)
            && self.break_before_heading(b.src.start, caret)
        {
            return;
        }
        if let Some(BlockKind::ListItem {
            marker: Some(_), ..
        }) = kind
        {
            return self.continue_list(caret);
        }
        if self.continue_quote(caret) {
            return;
        }
        if let Some(BlockKind::ListItem { marker: None, .. }) = kind {
            return self.continue_list(caret);
        }
        let paragraph = matches!(kind, Some(BlockKind::Paragraph));
        self.insert_plain(caret, if paragraph { "\n\n" } else { "\n" });
    }

    fn insert_plain(&mut self, at: usize, text: &str) {
        self.doc
            .replace(at..at, text, at + text.len(), EditKind::Other);
        self.settle();
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

    /// Selects source bytes exactly, as the platform input handler names
    /// them for a replacement.
    pub fn select_range(&mut self, range: Range<usize>) {
        self.doc.set_selection(Selection {
            anchor: range.start,
            head: range.end,
        });
    }
}

#[cfg(test)]
mod blocks_tests;
#[cfg(test)]
mod tests;
