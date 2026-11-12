//! ⌘B and ⌘I (Ctrl on Linux), the task box, Enter before a heading, and the
//! link under a position. Ports of `toggleInline`, the task widget's click,
//! `breakBeforeHeading` and `linkAt` in `liveMarkdown.ts`.

use std::ops::Range;

use super::LiveDoc;
use super::changes::Changes;
use crate::doc::{EditKind, Selection};

impl LiveDoc {
    /// Wraps the selection in `marker`, or unwraps it if it already is. An
    /// empty selection takes an empty pair with the caret between, as in
    /// Tauri. In a table cell, the cell's own rules.
    pub fn toggle_inline(&mut self, marker: &str) {
        self.commit_composition();
        if self.toggle_marker_in_cell(marker) {
            return;
        }
        self.cell_typed_end = None;
        let sel = self.doc.selection();
        let Range { start, end } = sel.range();
        let text = self.doc.text();
        let size = marker.len();
        let slice = |from: usize, to: usize| text.get(from..to).unwrap_or_default();
        let run = |pos: usize, forward: bool| {
            let side = if forward { &text[pos..] } else { &text[..pos] };
            let mut count = 0;
            while (forward && side[count * size..].starts_with(marker))
                || (!forward && side[..side.len() - count * size].ends_with(marker))
            {
                count += 1;
            }
            count
        };
        // Around `**bold**` the nearest `*` is half of the bold marker; an odd
        // run on both sides (`*it*`, `***both***`) is the one that holds an italic.
        let italic = size > 1 || (run(start, false) % 2 == 1 && run(end, true) % 2 == 1);
        let inner = &text[start..end];
        let inner_longer = size == 1
            && inner.as_bytes().get(1) == marker.as_bytes().first()
            && inner.len() >= 2
            && inner.as_bytes()[inner.len() - 2] == marker.as_bytes()[0];
        let (changes, from, to) = if start >= size
            && slice(start - size, start) == marker
            && slice(end, end + size) == marker
            && italic
        {
            (
                vec![
                    (start - size..start, String::new()),
                    (end..end + size, String::new()),
                ],
                start - size,
                end - size,
            )
        } else if inner.len() >= size * 2
            && inner.starts_with(marker)
            && inner.ends_with(marker)
            && !inner_longer
        {
            (
                vec![
                    (start..start + size, String::new()),
                    (end - size..end, String::new()),
                ],
                start,
                end - size * 2,
            )
        } else {
            (
                vec![(start..start, marker.into()), (end..end, marker.into())],
                start + size,
                end + size,
            )
        };
        let source = Changes::new(changes).apply(text);
        self.set_source(
            &source,
            Selection {
                anchor: from,
                head: to,
            },
        );
    }

    /// Ticks or clears the task box whose `[` is at `at`. Nothing else
    /// changes, the caret included.
    pub fn toggle_task(&mut self, at: usize) {
        let mark = self.doc.text().get(at..at + 3).unwrap_or_default();
        let next = match mark {
            "[ ]" => "x",
            "[x]" | "[X]" => " ",
            _ => return,
        };
        self.commit_composition();
        let sel = self.doc.selection();
        self.doc.edit(at + 1..at + 2, next, sel, EditKind::Other);
    }

    /// Enter at the start of a heading's text opens a line above it, as in
    /// Typora, rather than splitting the hidden `#` from its text.
    pub(super) fn break_before_heading(&mut self, heading: usize, caret: usize) -> bool {
        let text = self.doc.text();
        let hashes = text[heading..].bytes().take_while(|b| *b == b'#').count();
        let space = usize::from(text[heading + hashes..].starts_with(' '));
        let line_start = text[..heading].rfind('\n').map_or(0, |i| i + 1);
        if heading != line_start
            || hashes == 0
            || caret == heading
            || caret != heading + hashes + space
        {
            return false;
        }
        self.insert_plain_at(heading, caret + 1);
        true
    }

    fn insert_plain_at(&mut self, at: usize, caret: usize) {
        self.doc.replace(at..at, "\n", caret, EditKind::Other);
        self.settle();
    }

    /// The target of the link at `pos`, for a ⌘-click (Ctrl on Linux).
    pub fn link_at(&mut self, pos: usize) -> Option<String> {
        self.refresh();
        crate::link::link_at(&self.ast, pos)
    }
}
