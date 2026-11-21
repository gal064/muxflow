//! Several replacements made as one edit: one undo step, with positions mapped
//! through them, as a CodeMirror change set.

use std::ops::Range;

use super::LiveDoc;
use crate::doc::{EditKind, Selection, minimal_change};

/// Replacements against one text, sorted and not overlapping.
pub(super) struct Changes(Vec<(Range<usize>, String)>);

impl Changes {
    pub(super) fn new(mut changes: Vec<(Range<usize>, String)>) -> Self {
        changes.sort_by_key(|(range, _)| (range.start, range.end));
        Self(changes)
    }

    pub(super) fn apply(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for (range, inserted) in &self.0 {
            out.push_str(&text[at..range.start]);
            out.push_str(inserted);
            at = range.end;
        }
        out.push_str(&text[at..]);
        out
    }

    /// Where `pos` lands. `forward` puts it after text inserted at it, and at
    /// the end rather than the start of a replacement around it.
    pub(super) fn map(&self, pos: usize, forward: bool) -> usize {
        let mut shift = 0isize;
        for (range, inserted) in &self.0 {
            let inserting_here = range.is_empty() && range.start == pos;
            if pos < range.start || (pos == range.start && !(inserting_here && forward)) {
                break;
            }
            if pos >= range.end {
                shift += inserted.len() as isize - range.len() as isize;
                continue;
            }
            let start = (range.start as isize + shift) as usize;
            return if forward {
                start + inserted.len()
            } else {
                start
            };
        }
        (pos as isize + shift) as usize
    }

    /// The changes that take the changed text back to `text`.
    pub(super) fn invert(&self, text: &str) -> Changes {
        let mut shift = 0isize;
        let mut out = Vec::with_capacity(self.0.len());
        for (range, inserted) in &self.0 {
            let start = (range.start as isize + shift) as usize;
            out.push((
                start..start + inserted.len(),
                text[range.clone()].to_string(),
            ));
            shift += inserted.len() as isize - range.len() as isize;
        }
        Changes(out)
    }
}

impl LiveDoc {
    /// Makes `text` the source in one undo step, leaving `selection`.
    pub(super) fn set_source(&mut self, text: &str, selection: Selection) {
        let (start, old_end, new_end) = minimal_change(self.doc.text(), text);
        self.doc.edit(
            start..old_end,
            &text[start..new_end],
            selection,
            EditKind::Other,
        );
        self.settle();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_positions_and_inverts() {
        let text = "abcdef";
        let c = Changes::new(vec![(4..5, "XY".into()), (1..1, "--".into())]);
        let after = c.apply(text);
        assert_eq!(after, "a--bcdXYf");
        assert_eq!(c.map(1, false), 1);
        assert_eq!(c.map(1, true), 3);
        assert_eq!(c.map(4, true), 6, "a replacement starting here comes after");
        assert_eq!(c.map(5, false), 8);
        assert_eq!(c.map(6, false), 9);
        assert_eq!(c.invert(text).apply(&after), text);
        assert_eq!(
            c.invert(text).map(7, false),
            4,
            "inside a replacement maps to its start"
        );
    }
}
