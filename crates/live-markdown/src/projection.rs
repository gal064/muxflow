//! Source to display projection: what today's CodeMirror decorations do,
//! expressed as a pure function of (source, selection).
//!
//! `project` walks the Markdown AST and produces a flat list of text leaves.
//! A leaf's `text` is what is drawn; its `segs` say how each stretch of
//! display text maps back to source bytes. Hidden markers are `Hidden` segs
//! with no display text; atomic replacements (a list bullet, a soft line
//! break) are `Replaced` segs. Nothing here ever rewrites the source.

use std::ops::Range;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use markdown::mdast::{self, Node};
use markdown::unist::Position;
use markdown::{Constructs, ParseOptions};

use crate::table::{TableLayout, table_layout};

/// The tree as Tauri's parser has it; see [`crate::lists`]. The panics
/// known in markdown-rs are fixed in `vendor/markdown`; should another one
/// turn up, the source is drawn as its plain lines rather than taking the app
/// down, and the next edit that parses goes back to normal.
pub fn parse(src: &str) -> Node {
    parse_with(src, crate::lists::parse)
}

/// As [`parse`], but `None` where markdown-rs panics: a piece of a text can
/// panic where the whole parses.
pub(crate) fn try_parse(src: &str) -> Option<Node> {
    try_parse_with(src, crate::lists::parse)
}

fn parse_with(src: &str, parser: impl FnOnce(&str) -> Node + std::panic::UnwindSafe) -> Node {
    try_parse_with(src, parser).unwrap_or_else(|| plain_lines(src))
}

fn try_parse_with(
    src: &str,
    parser: impl FnOnce(&str) -> Node + std::panic::UnwindSafe,
) -> Option<Node> {
    quiet_parse_panics();
    PARSING.set(true);
    let ast = std::panic::catch_unwind(|| parser(src));
    PARSING.set(false);
    // Report the next stretch of unparsable text too.
    ast.inspect(|_| PANIC_REPORTED.store(false, Ordering::Relaxed))
        .ok()
}

static PANIC_REPORTED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static PARSING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Chains the panic hook once so a parse panic is reported once per stretch
/// of unparsable text: while the text stays, every key would print it again.
fn quiet_parse_panics() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !PARSING.get() || !PANIC_REPORTED.swap(true, Ordering::Relaxed) {
                previous(info);
            }
        }));
    });
}

/// Each line with text as a raw block, drawn as written, with nothing hidden.
fn plain_lines(src: &str) -> Node {
    let mut children = Vec::new();
    let mut at = 0;
    for (n, line) in src.split('\n').enumerate() {
        if !line.trim().is_empty() {
            children.push(Node::Html(mdast::Html {
                value: line.to_string(),
                position: Some(Position::new(
                    n + 1,
                    1,
                    at,
                    n + 1,
                    line.len() + 1,
                    at + line.len(),
                )),
            }));
        }
        at += line.len() + 1;
    }
    let lines = src.split('\n').count();
    let last = src.len() - src.rfind('\n').map_or(0, |i| i + 1);
    Node::Root(mdast::Root {
        children,
        position: Some(Position::new(1, 1, 0, lines, last + 1, src.len())),
    })
}

