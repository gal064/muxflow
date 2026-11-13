//! Draws only the blocks on screen. Each block is laid out on its own, and its
//! height is kept with its segment ([`Segment::heights`]); a block not laid
//! out yet has an estimated height. Where the view is scrolled is a block and
//! an offset into it ([`Anchor`]), so an edit above the viewport does not move
//! what it shows, and the scrollbar works from the summed heights.
//!
//! A page above and below the viewport is laid out but not painted, so Up,
//! Down, Page Up, Page Down and a drag past the edge land on laid-out lines.
//! The caret's block and the preedit's are laid out wherever they are, for the
//! caret, reveal and the input method, and the blocks either side of the
//! caret's, so Up and Down move off it when it is off screen.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::base::ScrollbarHandle;
use gpui_kit::base::text::editor_host::{LeafGeometry, RenderContext, collect_leaf_geometry};
use gpui_kit::*;

use super::{
    FONT_SIZE, LINE_HEIGHT, LiveStyle, LiveView, caret_bounds, cell_ring, heading_padding,
    heading_scale, paint_task, task_box, wide,
};
use crate::edit::Segment;
use crate::projection::{BlockKind, Projection};

/// A table drawn wider than its column, as the last layout placed it.
#[derive(Clone)]
struct WideTable {
    /// Where the table starts in the source: its key in the scroll map.
    start: usize,
    /// What of it shows.
    clip: Bounds<Pixels>,
    /// How far it scrolls.
    max: Pixels,
}

/// Space above the first block, as Tauri's padding.
pub(super) const PAD_TOP: f32 = 48.;

/// A block: its segment and its index there.
type At = (usize, usize);

/// The block at the top of the viewport and how far into it the viewport
/// starts. Held by segment id, so an edit elsewhere does not move it.
#[derive(Clone, Copy)]
struct Anchor {
    id: u64,
    /// Where that segment was: looked at first, and kept when an edit
    /// replaced the segment.
    segment: usize,
    block: usize,
    /// Negative above the first block, in the top padding.
    offset: Pixels,
}

impl Default for Anchor {
    /// The top, with its padding.
    fn default() -> Self {
        Self {
            id: 0,
            segment: 0,
            block: 0,
            offset: px(-PAD_TOP),
        }
    }
}

#[derive(Default)]
struct ScrollState {
    anchor: Anchor,
    /// Where the scrollbar asked to go, as a fraction of the content.
    jump: Option<f32>,
    viewport: Bounds<Pixels>,
    /// Estimates from the summed heights: the whole content, and how far
    /// into it the viewport starts.
    content: Pixels,
    top: Pixels,
    /// The content height while the thumb is dragged, so the thumb stays
    /// under the pointer as blocks are measured.
    dragged: Option<Pixels>,
}

/// The view's scroll position, shared with its scrollbar.
#[derive(Clone, Default)]
pub(super) struct LiveScroll(Rc<RefCell<ScrollState>>);

impl LiveScroll {
    /// Scrolls by `delta` (positive moves the content up), at the next layout.
    pub(super) fn scroll_by(&self, delta: Pixels) {
        self.0.borrow_mut().anchor.offset += delta;
    }

    pub(super) fn viewport(&self) -> Bounds<Pixels> {
        self.0.borrow().viewport
    }
}

impl ScrollbarHandle for LiveScroll {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.0.borrow().viewport
    }

    fn offset(&self) -> Point<Pixels> {
        point(px(0.), -self.0.borrow().top)
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        let mut state = self.0.borrow_mut();
        let content = state.dragged.unwrap_or(state.content);
        if content > px(0.) {
            state.jump = Some((-offset.y / content).clamp(0., 1.));
        }
    }

    fn content_size(&self) -> Size<Pixels> {
        let state = self.0.borrow();
        size(
            state.viewport.size.width,
            state.dragged.unwrap_or(state.content),
        )
    }

    fn start_drag(&self) {
        let mut state = self.0.borrow_mut();
        state.dragged = Some(state.content);
    }

    fn end_drag(&self) {
        self.0.borrow_mut().dragged = None;
    }
}

