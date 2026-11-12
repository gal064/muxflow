//! Source to display projection: what today's CodeMirror decorations do,
//! expressed as a pure function of (source, selection).
//!
//! `project` walks the Markdown AST and produces a flat list of text leaves.
//! A leaf's `text` is what is drawn; its `segs` say how each stretch of
//! display text maps back to source bytes. Hidden markers are `Hidden` segs
//! with no display text; atomic replacements (a list bullet, a soft line
//! break) are `Replaced` segs. Nothing here ever rewrites the source.

use std::ops::Range;

use markdown::mdast::{self, Node};
use markdown::{Constructs, ParseOptions};

use crate::table::{TableLayout, table_layout};

pub fn parse(src: &str) -> Node {
    markdown::to_mdast(src, &ParseOptions::gfm()).expect("plain markdown cannot fail to parse")
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    Heading(u8),
    ListItem {
        depth: usize,
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableInfo {
    pub src: Range<usize>,
    pub layout: TableLayout,
    pub cells: Vec<Cell>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projection {
    pub blocks: Vec<Block>,
    pub leaves: Vec<Leaf>,
    pub tables: Vec<TableInfo>,
}

/// What the projection needs to know about the editor state.
#[derive(Clone, Debug, Default)]
pub struct Active {
    /// The selection; empty while the editor does not have focus, so
    /// everything renders.
    pub ranges: Vec<Range<usize>>,
    /// IME preedit range in source bytes.
    pub marked: Option<Range<usize>>,
    /// Just past whitespace the user typed at the end of a table cell, so the
    /// caret can stand there. Alignment padding is otherwise not a caret stop.
    pub cell_typed_end: Option<usize>,
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
    window: Option<Range<usize>>,
) -> Projection {
    let mut b = Builder {
        src,
        active,
        blocks: Vec::new(),
        leaves: Vec::new(),
        tables: Vec::new(),
        pending_prefix: None,
        quote: 0,
        rendered_only: false,
    };
    let children = ast.children().map_or(&[][..], |c| &c[..]);
    let (first, last) = match &window {
        Some(window) => (
            children
                .partition_point(|node| pos(node).end < window.start)
                .saturating_sub(1),
            (children.partition_point(|node| pos(node).start <= window.end) + 1)
                .min(children.len()),
        ),
        None => (0, children.len()),
    };
    let mut cursor = if first == 0 {
        0
    } else {
        pos(&children[first - 1]).end
    };
    for node in &children[first..last] {
        let p = pos(node);
        b.gap_lines(cursor..p.start);
        b.block(node, 0);
        cursor = p.end;
    }
    if last == children.len() {
        b.gap_lines(cursor..src.len());
        b.pending_prefix = None;
        if src.is_empty() || src.ends_with('\n') {
            b.point_block(src.len(), BlockKind::Blank, src.len()..src.len());
        }
    }
    Projection {
        blocks: b.blocks,
        leaves: b.leaves,
        tables: b.tables,
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

    fn finish_leaf(&mut self, mut l: LeafB, end: usize) -> usize {
        l.leaf.src.end = end;
        if !l.leaf.segs.iter().any(|s| s.kind == SegKind::Text) {
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
                    self.pending_prefix = Some(p..range.end);
                    return;
                }
                Some(i) => p += i + 1,
            }
        }
        while p < range.end {
            let Some(nl) = src[p..range.end].find('\n').map(|i| p + i) else {
                self.pending_prefix = Some(p..range.end);
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
            if p.start > cursor {
                self.text(l, cursor..p.start, style);
            }
            match node {
                Node::Text(_) => self.text(l, p.clone(), style),
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
                    l.visible(src, run..i, style);
                    let mut j = i + 1;
                    while j < range.end && matches!(bytes[j], b' ' | b'\t' | b'>') {
                        j += 1;
                    }
                    l.replaced(i..j, " ", style);
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
                    BlockKind::ListItem { depth: depth - 1 }
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
                let mut cursor = p.start;
                for item in &n.children {
                    let ip = pos(item);
                    self.gap_lines(cursor..ip.start);
                    if let Node::ListItem(it) = item {
                        self.list_item(ip.clone(), it, n.ordered, depth);
                    }
                    cursor = ip.end;
                }
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
        let marker_end = n.children.first().map_or(p.end, |c| pos(c).start);
        let plain = Style::default();
        let mut l = self.start_leaf(p.start);
        match n.checked {
            Some(checked) => {
                // The list marker and the checkbox are separate atomic stretches, so
                // Backspace takes the checkbox first, then the bullet (today's behavior).
                let shown = if checked { "\u{2611} " } else { "\u{2610} " };
                let bracket = src[p.start..marker_end]
                    .find('[')
                    .map_or(p.start, |i| p.start + i);
                l.replaced(p.start..bracket, "", &plain);
                l.replaced(bracket..marker_end, shown, &plain);
            }
            None if ordered => l.visible(src, p.start..marker_end, &plain),
            None => l.replaced(p.start..marker_end, "\u{2022} ", &plain),
        }
        let mut rest = &n.children[..];
        if let Some(Node::Paragraph(para)) = n.children.first() {
            let para_range = pos(&n.children[0]);
            self.inlines(
                &mut l,
                &para.children,
                para_range.clone(),
                &Style::default(),
            );
            let idx = self.finish_leaf(l, para_range.end);
            self.push_block(
                BlockKind::ListItem { depth },
                vec![idx],
                p.start..para_range.end,
            );
            rest = &n.children[1..];
        } else {
            let idx = self.finish_leaf(l, marker_end);
            self.push_block(
                BlockKind::ListItem { depth },
                vec![idx],
                p.start..marker_end,
            );
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
        let mut rows = Vec::new();
        let mut cells = Vec::new();
        for (row, span) in layout.rows.iter().enumerate() {
            let mut leaf_row = Vec::new();
            for (col, cell) in span.cells.iter().enumerate() {
                let bytes = src.as_bytes();
                let mut inner_end = cell.to;
                while inner_end < span.to && bytes[inner_end] == b' ' {
                    inner_end += 1;
                }
                let limit = match self.active.cell_typed_end {
                    Some(end) if end > cell.to && end <= inner_end => end,
                    _ => cell.to,
                };
                let leaf = self.table_cell(cell.from, cell.to, limit);
                leaf_row.push(leaf);
                cells.push(Cell {
                    leaf,
                    row,
                    col,
                    from: cell.from,
                    to: cell.to,
                    limit,
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
        });
    }

    fn table_cell(&mut self, from: usize, to: usize, limit: usize) -> usize {
        let src = self.src;
        let mut l = self.start_leaf(from);
        let plain = Style::default();
        if self
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

    pub fn cell_at(&self, pos: usize) -> Option<(usize, usize, &Cell)> {
        for (t, table) in self.tables.iter().enumerate() {
            if pos < table.src.start || pos > table.src.end {
                continue;
            }
            for (c, cell) in table.cells.iter().enumerate() {
                if pos >= cell.from && pos <= cell.limit {
                    return Some((t, c, cell));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            displays(&p),
            vec![
                "\u{2022} one",
                "\u{2022} nested",
                "",
                "1. first",
                "2. second",
                "\u{2611} done",
                "\u{2610} todo"
            ]
        );
        let depths: Vec<_> = p
            .blocks
            .iter()
            .filter_map(|b| match b.kind {
                BlockKind::ListItem { depth } => Some(depth),
                _ => None,
            })
            .collect();
        assert_eq!(depths, vec![0, 1, 0, 0, 0, 0]);
        // The bullet is atomic: not a caret stop.
        assert!(!p.is_stop(0));
        assert!(p.is_stop(2));
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
                "\u{2022} one",
                "\u{2610} open",
                "\u{2611} done",
                "1. first",
                ""
            ]
        );
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
}
