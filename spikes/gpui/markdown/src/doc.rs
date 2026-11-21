//! The document: the Markdown source string is the single truth.
//!
//! Everything the editor shows is derived from `text` and the selection. Undo
//! records byte-range source edits only, so a change made through a rendered
//! table cell undoes exactly like one made while the raw markers were showing.

use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn caret(pos: usize) -> Self {
        Self { anchor: pos, head: pos }
    }
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// Why an edit happened; adjacent edits of the same coalescing kind share one
/// undo step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    Typing,
    Deletion,
    Composition,
    Other,
}

#[derive(Clone, Debug)]
struct Edit {
    from: usize,
    removed: String,
    inserted: String,
}

#[derive(Clone, Debug)]
struct Entry {
    edits: Vec<Edit>,
    before: Selection,
    after: Selection,
    kind: EditKind,
}

#[derive(Debug)]
pub struct Document {
    text: String,
    selection: Selection,
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    /// The next edit starts a new undo step (set by caret moves and by a
    /// composition's commit).
    break_coalescing: bool,
    revision: u64,
}

impl Document {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            selection: Selection::caret(0),
            undo: Vec::new(),
            redo: Vec::new(),
            break_coalescing: true,
            revision: 0,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn selection(&self) -> Selection {
        self.selection
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn set_selection(&mut self, selection: Selection) {
        self.selection = selection;
        self.break_coalescing = true;
    }

    /// Selection change that keeps the current undo step open (used while the
    /// caret follows typing).
    pub fn set_selection_coalescing(&mut self, selection: Selection) {
        self.selection = selection;
    }

    pub fn end_undo_group(&mut self) {
        self.break_coalescing = true;
    }

    /// Replace `range` and leave the caret at `caret`.
    pub fn replace(&mut self, range: Range<usize>, inserted: &str, caret: usize, kind: EditKind) {
        let removed = self.text[range.clone()].to_string();
        if removed.is_empty() && inserted.is_empty() {
            return;
        }
        let before = self.selection;
        let after = Selection::caret(caret);
        self.text.replace_range(range.clone(), inserted);
        let edit = Edit { from: range.start, removed, inserted: inserted.to_string() };
        self.selection = after;
        self.redo.clear();
        self.revision += 1;

        let coalesce = !self.break_coalescing
            && kind != EditKind::Other
            && self.undo.last().is_some_and(|last| last.kind == kind);
        if coalesce {
            let last = self.undo.last_mut().expect("checked above");
            last.edits.push(edit);
            last.after = after;
        } else {
            self.undo.push(Entry { edits: vec![edit], before, after, kind });
        }
        self.break_coalescing = kind == EditKind::Other;
    }

    pub fn undo(&mut self) -> bool {
        let Some(entry) = self.undo.pop() else { return false };
        for edit in entry.edits.iter().rev() {
            let end = edit.from + edit.inserted.len();
            self.text.replace_range(edit.from..end, &edit.removed);
        }
        self.selection = entry.before;
        self.redo.push(entry);
        self.revision += 1;
        self.break_coalescing = true;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(entry) = self.redo.pop() else { return false };
        for edit in &entry.edits {
            let end = edit.from + edit.removed.len();
            self.text.replace_range(edit.from..end, &edit.inserted);
        }
        self.selection = entry.after;
        self.undo.push(entry);
        self.revision += 1;
        self.break_coalescing = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_coalesces_into_one_undo_and_caret_moves_end_it() {
        let mut d = Document::new("ab");
        d.set_selection(Selection::caret(1));
        d.replace(1..1, "x", 2, EditKind::Typing);
        d.replace(2..2, "y", 3, EditKind::Typing);
        assert_eq!(d.text(), "axyb");
        d.undo();
        assert_eq!(d.text(), "ab");
        assert_eq!(d.selection(), Selection::caret(1));
        d.redo();
        assert_eq!(d.text(), "axyb");
    }

    #[test]
    fn undo_restores_the_exact_source() {
        let mut d = Document::new("| a |");
        d.replace(2..3, "bbb", 5, EditKind::Other);
        assert_eq!(d.text(), "| bbb |");
        d.undo();
        assert_eq!(d.text(), "| a |");
    }
}
