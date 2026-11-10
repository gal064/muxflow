//! GPUI view: renders the projection through gpui-base's `editor_host`
//! (BlockNode tree + per-leaf geometry) and implements the platform input
//! handler. All editing logic lives in `live.rs`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::base::text::editor_host::{
    BlockNode, ColumnAlign, FlowPosition, LeafGeometry, LinkMark, Paragraph, RenderContext, Table, TextMark,
    collect_leaf_geometry,
};
use gpui_kit::base::text::editor_host::InlineNode;
use gpui_kit::*;

use spike_shared::fonts::MONO;
use spike_shared::theme;

use crate::live::{CellDir, LiveDoc};
use crate::projection::{Align, Block, BlockKind, Leaf, Projection, Style};

actions!(
    live_md,
    [
        MoveLeft, MoveRight, MoveUp, MoveDown, SelectLeft, SelectRight, SelectUp, SelectDown,
        MoveHome, MoveEnd, SelectHome, SelectEnd, Backspace, Delete, Enter, TabNext, TabPrev,
        Undo, Redo, Save, SelectAll, Copy, Paste, ImeDemo,
    ]
);

const CONTEXT: &str = "LiveMarkdown";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("left", MoveLeft, Some(CONTEXT)),
        KeyBinding::new("right", MoveRight, Some(CONTEXT)),
        KeyBinding::new("up", MoveUp, Some(CONTEXT)),
        KeyBinding::new("down", MoveDown, Some(CONTEXT)),
        KeyBinding::new("shift-left", SelectLeft, Some(CONTEXT)),
        KeyBinding::new("shift-right", SelectRight, Some(CONTEXT)),
        KeyBinding::new("shift-up", SelectUp, Some(CONTEXT)),
        KeyBinding::new("shift-down", SelectDown, Some(CONTEXT)),
        KeyBinding::new("home", MoveHome, Some(CONTEXT)),
        KeyBinding::new("end", MoveEnd, Some(CONTEXT)),
        KeyBinding::new("shift-home", SelectHome, Some(CONTEXT)),
        KeyBinding::new("shift-end", SelectEnd, Some(CONTEXT)),
        KeyBinding::new("backspace", Backspace, Some(CONTEXT)),
        KeyBinding::new("delete", Delete, Some(CONTEXT)),
        KeyBinding::new("enter", Enter, Some(CONTEXT)),
        KeyBinding::new("tab", TabNext, Some(CONTEXT)),
        KeyBinding::new("shift-tab", TabPrev, Some(CONTEXT)),
        KeyBinding::new("ctrl-z", Undo, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-z", Redo, Some(CONTEXT)),
        KeyBinding::new("ctrl-y", Redo, Some(CONTEXT)),
        KeyBinding::new("ctrl-s", Save, Some(CONTEXT)),
        KeyBinding::new("ctrl-a", SelectAll, Some(CONTEXT)),
        KeyBinding::new("ctrl-c", Copy, Some(CONTEXT)),
        KeyBinding::new("ctrl-v", Paste, Some(CONTEXT)),
        KeyBinding::new("f8", ImeDemo, Some(CONTEXT)),
    ]);
}

/// Geometry reported by the last prepaint, read by caret placement and input.
#[derive(Default)]
struct Frame {
    projection: Rc<Projection>,
    geometry: HashMap<u64, LeafGeometry>,
}

pub struct LiveEditor {
    pub live: LiveDoc,
    focus: FocusHandle,
    out_path: PathBuf,
    state_log: Option<PathBuf>,
    frame: Rc<RefCell<Frame>>,
    goal_x: Option<Pixels>,
    selecting: bool,
    ime_step: usize,
    scroll: ScrollHandle,
}

