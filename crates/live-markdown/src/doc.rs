//! The document: the Markdown source string is the single truth.
//!
//! Everything the editor shows is derived from `text` and the selection. Undo
//! records byte-range source edits only, so a change made through a rendered
//! table cell undoes exactly like one made while the raw markers were showing.
//! The host is told the text changed, and takes the new text into the buffer
//! it saves from.

use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn caret(pos: usize) -> Self {
        Self {
            anchor: pos,
            head: pos,
        }
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
    /// What changed since the host last took the text.
    unsent: Option<Change>,
    /// What changed since [`Self::take_change`] last took it, so the parse can
    /// redo only that part.
    change: Option<Change>,
}

/// One span that covers every edit since a point in time: the text before it
/// is unchanged, `start..old_end` of the old text is now `start..new_end`,
/// and what follows is unchanged but shifted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub start: usize,
    pub old_end: usize,
    pub new_end: usize,
}

impl Change {
    /// How far text after the change moved.
    pub fn delta(&self) -> isize {
        self.new_end as isize - self.old_end as isize
    }

    /// Where an old position is now. A position inside the replaced span
    /// moves to its start.
    pub fn map(&self, pos: usize) -> usize {
        if pos <= self.start {
            pos
        } else if pos >= self.old_end {
            pos - self.old_end + self.new_end
        } else {
            self.start
        }
    }

    /// `change`, if any, and then `range` replaced by `len` bytes.
    fn grow(change: Option<Self>, range: Range<usize>, len: usize) -> Self {
        let first = Self {
            start: range.start,
            old_end: range.start,
            new_end: range.start,
        };
        change.unwrap_or(first).then(range, len)
    }

    /// This change followed by the replacement of `range` (in the text after
    /// this change) with `len` bytes.
    fn then(self, range: Range<usize>, len: usize) -> Self {
        let end = self.new_end.max(range.end);
        Self {
            start: self.start.min(range.start),
            old_end: self.old_end + (end - self.new_end),
            new_end: end + len - range.len(),
        }
    }
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
            unsent: None,
            change: None,
        }
    }

    /// The span changed since the last call, if any.
    pub fn take_change(&mut self) -> Option<Change> {
        self.change.take()
    }

    pub(crate) fn pending_change(&self) -> Option<Change> {
        self.change
    }

    fn record(&mut self, range: Range<usize>, len: usize) {
        self.change = Some(Change::grow(self.change, range, len));
    }

    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn selection(&self) -> Selection {
        self.selection
    }
    /// Advances on every change to the text.
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

    /// Whether the text changed since the host last took it.
    pub fn edited(&self) -> bool {
        self.unsent.is_some()
    }

    /// The span changed since the last call, if any.
    pub fn take_edited(&mut self) -> Option<Change> {
        self.unsent.take()
    }

    fn splice(&mut self, range: Range<usize>, text: &str) {
        self.unsent = Some(Change::grow(self.unsent, range.clone(), text.len()));
        self.record(range.clone(), text.len());
        self.text.replace_range(range, text);
        self.revision += 1;
    }

    /// Replace `range` and leave the caret at `caret`.
    pub fn replace(&mut self, range: Range<usize>, inserted: &str, caret: usize, kind: EditKind) {
        self.edit(range, inserted, Selection::caret(caret), kind);
    }

    /// Replace `range` and leave `after` selected.
    pub fn edit(&mut self, range: Range<usize>, inserted: &str, after: Selection, kind: EditKind) {
        let removed = self.text[range.clone()].to_string();
        if removed.is_empty() && inserted.is_empty() {
            return;
        }
        let before = self.selection;
        self.splice(range.clone(), inserted);
        let edit = Edit {
            from: range.start,
            removed,
            inserted: inserted.to_string(),
        };
        self.selection = after;
        self.redo.clear();

        let coalesce = !self.break_coalescing
            && kind != EditKind::Other
            && self.undo.last().is_some_and(|last| last.kind == kind);
        if coalesce {
            let last = self.undo.last_mut().expect("checked above");
            last.edits.push(edit);
            last.after = after;
        } else {
            self.undo.push(Entry {
                edits: vec![edit],
                before,
                after,
                kind,
            });
        }
        self.break_coalescing = kind == EditKind::Other;
    }

    pub fn undo(&mut self) -> bool {
        let Some(entry) = self.undo.pop() else {
            return false;
        };
        for edit in entry.edits.iter().rev() {
            let end = edit.from + edit.inserted.len();
            self.splice(edit.from..end, &edit.removed);
        }
        self.selection = entry.before;
        self.redo.push(entry);
        self.break_coalescing = true;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(entry) = self.redo.pop() else {
            return false;
        };
        for edit in &entry.edits {
            let end = edit.from + edit.removed.len();
            self.splice(edit.from..end, &edit.inserted);
        }
        self.selection = entry.after;
        self.undo.push(entry);
        self.break_coalescing = true;
        true
    }

    /// The text changed outside this editor (on disk, or in another view).
    /// Only the span that differs is replaced, so the caret stays where it was
    /// in the unchanged text. The host, where it came from, is not told of it.
    ///
    /// As CodeMirror's `addToHistory: false`: the change itself is not
    /// undoable, and the user's own edits stay undoable, moved past it. Were
    /// the change undoable, undoing it would bring back text the file no
    /// longer has, and autosave would write it straight back. An edit the
    /// change overlaps cannot be moved, and the history is dropped.
    pub fn reset_text(&mut self, text: &str) {
        let (start, old_end, new_end) = minimal_change(&self.text, text);
        if start == old_end && start == new_end {
            return;
        }
        let change = Outside {
            from: start,
            to: old_end,
            len: new_end - start,
        };
        self.record(start..old_end, new_end - start);
        self.text
            .replace_range(start..old_end, &text[start..new_end]);
        self.selection = change.map_selection(self.selection);
        if !change.rebase_undo(&mut self.undo) || !change.rebase_redo(&mut self.redo) {
            self.undo.clear();
            self.redo.clear();
        }
        self.unsent = None;
        self.break_coalescing = true;
        self.revision += 1;
    }
}