/// Geometry reported by the last prepaint, read by caret placement and input.
#[derive(Default)]
pub(super) struct Frame {
    /// The segments with a block laid out, in document order.
    pub(super) segments: Vec<Rc<Projection>>,
    pub(super) geometry: HashMap<u64, LeafGeometry>,
    /// Home put the caret here, at a wrapped line's start, which is also the
    /// end of the line above: draw it on the later line, as CodeMirror's
    /// caret association does.
    pub(super) wrap_start: Option<usize>,
    wide: Vec<WideTable>,
}

impl Frame {
    /// The laid-out segment, leaf and display offset of source position `pos`.
    pub(super) fn locate(&self, pos: usize) -> Option<(&Projection, usize, usize)> {
        let geometry =
            |p: &Projection, leaf: usize| self.geometry.contains_key(&p.leaves[leaf].key);
        self.segments.iter().find_map(|p| {
            let (leaf, disp) = p.locate(pos).filter(|&(leaf, _)| geometry(p, leaf))?;
            Some((p.as_ref(), leaf, disp))
        })
    }

    /// What shows of the wide table at height `y`, if one is there.
    pub(super) fn clip_at(&self, y: Pixels) -> Option<Bounds<Pixels>> {
        (self.wide.iter())
            .find(|t| t.clip.top() <= y && y < t.clip.bottom())
            .map(|t| t.clip)
    }
}

/// Whether a table starts at source position `at`.
fn table_at(segments: &[Segment], at: usize) -> bool {
    let i = segments.partition_point(|s| s.src.end <= at);
    (segments.get(i)).is_some_and(|s| s.projection.tables.iter().any(|t| t.src.start == at))
}

fn next(segments: &[Segment], (i, j): At) -> Option<At> {
    if j + 1 < segments[i].projection.blocks.len() {
        return Some((i, j + 1));
    }
    (i + 1..segments.len())
        .find(|&k| !segments[k].projection.blocks.is_empty())
        .map(|k| (k, 0))
}

fn prev(segments: &[Segment], (i, j): At) -> Option<At> {
    if j > 0 {
        return Some((i, j - 1));
    }
    (0..i)
        .rev()
        .find(|&k| !segments[k].projection.blocks.is_empty())
        .map(|k| (k, segments[k].projection.blocks.len() - 1))
}

/// The block that shows source position `pos`, or holds it.
fn block_at(segments: &[Segment], pos: usize) -> Option<At> {
    let first = segments.partition_point(|s| s.src.end < pos);
    (first..segments.len())
        .take_while(|&i| segments[i].src.start <= pos)
        .find_map(|i| {
            let p = &segments[i].projection;
            let shown = p
                .locate(pos)
                .and_then(|(leaf, _)| p.blocks.iter().position(|b| b.leaves.contains(&leaf)));
            shown
                .or_else(|| p.blocks.iter().rposition(|b| b.src.start <= pos))
                .map(|j| (i, j))
        })
}

/// A block's height before it is laid out, from its kind and text length.
fn estimate(p: &Projection, block: usize, width: f32) -> f32 {
    let b = &p.blocks[block];
    let text = b.leaves.first().map_or("", |&l| p.leaves[l].text.as_str());
    // Rows of text at a font size, at about half an em per character.
    let rows = |size: f32| -> f32 {
        let per_row = (width / (size * 0.5)).max(8.);
        text.split('\n')
            .map(|line| (line.len() as f32 / per_row).ceil().max(1.))
            .sum::<f32>()
            * size
            * LINE_HEIGHT
    };
    let line = FONT_SIZE * LINE_HEIGHT;
    let height = match &b.kind {
        BlockKind::Heading(level) => {
            let size = FONT_SIZE * heading_scale(*level);
            let (top, bottom) = heading_padding(*level);
            rows(size) + (top + bottom) * size
        }
        BlockKind::Code => rows(FONT_SIZE * 0.875) + 26.,
        BlockKind::Table { rows, .. } => rows.len() as f32 * (line + 12.) + 2.,
        _ => rows(FONT_SIZE),
    };
    height.max(line)
}