impl LiveEditor {
    pub fn new(source: String, out_path: PathBuf, state_log: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            live: LiveDoc::new(source),
            focus,
            out_path,
            state_log,
            frame: Rc::default(),
            goal_x: None,
            selecting: false,
            ime_step: 0,
            scroll: ScrollHandle::new(),
        }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.log_state();
        cx.notify();
    }

    /// One state snapshot per action for QA: caret offsets and what the
    /// caret's leaf shows. Overwritten each time; the last line is the state.
    fn log_state(&mut self) {
        let Some(path) = self.state_log.clone() else { return };
        let caret = self.caret_rect();
        let sel = self.live.selection();
        let p = self.live.projection();
        let shown = p.locate(sel.head).map(|(l, _)| p.leaves[l].text.clone()).unwrap_or_default();
        let line = format!(
            "{{\"anchor\":{},\"head\":{},\"marked\":{:?},\"leaf\":{:?},\"source_len\":{},\"revision\":{},\"caret\":{:?}}}\n",
            sel.anchor,
            sel.head,
            self.live.marked().map(|r| (r.start, r.end)),
            shown,
            self.live.text().len(),
            self.live.doc.revision(),
            caret,
        );
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::write(&path, existing + &line);
    }

    fn hit(&self, at: Point<Pixels>) -> Option<usize> {
        let frame = self.frame.borrow();
        let projection = frame.projection.clone();
        let mut leaves: Vec<(Pixels, Pixels, usize, &LeafGeometry)> = Vec::new();
        for (i, leaf) in projection.leaves.iter().enumerate() {
            let Some(geometry) = frame.geometry.get(&leaf.key) else { continue };
            let bounds = geometry.bounds();
            let dy = if at.y < bounds.top() {
                bounds.top() - at.y
            } else if at.y > bounds.bottom() {
                at.y - bounds.bottom()
            } else {
                px(0.)
            };
            let dx = if at.x < bounds.left() {
                bounds.left() - at.x
            } else if at.x > bounds.right() {
                at.x - bounds.right()
            } else {
                px(0.)
            };
            leaves.push((dy, dx, i, geometry));
        }
        // Nearest row first, then nearest column: leaves on one visual row differ
        // by a few pixels (inline code is a taller run), so compare rows loosely.
        let nearest_row = leaves.iter().map(|l| l.0).fold(None, |m: Option<Pixels>, d| Some(m.map_or(d, |m| m.min(d))))?;
        let &(_, _, i, geometry) = leaves
            .iter()
            .filter(|l| l.0 <= nearest_row + px(8.))
            .min_by(|a, b| (a.1, a.0).partial_cmp(&(b.1, b.0)).unwrap_or(std::cmp::Ordering::Equal))?;
        Some(projection.from_display(i, geometry.index_for_position(at)))
    }

    fn caret_rect(&self) -> Option<Bounds<Pixels>> {
        let frame = self.frame.borrow();
        caret_bounds(&frame, self.live.selection().head)
    }

    fn vertical(&mut self, down: bool, extend: bool, cx: &mut Context<Self>) {
        if !extend && self.live.cell_vertical(down) {
            self.changed(cx);
            return;
        }
        let Some(rect) = self.caret_rect() else { return };
        let goal = self.goal_x.unwrap_or(rect.origin.x);
        let head = self.live.selection().head;
        let step = rect.size.height / 2.;
        // Probe downward or upward until the hit lands on another visual line:
        // a table row can be several caret heights tall.
        let mut y = rect.origin.y + step;
        for _ in 0..16 {
            y = if down { y + step } else { y - step };
            let Some(pos) = self.hit(point(goal, y)) else { return };
            if pos == head {
                continue;
            }
            let moved = caret_bounds(&self.frame.borrow(), pos).is_none_or(|c| {
                if down { c.top() > rect.top() + step } else { c.top() < rect.top() - step }
            });
            if moved {
                if extend { self.live.extend_to(pos, down) } else { self.live.set_caret(pos) }
                self.log_state();
                self.goal_x = Some(goal);
                cx.notify();
                return;
            }
        }
    }

    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut LiveDoc)) {
        f(&mut self.live);
        self.changed(cx);
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let _ = std::fs::write(&self.out_path, self.live.save_bytes());
        self.changed(cx);
    }
}