/// An outside change: `from..to` of the text became `len` bytes.
#[derive(Clone, Copy, Debug)]
struct Outside {
    from: usize,
    to: usize,
    len: usize,
}

impl Outside {
    fn map(&self, at: usize) -> usize {
        if at <= self.from {
            at
        } else if at >= self.to {
            at - self.to + self.from + self.len
        } else {
            self.from + self.len
        }
    }

    fn map_selection(&self, s: Selection) -> Selection {
        Selection {
            anchor: self.map(s.anchor),
            head: self.map(s.head),
        }
    }

    /// Whether `edit`, spanning `start..start + width` of the text the change
    /// applies to, is clear of it. If so the edit moves past the change, and
    /// the change moves to the other side of the edit, where the edit's
    /// `width` bytes are `other` bytes.
    fn pass(&mut self, edit: &mut Edit, start: usize, width: usize, other: usize) -> bool {
        if self.to <= start {
            edit.from = edit.from - (self.to - self.from) + self.len;
        } else if self.from >= start + width {
            self.from = self.from - width + other;
            self.to = self.to - width + other;
        } else {
            return false;
        }
        true
    }

    /// Moves the undo history past the change, newest edit first: each step
    /// carries the change back to the text before that edit.
    fn rebase_undo(mut self, entries: &mut [Entry]) -> bool {
        for entry in entries.iter_mut().rev() {
            entry.after = self.map_selection(entry.after);
            for edit in entry.edits.iter_mut().rev() {
                let (start, width, other) = (edit.from, edit.inserted.len(), edit.removed.len());
                if !self.pass(edit, start, width, other) {
                    return false;
                }
            }
            entry.before = self.map_selection(entry.before);
        }
        true
    }

    /// Moves the redo history past the change, next redo first: each step
    /// carries the change forward to the text after that edit.
    fn rebase_redo(mut self, entries: &mut [Entry]) -> bool {
        for entry in entries.iter_mut().rev() {
            entry.before = self.map_selection(entry.before);
            for edit in entry.edits.iter_mut() {
                let (start, width, other) = (edit.from, edit.removed.len(), edit.inserted.len());
                if !self.pass(edit, start, width, other) {
                    return false;
                }
            }
            entry.after = self.map_selection(entry.after);
        }
        true
    }
}

