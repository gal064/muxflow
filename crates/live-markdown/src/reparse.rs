//! The parsed tree, kept up to date by parsing again only around each change.
//!
//! A Markdown block's extent depends on its neighbours: a fence left open runs
//! to the end, a paragraph or a list takes the lines that continue it, a line
//! of dashes turns the paragraph above into a heading, and the parser carries
//! a little state from one block to the next. So the window parsed again runs
//! from a blank line before the change to a blank line after it, with two
//! unchanged top-level nodes at each end, and those must come back exactly as
//! they were: then the parser entered and left the window as the old tree
//! had, and nothing outside it can have changed. If they do not, the window
//! grows, up to the whole document. Tested against a fresh parse after
//! thousands of random edits; `markdown` carries a little table state across
//! even a blank line, so a rare window can still read a table differently.
//!
//! Link and footnote definitions decide how references parse anywhere in the
//! file, so the window is parsed with a stand-in for each one before it (a
//! reference keeps only the label, not the target), and a change that reaches
//! a definition parses the whole document, keeping the nodes that come back
//! unchanged.
//!
//! `markdown` takes time that grows faster than the text on tables, so a new
//! tree, or a large window, is built a piece at a time, each piece added as an
//! edit at the end.

use std::mem::discriminant;
use std::ops::Range;

use markdown::mdast::Node;

use crate::doc::Change;
use crate::projection::{parse, try_parse};

/// Top-level nodes `old` of the previous tree are now nodes `new`; the ones
/// before are the same, and the ones after the same but moved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Splice {
    pub old: Range<usize>,
    pub new: Range<usize>,
    /// The last of `old` and of `new` is the same node, moved: it is in the
    /// splice only because the blank lines before it may have changed.
    pub keep: bool,
}

pub struct Tree {
    ast: Node,
    /// How far what is inside each top-level node has still to move: an edit
    /// moves the nodes after it at once, and their insides when they are read.
    owed: Vec<(isize, isize)>,
    /// A stand-in for each link and footnote definition in the document;
    /// `None` when one cannot stand in, and only whole parses are right.
    definitions: Option<String>,
    /// Bytes parsed so far, to bound building a tree a piece at a time.
    parsed: usize,
    /// Being built a piece at a time, with `definitions` found beforehand: a
    /// piece that adds a definition does not parse the whole text.
    building: bool,
}

/// About how much of the text a new tree parses at a time: `markdown`'s time
/// grows about as the square of the text on files of small tables.
const PIECE: usize = 4 * 1024;

fn span(node: &Node) -> Range<usize> {
    let p = node
        .position()
        .expect("parser positions are always present");
    p.start.offset..p.end.offset
}

fn children(node: &Node) -> &[Node] {
    node.children().map_or(&[][..], |c| &c[..])
}

/// Each link and footnote definition in `nodes`: whether it is a footnote,
/// its identifier and its label.
type Definitions = Vec<(bool, String, String)>;

fn definitions(nodes: &[Node]) -> Definitions {
    let mut out = Vec::new();
    nodes.iter().for_each(|n| collect(n, &mut out));
    out
}

/// Only containers can hold a definition; a footnote's can too.
fn collect(node: &Node, out: &mut Definitions) {
    let label = |label: &Option<String>, id: &String| label.clone().unwrap_or_else(|| id.clone());
    match node {
        Node::Definition(d) => {
            out.push((false, d.identifier.clone(), label(&d.label, &d.identifier)))
        }
        Node::FootnoteDefinition(d) => {
            out.push((true, d.identifier.clone(), label(&d.label, &d.identifier)));
            children(node).iter().for_each(|c| collect(c, out));
        }
        Node::Root(_) | Node::Blockquote(_) | Node::List(_) | Node::ListItem(_) => {
            children(node).iter().for_each(|c| collect(c, out));
        }
        _ => {}
    }
}

/// A definition line for each of `defs`, to parse references with, then a
/// paragraph that closes a footnote (which would take the indented lines
/// after it). Written from the identifier, the label as written but folded,
/// not the label, which has its escapes decoded. `None` when they do not read
/// back as the same definitions (a link label starting with `^`).
fn stand_ins(defs: &Definitions) -> Option<String> {
    if defs.is_empty() {
        return Some(String::new());
    }
    let mut out = String::new();
    for (footnote, id, _) in defs {
        out += &format!("[{}{id}]: x\n\n", if *footnote { "^" } else { "" });
    }
    out.push_str("x\n\n");
    let ids = |d: &Definitions| {
        d.iter()
            .map(|(f, id, _)| (*f, id.clone()))
            .collect::<Vec<_>>()
    };
    let back = definitions(children(&try_parse(&out)?));
    (ids(&back) == ids(defs)).then_some(out)
}