fn caret_bounds(frame: &Frame, head: usize) -> Option<Bounds<Pixels>> {
    let (leaf, disp) = frame.projection.locate(head)?;
    let leaf = &frame.projection.leaves[leaf];
    let geometry = frame.geometry.get(&leaf.key)?;
    let origin = geometry.position_for_index(disp);
    let height = geometry.caret_height();
    Some(Bounds::new(origin, size(px(2.), height)))
}

fn text_mark(style: &Style) -> TextMark {
    let mut mark = TextMark::default();
    if style.bold {
        mark = mark.bold();
    }
    if style.italic {
        mark = mark.italic();
    }
    if style.strike {
        mark = mark.strikethrough();
    }
    if style.code {
        mark = mark.code();
    }
    if style.underline {
        mark = mark.underline();
    }
    if style.link {
        mark = mark.link(LinkMark { url: SharedString::default(), ..Default::default() });
    }
    mark
}

/// Non-overlapping runs over the leaf text, each with its merged style, as
/// `Paragraph` marks expect.
fn merged_marks(leaf: &Leaf) -> Vec<(Range<usize>, TextMark)> {
    let mut points: Vec<usize> = leaf.marks.iter().flat_map(|(r, _)| [r.start, r.end]).collect();
    points.sort_unstable();
    points.dedup();
    let mut out = Vec::new();
    for window in points.windows(2) {
        let run = window[0]..window[1];
        let mut style = Style::default();
        let mut any = false;
        for (r, s) in &leaf.marks {
            if r.start <= run.start && r.end >= run.end {
                style.bold |= s.bold;
                style.italic |= s.italic;
                style.strike |= s.strike;
                style.code |= s.code;
                style.link |= s.link;
                style.underline |= s.underline;
                any = true;
            }
        }
        if any {
            out.push((run, text_mark(&style)));
        }
    }
    out
}

fn paragraph(leaf: &Leaf) -> Paragraph {
    let mut p = Paragraph::default();
    p.push(InlineNode::new(leaf.text.clone()).marks(merged_marks(leaf)));
    p.with_leaf_key(leaf.key)
}

fn alone<'a>(block: &'a [&'a BlockNode]) -> FlowPosition {
    FlowPosition::at(block, 0, true).first(true).last(true)
}

fn render_block(ix: usize, block: &Block, p: &Projection, rcx: &RenderContext, window: &mut Window, cx: &mut App) -> AnyElement {
    let palette = theme::dark();
    let leaf0 = block.leaves.first().map(|l| &p.leaves[*l]);
    let node: BlockNode = match &block.kind {
        BlockKind::Paragraph | BlockKind::Raw | BlockKind::Blank | BlockKind::Rule { revealed: true } => {
            BlockNode::Paragraph(paragraph(leaf0.expect("text block has a leaf")))
        }
        BlockKind::Heading(level) => BlockNode::Heading {
            level: *level,
            children: paragraph(leaf0.expect("heading has a leaf")),
            span: None,
        },
        BlockKind::ListItem { .. } => BlockNode::Paragraph(paragraph(leaf0.expect("item has a leaf"))),
        BlockKind::Code => BlockNode::Paragraph(paragraph(leaf0.expect("code has a leaf"))),
        BlockKind::Rule { revealed: false } => BlockNode::HorizontalRule { span: None },
        BlockKind::Table { rows, aligns } => {
            let rows: Vec<Vec<Paragraph>> = rows
                .iter()
                .map(|row| row.iter().map(|l| paragraph(&p.leaves[*l])).collect())
                .collect();
            let aligns = aligns
                .iter()
                .map(|a| match a {
                    Align::None | Align::Left => ColumnAlign::Left,
                    Align::Center => ColumnAlign::Center,
                    Align::Right => ColumnAlign::Right,
                })
                .collect();
            BlockNode::Table(Table::new(rows, aligns))
        }
    };
    let nodes = [&node];
    let flow = alone(&nodes);
    let element = node.render(ix, flow, rcx, window, cx);
    let inner = match &block.kind {
        BlockKind::Code => div()
            .bg(palette.chrome_border)
            .rounded_md()
            .p_2()
            .font_family(MONO)
            .child(element)
            .into_any_element(),
        BlockKind::ListItem { depth } => div().pl(px(18. * (*depth as f32 + 1.))).child(element).into_any_element(),
        BlockKind::Rule { revealed: false } => div().child(element).into_any_element(),
        _ => element,
    };
    if block.quote > 0 {
        div()
            .pl(px(12. * block.quote as f32))
            .border_l_2()
            .border_color(palette.chrome_dim)
            .child(inner)
            .into_any_element()
    } else {
        inner
    }
}

