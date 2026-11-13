//! What the view draws, kept per top-level node so that an edit or a caret
//! move projects only the nodes it reaches: the blocks after an edit are moved
//! rather than projected again, and the ones a selection touches are projected
//! again when it moves (reveal is a function of what the selection touches).

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use super::LiveDoc;
use crate::doc::Change;
use crate::projection::{Active, Projection, project_children};
use crate::reparse::Splice;

/// One top-level node as it is drawn, with the blank lines before it; the
/// last one also has the lines after it. An empty document has one segment.
#[derive(Clone)]
pub struct Segment {
    /// New when an edit reaches the segment's node; kept when only reveal
    /// changes, or the node only moved.
    pub id: u64,
    /// From the end of the node before to the end of this one (for the
    /// last, to the end of the text).
    pub src: Range<usize>,
    pub projection: Rc<Projection>,
    /// The height of each block, as the view last measured or estimated it,
    /// kept with the segment so it outlasts edits elsewhere. Zero until then.
    pub heights: Rc<[Cell<f32>]>,
}

#[derive(Default)]
pub(super) struct Drawn {
    segments: Rc<Vec<Segment>>,
    /// Segments to project before drawing: the ones an edit replaced.
    stale: Vec<bool>,
    /// What the segments were projected with, in positions of the text now.
    active: Active,
}

impl Drawn {
    /// After `change` replaced top-level nodes `splice.old` by `splice.new`.
    pub(super) fn splice(&mut self, splice: &Splice, change: Change) {
        if self.segments.len() < splice.old.end || splice.old.is_empty() || splice.new.is_empty() {
            // Nothing drawn yet, or an empty document on one side.
            *self = Self::default();
            return;
        }
        let segments = Rc::make_mut(&mut self.segments);
        let blank = |_| Segment {
            id: 0,
            src: 0..0,
            projection: Rc::default(),
            heights: Rc::new([]),
        };
        let count = splice.new.len();
        let kept = splice.keep.then(|| {
            let old = &segments[splice.old.end - 1];
            (old.id, old.heights.clone())
        });
        segments.splice(splice.old.clone(), (0..count).map(blank));
        if let Some((id, heights)) = kept {
            let last = &mut segments[splice.new.end - 1];
            (last.id, last.heights) = (id, heights);
        }
        self.stale
            .splice(splice.old.clone(), std::iter::repeat_n(true, count));
        let delta = change.delta();
        if delta != 0 {
            for segment in &mut segments[splice.new.end..] {
                segment.src.start = segment.src.start.wrapping_add_signed(delta);
                segment.src.end = segment.src.end.wrapping_add_signed(delta);
                Rc::make_mut(&mut segment.projection).shift(delta);
            }
        }
        let map = |r: &mut Range<usize>| *r = change.map(r.start)..change.map(r.end);
        self.active.ranges.iter_mut().for_each(map);
        if let Some(marked) = &mut self.active.marked {
            map(marked);
        }
        self.active.cell_typed_end = self.active.cell_typed_end.map(|p| change.map(p));
        self.active.missing = (self.active.missing).map(|(at, col)| (change.map(at), col));
    }
}

/// Where each top-level node's segment starts and ends in `len` bytes of
/// text, from the ends of the nodes.
fn spans(ends: &[usize], len: usize) -> Vec<Range<usize>> {
    let count = ends.len().max(1);
    (0..count)
        .map(|i| {
            let start = if i == 0 { 0 } else { ends[i - 1] };
            let end = if i + 1 == count { len } else { ends[i] };
            start..end
        })
        .collect()
}

/// Where what reveal depends on (the selection, the preedit, a cell's typed
/// end, the missing cell with the caret) differs between `old` and `new`. A node inside both selections, or
/// outside both, is drawn the same: of two that overlap, only the ends count.
fn differ(old: &Active, new: &Active) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    match (old.ranges.as_slice(), new.ranges.as_slice()) {
        ([o], [n]) if o.start <= n.end && n.start <= o.end => {
            let between = |a: usize, b: usize| a.min(b)..a.max(b);
            out.extend([between(o.start, n.start), between(o.end, n.end)]);
        }
        (o, n) => out.extend(o.iter().chain(n).cloned()),
    }
    for active in [old, new] {
        out.extend(active.marked.clone());
        out.extend(active.cell_typed_end.map(|p| p..p));
        out.extend(active.missing.map(|(at, _)| at..at));
    }
    out
}