/// What to draw again after `change` turned top-level nodes `old` into `new`
/// (two runs that start at the same node): from the first that is not the
/// same to the first after the change that only moved, which is drawn again
/// because the blank lines before it are part of it. Without one, to the
/// end, and at least the last node, which also has the lines after it.
fn narrow(
    old: &[Node],
    new: &[Node],
    change: Change,
    lines: isize,
) -> (Range<usize>, Range<usize>, bool) {
    let prefix = old
        .iter()
        .zip(new)
        .take_while(|(o, n)| span(o).end < change.start && o == n)
        .count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(o, n)| span(o).start >= change.old_end && moved(o, n, change.delta(), lines))
        .count();
    if suffix > 0 {
        (
            prefix..old.len() - suffix + 1,
            prefix..new.len() - suffix + 1,
            true,
        )
    } else {
        let prefix = prefix.min(old.len().min(new.len()).saturating_sub(1));
        (prefix..old.len(), prefix..new.len(), false)
    }
}

/// Moves only the node itself, not what is inside it.
fn shift_own(node: &mut Node, offset: isize, lines: isize) {
    if let Some(p) = node.position_mut() {
        for point in [&mut p.start, &mut p.end] {
            point.offset = point.offset.wrapping_add_signed(offset);
            point.line = point.line.wrapping_add_signed(lines);
        }
    }
}

/// Moves a node and everything in it by `offset` bytes and `lines` lines.
/// Columns stay: a moved node starts on a line of its own.
fn shift(node: &mut Node, offset: isize, lines: isize) {
    shift_own(node, offset, lines);
    if let Some(children) = node.children_mut() {
        for child in children {
            shift(child, offset, lines);
        }
    }
}

/// The same node and descendants, moved by `offset` bytes and `lines` lines:
/// its source is unchanged, so this says it parsed the same. Descendants
/// count, as a fence left open in a list item reaches past its item.
fn moved(old: &Node, new: &Node, offset: isize, lines: isize) -> bool {
    let (Some(o), Some(n)) = (old.position(), new.position()) else {
        return false;
    };
    let point = |a: &markdown::unist::Point, b: &markdown::unist::Point| {
        a.offset.wrapping_add_signed(offset) == b.offset
            && a.line.wrapping_add_signed(lines) == b.line
            && a.column == b.column
    };
    discriminant(old) == discriminant(new)
        && point(&o.start, &n.start)
        && point(&o.end, &n.end)
        && children(old).len() == children(new).len()
        && children(old)
            .iter()
            .zip(children(new))
            .all(|(o, n)| moved(o, n, offset, lines))
}

/// Where to cut `src` into pieces of about `piece` bytes: after a line break
/// that a blank line follows.
fn cuts(src: &str, piece: usize) -> Vec<usize> {
    let mut cuts = Vec::new();
    let mut done = 0;
    while done < src.len() {
        let at = (done + piece).min(src.len());
        // Bytes: `at` can fall inside a character.
        done = (src.as_bytes()[at..].windows(2))
            .position(|w| w == b"\n\n")
            .map_or(src.len(), |i| at + i + 1);
        cuts.push(done);
    }
    cuts
}

fn line_start(src: &str, pos: usize) -> usize {
    src[..pos].rfind('\n').map_or(0, |i| i + 1)
}

impl Tree {
    pub fn new(src: &str) -> Self {
        Self::build(src, PIECE)
    }

    fn build(src: &str, piece: usize) -> Self {
        let cuts = cuts(src, piece);
        // A definition decides how the references before it parse too, so
        // they are found first, from each piece parsed on its own.
        let mut found = Vec::new();
        if src.contains("]:") {
            let mut from = 0;
            for &to in &cuts {
                let Some(ast) = try_parse(&src[from..to]) else {
                    return Self::parsed(parse(src), src.len());
                };
                found.extend(definitions(children(&ast)));
                from = to;
            }
        }
        // `markdown` can panic on a piece that the whole text parses: then,
        // as when the pieces do not hold, the whole text is parsed at once.
        let built = stand_ins(&found).and_then(|stand_ins| {
            // Every piece is parsed after the stand-ins: with many, larger
            // pieces.
            let cuts = if stand_ins.len() * 2 > piece {
                self::cuts(src, stand_ins.len() * 2)
            } else {
                cuts
            };
            std::panic::catch_unwind(|| Self::pieces(src, &cuts, stand_ins, &found)).ok()?
        });
        built.unwrap_or_else(|| Self::parsed(parse(src), src.len()))
    }