fn height(segments: &[Segment], (i, j): At, width: f32) -> Pixels {
    let cell = &segments[i].heights[j];
    if cell.get() <= 0. {
        cell.set(estimate(&segments[i].projection, j, width));
    }
    px(cell.get())
}

/// The summed heights of blocks `from` up to (not including) `to`.
fn between(segments: &[Segment], from: At, to: At, width: f32) -> Pixels {
    let mut sum = px(0.);
    let mut at = Some(from);
    while let Some(a) = at.filter(|a| *a < to) {
        sum += height(segments, a, width);
        at = next(segments, a);
    }
    sum
}

/// Lays out blocks on demand, each once per frame.
struct Layout<'a> {
    segments: &'a [Segment],
    style: &'a LiveStyle,
    rcx: &'a RenderContext,
    width: Pixels,
    laid: HashMap<At, (AnyElement, Pixels)>,
    table_x: &'a RefCell<HashMap<usize, f32>>,
    /// The source position whose table cell to scroll into view.
    follow: Option<usize>,
    /// The wide tables laid out: where each starts and how far it scrolls.
    wide: HashMap<At, (usize, Pixels)>,
}

impl Layout<'_> {
    fn measure(&mut self, at: At, window: &mut Window, cx: &mut App) -> Pixels {
        if let Some((_, height)) = self.laid.get(&at) {
            return *height;
        }
        let segment = &self.segments[at.0];
        let p = &segment.projection;
        let block = &p.blocks[at.1];
        // The first leaf's key: unique, and stable while the block is.
        let ix = block
            .leaves
            .first()
            .map_or(0, |&l| p.leaves[l].key as usize);
        let wide = match &block.kind {
            BlockKind::Table { rows, .. } => self.wide(at, p, block.src.start, rows, window, cx),
            _ => None,
        };
        let block = super::render_block(ix, block, p, self.style, self.rcx, wide, window, cx);
        // The column's width, as a block in a full-width column has: a rule
        // has no width of its own.
        let mut element = div().w(self.width).child(block).into_any_element();
        let available = size(
            AvailableSpace::Definite(self.width),
            AvailableSpace::MinContent,
        );
        let height = element.layout_as_root(available, window, cx).height;
        segment.heights[at.1].set(f32::from(height));
        self.laid.insert(at, (element, height));
        height
    }

    /// A table too wide for the column: the width it is drawn at and how far
    /// it is scrolled, its cell with `follow` brought into view.
    fn wide(
        &mut self,
        at: At,
        p: &Projection,
        start: usize,
        rows: &[Vec<usize>],
        window: &mut Window,
        cx: &App,
    ) -> Option<(Pixels, Pixels)> {
        // A table in a quote or a list is drawn as source, never this wide.
        let avail = self.width;
        let wide = wide::measure(p, rows, avail, window, cx)?;
        let mut table_x = self.table_x.borrow_mut();
        let mut x = px(table_x.get(&start).copied().unwrap_or(0.));
        if let Some((t, _, cell)) = self.follow.and_then(|pos| p.cell_at(pos))
            && p.tables[t].src.start == start
            && let Some(&(left, right)) = wide.columns.get(cell.col)
        {
            x = x.max(right - avail).min(left);
        }
        let max = wide.track - avail;
        x = x.clamp(px(0.), max);
        table_x.insert(start, f32::from(x));
        self.wide.insert(at, (start, max));
        Some((wide.track, x))
    }
}

/// The document column: lays out and paints the blocks in and near the
/// viewport, then the caret, and registers the platform input handler.
pub(super) struct Viewport {
    pub(super) segments: Rc<Vec<Segment>>,
    pub(super) style: LiveStyle,
    pub(super) rcx: RenderContext,
    pub(super) gutter: Pixels,
    /// Space below the last block.
    pub(super) bottom: Pixels,
    pub(super) selection: Range<usize>,
    pub(super) head: usize,
    /// Where the preedit starts.
    pub(super) marked: Option<usize>,
    pub(super) show_caret: bool,
    pub(super) read_only: bool,
    /// The cell with the caret gets a focus ring, unless read-only.
    pub(super) cell_ring: bool,
    pub(super) reveal: Rc<Cell<bool>>,
    pub(super) frame: Rc<RefCell<Frame>>,
    pub(super) scroll: LiveScroll,
    /// Each wide table's sideways scroll, from `LiveDoc`.
    pub(super) table_x: Rc<RefCell<HashMap<usize, f32>>>,
    pub(super) follow: Option<usize>,
    pub(super) view: Entity<LiveView>,
    pub(super) focus: FocusHandle,
}