/// Where `before` and `after` differ: the common prefix's end, then the ends
/// of the differing spans in each, all on character boundaries. Port of
/// `minimalChange` in `LiveMarkdownEditor.tsx`.
pub fn minimal_change(before: &str, after: &str) -> (usize, usize, usize) {
    let start = before
        .char_indices()
        .zip(after.chars())
        .find(|((_, a), b)| a != b)
        .map_or(before.len().min(after.len()), |((at, _), _)| at);
    let suffix: usize = before[start..]
        .chars()
        .rev()
        .zip(after[start..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum();
    (start, before.len() - suffix, after.len() - suffix)
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

    #[test]
    fn the_host_is_told_of_an_edit_once() {
        let mut d = Document::new("ab");
        assert!(d.take_edited().is_none());
        d.replace(1..1, "x", 2, EditKind::Typing);
        d.undo();
        assert!(d.edited());
        assert!(d.take_edited().is_some());
        assert!(d.take_edited().is_none());
    }

    // Mirrors LiveMarkdownEditor.test.tsx "minimalChange".
    #[test]
    fn replaces_only_what_differs() {
        let span = |a: &str, b: &str| {
            let (from, to, end) = minimal_change(a, b);
            (from, to, b[from..end].to_string())
        };
        assert_eq!(
            span("hello world", "hello brave world"),
            (6, 6, "brave ".into())
        );
        assert_eq!(span("aaa", "aa"), (2, 3, String::new()));
        assert_eq!(span("same", "same"), (4, 4, String::new()));
        assert_eq!(span("", "new"), (0, 0, "new".into()));
        // Never inside a character.
        assert_eq!(span("a\u{e9}", "a\u{e8}"), (1, 3, "\u{e8}".into()));
    }

    #[test]
    fn one_change_covers_every_edit_since_it_was_taken() {
        let mut d = Document::new("0123456789");
        let base = d.text().to_owned();
        d.replace(5..5, "ab", 7, EditKind::Typing);
        d.replace(1..2, "", 1, EditKind::Deletion);
        d.replace(9..11, "xyz", 12, EditKind::Other);
        d.undo();
        d.reset_text("0234ab5678X9");
        let c = d.take_change().unwrap();
        let rebuilt = format!(
            "{}{}{}",
            &base[..c.start],
            &d.text()[c.start..c.new_end],
            &base[c.old_end..]
        );
        assert_eq!(rebuilt, d.text());
        assert_eq!(c.map(0), 0);
        assert_eq!(c.map(base.len()), d.text().len());
        assert_eq!(d.take_change(), None);
    }

    // Mirrors LiveMarkdownEditor.test.tsx "takes an outside change in place".
    #[test]
    fn an_outside_change_keeps_the_caret_and_is_not_undoable_or_queued() {
        let mut d = Document::new("one\ntwo\n");
        d.set_selection(Selection::caret(8));
        d.replace(8..8, "!", 9, EditKind::Typing);
        d.take_edited();
        d.set_selection(Selection::caret(6));
        d.reset_text("zero\none\ntwo\n!");
        assert_eq!(d.text(), "zero\none\ntwo\n!");
        assert_eq!(d.selection().head, 11);
        assert!(d.take_edited().is_none());
        // Undo takes back the user's edit, past the outside change, and never
        // the outside change itself.
        assert!(d.undo());
        assert_eq!(d.text(), "zero\none\ntwo\n");
        assert_eq!(d.selection().head, 13);
        assert!(!d.undo());
        assert_eq!(d.text(), "zero\none\ntwo\n");
    }

    #[test]
    fn undo_and_redo_survive_an_outside_change_on_either_side_of_them() {
        let mut d = Document::new("a\nb\nc\n");
        d.set_selection(Selection::caret(3));
        d.replace(3..3, "X", 4, EditKind::Typing);
        d.end_undo_group();
        d.replace(4..4, "Y", 5, EditKind::Other);
        d.undo();
        assert_eq!(d.text(), "a\nbX\nc\n");
        // Above both edits, then below them.
        d.reset_text("top\na\nbX\nc\n");
        d.reset_text("top\na\nbX\nc\nend\n");
        d.redo();
        assert_eq!(d.text(), "top\na\nbXY\nc\nend\n");
        d.undo();
        d.undo();
        assert_eq!(d.text(), "top\na\nb\nc\nend\n");
        assert_eq!(d.selection().head, 7);
        d.redo();
        d.redo();
        assert_eq!(d.text(), "top\na\nbXY\nc\nend\n");
    }

    #[test]
    fn an_outside_change_over_an_edit_drops_the_history() {
        let mut d = Document::new("one two");
        d.replace(3..3, "!", 4, EditKind::Typing);
        d.reset_text("one");
        assert!(!d.undo());
        assert!(!d.redo());
        assert_eq!(d.text(), "one");
    }
}