fn parse_inline(src: &str) -> Node {
    let mut options = ParseOptions::gfm();
    options.constructs = Constructs {
        block_quote: false,
        code_indented: false,
        code_fenced: false,
        heading_atx: false,
        heading_setext: false,
        html_flow: false,
        list_item: false,
        thematic_break: false,
        ..Constructs::gfm()
    };
    markdown::to_mdast(src, &options).expect("plain markdown cannot fail to parse")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegKind {
    /// Display bytes equal source bytes, one to one.
    Text,
    /// Source bytes with no display (a hidden marker).
    Hidden,
    /// Source bytes shown as other text (a bullet for `- `, a space for a soft
    /// break). Atomic for the caret.
    Replaced,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seg {
    pub src: Range<usize>,
    pub disp: Range<usize>,
    pub kind: SegKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: bool,
    pub underline: bool,
}

impl Style {
    fn is_default(&self) -> bool {
        *self == Style::default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leaf {
    /// Geometry key for gpui-base's `editor_host`.
    pub key: u64,
    pub src: Range<usize>,
    pub text: String,
    pub segs: Vec<Seg>,
    pub marks: Vec<(Range<usize>, Style)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

/// What a list item draws in its marker column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    Bullet,
    /// An ordered item's number, which stays text and editable.
    Number,
    /// A checkbox, drawn by the view; `at` is the source offset of its `[`.
    Task {
        checked: bool,
        at: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    Heading(u8),
    /// An item's text beside its marker column, so wrapped lines hang at the
    /// text, as Tauri's `cm-live-list` lines.
    ListItem {
        depth: usize,
        /// The marker column in characters: an ordered list's widest marker
        /// and a space, shared by its siblings; 0 for the default 1.5em.
        columns: usize,
        /// The item's first block holds the marker as its first leaf. `None`
        /// is a later paragraph of the item, drawn at the item's text.
        marker: Option<Marker>,
    },
    Code,
    Blank,
    Rule {
        revealed: bool,
    },
    Table {
        rows: Vec<Vec<usize>>,
        aligns: Vec<Align>,
    },
    Raw,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    /// Blockquote nesting depth, for the left rule.
    pub quote: usize,
    pub leaves: Vec<usize>,
    pub src: Range<usize>,
}

/// A table cell's editable source span and where it is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub leaf: usize,
    pub row: usize,
    pub col: usize,
    pub from: usize,
    /// Trimmed end of the cell text.
    pub to: usize,
    /// Last position the caret may take: the trimmed end, or just past
    /// whitespace typed there.
    pub limit: usize,
    /// A short row's cell, drawn empty as GFM draws it, with no source until
    /// it is typed into. `from`, `to` and `limit` are the row's end.
    pub missing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableInfo {
    pub src: Range<usize>,
    pub layout: TableLayout,
    /// Row by row, each row cut or padded to the header's width.
    pub cells: Vec<Cell>,
    /// No cell takes the caret, so the caret crosses the table whole.
    pub read_only: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projection {
    pub blocks: Vec<Block>,
    pub leaves: Vec<Leaf>,
    pub tables: Vec<TableInfo>,
    /// The missing cell holding the caret, as (table, cell) indices.
    pub missing: Option<(usize, usize)>,
}

/// What the projection needs to know about the editor state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Active {
    /// The selection; empty while the editor does not have focus, so
    /// everything renders.
    pub ranges: Vec<Range<usize>>,
    /// IME preedit range in source bytes.
    pub marked: Option<Range<usize>>,
    /// Just past whitespace the user typed at the end of a table cell, so the
    /// caret can stand there. Alignment padding is otherwise not a caret stop.
    pub cell_typed_end: Option<usize>,
    /// The missing cell the caret was put in, as (its row's end, column).
    pub missing: Option<(usize, usize)>,
    /// Tables are drawn but take no caret, as today's read-only widget.
    pub read_only: bool,
}

fn touches(active: &Active, from: usize, to: usize) -> bool {
    active.ranges.iter().any(|r| r.start <= to && r.end >= from)
}

fn line_start(src: &str, pos: usize) -> usize {
    src[..pos].rfind('\n').map_or(0, |i| i + 1)
}

fn line_end(src: &str, pos: usize) -> usize {
    src[pos..].find('\n').map_or(src.len(), |i| pos + i)
}

fn pos(node: &Node) -> Range<usize> {
    let p = node
        .position()
        .expect("parser positions are always present");
    p.start.offset..p.end.offset
}

struct LeafB {
    leaf: Leaf,
}

impl LeafB {
    fn visible(&mut self, src: &str, r: Range<usize>, style: &Style) {
        if r.is_empty() {
            return;
        }
        let start = self.leaf.text.len();
        self.leaf.text.push_str(&src[r.clone()]);
        let disp = start..self.leaf.text.len();
        self.leaf.segs.push(Seg {
            src: r,
            disp: disp.clone(),
            kind: SegKind::Text,
        });
        if !style.is_default() {
            self.leaf.marks.push((disp, style.clone()));
        }
    }

    fn hidden(&mut self, r: Range<usize>) {
        if r.is_empty() {
            return;
        }
        let at = self.leaf.text.len();
        self.leaf.segs.push(Seg {
            src: r,
            disp: at..at,
            kind: SegKind::Hidden,
        });
    }

    fn replaced(&mut self, r: Range<usize>, shown: &str, style: &Style) {
        let start = self.leaf.text.len();
        self.leaf.text.push_str(shown);
        let disp = start..self.leaf.text.len();
        self.leaf.segs.push(Seg {
            src: r,
            disp: disp.clone(),
            kind: SegKind::Replaced,
        });
        if !style.is_default() {
            self.leaf.marks.push((disp, style.clone()));
        }
    }
}

struct Builder<'a> {
    src: &'a str,
    active: &'a Active,
    blocks: Vec<Block>,
    leaves: Vec<Leaf>,
    tables: Vec<TableInfo>,
    /// Source between a line start and the block that follows: hidden at the
    /// front of the next leaf (`> ` before a quoted paragraph, an item's
    /// indentation).
    pending_prefix: Option<Range<usize>>,
    quote: usize,
    /// The marker column of the list item being drawn, for its later paragraphs.
    columns: usize,
    /// Set while drawing a cell the caret is not in: a selection passing over
    /// a table leaves it rendered.
    rendered_only: bool,
}

/// The whole document, as it is drawn.
pub fn project(src: &str, ast: &Node, active: &Active) -> Projection {
    project_window(src, ast, active, None)
}

/// Only the top-level blocks around `around`, plus one on each side: enough to
/// answer what the caret can do there (stops, atomic markers, the cell or
/// block it is in) without projecting a long document for every query.
/// Reveal depends only on what the selection touches, so these blocks project
/// exactly as they do in the whole document; their leaf keys differ.
pub fn project_near(src: &str, ast: &Node, active: &Active, around: Range<usize>) -> Projection {
    project_window(src, ast, active, Some(around))
}

fn project_window(
    src: &str,
    ast: &Node,
    active: &Active,
    around: Option<Range<usize>>,
) -> Projection {
    let range = match &around {
        Some(around) => window(ast, around),
        None => 0..ast.children().map_or(0, Vec::len),
    };
    project_children(src, ast, active, range)
}

/// The top-level nodes [`project_near`] projects for `around`. Reads only their
/// own positions.
pub fn window(ast: &Node, around: &Range<usize>) -> Range<usize> {
    let children = ast.children().map_or(&[][..], |c| &c[..]);
    children
        .partition_point(|node| pos(node).end < around.start)
        .saturating_sub(1)
        ..(children.partition_point(|node| pos(node).start <= around.end) + 1).min(children.len())
}

/// The top-level nodes `range` of `ast`, each with the blank lines before it,
/// and when the range reaches the end, the lines after the last node.
pub fn project_children(src: &str, ast: &Node, active: &Active, range: Range<usize>) -> Projection {
    let mut b = Builder {
        src,
        active,
        blocks: Vec::new(),
        leaves: Vec::new(),
        tables: Vec::new(),
        pending_prefix: None,
        quote: 0,
        columns: 0,
        rendered_only: false,
    };
    let children = ast.children().map_or(&[][..], |c| &c[..]);
    let (first, last) = (range.start, range.end);
    let mut cursor = if first == 0 {
        0
    } else {
        pos(&children[first - 1]).end
    };
    let start = cursor;
    for node in &children[first..last] {
        let p = pos(node);
        b.gap_lines(cursor..p.start);
        b.block(node, 0);
        // A quote's last bare `>` line is left as a prefix: it is the node's
        // own, so it is drawn with it, hidden, as a line of the quote.
        if let Some(prefix) = b.pending_prefix.clone() {
            b.quote = src[prefix.clone()].matches('>').count();
            b.point_block(prefix.end, BlockKind::Blank, prefix);
            b.quote = 0;
        }
        cursor = p.end;
    }
    if last == children.len() {
        b.gap_lines(cursor..src.len());
        // A last line of only indentation or quote markers (`> ` after Enter
        // in a quote, `  ` after Enter ends a nested list) still takes the
        // caret, past them.
        let open_line = b.pending_prefix.as_ref().is_some_and(|r| {
            r.end == src.len() && (r.start == 0 || src.as_bytes()[r.start - 1] == b'\n')
        });
        if !open_line {
            b.pending_prefix = None;
        }
        if open_line || src.is_empty() || src.ends_with('\n') {
            b.point_block(src.len(), BlockKind::Blank, src.len()..src.len());
        }
    }
    let missing = active.missing.and_then(|(at, col)| {
        b.tables.iter().enumerate().find_map(|(t, table)| {
            let found = |c: &Cell| c.missing && c.from == at && c.col == col;
            table.cells.iter().position(found).map(|c| (t, c))
        })
    });
    if b.blocks.is_empty() {
        // Every run of nodes draws something, for the caret and the viewport.
        let end = if last == children.len() {
            src.len()
        } else {
            cursor
        };
        b.pending_prefix = Some(start..end);
        b.point_block(end, BlockKind::Blank, start..end);
    }
    Projection {
        blocks: b.blocks,
        leaves: b.leaves,
        tables: b.tables,
        missing,
    }
}

impl<'a> Builder<'a> {
    fn touching(&self, from: usize, to: usize) -> bool {
        !self.rendered_only && touches(self.active, from, to)
    }

    fn start_leaf(&mut self, start: usize) -> LeafB {
        let mut leaf = LeafB {
            leaf: Leaf {
                key: self.leaves.len() as u64 + 1,
                src: start..start,
                text: String::new(),
                segs: Vec::new(),
                marks: Vec::new(),
            },
        };
        if let Some(prefix) = self.pending_prefix.take() {
            leaf.leaf.src.start = prefix.start;
            leaf.hidden(prefix);
        }
        leaf
    }

    fn finish_leaf(&mut self, l: LeafB, end: usize) -> usize {
        self.finish(l, end, true)
    }

    /// `stop`: a leaf with no visible source still takes the caret at its end.
    /// A bullet's or a task's marker leaf does not: the caret there belongs to
    /// the item's text.
    fn finish(&mut self, mut l: LeafB, end: usize, stop: bool) -> usize {
        l.leaf.src.end = end;
        if stop && !l.leaf.segs.iter().any(|s| s.kind == SegKind::Text) {
            let at = l.leaf.text.len();
            l.leaf.segs.push(Seg {
                src: end..end,
                disp: at..at,
                kind: SegKind::Text,
            });
        }
        if let Some(marked) = &self.active.marked {
            let underline = Style {
                underline: true,
                ..Style::default()
            };
            for seg in &l.leaf.segs {
                let from = marked.start.max(seg.src.start);
                let to = marked.end.min(seg.src.end);
                if seg.kind == SegKind::Text && from < to {
                    let disp = seg.disp.start + (from - seg.src.start)
                        ..seg.disp.start + (to - seg.src.start);
                    l.leaf.marks.push((disp, underline.clone()));
                }
            }
        }
        self.leaves.push(l.leaf);
        self.leaves.len() - 1
    }

    fn push_block(&mut self, kind: BlockKind, leaves: Vec<usize>, src: Range<usize>) {
        self.blocks.push(Block {
            kind,
            quote: self.quote,
            leaves,
            src,
        });
    }

    fn point_block(&mut self, at: usize, kind: BlockKind, src: Range<usize>) {
        let l = self.start_leaf(at);
        let idx = self.finish_leaf(l, at);
        self.push_block(kind, vec![idx], src);
    }

    /// Source before the next leaf, hidden at its front. A nested quote's
    /// markers come in pieces, one per level.
    fn prefix(&mut self, r: Range<usize>) {
        self.pending_prefix = Some(match self.pending_prefix.take() {
            Some(prev) if prev.end == r.start => prev.start..r.end,
            _ => r,
        });
    }

    /// Blank and otherwise unclaimed whole lines inside `range`. The tail of a
    /// line before the next block is kept as that block's prefix.
    fn gap_lines(&mut self, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        let src = self.src;
        let mut p = range.start;
        if p != 0 && src.as_bytes()[p - 1] != b'\n' {
            match src[p..range.end].find('\n') {
                None => {
                    self.prefix(p..range.end);
                    return;
                }
                Some(i) => p += i + 1,
            }
        }
        while p < range.end {
            // A last line of indentation alone that the range stops just
            // short of ending (Enter on an empty item between two others
            // leaves one) is a blank line of its own, not the next leaf's.
            let nl = src[p..range.end].find('\n').map(|i| p + i).or_else(|| {
                (src[range.end..].starts_with('\n')
                    && src[p..range.end].trim_matches([' ', '\t', '>']).is_empty())
                .then_some(range.end)
            });
            let Some(nl) = nl else {
                self.prefix(p..range.end);
                return;
            };
            let line = &src[p..nl];
            let quote_only = line
                .trim_matches(|c: char| c == '>' || c == ' ' || c == '\t')
                .is_empty();
            let mut l = self.start_leaf(p);
            if quote_only {
                l.hidden(p..nl);
                let idx = self.finish_leaf(l, nl);
                self.push_block(BlockKind::Blank, vec![idx], p..nl);
            } else {
                l.visible(src, p..nl, &Style::default());
                let idx = self.finish_leaf(l, nl);
                self.push_block(BlockKind::Raw, vec![idx], p..nl);
            }
            p = nl + 1;
        }
    }

    fn inlines(&self, l: &mut LeafB, nodes: &[Node], range: Range<usize>, style: &Style) {
        let mut cursor = range.start;
        for node in nodes {
            let p = pos(node);
            // Text before `range` belongs to a marker (a task box's spaces).
            if p.end <= range.start && !p.is_empty() {
                continue;
            }
            if p.start > cursor {
                self.text(l, cursor..p.start, style);
            }
            match node {
                Node::Text(_) => self.text(l, p.start.max(range.start)..p.end, style),
                Node::Strong(n) => self.wrapped(
                    l,
                    p.clone(),
                    &n.children,
                    &Style {
                        bold: true,
                        ..style.clone()
                    },
                    style,
                ),
                Node::Emphasis(n) => self.wrapped(
                    l,
                    p.clone(),
                    &n.children,
                    &Style {
                        italic: true,
                        ..style.clone()
                    },
                    style,
                ),
                Node::Delete(n) => self.wrapped(
                    l,
                    p.clone(),
                    &n.children,
                    &Style {
                        strike: true,
                        ..style.clone()
                    },
                    style,
                ),
                Node::InlineCode(_) => self.inline_code(l, p.clone(), style),
                Node::Link(n) => self.wrapped(
                    l,
                    p.clone(),
                    &n.children,
                    &Style {
                        link: true,
                        ..style.clone()
                    },
                    style,
                ),
                Node::LinkReference(n) if n.reference_kind != mdast::ReferenceKind::Shortcut => {
                    self.wrapped(
                        l,
                        p.clone(),
                        &n.children,
                        &Style {
                            link: true,
                            ..style.clone()
                        },
                        style,
                    )
                }
                // `[text]` alone stays text, brackets and all, as in Tauri;
                // what it holds is still drawn.
                Node::LinkReference(n) => match (n.children.first(), n.children.last()) {
                    (Some(first), Some(last)) => {
                        let (first, last) = (pos(first).start, pos(last).end);
                        l.visible(self.src, p.start..first, style);
                        self.inlines(l, &n.children, first..last, style);
                        l.visible(self.src, last..p.end, style);
                    }
                    _ => l.visible(self.src, p.clone(), style),
                },
                Node::Image(_) => self.image(l, p.clone(), style),
                Node::ImageReference(n) if n.reference_kind != mdast::ReferenceKind::Shortcut => {
                    self.image(l, p.clone(), style)
                }
                Node::Break(_) => self.hard_break(l, p.clone(), style),
                _ => l.visible(self.src, p.clone(), style),
            }
            cursor = p.end;
        }
        if cursor < range.end {
            self.text(l, cursor..range.end, style);
        }
    }

    /// Plain text: backslash escapes hide their backslash (unless the caret
    /// is on the escape) and a soft line break is drawn as a space.
    fn text(&self, l: &mut LeafB, range: Range<usize>, style: &Style) {
        let src = self.src;
        let bytes = src.as_bytes();
        let mut run = range.start;
        let mut i = range.start;
        // A line's indentation and quote markers after a hard break.
        if i > 0 && bytes[i - 1] == b'\n' {
            while i < range.end && matches!(bytes[i], b' ' | b'\t' | b'>') {
                i += 1;
            }
            l.hidden(run..i);
            run = i;
        }
        while i < range.end {
            match bytes[i] {
                b'\\' if i + 1 < range.end && bytes[i + 1].is_ascii_punctuation() => {
                    l.visible(src, run..i, style);
                    if self.touching(i, i + 2) {
                        l.visible(src, i..i + 2, style);
                    } else {
                        l.hidden(i..i + 1);
                        l.visible(src, i + 1..i + 2, style);
                    }
                    i += 2;
                    run = i;
                }
                b'\n' => {
                    // A single trailing space goes with the break, as in Tauri,
                    // unless the caret is just past it: CodeMirror leaves a
                    // caret where typing put it, where this would move it on.
                    let space = if self.touching(i, i) {
                        i
                    } else {
                        run + src[run..i].trim_end_matches(' ').len()
                    };
                    l.visible(src, run..space, style);
                    let mut j = i + 1;
                    while j < range.end && matches!(bytes[j], b' ' | b'\t' | b'>') {
                        j += 1;
                    }
                    l.replaced(space..j, " ", style);
                    i = j;
                    run = i;
                }
                _ => i += 1,
            }
        }
        l.visible(src, run..range.end, style);
    }

    fn hard_break(&self, l: &mut LeafB, range: Range<usize>, style: &Style) {
        let nl = self.src[range.clone()]
            .find('\n')
            .map_or(range.end, |i| range.start + i);
        l.hidden(range.start..nl);
        if nl < range.end {
            l.visible(self.src, nl..nl + 1, style);
            l.hidden(nl + 1..range.end);
        }
    }

    fn wrapped(
        &self,
        l: &mut LeafB,
        range: Range<usize>,
        children: &[Node],
        inner: &Style,
        outer: &Style,
    ) {
        let (first, last) = match (children.first(), children.last()) {
            (Some(f), Some(n)) => (pos(f).start, pos(n).end),
            _ => {
                l.visible(self.src, range, outer);
                return;
            }
        };
        let active = self.touching(range.start, range.end);
        self.marker(l, range.start..first, active, outer);
        self.inlines(l, children, first..last, inner);
        self.marker(l, last..range.end, active, outer);
    }

    fn marker(&self, l: &mut LeafB, range: Range<usize>, shown: bool, style: &Style) {
        if shown {
            l.visible(self.src, range, style);
        } else {
            l.hidden(range);
        }
    }

    /// No picture: the alt text, styled like a link, with `![` and the target
    /// hidden until the caret enters the image.
    fn image(&self, l: &mut LeafB, range: Range<usize>, style: &Style) {
        let alt = range.start + 2..label_end(self.src, range.start + 2, range.end);
        let active = self.touching(range.start, range.end);
        let link = Style {
            link: true,
            ..style.clone()
        };
        self.marker(l, range.start..alt.start, active, style);
        l.visible(self.src, alt.clone(), &link);
        self.marker(l, alt.end..range.end, active, style);
    }

    fn inline_code(&self, l: &mut LeafB, range: Range<usize>, style: &Style) {
        let text = &self.src[range.clone()];
        let ticks = text.bytes().take_while(|b| *b == b'`').count();
        let active = self.touching(range.start, range.end);
        let inner = Style {
            code: true,
            ..style.clone()
        };
        // Revealed backticks sit inside the chip, as Tauri's mark spans them.
        self.marker(l, range.start..range.start + ticks, active, &inner);
        l.visible(self.src, range.start + ticks..range.end - ticks, &inner);
        self.marker(l, range.end - ticks..range.end, active, &inner);
    }

    fn block(&mut self, node: &Node, depth: usize) {
        let p = pos(node);
        match node {
            Node::Paragraph(n) => {
                let mut l = self.start_leaf(p.start);
                self.inlines(&mut l, &n.children, p.clone(), &Style::default());
                let idx = self.finish_leaf(l, p.end);
                let kind = if depth > 0 {
                    BlockKind::ListItem {
                        depth: depth - 1,
                        columns: self.columns,
                        marker: None,
                    }
                } else {
                    BlockKind::Paragraph
                };
                self.push_block(kind, vec![idx], p);
            }
            Node::Heading(n) => self.heading(p, n),
            Node::ThematicBreak(_) => {
                let shown = touches(self.active, p.start, p.end);
                let mut l = self.start_leaf(p.start);
                if shown {
                    l.visible(self.src, p.clone(), &Style::default());
                } else {
                    l.hidden(p.clone());
                }
                let idx = self.finish_leaf(l, p.end);
                self.push_block(BlockKind::Rule { revealed: shown }, vec![idx], p);
            }
            Node::Code(_) => self.code(p),
            Node::Table(n) => self.table(p, n),
            Node::List(n) => {
                // Siblings share one marker column, wide enough for the
                // longest number and a space.
                let columns = if n.ordered {
                    // A checkbox after a number takes about two more.
                    let task = n
                        .children
                        .iter()
                        .any(|item| matches!(item, Node::ListItem(i) if i.checked.is_some()));
                    n.children
                        .iter()
                        .map(|item| {
                            let at = pos(item).start;
                            number_len(self.src, at + blank_len(&self.src[at..])) + 1
                        })
                        .max()
                        .unwrap_or(0)
                        + if task { 2 } else { 0 }
                } else {
                    0
                };
                let outer = self.columns;
                let mut cursor = p.start;
                for item in &n.children {
                    let ip = pos(item);
                    self.gap_lines(cursor..ip.start);
                    if let Node::ListItem(it) = item {
                        self.columns = columns;
                        self.list_item(ip.clone(), it, n.ordered, depth);
                    }
                    cursor = ip.end;
                }
                self.columns = outer;
                self.gap_lines(cursor..p.end);
            }
            Node::Blockquote(n) => {
                self.quote += 1;
                let mut cursor = p.start;
                for child in &n.children {
                    let cp = pos(child);
                    self.gap_lines(cursor..cp.start);
                    self.block(child, depth);
                    cursor = cp.end;
                }
                self.gap_lines(cursor..p.end);
                self.quote -= 1;
            }
            _ => {
                let mut l = self.start_leaf(p.start);
                l.visible(self.src, p.clone(), &Style::default());
                let idx = self.finish_leaf(l, p.end);
                self.push_block(BlockKind::Raw, vec![idx], p);
            }
        }
    }

    fn heading(&mut self, p: Range<usize>, n: &mdast::Heading) {
        let active = touches(self.active, p.start, p.end);
        let underline = line_start(self.src, p.end);
        if underline > p.start
            && let Some(last) = n.children.last()
        {
            return self.setext(p, n, pos(last).end, underline, active);
        }
        let mut l = self.start_leaf(p.start);
        match (n.children.first(), n.children.last()) {
            (Some(f), Some(e)) => {
                let (first, last) = (pos(f).start, pos(e).end);
                self.marker(&mut l, p.start..first, active, &Style::default());
                self.inlines(&mut l, &n.children, first..last, &Style::default());
                self.marker(&mut l, last..p.end, active, &Style::default());
            }
            _ => self.marker(&mut l, p.clone(), active, &Style::default()),
        }
        let idx = self.finish_leaf(l, p.end);
        self.push_block(BlockKind::Heading(n.depth), vec![idx], p);
    }

    /// A setext heading's underline is a line of its own: hidden away from
    /// the caret, and shown as plain text while the heading is edited, as in
    /// Tauri, rather than drawn as a second heading line.
    fn setext(
        &mut self,
        p: Range<usize>,
        n: &mdast::Heading,
        text_end: usize,
        underline: usize,
        active: bool,
    ) {
        let mut l = self.start_leaf(p.start);
        self.inlines(&mut l, &n.children, p.start..text_end, &Style::default());
        if !active {
            l.hidden(text_end..p.end);
            let idx = self.finish_leaf(l, p.end);
            return self.push_block(BlockKind::Heading(n.depth), vec![idx], p);
        }
        let idx = self.finish_leaf(l, text_end);
        self.push_block(BlockKind::Heading(n.depth), vec![idx], p.start..text_end);
        // Quote markers before the underline stay hidden, as on any line.
        let rule = underline
            + self.src[underline..p.end]
                .find(|c: char| !matches!(c, ' ' | '\t' | '>'))
                .unwrap_or(0);
        let mut l = self.start_leaf(underline);
        l.hidden(underline..rule);
        l.visible(self.src, rule..p.end, &Style::default());
        let idx = self.finish_leaf(l, p.end);
        self.push_block(BlockKind::Raw, vec![idx], underline..p.end);
    }

    fn code(&mut self, p: Range<usize>) {
        let src = self.src;
        let text = &src[p.clone()];
        let plain = Style::default();
        let mut l = self.start_leaf(p.start);
        let fence = text.bytes().next().filter(|b| matches!(b, b'`' | b'~'));
        match fence {
            Some(fence) if !touches(self.active, p.start, p.end) => {
                let ticks = text.bytes().take_while(|b| *b == fence).count();
                let first_end = text.find('\n').map_or(p.end, |i| p.start + i);
                let info = src[p.start + ticks..first_end].trim();
                l.hidden(p.start..p.start + ticks);
                let last_start = line_start(src, p.end);
                let closed = last_start > first_end
                    && src[last_start..p.end]
                        .trim_start()
                        .starts_with(fence as char);
                if info.is_empty() {
                    l.hidden(p.start + ticks..(first_end + 1).min(p.end));
                } else {
                    l.visible(src, p.start + ticks..first_end, &plain);
                    if first_end < p.end {
                        l.visible(src, first_end..first_end + 1, &plain);
                    }
                }
                if first_end < p.end {
                    let body_end = if closed {
                        last_start.saturating_sub(1).max(first_end + 1)
                    } else {
                        p.end
                    };
                    if body_end > first_end + 1 {
                        l.visible(src, first_end + 1..body_end, &plain);
                    }
                    // The closing fence keeps its row, empty: entering the
                    // block must not move what is below it.
                    if closed {
                        l.visible(src, body_end..last_start, &plain);
                        l.hidden(last_start..p.end);
                    }
                }
            }
            _ => l.visible(src, p.clone(), &plain),
        }
        let idx = self.finish_leaf(l, p.end);
        self.push_block(BlockKind::Code, vec![idx], p);
    }

    fn list_item(&mut self, p: Range<usize>, n: &mdast::ListItem, ordered: bool, depth: usize) {
        let src = self.src;
        // markdown-rs can end an empty nested item past its line break, over
        // the blank lines after it. The marker stops at its own line, so the
        // caret after it stays there and those blank lines draw as rows.
        let marker_end = n
            .children
            .first()
            .map_or(line_end(src, p.start).min(p.end), |c| pos(c).start);
        let plain = Style::default();
        // The marker is a leaf of its own, drawn in the marker column.
        let mut m = self.start_leaf(p.start);
        // A top-level item's range takes its indentation.
        let mark = p.start + blank_len(&src[p.start..marker_end]);
        let bracket = match n.checked {
            Some(_) => src[p.start..marker_end]
                .find('[')
                .map_or(p.start, |i| p.start + i),
            None => marker_end,
        };
        // As Tauri's task widget, the box takes the spaces after it.
        let text_start = match n.checked {
            Some(_) => (bracket + 3 + blank_len(&src[(bracket + 3).min(p.end)..p.end])).min(p.end),
            None => marker_end,
        };
        if ordered {
            // The number is text; the spaces after it are one atomic
            // stretch, so the caret has a stop at each side of it.
            let number = mark + number_len(src, mark);
            m.hidden(p.start..mark);
            m.visible(src, mark..number, &plain);
            if number < bracket {
                m.replaced(number..bracket, "", &plain);
            }
        } else if n.checked.is_some() {
            m.replaced(p.start..bracket, "", &plain);
        } else {
            m.replaced(p.start..marker_end, "\u{2022}", &plain);
        }
        // The list marker and the checkbox are separate atomic stretches, so
        // Backspace takes the checkbox first, then the bullet (today's
        // behavior). The view draws the box over the space left for it.
        let marker = match n.checked {
            Some(checked) => {
                m.replaced(bracket..text_start, TASK_SPACE, &plain);
                Marker::Task {
                    checked,
                    at: bracket,
                }
            }
            None if ordered => Marker::Number,
            None => Marker::Bullet,
        };
        let marker_leaf = self.finish(m, text_start, ordered);
        let kind = BlockKind::ListItem {
            depth,
            columns: self.columns,
            marker: Some(marker),
        };
        let mut l = self.start_leaf(text_start);
        let mut rest = &n.children[..];
        if let Some(Node::Paragraph(para)) = n.children.first() {
            let para_range = pos(&n.children[0]);
            self.inlines(
                &mut l,
                &para.children,
                text_start.max(para_range.start)..para_range.end,
                &Style::default(),
            );
            let idx = self.finish_leaf(l, para_range.end);
            self.push_block(kind, vec![marker_leaf, idx], p.start..para_range.end);
            rest = &n.children[1..];
        } else {
            let idx = self.finish_leaf(l, text_start);
            self.push_block(kind, vec![marker_leaf, idx], p.start..text_start);
        }
        let mut cursor = rest.first().map_or(p.end, |c| {
            let start = pos(c).start;
            self.gap_lines(n.children.first().map_or(p.start, |f| pos(f).end)..start);
            start
        });
        for (i, child) in rest.iter().enumerate() {
            if i > 0 {
                let cp = pos(child);
                self.gap_lines(cursor..cp.start);
            }
            self.block(child, depth + 1);
            cursor = pos(child).end;
        }
        // An item can end in a line of its indentation alone (Enter on an
        // empty nested item leaves one), which takes the caret.
        let last = n.children.last().map_or(text_start, |c| pos(c).end);
        self.gap_lines(last.max(text_start)..p.end);
    }

    fn table(&mut self, p: Range<usize>, n: &mdast::Table) {
        let src = self.src;
        let start = line_start(src, p.start);
        // A table inside a quote or list item shares its line with markers
        // table_layout would read as cells. Today's tableRanges leaves these
        // as source too.
        if !src[start..p.start].trim().is_empty() {
            let mut l = self.start_leaf(p.start);
            l.visible(src, p.clone(), &Style::default());
            let idx = self.finish_leaf(l, p.end);
            self.push_block(BlockKind::Raw, vec![idx], p);
            return;
        }
        let end = line_end(src, p.end);
        let layout = table_layout(&src[start..end], start);
        let aligns = n
            .align
            .iter()
            .map(|a| match a {
                mdast::AlignKind::Left => Align::Left,
                mdast::AlignKind::Center => Align::Center,
                mdast::AlignKind::Right => Align::Right,
                mdast::AlignKind::None => Align::None,
            })
            .collect();
        let read_only = self.active.read_only;
        let columns = layout.rows.first().map_or(0, |r| r.cells.len());
        let mut rows = Vec::new();
        let mut cells = Vec::new();
        for (row, span) in layout.rows.iter().enumerate() {
            let mut leaf_row = Vec::new();
            // A caret in this row's missing cell is at the row's end, which
            // can also be the last real cell's end: that cell stays rendered.
            let reveal = !read_only && self.active.missing.is_none_or(|(at, _)| at != span.end);
            // As GFM draws a row: cut or padded to the header's width.
            for col in 0..columns {
                let Some(cell) = span.cells.get(col) else {
                    let active = self.active.missing == Some((span.end, col));
                    let leaf = self.missing_cell(span.end, active);
                    leaf_row.push(leaf);
                    cells.push(Cell {
                        leaf,
                        row,
                        col,
                        from: span.end,
                        to: span.end,
                        limit: span.end,
                        missing: true,
                    });
                    continue;
                };
                let bytes = src.as_bytes();
                let mut inner_end = cell.to;
                while inner_end < span.to && bytes[inner_end] == b' ' {
                    inner_end += 1;
                }
                let limit = match self.active.cell_typed_end {
                    Some(end) if end > cell.to && end <= inner_end => end,
                    _ => cell.to,
                };
                let leaf = self.table_cell(cell.from, cell.to, limit, reveal);
                if read_only {
                    // Drawn and selectable, never a caret stop.
                    for seg in &mut self.leaves[leaf].segs {
                        if seg.kind == SegKind::Text {
                            seg.kind = SegKind::Replaced;
                        }
                    }
                }
                leaf_row.push(leaf);
                cells.push(Cell {
                    leaf,
                    row,
                    col,
                    from: cell.from,
                    to: cell.to,
                    limit,
                    missing: false,
                });
            }
            rows.push(leaf_row);
        }
        let leaves = rows.iter().flatten().copied().collect();
        self.push_block(BlockKind::Table { rows, aligns }, leaves, start..end);
        self.tables.push(TableInfo {
            src: start..end,
            layout,
            cells,
            read_only,
        });
    }

    /// An empty leaf for a short row's missing cell: a caret stop only while
    /// the caret is put there, since it has no source of its own.
    fn missing_cell(&mut self, at: usize, active: bool) -> usize {
        let segs = if active {
            vec![Seg {
                src: at..at,
                disp: 0..0,
                kind: SegKind::Text,
            }]
        } else {
            Vec::new()
        };
        self.leaves.push(Leaf {
            key: self.leaves.len() as u64 + 1,
            src: at..at,
            text: String::new(),
            segs,
            marks: Vec::new(),
        });
        self.leaves.len() - 1
    }

    fn table_cell(&mut self, from: usize, to: usize, limit: usize, reveal: bool) -> usize {
        let src = self.src;
        let mut l = self.start_leaf(from);
        let plain = Style::default();
        if reveal
            && self
                .active
                .ranges
                .iter()
                .any(|r| r.start >= from && r.end <= limit)
        {
            // The cell holding the caret or selection shows its own source.
            l.visible(src, from..limit, &plain);
        } else if from < to {
            let ast = parse_inline(&src[from..to]);
            let inner = ast
                .children()
                .and_then(|c| c.first())
                .and_then(|p| p.children())
                .map_or(&[][..], |c| &c[..]);
            let offset = |node: &Node| {
                let mut node = node.clone();
                shift_positions(&mut node, from);
                node
            };
            let shifted: Vec<Node> = inner.iter().map(offset).collect();
            self.rendered_only = true;
            self.inlines(&mut l, &shifted, from..to, &plain);
            self.rendered_only = false;
        }
        self.finish_leaf(l, limit.max(from))
    }
}

/// What a task's marker leaf shows: an em space, as wide as the box the view
/// draws over it, so the box has a place in the text's geometry.
pub const TASK_SPACE: &str = "\u{2003}";

/// Leading spaces and tabs.
fn blank_len(text: &str) -> usize {
    text.len() - text.trim_start_matches([' ', '\t']).len()
}

/// The length of an ordered marker (`12.` or `3)`) at `at`; 0 for a bullet.
fn number_len(src: &str, at: usize) -> usize {
    let digits = src[at..].bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && matches!(src.as_bytes().get(at + digits), Some(b'.' | b')')) {
        digits + 1
    } else {
        0
    }
}

/// The `]` closing a label that starts at `from`, skipping escapes and nested
/// brackets; `end` if there is none before it.
fn label_end(src: &str, from: usize, end: usize) -> usize {
    let bytes = src.as_bytes();
    let mut depth = 0usize;
    let mut i = from;
    while i < end {
        match bytes[i] {
            b'\\' => i += 1,
            b'[' => depth += 1,
            b']' if depth == 0 => return i,
            b']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    end
}

fn shift_positions(node: &mut Node, by: usize) {
    if let Some(p) = node.position_mut() {
        p.start.offset += by;
        p.end.offset += by;
    }
    if let Some(children) = node.children_mut() {
        for child in children {
            shift_positions(child, by);
        }
    }
}

impl Projection {
    /// The leaf and display offset that show source position `pos`, if the
    /// position is visible (a caret stop).
    pub fn locate(&self, pos: usize) -> Option<(usize, usize)> {
        if let Some(cell) = self.missing_cell()
            && cell.from == pos
        {
            return Some((cell.leaf, 0));
        }
        for (i, leaf) in self.leaves.iter().enumerate() {
            if pos < leaf.src.start || pos > leaf.src.end {
                continue;
            }
            for seg in &leaf.segs {
                if seg.kind == SegKind::Text && pos >= seg.src.start && pos <= seg.src.end {
                    return Some((i, seg.disp.start + (pos - seg.src.start)));
                }
            }
        }
        None
    }

    pub fn is_stop(&self, pos: usize) -> bool {
        self.locate(pos).is_some()
    }

    /// Past the hidden or replaced stretch around `pos`, in the given direction.
    pub fn jump_over(&self, pos: usize, forward: bool) -> Option<usize> {
        let locked = self
            .tables
            .iter()
            .find(|t| t.read_only && t.src.contains(&pos));
        if let Some(table) = locked {
            return Some(if forward {
                table.src.end
            } else {
                table.src.start
            })
            .filter(|&to| to != pos);
        }
        for leaf in &self.leaves {
            for seg in &leaf.segs {
                if seg.kind == SegKind::Text || seg.src.is_empty() {
                    continue;
                }
                if seg.src.start <= pos && pos <= seg.src.end {
                    if forward && seg.src.end > pos {
                        return Some(seg.src.end);
                    }
                    if !forward && seg.src.start < pos {
                        return Some(seg.src.start);
                    }
                }
            }
        }
        None
    }

    /// The atomic stretch ending exactly at `pos`, for Backspace.
    pub fn atomic_before(&self, pos: usize) -> Option<Range<usize>> {
        self.leaves
            .iter()
            .flat_map(|l| &l.segs)
            .find(|s| s.kind != SegKind::Text && !s.src.is_empty() && s.src.end == pos)
            .map(|s| s.src.clone())
    }

    /// The atomic stretch starting exactly at `pos`, for Delete.
    pub fn atomic_after(&self, pos: usize) -> Option<Range<usize>> {
        self.leaves
            .iter()
            .flat_map(|l| &l.segs)
            .find(|s| s.kind != SegKind::Text && !s.src.is_empty() && s.src.start == pos)
            .map(|s| s.src.clone())
    }

    /// The source position of display offset `index` in a leaf. A boundary
    /// between visible stretches resolves to the stretch after it.
    pub fn from_display(&self, leaf: usize, index: usize) -> usize {
        let leaf = &self.leaves[leaf];
        let mut best = None;
        for seg in &leaf.segs {
            match seg.kind {
                SegKind::Text if index >= seg.disp.start && index <= seg.disp.end => {
                    best = Some(seg.src.start + (index - seg.disp.start));
                }
                SegKind::Replaced if index > seg.disp.start && index <= seg.disp.end => {
                    best = Some(seg.src.end);
                }
                _ => {}
            }
        }
        best.unwrap_or(leaf.src.start)
    }

    /// The display ranges covered by the source range `sel` in a leaf.
    pub fn leaf_selection(&self, leaf: usize, sel: &Range<usize>) -> Option<Range<usize>> {
        let leaf = &self.leaves[leaf];
        let mut out: Option<Range<usize>> = None;
        for seg in &leaf.segs {
            let from = sel.start.max(seg.src.start);
            let to = sel.end.min(seg.src.end);
            if from >= to {
                continue;
            }
            let disp = match seg.kind {
                SegKind::Text => {
                    seg.disp.start + (from - seg.src.start)..seg.disp.start + (to - seg.src.start)
                }
                SegKind::Replaced => seg.disp.clone(),
                SegKind::Hidden => continue,
            };
            out = Some(match out {
                Some(r) => r.start.min(disp.start)..r.end.max(disp.end),
                None => disp,
            });
        }
        out.filter(|r| !r.is_empty())
    }

    /// The missing cell holding the caret.
    pub fn missing_cell(&self) -> Option<&Cell> {
        self.missing.map(|(t, c)| &self.tables[t].cells[c])
    }

    /// The missing cell drawn by `leaf`, if it is one.
    pub fn missing_leaf(&self, leaf: usize) -> Option<&Cell> {
        let cells = self.tables.iter().flat_map(|t| &t.cells);
        cells.into_iter().find(|c| c.missing && c.leaf == leaf)
    }

    /// The cell at `pos`: the missing cell holding the caret, ahead of a real
    /// cell ending at the same offset, else a real cell.
    pub fn cell_at(&self, pos: usize) -> Option<(usize, usize, &Cell)> {
        if let Some((t, c)) = self.missing
            && self.tables[t].cells[c].from == pos
        {
            return Some((t, c, &self.tables[t].cells[c]));
        }
        for (t, table) in self.tables.iter().enumerate() {
            if pos < table.src.start || pos > table.src.end {
                continue;
            }
            for (c, cell) in table.cells.iter().enumerate() {
                if !cell.missing && pos >= cell.from && pos <= cell.limit {
                    return Some((t, c, cell));
                }
            }
        }
        None
    }

    /// Moves every source position by `delta`, for blocks after an edit:
    /// cheaper than projecting them again. A new position field must be
    /// moved here too (`shifted_matches_projecting_later` checks it).
    pub fn shift(&mut self, delta: isize) {
        let at = |p: &mut usize| *p = p.wrapping_add_signed(delta);
        let range = |r: &mut Range<usize>| {
            at(&mut r.start);
            at(&mut r.end);
        };
        for block in &mut self.blocks {
            range(&mut block.src);
            if let BlockKind::ListItem {
                marker: Some(Marker::Task { at: task, .. }),
                ..
            } = &mut block.kind
            {
                at(task);
            }
        }
        for leaf in &mut self.leaves {
            range(&mut leaf.src);
            for seg in &mut leaf.segs {
                range(&mut seg.src);
            }
        }
        for table in &mut self.tables {
            range(&mut table.src);
            at(&mut table.layout.delimiter_end);
            for row in &mut table.layout.rows {
                at(&mut row.from);
                at(&mut row.to);
                at(&mut row.end);
                for cell in &mut row.cells {
                    at(&mut cell.from);
                    at(&mut cell.to);
                }
            }
            for cell in &mut table.cells {
                at(&mut cell.from);
                at(&mut cell.to);
                at(&mut cell.limit);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Should markdown-rs panic again, the source is drawn as written, with
    // a caret stop on each line, instead of the panic reaching the app.
    #[test]
    fn a_source_the_parser_panics_on_draws_as_plain_lines() {
        let src = "  - a\n\n> b";
        let ast = parse_with(src, |_| panic!("a parser bug"));
        let p = project(src, &ast, &Active::default());
        let drawn: Vec<_> = p.leaves.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(drawn, ["  - a", "", "> b"]);
        assert!(p.is_stop(0) && p.is_stop(src.len()));
    }

    fn at(src: &str, caret: Option<usize>) -> Projection {
        let active = Active {
            ranges: caret.map(|c| c..c).into_iter().collect(),
            ..Active::default()
        };
        project(src, &parse(src), &active)
    }

    fn displays(p: &Projection) -> Vec<String> {
        p.leaves.iter().map(|l| l.text.clone()).collect()
    }

    #[test]
    fn hides_emphasis_code_and_link_markers_away_from_the_caret() {
        let src = "plain **bold** *it* `code` [text](https://x.dev) ok";
        let p = at(src, Some(src.len()));
        assert_eq!(displays(&p), vec!["plain bold it code text ok"]);
        let leaf = &p.leaves[0];
        assert!(
            leaf.marks
                .iter()
                .any(|(r, s)| &leaf.text[r.clone()] == "bold" && s.bold)
        );
        assert!(
            leaf.marks
                .iter()
                .any(|(r, s)| &leaf.text[r.clone()] == "it" && s.italic)
        );
        assert!(
            leaf.marks
                .iter()
                .any(|(r, s)| &leaf.text[r.clone()] == "code" && s.code)
        );
        assert!(
            leaf.marks
                .iter()
                .any(|(r, s)| &leaf.text[r.clone()] == "text" && s.link)
        );
    }

    #[test]
    fn shows_the_markers_of_the_element_the_caret_is_in_and_only_that_one() {
        let src = "plain **bold** *it*";
        let caret = src.find("bold").unwrap() + 1;
        let p = at(src, Some(caret));
        assert_eq!(displays(&p), vec!["plain **bold** it"]);
        // Touching the boundary counts, as in today's editor.
        let p = at(src, Some(src.find("**").unwrap()));
        assert_eq!(displays(&p), vec!["plain **bold** it"]);
    }

    #[test]
    fn renders_everything_without_focus() {
        let src = "# Title **b**\n\ntext";
        let p = at(src, None);
        assert_eq!(displays(&p), vec!["Title b", "", "text"]);
    }

    #[test]
    fn reveals_active_headings_and_keeps_other_block_markers_hidden() {
        let src = "# Title\n\n> quote\n\n## Next";
        let caret_in_title = at(src, Some(4));
        assert_eq!(
            displays(&caret_in_title),
            vec!["# Title", "", "quote", "", "Next"]
        );
        let caret_in_quote = at(src, Some(src.find("quote").unwrap() + 2));
        assert_eq!(
            displays(&caret_in_quote),
            vec!["Title", "", "quote", "", "Next"]
        );
    }

    #[test]
    fn caret_maps_across_hidden_ranges() {
        let src = "aa **bb** cc";
        let p = at(src, Some(src.len()));
        assert_eq!(displays(&p), vec!["aa bb cc"]);
        // Inside rendered text: source and display line up.
        assert_eq!(p.locate(1), Some((0, 1)));
        // 'b' after the hidden `**`: display skips the marker.
        let b = src.find("bb").unwrap();
        assert_eq!(p.locate(b), Some((0, 3)));
        assert_eq!(p.from_display(0, 3), b);
        // After the closing `**`.
        let c = src.find("cc").unwrap();
        assert_eq!(p.locate(c), Some((0, 6)));
        // The markers are not stops.
        assert!(!p.is_stop(b - 1));
        assert_eq!(p.jump_over(b - 1, true), Some(b));
        assert_eq!(p.jump_over(b - 1, false), Some(b - 2));
    }

    #[test]
    fn maps_multibyte_text_around_hidden_markers() {
        let src = "前**太字**後 caf\u{e9}";
        let p = at(src, Some(src.len()));
        assert_eq!(displays(&p), vec!["前太字後 caf\u{e9}"]);
        let ji = src.find("字").unwrap();
        let (leaf, d) = p.locate(ji).unwrap();
        assert_eq!(&p.leaves[leaf].text[d..d + 3], "字");
        assert_eq!(p.from_display(leaf, d), ji);
    }

    #[test]
    fn soft_breaks_render_as_a_space_and_hard_breaks_stay() {
        let src = "one\ntwo  \nthree";
        let p = at(src, None);
        assert_eq!(displays(&p), vec!["one two\nthree"]);
        // The soft break is one atomic stop on each side.
        let two = src.find("two").unwrap();
        assert!(p.is_stop(3));
        assert!(p.is_stop(two));
    }

    #[test]
    fn draws_lists_with_bullets_and_keeps_numbers_as_text() {
        let src = "- one\n  - nested\n\n1. first\n2. second\n- [x] done\n- [ ] todo";
        let p = at(src, None);
        // Each item is its marker, drawn in the marker column, then its text.
        assert_eq!(
            displays(&p),
            vec![
                "\u{2022}", "one", "\u{2022}", "nested", "", "1.", "first", "2.", "second",
                TASK_SPACE, "done", TASK_SPACE, "todo"
            ]
        );
        let items: Vec<_> = p
            .blocks
            .iter()
            .filter_map(|b| match b.kind {
                BlockKind::ListItem { depth, marker, .. } => Some((depth, marker)),
                _ => None,
            })
            .collect();
        let task = |checked, at| Some(Marker::Task { checked, at });
        assert_eq!(
            items,
            vec![
                (0, Some(Marker::Bullet)),
                (1, Some(Marker::Bullet)),
                (0, Some(Marker::Number)),
                (0, Some(Marker::Number)),
                (0, task(true, src.find("[x]").unwrap())),
                (0, task(false, src.find("[ ]").unwrap())),
            ]
        );
        // The bullet is atomic: not a caret stop, and the text's start is
        // drawn in the text, not after the bullet.
        assert!(!p.is_stop(0));
        assert_eq!(p.locate(2), Some((1, 0)));
        // A number is text, with a stop on each side of its spacing.
        let first = src.find("1.").unwrap();
        assert!(p.is_stop(first) && p.is_stop(first + 2) && p.is_stop(first + 3));
        assert_eq!(p.jump_over(first + 2, true), Some(first + 3));
    }

    // Mirrors liveMarkdown.test.ts "shares an ordered marker column across
    // digit boundaries and continuation lines".
    #[test]
    fn ordered_siblings_share_a_marker_column_and_later_paragraphs_align_with_it() {
        let src =
            "8. eight  \n   continued\n9. nine\n10. ten\n\nOutside\n\n99. another\n100. hundred";
        let p = at(src, None);
        let columns: Vec<_> = p
            .blocks
            .iter()
            .filter_map(|b| match b.kind {
                BlockKind::ListItem { columns, .. } => Some(columns),
                _ => None,
            })
            .collect();
        assert_eq!(columns, vec![4, 4, 4, 5, 5]);
        // Nested lists size their own column.
        let src = "9. parent\n   99. nested\n   100. nested\n10. sibling";
        let kinds: Vec<_> = at(src, None)
            .blocks
            .iter()
            .filter_map(|b| match b.kind {
                BlockKind::ListItem { depth, columns, .. } => Some((depth, columns)),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, vec![(0, 4), (1, 5), (1, 5), (0, 4)]);
        // A later paragraph of an item is drawn at the item's text.
        let src = "- item\n\n  later";
        let p = at(src, None);
        assert_eq!(
            p.blocks.last().unwrap().kind,
            BlockKind::ListItem {
                depth: 0,
                columns: 0,
                marker: None
            }
        );
    }

    // Mirrors liveMarkdown.test.ts "includes task separator spaces in the
    // checkbox's layout column".
    #[test]
    fn a_task_box_takes_its_separator_spaces() {
        let src = "- [ ]   task  \n  continued";
        let p = at(src, Some(src.len()));
        assert_eq!(displays(&p), vec![TASK_SPACE, "task\ncontinued"]);
        assert_eq!(hidden(src, None), vec!["- ", "[ ]   ", "  ", "  "]);
    }

    #[test]
    fn an_ordered_task_keeps_its_number_beside_the_box() {
        let src = "1. [x] done";
        let p = at(src, None);
        assert_eq!(displays(&p), vec![format!("1.{TASK_SPACE}"), "done".into()]);
        assert_eq!(
            p.blocks[0].kind,
            BlockKind::ListItem {
                depth: 0,
                columns: 5,
                marker: Some(Marker::Task {
                    checked: true,
                    at: 3
                })
            }
        );
    }

    #[test]
    fn a_table_renders_cells_and_only_the_active_cell_shows_source() {
        let src = "| a **b** | c |\n|---|---|\n| 1 | `d` |\n";
        let p = at(src, Some(src.len()));
        assert_eq!(displays(&p), vec!["a b", "c", "1", "d", ""]);
        let caret = src.find("**").unwrap() + 3;
        let p = at(src, Some(caret));
        assert_eq!(displays(&p), vec!["a **b**", "c", "1", "d", ""]);
        // Pipes and padding are not stops.
        assert!(!p.is_stop(0));
        let (_, _, cell) = p.cell_at(caret).unwrap();
        assert_eq!((cell.row, cell.col), (0, 0));
    }

    #[test]
    fn alignment_padding_is_not_a_caret_stop() {
        let src = "| ab     | c |\n|---|---|\n";
        let caret = src.find("ab").unwrap() + 2;
        let p = at(src, Some(caret));
        assert_eq!(p.leaves[0].text, "ab");
        assert!(!p.is_stop(caret + 1));
    }

    #[test]
    fn a_table_inside_a_quote_or_list_stays_source() {
        let src = "> | a | b |\n> |---|---|\n> | 1 | 2 |";
        let p = at(src, Some(src.len()));
        assert!(p.tables.is_empty());
        assert!(matches!(p.blocks[0].kind, BlockKind::Raw));
        let src = "- | a | b |\n  |---|---|\n  | 1 | 2 |";
        assert!(at(src, Some(src.len())).tables.is_empty());
    }

    #[test]
    fn a_selection_passing_over_a_table_leaves_it_rendered() {
        let src = "x\n\n| **a** | b |\n|---|---|\n\ny";
        let active = Active {
            ranges: vec![Range {
                start: 0,
                end: src.len(),
            }],
            ..Active::default()
        };
        let p = project(src, &parse(src), &active);
        let cells: Vec<_> = p.tables[0]
            .cells
            .iter()
            .map(|c| p.leaves[c.leaf].text.clone())
            .collect();
        assert_eq!(cells, vec!["a", "b"]);
    }

    #[test]
    fn selection_maps_to_display_ranges_across_hidden_markers() {
        let src = "aa **bb** cc";
        let p = at(src, None);
        assert_eq!(p.leaf_selection(0, &(1..src.len() - 1)), Some(1..7));
        // A range entirely inside hidden markers shows nothing.
        assert_eq!(p.leaf_selection(0, &(3..5)), None);
    }

    #[test]
    fn fenced_code_hides_fences_keeps_the_language_and_reveals_on_entry() {
        let src = "```rust\nfn a() {}\n```\n\nx";
        let p = at(src, Some(src.len()));
        assert_eq!(displays(&p), vec!["rust\nfn a() {}\n", "", "x"]);
        let inside = at(src, Some(src.find("fn").unwrap()));
        assert_eq!(displays(&inside)[0], "```rust\nfn a() {}\n```");
    }

    #[test]
    fn a_shortcut_reference_without_definition_stays_text() {
        let src = "see [note] here";
        let p = at(src, Some(0));
        assert_eq!(displays(&p), vec!["see [note] here"]);
    }

    #[test]
    fn escapes_hide_their_backslash_until_the_caret_is_on_them() {
        let src = "a\\*b";
        assert_eq!(displays(&at(src, Some(src.len()))), vec!["a*b"]);
        assert_eq!(displays(&at(src, Some(1))), vec!["a\\*b"]);
    }

    #[test]
    fn preedit_is_underlined_at_the_caret() {
        let src = "ab";
        let active = Active {
            ranges: vec![Range { start: 1, end: 1 }],
            marked: Some(1..2),
            ..Active::default()
        };
        let p = project(src, &parse(src), &active);
        assert!(
            p.leaves[0]
                .marks
                .iter()
                .any(|(r, s)| r == &(1..2) && s.underline)
        );
    }

    /// The hidden source stretches, in order, as Tauri's tests list them.
    fn hidden(src: &str, caret: Option<usize>) -> Vec<&str> {
        let p = at(src, caret);
        p.leaves
            .iter()
            .flat_map(|l| &l.segs)
            .filter(|s| s.kind != SegKind::Text && !s.src.is_empty())
            .map(|s| &src[s.src.clone()])
            .collect()
    }

    fn marked(p: &Projection, text: &str, pick: impl Fn(&Style) -> bool) -> bool {
        p.leaves.iter().any(|l| {
            l.marks
                .iter()
                .any(|(r, s)| &l.text[r.clone()] == text && pick(s))
        })
    }

    // The cases below mirror liveMarkdown.test.ts "live Markdown rendering".

    #[test]
    fn hides_heading_emphasis_and_code_markers_away_from_the_caret() {
        let src = "# Title\n\nSome **bold** and *it* and `code`.\n";
        assert_eq!(
            hidden(src, None),
            vec!["# ", "**", "**", "*", "*", "`", "`"]
        );
        assert!(matches!(
            at(src, None).blocks[0].kind,
            BlockKind::Heading(1)
        ));
    }

    #[test]
    fn shows_the_markers_of_the_inline_element_the_caret_is_in() {
        let src = "# Title\n\nSome **bold** and `code`.\n";
        assert_eq!(hidden(src, Some(11)), vec!["# ", "**", "**", "`", "`"]);
        assert_eq!(hidden(src, Some(17)), vec!["# ", "`", "`"]);
        // At the edge counts as in it.
        assert_eq!(hidden(src, Some(22)), vec!["# ", "`", "`"]);
        assert_eq!(hidden(src, Some(23)), vec!["# ", "**", "**", "`", "`"]);
        assert_eq!(hidden(src, Some(30)), vec!["# ", "**", "**"]);
    }

    #[test]
    fn reveals_heading_markers_of_every_level_at_its_start_text_and_end() {
        for level in 1..=6 {
            let mark = "#".repeat(level);
            let src = format!("{mark} Title {mark}\n\nbody");
            assert_eq!(
                hidden(&src, None),
                vec![format!("{mark} "), format!(" {mark}")]
            );
            for caret in [0, level + 1, src.find('\n').unwrap()] {
                assert!(
                    hidden(&src, Some(caret)).is_empty(),
                    "level {level} caret {caret}"
                );
            }
            assert_eq!(hidden(&src, Some(src.len())).len(), 2);
        }
    }

    #[test]
    fn draws_a_link_as_its_text_and_keeps_the_target_out_of_sight() {
        let src = "See [the docs](./docs/a.md \"title\") now.\n\nnext";
        assert_eq!(
            hidden(src, Some(src.len())),
            vec!["[", "](./docs/a.md \"title\")"]
        );
        assert!(marked(&at(src, Some(src.len())), "the docs", |s| s.link));
    }

    #[test]
    fn draws_a_full_reference_link_as_a_link() {
        let src = "See [the docs][ref].\n\n[ref]: ./a.md";
        assert!(marked(&at(src, None), "the docs", |s| s.link));
    }

    #[test]
    fn leaves_a_bracketed_word_without_a_destination_as_text_keeping_its_emphasis() {
        let src = "Marks: **[beta]** means it.";
        let p = at(src, None);
        assert!(!p.leaves.iter().any(|l| l.marks.iter().any(|(_, s)| s.link)));
        assert_eq!(hidden(src, None), vec!["**", "**"]);
    }

    #[test]
    fn draws_an_image_as_its_alt_text_styled_like_a_link() {
        // No picture, embedded or not: the markup hides until the caret enters.
        for src in [
            "![logo](https://example.com/a.png)\n\nx",
            "![dot](data:image/png;base64,AAAA)\n\nx",
        ] {
            let away = at(src, Some(src.len()));
            let alt = &src[2..src.find(']').unwrap()];
            assert_eq!(away.leaves[0].text, alt);
            assert!(marked(&away, alt, |s| s.link));
            assert_eq!(hidden(src, Some(src.len()))[0], "![");
            let inside = at(src, Some(3));
            assert_eq!(inside.leaves[0].text, src[..src.find('\n').unwrap()]);
        }
        // Brackets and escapes inside the alt text stay in it.
        let src = "![a [b] \\] c](x.png)";
        assert_eq!(at(src, None).leaves[0].text, "a [b] \\] c");
    }

    #[test]
    fn draws_bullets_and_tasks_and_leaves_ordered_numbers_alone() {
        let src = "- one\n- [ ] open\n- [x] done\n1. first\n";
        let p = at(src, None);
        let shown: Vec<_> = p.leaves.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            shown,
            vec![
                "\u{2022}", "one", TASK_SPACE, "open", TASK_SPACE, "done", "1.", "first", ""
            ]
        );
        assert_eq!(
            hidden(src, None),
            vec!["- ", "- ", "[ ] ", "- ", "[x] ", " "]
        );
    }

    // The cases below are T7.2's: parity with Tauri's rendering beyond T7.1.

    #[test]
    fn a_setext_heading_hides_its_underline_and_shows_it_as_its_own_line_when_edited() {
        let src = "Title\n===\n\nbody";
        let p = at(src, Some(src.len()));
        assert_eq!(displays(&p), vec!["Title", "", "body"]);
        assert!(matches!(p.blocks[0].kind, BlockKind::Heading(1)));
        assert_eq!(hidden(src, Some(src.len())), vec!["\n==="]);
        for caret in [2, 7] {
            let p = at(src, Some(caret));
            assert_eq!(displays(&p), vec!["Title", "===", "", "body"]);
            assert!(matches!(p.blocks[0].kind, BlockKind::Heading(1)));
            assert!(matches!(p.blocks[1].kind, BlockKind::Raw));
            assert!(p.is_stop(caret));
        }
        // In a quote the underline's marker stays hidden.
        let src = "> Sub\n> ---";
        assert_eq!(displays(&at(src, Some(3))), vec!["Sub", "---"]);
        assert!(matches!(
            at(src, None).blocks[0].kind,
            BlockKind::Heading(2)
        ));
    }

    #[test]
    fn nested_quotes_hide_every_marker_and_keep_their_depth() {
        let src = "> a\n> > b\n> > c\n>\n> d";
        let p = at(src, None);
        assert_eq!(displays(&p), vec!["a", "b c", "", "d"]);
        let quotes: Vec<_> = p.blocks.iter().map(|b| b.quote).collect();
        assert_eq!(quotes, vec![1, 2, 1, 1]);
        assert_eq!(hidden(src, None), vec!["> ", "> > ", "\n> > ", ">", "> "]);
    }

    #[test]
    fn autolinks_hide_their_brackets_until_the_caret_enters() {
        let src = "x <https://a.dev> https://b.dev www.c.dev";
        let p = at(src, None);
        assert_eq!(
            displays(&p),
            vec!["x https://a.dev https://b.dev www.c.dev"]
        );
        for url in ["https://a.dev", "https://b.dev", "www.c.dev"] {
            assert!(marked(&p, url, |s| s.link), "{url}");
        }
        assert_eq!(displays(&at(src, Some(4))), vec![src]);
    }

    #[test]
    fn a_shortcut_reference_stays_text_and_its_emphasis_still_hides() {
        let src = "see [**note**] here\n\n[**note**]: ./a.md";
        let p = at(src, Some(src.len()));
        assert_eq!(p.leaves[0].text, "see [note] here");
        assert!(!p.leaves[0].marks.iter().any(|(_, s)| s.link));
        // A collapsed reference is a link.
        let src = "[a][]\n\n[a]: ./a.md";
        assert!(marked(&at(src, None), "a", |s| s.link));
    }

    #[test]
    fn a_rule_is_a_caret_stop_and_shows_its_source_there() {
        let src = "a\n\n---\n\nb";
        let rule = src.find("---").unwrap();
        let p = at(src, None);
        assert_eq!(p.blocks[2].kind, BlockKind::Rule { revealed: false });
        // An empty leaf at the rule's end, which the view draws the rule over.
        assert_eq!(p.locate(rule + 3), Some((2, 0)));
        let p = at(src, Some(rule + 3));
        assert_eq!(p.blocks[2].kind, BlockKind::Rule { revealed: true });
        assert_eq!(p.leaves[2].text, "---");
    }

    #[test]
    fn hides_quote_markers_and_escapes() {
        assert_eq!(
            hidden("> quoted \\*text\\*\n", None),
            vec!["> ", "\\", "\\"]
        );
    }

    #[test]
    fn hides_fences_but_keeps_the_language_visible() {
        let src = "```ts\nconst a = 1;\n```\n\ntext";
        // The closing fence takes its line break with it: the block draws no
        // empty last line.
        assert_eq!(hidden(src, Some(src.len())), vec!["```", "```"]);
        assert!(hidden(src, Some(8)).is_empty());
    }

    #[test]
    fn renders_a_table_whole_wherever_the_caret_is() {
        let src = "| a | b |\n| - | - |\n| 1 | 2 |\n\nafter";
        for caret in [src.len(), 12] {
            let p = at(src, Some(caret));
            assert_eq!(p.tables.len(), 1);
            assert_eq!(p.tables[0].src, 0..29);
        }
    }

    #[test]
    fn keeps_a_link_or_image_that_spans_lines_without_failing() {
        for src in [
            "[a](x.md\n\"title\")\n\nend",
            "[a](\nx.md)\n\nend",
            "![a\nb](data:image/png;base64,AAAA)\n\nend",
        ] {
            for caret in [Some(1), Some(src.len()), None] {
                at(src, caret);
            }
        }
    }

    #[test]
    fn shifted_matches_projecting_later() {
        let src = "# T **b**\n\n- [ ] a\n\n| a | `b` |\n|---|---|\n| 1 |  2 |\n\n```\nx\n```\n";
        let caret = src.find("1 |").unwrap();
        let prefix = "intro\n";
        let later = format!("{prefix}{src}");
        let at = |caret: usize| Active {
            ranges: vec![Range {
                start: caret,
                end: caret,
            }],
            cell_typed_end: Some(caret + 2),
            ..Active::default()
        };
        let mut shifted = project(src, &parse(src), &at(caret));
        shifted.shift(prefix.len() as isize);
        let whole = project(&later, &parse(&later), &at(caret + prefix.len()));
        // The intro paragraph comes first.
        let mut moved = project_children(&later, &parse(&later), &at(caret + prefix.len()), 1..5);
        moved.leaves.iter_mut().for_each(|l| l.key = 0);
        shifted.leaves.iter_mut().for_each(|l| l.key = 0);
        assert_eq!(whole.blocks.len(), shifted.blocks.len() + 1);
        assert_eq!(moved, shifted);
    }

    #[test]
    fn a_window_projects_only_the_blocks_around_it() {
        let src = "one\n\ntwo\n\nthree\n\nfour\n\nfive";
        let three = src.find("three").unwrap();
        let p = project_near(src, &parse(src), &Active::default(), three..three);
        let shown: Vec<_> = p.leaves.iter().map(|l| l.text.as_str()).collect();
        // The blank line before `two` belongs to the gap the window starts in.
        assert_eq!(shown, vec!["", "two", "", "three", "", "four"]);
        // At the end, the trailing blank line is there.
        let src = "a\n\nb\n";
        let p = project_near(src, &parse(src), &Active::default(), src.len()..src.len());
        assert!(p.is_stop(src.len()));
    }
    // Mirrors liveMarkdown.test.ts "finds the soft line breaks a paragraph
    // flows across, and only those".
    #[test]
    fn finds_the_soft_line_breaks_a_paragraph_flows_across_and_only_those() {
        let joined = |src: &str| -> Vec<String> {
            at(src, None)
                .leaves
                .iter()
                .flat_map(|l| &l.segs)
                .filter(|s| s.kind == SegKind::Replaced && src[s.src.clone()].contains('\n'))
                .map(|s| src[s.src.clone()].to_string())
                .collect()
        };
        assert_eq!(
            joined("one\ntwo\nthree\n\nnext\npara"),
            vec!["\n", "\n", "\n"]
        );
        // A list item's continuation takes its indentation with the break.
        assert_eq!(joined("- item that\n  wraps"), vec!["\n  "]);
        // Hard breaks, separate blocks and code stay as they are.
        assert!(joined("hard  \nbreak\\\nhere").is_empty());
        assert!(joined("# Title\ntext\n```\ncode\nmore\n```").is_empty());
        // The quote marker goes with the break, hidden as it is on any line.
        assert_eq!(joined("> quoted\n> on two"), vec!["\n> "]);
        // A single trailing space goes with the break; an escaped backslash is
        // text, not a hard break.
        assert_eq!(joined("one \ntwo"), vec![" \n"]);
        assert_eq!(displays(&at("one \ntwo", None)), vec!["one two"]);
        assert_eq!(joined("one \\\\\ntwo"), vec!["\n"]);
    }

    // Mirrors liveMarkdown.test.ts "aligns visible continuation lines with
    // their nested item's content".
    #[test]
    fn continuation_lines_hang_at_their_items_text() {
        let src = "- parent\n  - item  \n    continued\n\n    another paragraph";
        let p = at(src, Some(src.len()));
        assert_eq!(
            displays(&p),
            vec![
                "\u{2022}",
                "parent",
                "\u{2022}",
                "item\ncontinued",
                "",
                "another paragraph"
            ]
        );
        let last = p.blocks.last().unwrap();
        assert_eq!(
            last.kind,
            BlockKind::ListItem {
                depth: 1,
                columns: 0,
                marker: None
            }
        );
        let src = "1. parent\n\n   2. item\\\n      continued";
        let p = at(src, Some(src.len()));
        assert!(p.leaves.iter().any(|l| l.text == "item\ncontinued"));
    }
}