pub(super) struct Prepainted {
    hitbox: Hitbox,
    blocks: Vec<(AnyElement, Bounds<Pixels>)>,
    /// The content column, as wide as a table's frame.
    column: Bounds<Pixels>,
}

impl IntoElement for Viewport {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Viewport {
    type RequestLayoutState = ();
    type PrepaintState = Prepainted;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepainted {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let segments = self.segments.clone();
        let segments = &segments[..];
        // Centred, at most the content width, as the column Tauri draws.
        let column = bounds
            .size
            .width
            .min(px(super::CONTENT_WIDTH) + self.gutter * 2.);
        let left = bounds.left() + (bounds.size.width - column) / 2. + self.gutter;
        let width = (column - self.gutter * 2.).max(px(1.));
        let w = f32::from(width);
        let page = bounds.size.height;
        // A table that went away takes its scroll with it.
        self.table_x
            .borrow_mut()
            .retain(|&at, _| table_at(segments, at));
        let mut layout = Layout {
            segments,
            style: &self.style,
            rcx: &self.rcx,
            width,
            laid: HashMap::new(),
            table_x: &self.table_x,
            follow: self.follow,
            wide: HashMap::new(),
        };
        let mut state = self.scroll.0.borrow_mut();
        let first = (0, 0);
        // Where the anchor is now: its segment by id, else where it was.
        let mut anchor = state.anchor;
        let found = segments
            .get(anchor.segment)
            .filter(|s| s.id == anchor.id)
            .map(|_| anchor.segment);
        let found = found.or_else(|| segments.iter().position(|s| s.id == anchor.id));
        let mut at = match found {
            Some(i) => (i, anchor.block),
            None => (anchor.segment.min(segments.len() - 1), anchor.block),
        };
        at.1 =
            at.1.min(segments[at.0].projection.blocks.len().saturating_sub(1));
        let mut offset = anchor.offset;
        if let Some(fraction) = state.jump.take() {
            // The scrollbar: walk the summed heights to the point it names.
            let content =
                px(PAD_TOP) + between(segments, first, (segments.len(), 0), w) + self.bottom;
            let mut y = content * fraction - px(PAD_TOP);
            at = first;
            while let Some(n) = next(segments, at) {
                let h = height(segments, at, w);
                if y < h {
                    break;
                }
                y -= h;
                at = n;
            }
            offset = y;
        }
        // A long way off (a jump): skip on the known heights first.
        while offset > page * 3.
            && let Some(n) = next(segments, at)
        {
            offset -= height(segments, at, w);
            at = n;
        }
        while offset < -page * 3.
            && let Some(p) = prev(segments, at)
        {
            offset += height(segments, p, w);
            at = p;
        }
        let mut placed: Vec<(At, Pixels)> = Vec::new();
        for _ in 0..2 {
            // The block at the top of the viewport, laid out.
            while offset < px(0.) {
                match prev(segments, at) {
                    Some(p) => {
                        offset += layout.measure(p, window, cx);
                        at = p;
                    }
                    None => {
                        offset = offset.max(px(-PAD_TOP));
                        break;
                    }
                }
            }
            loop {
                let h = layout.measure(at, window, cx);
                match next(segments, at) {
                    Some(n) if offset >= h => {
                        offset -= h;
                        at = n;
                    }
                    _ => break,
                }
            }
            // Down to a page past the bottom.
            placed.clear();
            let mut y = bounds.top() - offset;
            let mut block = Some(at);
            while let Some(a) = block {
                // At least one, however short the view.
                if y >= bounds.bottom() + page && !placed.is_empty() {
                    break;
                }
                placed.push((a, y));
                y += layout.measure(a, window, cx);
                block = next(segments, a);
            }
            // Past the end: scroll back so the content's end meets the
            // viewport's, unless the top is already showing.
            let short = bounds.bottom() - (y + self.bottom);
            if block.is_some()
                || short <= px(0.)
                || (prev(segments, at).is_none() && offset <= px(-PAD_TOP))
            {
                break;
            }
            offset -= short;
        }
        // And a page above the top.
        let mut y = bounds.top() - offset;
        let mut block = prev(segments, at);
        let mut above = Vec::new();
        while let Some(a) = block {
            if y <= bounds.top() - page {
                break;
            }
            y -= layout.measure(a, window, cx);
            above.push((a, y));
            block = prev(segments, a);
        }
        above.reverse();
        above.append(&mut placed);
        let mut placed = above;
        // The caret's block and those beside it, for Up and Down, and the
        // preedit's, wherever they are.
        let caret = block_at(segments, self.head);
        let beside = caret.map(|a| [prev(segments, a), Some(a), next(segments, a)]);
        let marked = self.marked.and_then(|pos| block_at(segments, pos));
        // And the table asked to show a cell, off screen too.
        let follow = self.follow.and_then(|pos| block_at(segments, pos));
        let around: Vec<At> = beside
            .into_iter()
            .flatten()
            .chain([marked, follow])
            .flatten()
            .collect();
        let (&(first_at, first_top), &(last_at, last_top)) = (
            placed.first().expect("a block"),
            placed.last().expect("a block"),
        );
        let below = last_top + layout.measure(last_at, window, cx);
        for a in around {
            if placed.iter().any(|(p, _)| *p == a) {
                continue;
            }
            layout.measure(a, window, cx);
            let y = if a < first_at {
                first_top - between(segments, a, first_at, w)
            } else {
                let after = next(segments, last_at).unwrap_or(a);
                below + between(segments, after, a, w)
            };
            placed.push((a, y));
        }
        placed.sort_by_key(|(a, _)| *a);

        let mut decorations: HashMap<u64, Vec<(Range<usize>, Hsla)>> = HashMap::new();
        let mut drawn: Vec<usize> = placed.iter().map(|((i, _), _)| *i).collect();
        drawn.dedup();
        if !self.selection.is_empty() {
            for &i in &drawn {
                let p = &segments[i].projection;
                for (l, leaf) in p.leaves.iter().enumerate() {
                    if let Some(range) = p.leaf_selection(l, &self.selection) {
                        decorations
                            .entry(leaf.key)
                            .or_default()
                            .push((range, self.style.selection));
                    }
                }
            }
        }
        let mut blocks = Vec::with_capacity(placed.len());
        let mut wide = Vec::new();
        for (a, top) in &placed {
            let (element, height) = layout.laid.remove(a).expect("laid out above");
            let at = Bounds::new(point(left, *top), size(width, height));
            if let Some(&(start, max)) = layout.wide.get(a) {
                wide.push(WideTable {
                    start,
                    clip: at,
                    max,
                });
            }
            blocks.push((element, at));
        }
        let ((), geometry) = collect_leaf_geometry(decorations, || {
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                for (element, at) in &mut blocks {
                    element.prepaint_at(at.origin, window, cx);
                }
            })
        });
        {
            let mut frame = self.frame.borrow_mut();
            frame.segments = (drawn.iter())
                .map(|&i| segments[i].projection.clone())
                .collect();
            frame.geometry = geometry;
            frame.wide = wide;
        }