impl LiveDoc {
    /// The document as it is drawn, one segment per top-level node.
    pub fn segments(&mut self) -> Rc<Vec<Segment>> {
        self.refresh();
        let active = self.active(self.doc.selection());
        if !self.drawn.segments.is_empty()
            && active == self.drawn.active
            && !self.drawn.stale.contains(&true)
        {
            return self.drawn.segments.clone();
        }
        let ends: Vec<usize> = (self.tree.root().children().into_iter().flatten())
            .map(|n| n.position().map_or(0, |p| p.end.offset))
            .collect();
        let nodes = ends.len();
        let spans = spans(&ends, self.doc.text().len());
        // Read-only changes how every table draws.
        if self.drawn.segments.len() != spans.len()
            || self.drawn.active.read_only != active.read_only
        {
            self.drawn = Drawn::default();
        }
        let fresh = self.drawn.segments.is_empty();
        let mut project = vec![fresh; spans.len()];
        if !fresh {
            for (i, stale) in self.drawn.stale.iter().enumerate() {
                project[i] |= stale;
            }
            // After an edit too: the selection moved with the text, but what
            // it touches may not have.
            if active != self.drawn.active || self.drawn.stale.contains(&true) {
                // And the node before when what is in it reaches past it (a
                // fence left open in a list item).
                let old = &self.drawn.segments;
                let reaches = |i: usize| {
                    let segment = &old[i];
                    (segment.projection.blocks.iter()).any(|b| b.src.end > segment.src.end)
                };
                for range in differ(&self.drawn.active, &active) {
                    let mut first = spans.partition_point(|s| s.end < range.start);
                    let last = spans.partition_point(|s| s.start <= range.end);
                    if first > 0 && first <= old.len() && reaches(first - 1) {
                        first -= 1;
                    }
                    project[first..last.max(first)].fill(true);
                }
            }
        }
        let mut segments = std::mem::take(&mut self.drawn.segments);
        let list = Rc::make_mut(&mut segments);
        for (i, span) in spans.iter().enumerate() {
            if !project[i] {
                continue;
            }
            let range = i..(i + 1).min(nodes);
            let ast = self.tree.settled(range.clone());
            let mut projection = project_children(self.doc.text(), ast, &active, range);
            for leaf in &mut projection.leaves {
                leaf.key = self.next_key;
                self.next_key += 1;
            }
            // The same node: its heights still hold as estimates.
            let blocks = projection.blocks.len();
            let fresh_heights = || (0..blocks).map(|_| Cell::new(0.)).collect();
            let (id, heights) = match list.get(i).filter(|old| old.id != 0) {
                Some(old) if old.heights.len() == blocks => (old.id, old.heights.clone()),
                Some(old) => (old.id, fresh_heights()),
                None => {
                    self.next_id += 1;
                    (self.next_id, fresh_heights())
                }
            };
            let segment = Segment {
                id,
                src: span.clone(),
                projection: Rc::new(projection),
                heights,
            };
            if fresh {
                list.push(segment);
            } else {
                list[i] = segment;
            }
        }
        self.drawn = Drawn {
            stale: vec![false; list.len()],
            segments,
            active,
        };
        self.drawn.segments.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Selection;
    use crate::projection::BlockKind;

    /// The segments as one projection, as `project` makes it, keys aside.
    fn flatten(segments: &[Segment]) -> Projection {
        let mut out = Projection::default();
        for segment in segments {
            let p = &segment.projection;
            let base = out.leaves.len();
            out.leaves.extend(p.leaves.iter().cloned().map(|mut l| {
                l.key = 0;
                l
            }));
            out.blocks.extend(p.blocks.iter().cloned().map(|mut b| {
                b.leaves.iter_mut().for_each(|l| *l += base);
                if let BlockKind::Table { rows, .. } = &mut b.kind {
                    rows.iter_mut().flatten().for_each(|l| *l += base);
                }
                b
            }));
            out.tables.extend(p.tables.iter().cloned().map(|mut t| {
                t.cells.iter_mut().for_each(|c| c.leaf += base);
                t
            }));
        }
        out
    }

    fn whole(d: &mut LiveDoc) -> Projection {
        let mut p = (*d.projection()).clone();
        p.leaves.iter_mut().for_each(|l| l.key = 0);
        p
    }

    const TEXT: &str = "# Title **b**\n\npara *it* and `code`\n\n- one\n- [ ] two\n\n\
        | a | **b** |\n|---|---|\n| 1 | 2 |\n\n> quote\n\n```\nfn x() {}\n```\n\nend\n";

    #[test]
    fn segments_draw_what_the_whole_projection_draws_through_edits_and_moves() {
        let mut d = LiveDoc::new(TEXT);
        for pos in (0..=TEXT.len()).step_by(3) {
            d.set_caret(pos);
            assert_eq!(flatten(&d.segments()), whole(&mut d), "caret {pos}");
        }
        for (at, typed) in [
            (0, "x"),
            (20, "\n\n- "),
            (40, "```\n"),
            (5, "**"),
            (60, "|---|\n"),
        ] {
            d.set_caret(at.min(d.text().len()));
            d.insert_text(typed);
            assert_eq!(
                flatten(&d.segments()),
                whole(&mut d),
                "typed {typed:?} at {at}"
            );
            d.undo();
            assert_eq!(
                flatten(&d.segments()),
                whole(&mut d),
                "undid {typed:?} at {at}"
            );
        }
        d.select_all();
        assert_eq!(flatten(&d.segments()), whole(&mut d));
        d.focused = false;
        assert_eq!(flatten(&d.segments()), whole(&mut d));
        d.reset_text("");
        assert_eq!(flatten(&d.segments()), whole(&mut d));
        d.reset_text(TEXT);
        assert_eq!(flatten(&d.segments()), whole(&mut d));
    }

    #[test]
    fn only_what_an_edit_or_the_selection_reaches_is_projected_again() {
        let text = "a **b** c\n\nfar\n\nfarther\n\nstill\n\nend **x**\n";
        let mut d = LiveDoc::new(text);
        d.doc.set_selection(Selection::caret(text.len()));
        let first = d.segments();
        assert!(Rc::ptr_eq(&first, &d.segments()), "nothing changed");
        let ids: Vec<u64> = first.iter().map(|s| s.id).collect();
        let far = Rc::as_ptr(&first[2].projection);
        drop(first);
        // Into `**b**`: only its segment reveals, and keeps its id.
        d.set_caret(4);
        let moved = d.segments();
        assert_eq!(moved.iter().map(|s| s.id).collect::<Vec<_>>(), ids);
        assert_eq!(Rc::as_ptr(&moved[2].projection), far);
        assert_eq!(moved[0].projection.leaves[0].text, "a **b** c");
        drop(moved);
        // Typing there: the segments after keep their ids and move.
        d.insert_text("x");
        let typed = d.segments();
        assert_eq!(
            typed.iter().skip(4).map(|s| s.id).collect::<Vec<_>>(),
            ids[4..]
        );
        assert_eq!(typed[4].src.start, text.find("end").unwrap() - 2 + 1);
    }

    #[test]
    fn segments_draw_what_the_whole_projection_draws_through_random_edits() {
        let docs = [
            TEXT,
            "> a\n>\n\nb\n",
            ">\n\nx\n",
            ">",
            "a\n>\n\nb",
            "- a\n1. ```\n\ned\n\npara\n\n[r]: /u\n",
        ];
        let pieces = [
            "x", "\n", "\n\n", "> ", "- ", "```\n", "|", "|---|\n", "**", "é",
        ];
        for (n, start) in docs.into_iter().enumerate() {
            for seed in 1..=8u64 {
                let mut seed = seed + 100 * n as u64;
                let mut next = |n: usize| {
                    seed = seed
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    (seed >> 33) as usize % n.max(1)
                };
                let mut d = LiveDoc::new(start);
                for step in 0..60 {
                    let len = d.text().len();
                    let mut at = next(len + 1);
                    while !d.text().is_char_boundary(at) {
                        at -= 1;
                    }
                    let (op, extra) = (next(6), next(12));
                    let piece = pieces[next(pieces.len())];
                    match op {
                        0 => d.set_caret(at),
                        1 => {
                            let mut head = (at + extra).min(len);
                            while !d.text().is_char_boundary(head) {
                                head -= 1;
                            }
                            d.doc.set_selection(Selection { anchor: at, head });
                        }
                        2 => d.backspace(),
                        3 => d.undo(),
                        _ => {
                            d.set_caret(at);
                            d.insert_text(piece);
                        }
                    }
                    // `markdown` itself panics on a few texts, drawn whole as
                    // plain lines.
                    if crate::projection::try_parse(d.text()).is_none() {
                        break;
                    }
                    let segments = d.segments();
                    assert!(segments.iter().all(|s| !s.projection.blocks.is_empty()));
                    assert_eq!(
                        flatten(&segments),
                        whole(&mut d),
                        "step {step} of {start:?}: {:?} at {:?}",
                        d.text(),
                        d.selection()
                    );
                }
            }
        }
    }

    #[test]
    fn a_paragraph_added_above_keeps_the_segments_after_it() {
        let text = (0..20).map(|i| format!("para {i}\n\n")).collect::<String>();
        let mut d = LiveDoc::new(&text);
        d.focused = false;
        let ids: Vec<u64> = d.segments().iter().map(|s| s.id).collect();
        // Right above paragraph 10, as an agent writing the file would.
        let at = text.find("para 10").unwrap();
        d.reset_text(&format!("{}new\n\n{}", &text[..at], &text[at..]));
        let after: Vec<u64> = d.segments().iter().map(|s| s.id).collect();
        assert_eq!(after.len(), ids.len() + 1);
        assert_eq!(after[11..], ids[10..], "paragraph 10 on keep their ids");
        assert_eq!(after[..10], ids[..10]);
    }
}