/// Runs the child under an editor-host geometry scope, then paints the caret
/// and registers the platform input handler.
struct LiveRoot {
    child: Option<AnyElement>,
    frame: Rc<RefCell<Frame>>,
    projection: Rc<Projection>,
    selection: Range<usize>,
    head: usize,
    show_caret: bool,
    view: Entity<LiveEditor>,
    focus: FocusHandle,
    scroll: ScrollHandle,
}

impl IntoElement for LiveRoot {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for LiveRoot {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        (self.child.as_mut().expect("child set").request_layout(window, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        let selection_color: Hsla = theme::dark().accent_wash.into();
        let mut decorations: HashMap<u64, Vec<(Range<usize>, Hsla)>> = HashMap::new();
        if !self.selection.is_empty() {
            for (i, leaf) in self.projection.leaves.iter().enumerate() {
                if let Some(range) = self.projection.leaf_selection(i, &self.selection) {
                    decorations.entry(leaf.key).or_default().push((range, selection_color));
                }
            }
        }
        let child = self.child.as_mut().expect("child set");
        let (_, geometry) = collect_leaf_geometry(decorations, || child.prepaint(window, cx));
        let mut frame = self.frame.borrow_mut();
        frame.projection = self.projection.clone();
        frame.geometry = geometry;
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        self.child.as_mut().expect("child set").paint(window, cx);
        let frame = self.frame.borrow();
        if self.show_caret {
            if let Some(rect) = caret_bounds(&frame, self.head) {
                window.paint_quad(fill(rect, theme::dark().accent));
                // Keep the caret in view.
                let viewport = self.scroll.bounds();
                if viewport.size.height > px(0.) {
                    let mut offset = self.scroll.offset();
                    if rect.bottom() > viewport.bottom() {
                        offset.y -= rect.bottom() - viewport.bottom() + px(8.);
                        self.scroll.set_offset(offset);
                        window.refresh();
                    } else if rect.top() < viewport.top() {
                        offset.y += viewport.top() - rect.top() + px(8.);
                        self.scroll.set_offset(offset);
                        window.refresh();
                    }
                }
            }
        }
        drop(frame);
        window.handle_input(&self.focus, ElementInputHandler::new(bounds, self.view.clone()), cx);
    }
}

impl Render for LiveEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        self.live.focused = focused;
        let palette = theme::dark();
        let projection = Rc::new(self.live.projection());
        let rcx = RenderContext::from_app(cx)
            .with_body_font_size(px(15.))
            .on_link_click(|_, _, _, _| {});
        let blocks: Vec<AnyElement> = projection
            .blocks
            .iter()
            .enumerate()
            .map(|(ix, block)| render_block(ix, block, &projection, &rcx, window, cx))
            .collect();
        let sel = self.live.selection();
        let root = LiveRoot {
            child: Some(div().w_full().flex().flex_col().gap_1().children(blocks).into_any_element()),
            frame: self.frame.clone(),
            projection,
            selection: sel.range(),
            head: sel.head,
            show_caret: focused,
            view: cx.entity(),
            focus: self.focus.clone(),
            scroll: self.scroll.clone(),
        };