        // Keep a caret moved by key in view, next frame; the pointer scrolls freely.
        if self.reveal.take()
            && let Some(rect) = caret_bounds(&self.frame.borrow(), self.head)
            && let Some(&(caret_at, caret_top)) =
                block_at(segments, self.head).and_then(|a| placed.iter().find(|(p, _)| *p == a))
        {
            let margin = px(FONT_SIZE * LINE_HEIGHT);
            let into = |y: Pixels| y - caret_top;
            let to = if rect.bottom() > bounds.bottom() {
                Some(into(rect.bottom()) + margin - page)
            } else if rect.top() < bounds.top() {
                Some(into(rect.top()) - margin)
            } else {
                None
            };
            if let Some(to) = to {
                at = caret_at;
                offset = to;
                // A notify while drawing is not a new frame.
                let view = self.view.entity_id();
                window.on_next_frame(move |_, cx| cx.notify(view));
            }
        }

        anchor = Anchor {
            id: segments[at.0].id,
            segment: at.0,
            block: at.1,
            offset,
        };
        let top = between(segments, first, at, w);
        let rest = between(segments, at, (segments.len(), 0), w);
        state.anchor = anchor;
        state.viewport = bounds;
        state.top = px(PAD_TOP) + top + offset;
        state.content = px(PAD_TOP) + top + rest + self.bottom;
        let column = Bounds::new(point(left, bounds.top()), size(width, bounds.size.height));
        Prepainted {
            hitbox,
            blocks,
            column,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepainted: &mut Prepainted,
        window: &mut Window,
        cx: &mut App,
    ) {
        let view = self.view.entity_id();
        let scroll = self.scroll.clone();
        let hitbox = prepainted.hitbox.id;
        let line = px(FONT_SIZE * LINE_HEIGHT);
        // A drag is followed off the view too, where its own listeners do
        // not hear the pointer.
        let dragged = self.view.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Capture {
                dragged.update(cx, |view, cx| view.drag(event, cx));
            }
        });
        let wide = self.frame.borrow().wide.clone();
        let table_x = self.table_x.clone();
        window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.should_handle_scroll(window) {
                let mut delta = event.delta.pixel_delta(line);
                let table = wide.iter().find(|t| t.clip.contains(&event.position));
                if let Some(table) = table {
                    // Shift turns the wheel sideways over a table, where the
                    // platform does not already (X11 does).
                    if event.modifiers.shift && delta.x == px(0.) {
                        delta = point(delta.y, px(0.));
                    }
                    // One axis at a time: a swipe down the page wanders
                    // sideways a little, and must not move the table.
                    if delta.x.abs() > delta.y.abs() {
                        delta.y = px(0.);
                    } else {
                        delta.x = px(0.);
                    }
                    if delta.x != px(0.) {
                        let mut table_x = table_x.borrow_mut();
                        let x = table_x.entry(table.start).or_default();
                        *x = (*x - f32::from(delta.x)).clamp(0., f32::from(table.max));
                        cx.notify(view);
                    }
                }
                if delta.y != px(0.) {
                    scroll.scroll_by(-delta.y);
                    cx.notify(view);
                }
            }
        });
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for (element, at) in &mut prepainted.blocks {
                // Laid out for the caret and the keys, not shown.
                if at.bottom() > bounds.top() && at.top() < bounds.bottom() {
                    element.paint(window, cx);
                }
            }
            let frame = self.frame.borrow();
            for p in &frame.segments {
                for block in &p.blocks {
                    if let Some((bounds, checked, _)) = task_box(&frame, p, block) {
                        paint_task(bounds, checked, self.read_only, &self.style, window);
                    }
                }
            }
            if self.cell_ring
                && let Some(ring) =
                    cell_ring(&frame, self.head, prepainted.column, window.rem_size())
            {
                // As Tauri's focused cell: a 2px inset ring, the accent at 70%.
                let color = self.style.accent.opacity(0.7);
                window.paint_quad(quad(
                    ring,
                    px(0.),
                    transparent_black(),
                    px(2.),
                    color,
                    BorderStyle::Solid,
                ));
            }
            if self.show_caret
                && let Some(rect) = caret_bounds(&frame, self.head)
            {
                // In a wide table, only where the table shows.
                let mask = frame
                    .clip_at(rect.center().y)
                    .map(|bounds| ContentMask { bounds });
                window.with_content_mask(mask, |window| {
                    window.paint_quad(fill(rect, self.style.ink_strong))
                });
            }
        });
        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
    }
}