    fn pieces(src: &str, cuts: &[usize], stand_ins: String, found: &Definitions) -> Option<Self> {
        let mut tree = Self {
            ast: parse(""),
            owed: Vec::new(),
            definitions: Some(stand_ins),
            parsed: 0,
            building: true,
        };
        let mut done = 0;
        for &next in cuts {
            let change = Change {
                start: done,
                old_end: done,
                new_end: next,
            };
            tree.update(&src[..next], change);
            done = next;
            if tree.parsed > 2 * src.len() {
                // The pieces keep parsing far back.
                return None;
            }
        }
        tree.building = false;
        // Not when a piece read a line in a fence that runs across pieces as
        // a definition, or the other way round.
        (definitions(children(&tree.ast)) == *found).then_some(tree)
    }

    fn parsed(ast: Node, len: usize) -> Self {
        Self {
            definitions: stand_ins(&definitions(children(&ast))),
            parsed: len,
            owed: vec![(0, 0); children(&ast).len()],
            ast,
            building: false,
        }
    }

    /// The tree, with only the top-level nodes' own positions sure to be
    /// current: read inside a node through [`Self::settled`].
    pub fn root(&self) -> &Node {
        &self.ast
    }

    /// The tree, with top-level nodes `range` current inside too.
    pub fn settled(&mut self, range: Range<usize>) -> &Node {
        let nodes = self.ast.children_mut().expect("a root has children");
        let end = range.end.min(nodes.len());
        let start = range.start.min(end);
        for (node, owed) in nodes[start..end].iter_mut().zip(&mut self.owed[start..end]) {
            let (offset, lines) = std::mem::take(owed);
            if offset != 0 || lines != 0 {
                for child in node.children_mut().into_iter().flatten() {
                    shift(child, offset, lines);
                }
            }
        }
        &self.ast
    }

    /// The whole tree, current.
    pub fn ast(&mut self) -> &Node {
        self.settled(0..self.owed.len())
    }

    /// Takes `change`, made to the text the tree was parsed from; `src` is
    /// the text now.
    pub fn update(&mut self, src: &str, change: Change) -> Splice {
        self.reparse_window(src, change)
            .unwrap_or_else(|| self.reparse_whole(src, change))
    }

    fn reparse_whole(&mut self, src: &str, change: Change) -> Splice {
        self.ast();
        let new = if self.building {
            // A piece markdown cannot parse: back to `build`, which parses
            // the whole text instead.
            try_parse(src).unwrap_or_else(|| std::panic::resume_unwind(Box::new(())))
        } else {
            // `markdown` takes time that grows faster than the text on tables.
            let mut built = Self::build(src, PIECE);
            built.ast();
            built.ast
        };
        let (old_nodes, new_nodes) = (children(&self.ast), children(&new));
        let line = |n: &Node| n.position().expect("positioned").end.line as isize;
        let lines = line(&new) - line(&self.ast);
        // A node a changed definition reads differently is not the same.
        let (old, new_range, keep) = narrow(old_nodes, new_nodes, change, lines);
        let splice = Splice {
            old,
            new: new_range,
            keep,
        };
        let parsed = self.parsed + src.len();
        *self = Self::parsed(new, parsed);
        splice
    }

