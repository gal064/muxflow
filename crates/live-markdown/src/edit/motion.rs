//! Word motion, and selection by double and triple click, as CodeMirror does
//! them over the source: `cursorGroupForward` and its kin, `groupAt`, and the
//! triple click's whole drawn line.

use unicode_segmentation::UnicodeSegmentation;

use super::LiveDoc;
use crate::doc::Selection;
use crate::projection::SegKind;

/// CodeMirror's character categories.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Word,
    Space,
    Other,
}

fn group(grapheme: &str) -> Group {
    match grapheme.chars().next() {
        Some(c) if c.is_alphanumeric() || c == '_' => Group::Word,
        Some(c) if c.is_whitespace() => Group::Space,
        _ => Group::Other,
    }
}

impl LiveDoc {
    /// Alt (Ctrl on Linux) with Left or Right: past any spaces, then past the
    /// run of word characters or of punctuation that follows, over the source
    /// as CodeMirror's `moveByChar`, hidden markup included.
    pub fn move_word(&mut self, forward: bool, extend: bool) {
        let sel = self.doc.selection();
        if !extend && !sel.is_empty() {
            return self.move_horizontal(forward, false);
        }
        let text = self.doc.text();
        let mut pos = sel.head;
        let mut kind = None;
        loop {
            let next = if forward {
                text[pos..].graphemes(true).next()
            } else {
                text[..pos].graphemes(true).next_back()
            };
            let Some(next) = next else {
                break;
            };
            let g = group(next);
            match kind {
                Some(k) if k != Group::Space && k != g => break,
                _ => kind = Some(g),
            }
            pos = if forward {
                pos + next.len()
            } else {
                pos - next.len()
            };
        }
        // As CodeMirror skips atomic ranges after the move: from inside
        // hidden markup, on to the next stop.
        if !self.is_stop(pos) {
            pos = self.next_stop(pos, forward).unwrap_or(pos);
        }
        if pos == sel.head {
            return;
        }
        if extend {
            self.extend_to(pos, forward)
        } else {
            self.set_caret(pos)
        }
    }

    /// Double click: the run of word characters, spaces or punctuation at
    /// `pos`, within its line. `before_side`: the click fell on the character
    /// before `pos`.
    pub fn select_word(&mut self, pos: usize, before_side: bool) {
        let text = self.doc.text();
        let line_start = text[..pos].rfind('\n').map_or(0, |i| i + 1);
        let line_end = text[pos..].find('\n').map_or(text.len(), |i| pos + i);
        let after = text[pos..line_end].graphemes(true).next();
        let before = text[line_start..pos].graphemes(true).next_back();
        // The clicked character, as CodeMirror's `groupAt` with the click's
        // side; at the end of a line, the one before.
        let (mut from, mut to, g) = match (after, before) {
            (_, Some(b)) if before_side || after.is_none() => (pos - b.len(), pos, group(b)),
            (Some(a), _) => (pos, pos + a.len(), group(a)),
            _ => return self.set_caret(pos),
        };
        while let Some(b) = text[line_start..from].graphemes(true).next_back()
            && group(b) == g
        {
            from -= b.len();
        }
        while let Some(a) = text[to..line_end].graphemes(true).next()
            && group(a) == g
        {
            to += a.len();
        }
        self.select(from, to);
    }

    /// Triple click: the drawn line at `pos` (a paragraph's softly broken
    /// lines are drawn as one) and its line break, or the table cell.
    pub fn select_block(&mut self, pos: usize) {
        let p = self.near(Selection::caret(pos), pos..pos);
        if let Some((_, _, cell)) = p.cell_at(pos) {
            return self.select(cell.from, cell.limit);
        }
        let Some((leaf, _)) = p.locate(pos) else {
            return;
        };
        let Some(block) = p.blocks.iter().find(|b| b.leaves.contains(&leaf)) else {
            return;
        };
        // The source line, joined across the soft breaks drawn as spaces only:
        // a code line or a line after a hard break is a line of its own.
        let text = self.doc.text();
        let joined = |nl: usize| {
            p.leaves
                .iter()
                .flat_map(|l| &l.segs)
                .any(|s| s.kind == SegKind::Replaced && s.src.contains(&nl))
        };
        let mut from = text[..pos].rfind('\n').map_or(0, |i| i + 1);
        while from > 0 && joined(from - 1) {
            from = text[..from - 1].rfind('\n').map_or(0, |i| i + 1);
        }
        let mut to = text[pos..].find('\n').map_or(text.len(), |i| pos + i);
        while to < text.len() && joined(to) {
            to = text[to + 1..].find('\n').map_or(text.len(), |i| to + 1 + i);
        }
        let from = from.max(p.leaves[block.leaves[0]].src.start);
        let mut to = to.min(p.leaves[block.leaves[block.leaves.len() - 1]].src.end);
        if text[to..].starts_with('\n') {
            to += 1;
        }
        self.select(from, to);
    }

    fn select(&mut self, anchor: usize, head: usize) {
        self.cell_typed_end = None;
        self.commit_composition();
        self.doc.set_selection(Selection { anchor, head });
    }

    /// Home or End to `target`, the edge of the visual line the view found;
    /// clamped to the current cell when editing a table.
    pub fn move_to_line_edge(&mut self, _end: bool, extend: bool, target: usize) {
        let head = self.doc.selection().head;
        let target = self
            .here()
            .cell_at(head)
            .map_or(target, |(_, _, cell)| target.clamp(cell.from, cell.limit));
        if extend {
            self.set_head(target)
        } else {
            self.set_caret(target)
        }
    }
}