        div()
            .id("live-markdown")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .bg(palette.chrome_bg)
            .text_color(palette.chrome_ink)
            .font_family(spike_shared::fonts::SANS)
            .text_size(px(15.))
            .on_action(cx.listener(|this, _: &MoveLeft, _, cx| this.edit(cx, |l| l.move_horizontal(false, false))))
            .on_action(cx.listener(|this, _: &MoveRight, _, cx| this.edit(cx, |l| l.move_horizontal(true, false))))
            .on_action(cx.listener(|this, _: &SelectLeft, _, cx| this.edit(cx, |l| l.move_horizontal(false, true))))
            .on_action(cx.listener(|this, _: &SelectRight, _, cx| this.edit(cx, |l| l.move_horizontal(true, true))))
            .on_action(cx.listener(|this, _: &MoveUp, _, cx| this.vertical(false, false, cx)))
            .on_action(cx.listener(|this, _: &MoveDown, _, cx| this.vertical(true, false, cx)))
            .on_action(cx.listener(|this, _: &SelectUp, _, cx| this.vertical(false, true, cx)))
            .on_action(cx.listener(|this, _: &SelectDown, _, cx| this.vertical(true, true, cx)))
            .on_action(cx.listener(|this, _: &MoveHome, _, cx| this.edit(cx, |l| l.move_line_edge(false, false))))
            .on_action(cx.listener(|this, _: &MoveEnd, _, cx| this.edit(cx, |l| l.move_line_edge(true, false))))
            .on_action(cx.listener(|this, _: &SelectHome, _, cx| this.edit(cx, |l| l.move_line_edge(false, true))))
            .on_action(cx.listener(|this, _: &SelectEnd, _, cx| this.edit(cx, |l| l.move_line_edge(true, true))))
            .on_action(cx.listener(|this, _: &Backspace, _, cx| this.edit(cx, |l| l.backspace())))
            .on_action(cx.listener(|this, _: &Delete, _, cx| this.edit(cx, |l| l.delete_forward())))
            .on_action(cx.listener(|this, _: &Enter, _, cx| this.edit(cx, |l| l.enter())))
            .on_action(cx.listener(|this, _: &TabNext, _, cx| this.edit(cx, |l| { let _ = l.cell_navigate(CellDir::Next); })))
            .on_action(cx.listener(|this, _: &TabPrev, _, cx| this.edit(cx, |l| { let _ = l.cell_navigate(CellDir::Prev); })))
            .on_action(cx.listener(|this, _: &Undo, _, cx| this.edit(cx, |l| l.undo())))
            .on_action(cx.listener(|this, _: &Redo, _, cx| this.edit(cx, |l| l.redo())))
            .on_action(cx.listener(|this, _: &Save, _, cx| this.save(cx)))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| this.edit(cx, |l| l.select_all())))
            .on_action(cx.listener(|this, _: &Copy, _, cx| {
                let range = this.live.selection().range();
                let text = this.live.text()[range].to_string();
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }))
            .on_action(cx.listener(|this, _: &ImeDemo, window, cx| this.ime_demo(window, cx)))
            .on_action(cx.listener(|this, _: &Paste, _, cx| {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    this.edit(cx, |l| l.insert_text(&text));
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, window, cx| {
                window.focus(&this.focus, cx);
                if let Some(pos) = this.hit(event.position) {
                    if event.modifiers.shift { this.live.set_head(pos) } else { this.live.set_caret(pos) }
                    this.selecting = true;
                    this.changed(cx);
                }
            }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this.selecting && event.pressed_button == Some(MouseButton::Left) {
                    if let Some(pos) = this.hit(event.position) {
                        this.live.set_head(pos);
                        this.changed(cx);
                    }
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, _| this.selecting = false))
            .child(
                div()
                    .id("live-markdown-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .p_6()
                    .child(root),
            )
    }
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

fn to_byte(text: &str, utf16: usize) -> usize {
    let mut count = 0;
    for (i, c) in text.char_indices() {
        if count >= utf16 {
            return i;
        }
        count += c.len_utf16();
    }
    text.len()
}

fn to_byte_range(text: &str, r: &Range<usize>) -> Range<usize> {
    to_byte(text, r.start)..to_byte(text, r.end)
}

fn to_utf16_range(text: &str, r: Range<usize>) -> Range<usize> {
    utf16_len(&text[..r.start])..utf16_len(&text[..r.end])
}

impl EntityInputHandler for LiveEditor {
    fn text_for_range(&mut self, range: Range<usize>, adjusted: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        let text = self.live.text();
        let bytes = to_byte_range(text, &range);
        *adjusted = Some(to_utf16_range(text, bytes.clone()));
        Some(text[bytes].to_string())
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        let sel = self.live.selection();
        let text = self.live.text();
        Some(UTF16Selection {
            range: to_utf16_range(text, sel.range()),
            reversed: sel.head < sel.anchor,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.live.marked().map(|r| to_utf16_range(self.live.text(), r))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.live.commit_composition();
        self.changed(cx);
    }

    fn replace_text_in_range(&mut self, range: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(range) = range {
            let bytes = to_byte_range(self.live.text(), &range);
            self.live.doc.set_selection(crate::doc::Selection { anchor: bytes.start, head: bytes.end });
        }
        self.live.insert_text(text);
        self.changed(cx);
    }

    fn replace_and_mark_text_in_range(&mut self, range: Option<Range<usize>>, new_text: &str, selected: Option<Range<usize>>, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(range) = range {
            let bytes = to_byte_range(self.live.text(), &range);
            self.live.doc.set_selection(crate::doc::Selection { anchor: bytes.start, head: bytes.end });
        }
        // `selected` is in UTF-16 offsets within `new_text`.
        let caret = selected.map(|r| to_byte(new_text, r.end));
        self.live.set_preedit(new_text, caret);
        self.changed(cx);
    }

    fn bounds_for_range(&mut self, range: Range<usize>, _: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let frame = self.frame.borrow();
        let start = to_byte(self.live.text(), range.start);
        let rect = caret_bounds(&frame, start)?;
        Some(rect)
    }

    fn character_index_for_point(&mut self, at: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        let pos = self.hit(at)?;
        Some(utf16_len(&self.live.text()[..pos]))
    }
}

impl LiveEditor {
    /// F8 steps a composition through the same calls the platform input
    /// handler makes: preedit, longer preedit, commit. Real compositor IME is
    /// out of scope here (see REPORT.md); this exercises the editor half.
    fn ime_demo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let step = self.ime_step;
        self.ime_step = (step + 1) % 4;
        match step {
            0 => self.replace_and_mark_text_in_range(None, "n", Some(1..1), window, cx),
            1 => self.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx),
            2 => self.replace_and_mark_text_in_range(None, "nihon", Some(5..5), window, cx),
            _ => self.replace_text_in_range(None, "\u{65e5}\u{672c}", window, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{to_byte, to_byte_range, to_utf16_range, utf16_len};

    #[test]
    fn utf16_offsets_convert_around_astral_and_multibyte_text() {
        let text = "a\u{65e5}\u{1F600}b";
        assert_eq!(utf16_len(text), 5);
        assert_eq!(to_byte(text, 1), 1);
        assert_eq!(to_byte(text, 2), 4);
        assert_eq!(to_byte(text, 4), 8);
        assert_eq!(to_byte(text, 5), 9);
        assert_eq!(to_utf16_range(text, 4..8), 2..4);
        assert_eq!(to_byte_range(text, &(2..4)), 4..8);
    }
}