    /// Parses the window around `change` again; `None` when the document must
    /// be parsed whole.
    fn reparse_window(&mut self, src: &str, change: Change) -> Option<Splice> {
        let delta = change.delta();
        let count = children(&self.ast).len();
        // Two unchanged nodes before the change and two after it. The last
        // node never counts as unchanged: a list at the end of the text ends
        // before its last line break and takes it once more text follows.
        let top = children(&self.ast);
        let mut first = top
            .partition_point(|n| span(n).end < change.start)
            .saturating_sub(2)
            .min(count.saturating_sub(3));
        let mut end = top.partition_point(|n| span(n).start <= change.old_end) + 2;
        // Where a node ends and the next starts, in the text now.
        let at = |p: usize| {
            if p > change.old_end {
                p.wrapping_add_signed(delta)
            } else {
                p
            }
        };
        // A list ends after its last line break, so that one counts too.
        let blank = |nodes: &[Node], i: usize| {
            let (a, b) = (at(span(&nodes[i - 1]).end), at(span(&nodes[i]).start));
            src[a..b].matches('\n').count() + usize::from(src[..a].ends_with('\n')) > 1
        };
        let fragment = loop {
            end = end.min(count);
            // And a blank line on each side: it resets nearly everything.
            let top = children(&self.ast);
            while first > 0 && !blank(top, first) {
                first -= 1;
            }
            while end < count && !blank(top, end) {
                end += 1;
            }
            let to_end = end == count;
            let (from, line) = if first == 0 {
                (0, 1)
            } else {
                let start = &top[first].position().expect("positioned").start;
                (line_start(src, start.offset), start.line)
            };
            // Up to the next node's line, with the blank line before it: a
            // list followed by one takes its last line break.
            let to = if to_end {
                src.len()
            } else {
                line_start(src, span(&top[end]).start.wrapping_add_signed(delta))
            };
            // The stand-ins go first and end with a blank line, so the window
            // starts as it would after any blank line.
            let lead = self.definitions.clone()?;
            let text = lead.clone() + &src[from..to];
            let mut fragment = if to - from > 4 * PIECE && !self.building {
                // A large window (a fence closed above much of the file, a
                // large paste) is parsed a piece at a time too.
                std::panic::catch_unwind(|| {
                    let mut built = Self::build(&text, PIECE);
                    built.ast();
                    built.ast
                })
                .ok()?
            } else {
                try_parse(&text)?
            };
            self.parsed += to - from;
            let parsed = fragment.children_mut().expect("a root has children");
            parsed.retain(|n| span(n).start >= lead.len());
            let lead_lines = lead.matches('\n').count() as isize;
            shift(
                &mut fragment,
                from as isize - lead.len() as isize,
                line as isize - 1 - lead_lines,
            );
            let nodes = children(self.settled(first..end));
            let new = children(&fragment);
            // What came before a node can change how the next one parses: a
            // paragraph just after a list ends at an empty list item, and a
            // list item that interrupts a paragraph reads its content
            // differently. So the window starts and ends with two nodes that
            // come back exactly as they were (moved, after the change).
            let head = first == 0 || new.get(..2) == nodes.get(first..first + 2);
            let tail = to_end
                || new.len() >= 2 && {
                    let (old, new) = (&nodes[end - 2..end], &new[new.len() - 2..]);
                    let line = |n: &Node| n.position().expect("positioned").start.line as isize;
                    let lines = line(&new[1]) - line(&old[1]);
                    moved(&old[0], &new[0], delta, lines) && moved(&old[1], &new[1], delta, lines)
                };
            if head && tail {
                break fragment;
            }
            let grow = (end - first).max(2);
            if !head {
                first = first.saturating_sub(grow);
            }
            if !tail {
                end += grow;
            }
        };
        let nodes = children(&self.ast);
        // A changed definition can change how any reference parses.
        if !self.building && definitions(&nodes[first..end]) != definitions(children(&fragment)) {
            return None;
        }
        let to_end = end == nodes.len();
        let fragment_end = fragment.position().expect("positioned").end.clone();
        let Node::Root(fragment) = fragment else {
            unreachable!("the parser returns a root");
        };
        let new = fragment.children;
        let lines = match (new.last(), nodes[..end].last()) {
            (Some(new), Some(old)) => {
                let line = |n: &Node| n.position().expect("positioned").end.line as isize;
                line(new) - line(old)
            }
            _ => 0,
        };
        let count = new.len();
        let (old_range, new_range, keep) = narrow(&nodes[first..end], &new, change, lines);
        let all = self.ast.children_mut().expect("a root has children");
        for (node, owed) in all[end..].iter_mut().zip(&mut self.owed[end..]) {
            shift_own(node, delta, lines);
            *owed = (owed.0 + delta, owed.1 + lines);
        }
        all.splice(first..end, new);
        self.owed
            .splice(first..end, std::iter::repeat_n((0, 0), count));
        if let Some(p) = self.ast.position_mut() {
            if to_end {
                p.end = fragment_end;
            } else {
                p.end.offset = src.len();
                p.end.line = p.end.line.wrapping_add_signed(lines);
            }
        }
        let at = |r: Range<usize>| first + r.start..first + r.end;
        Some(Splice {
            old: at(old_range),
            new: at(new_range),
            keep,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Document, EditKind};

    /// Applies each edit, keeping the tree up to date, and checks it against
    /// a fresh parse after every one.
    fn check(start: &str, edits: &[(usize, usize, &str)]) {
        let mut doc = Document::new(start);
        let mut tree = Tree::new(doc.text());
        for &(from, to, text) in edits {
            let (from, to) = (from.min(doc.text().len()), to.min(doc.text().len()));
            if !doc.text().is_char_boundary(from) || !doc.text().is_char_boundary(to) {
                continue;
            }
            let before = children(tree.ast()).len();
            let old_text = doc.text().to_owned();
            doc.replace(from..to, text, from + text.len(), EditKind::Other);
            let Some(change) = doc.take_change() else {
                continue;
            };
            // `markdown` itself panics on a few of these texts: stop there.
            let Some(fresh) = try_parse(doc.text()) else {
                return;
            };
            let splice = tree.update(doc.text(), change);
            assert_eq!(
                tree.ast(),
                &fresh,
                "after replacing {from}..{to} with {text:?} in {old_text:?}, giving {:?}",
                doc.text()
            );
            let mut built = Tree::build(doc.text(), 5);
            assert_eq!(built.ast(), &fresh, "built in pieces: {:?}", doc.text());
            let after = children(tree.ast()).len();
            assert_eq!(
                after as isize - before as isize,
                splice.new.len() as isize - splice.old.len() as isize
            );
        }
    }

    const DOC: &str = "# Title\n\nSome *text* here\nand more.\n\n- one\n- two\n\n  continued\n\n\
        > quote\n> more\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```rust\nfn x() {}\n```\n\n\
        Para\n\n---\n\nend\n";

    #[test]
    fn typing_anywhere_matches_a_fresh_parse() {
        for at in 0..=DOC.len() {
            check(DOC, &[(at, at, "x"), (at, at + 1, ""), (at, at, "\n")]);
        }
    }

    #[test]
    fn edits_that_change_block_boundaries_match_a_fresh_parse() {
        for at in 0..=DOC.len() {
            for text in [
                "```\n", "\n\n", "===\n", "- ", "> ", "|---|\n", "    ", "<div>\n",
            ] {
                check(DOC, &[(at, at, text)]);
            }
            check(DOC, &[(at, at + 3, ""), (at, at + 12, "")]);
        }
    }

    #[test]
    fn references_parse_as_the_definitions_say() {
        let text = "See [a][r] and [b].\n\nmore [^n]\n\n> [c]\n\n[r]: /x\n\n[^n]: note\n";
        for at in 0..=text.len() {
            check(text, &[(at, at, "x"), (at, at, "\n\n[b]: /y\n\n")]);
            check(text, &[(at, at, "[c]"), (at, at + 1, "")]);
        }
        check("[a]\n\nx\n", &[(4, 4, "[a]: /z\n\n")]);
        // In a footnote, and labels a stand-in cannot write back.
        let nested = "[^n]: note\n\n    [a]: /u\n\npara\n\nmore\n\n[a]\n";
        let odd = "[x\\]y] [^1]\n\n[x\\]y]: /u\n[^1]: /v\n\nmore\n\n[a\nb]\n\n> [a\n> b]: /w\n";
        for text in [nested, odd] {
            for at in 0..=text.len() {
                check(text, &[(at, at, "x"), (at, at + 1, "")]);
            }
        }
    }

    #[test]
    fn a_tree_built_a_piece_at_a_time_is_the_tree_parsed_whole() {
        let lists = "- a\n- b\n\n  c\n\n```\nopen\n\nfence\n\n";
        let defined = "[x] and [y]\n\n[x]: /u\n\n";
        let fenced = "[z]\n\n```\n\n[z]: /q\n\n```\n\n[^n]\n\n[^n]: note\n\n    more\n";
        let wide = "日本語の文章です。é\n\n".repeat(9) + &"é".repeat(40);
        for text in [
            DOC.repeat(4),
            lists.repeat(6),
            defined.repeat(5),
            fenced.repeat(3),
            wide,
            String::new(),
        ] {
            for piece in [1, 7, 30, 100] {
                let mut tree = Tree::build(&text, piece);
                assert_eq!(tree.ast(), &parse(&text), "{piece} at a time: {text:?}");
            }
        }
    }

    #[test]
    fn a_piece_markdown_cannot_parse_leaves_the_whole_text_parsed() {
        // `markdown` panics on this, so the whole text draws as plain lines.
        let bad = "~~~\n~~~\n   |a|b|\n---\n---\n|a|b|\n---\n";
        assert!(try_parse(bad).is_none());
        let filler = "p\n\n".repeat(30);
        let text = format!("{filler}{bad}\nq\n");
        let mut tree = Tree::build(&text, 8);
        assert_eq!(tree.ast(), &parse(&text));
    }

    #[test]
    fn a_fence_around_most_of_a_large_file_matches_a_fresh_parse() {
        let text = DOC.repeat(5 * PIECE / DOC.len());
        check(
            &text,
            &[(0, 0, "```\n"), (DOC.len() * 2, DOC.len() * 2, "```\n")],
        );
    }

    #[test]
    fn a_window_stops_at_the_blank_line_after_a_list() {
        // Keep a Changelog: headings right on their lists.
        let unit = "## [1.0]\n### Added\n- a\n- b\n\n### Fixed\n- c\n\n";
        let text = unit.repeat(50);
        let mut doc = Document::new(&text);
        let mut tree = Tree::new(&text);
        let at = unit.len() * 25 + unit.find("- b").unwrap() + 2;
        doc.replace(at..at, "x", at + 1, EditKind::Typing);
        let change = doc.take_change().unwrap();
        let splice = tree.update(doc.text(), change);
        assert!(splice.old.len() <= 2, "{splice:?}");
        assert!(
            tree.parsed < 2 * unit.len() + text.len(),
            "parsed {}",
            tree.parsed
        );
        assert_eq!(tree.ast(), &parse(doc.text()));
        // Built a piece at a time, each piece reaches back only a little.
        let text = unit.repeat(400);
        let built = Tree::build(&text, 1024);
        assert!(built.parsed < text.len() * 3 / 2, "parsed {}", built.parsed);
    }

    #[test]
    fn labels_with_escapes_stand_in() {
        let text = "[foo\\_bar] and [a&amp;b] and [Two\nWords]\n\n[foo\\_bar]: /u\n[a&amp;b]: /v\n[two words]: /w\n";
        assert!(Tree::new(text).definitions.is_some());
        for at in 0..=text.len() {
            check(text, &[(at, at, "x")]);
        }
    }

    #[test]
    fn an_edit_keeps_the_nodes_it_does_not_reach() {
        let text = "one\n\ntwo\n\nthree\n\nfour\n\nfive\n\nsix\n\nseven\n\neight\n";
        let mut doc = Document::new(text);
        let mut tree = Tree::new(text);
        let at = text.find("four").unwrap();
        doc.replace(at..at, "x", at + 1, EditKind::Typing);
        let change = doc.take_change().unwrap();
        let splice = tree.update(doc.text(), change);
        assert_eq!(
            splice,
            Splice {
                old: 3..5,
                new: 3..5,
                keep: true,
            },
            "the node edited, and the next for the blank lines before it"
        );
    }

    #[test]
    fn a_run_of_mixed_edits_matches_a_fresh_parse() {
        // Fixed pseudo-random sequences, so a failure reproduces.
        for seed in 1..=40u64 {
            mixed_edits(seed, DOC);
            mixed_edits(seed, "- a\n- b\n\nc\n\n> q\n\n1. x\n\n   y\n\nd\n");
            mixed_edits(seed, "[a] [b]\n\n- [c][a]\n\n[a]: /x\n\n> [b]: /y\n");
        }
    }

    fn mixed_edits(seed: u64, start: &str) {
        let mut seed = seed;
        let mut next = |n: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize % n.max(1)
        };
        let pieces = [
            "x", " ", "\n", "\n\n", "# ", "- ", "1. ", "> ", "```\n", "|", "|---|\n", "**", "    ",
            "***\n", "===\n", "<!-- ", " -->", "*\n", "[^1]", "[a]", "]: /z\n",
        ];
        let mut edits = Vec::new();
        let mut len = start.len();
        for _ in 0..400 {
            let at = next(len + 1);
            if next(3) == 0 {
                let to = at + next(8);
                edits.push((at, to, ""));
                len -= to.min(len) - at.min(len);
            } else {
                let piece = pieces[next(pieces.len())];
                edits.push((at, at, piece));
                len += piece.len();
            }
        }
        check(start, &edits);
    }
}
