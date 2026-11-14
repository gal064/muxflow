//! The GPUI view, and the only module with GPUI types. It draws the projection
//! through gpui-base's `editor_host` (a `BlockNode` tree plus per-leaf
//! geometry), paints the caret and the selection, registers the platform
//! input handler, and turns keys and the pointer into `edit` calls. Only the
//! blocks near the viewport are laid out (`viewport`).
//!
//! The host owns the buffer: it starts the view with the text, hands it every
//! change made elsewhere ([`LiveView::set_text`]) and takes the view's edits
//! when told ([`LiveEvent::Edited`], [`LiveView::take_edited`]).

mod viewport;
mod wide;

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::base::text::editor_host::{
    BlockNode, ColumnAlign, FlowPosition, InlineNode, LinkMark, Paragraph, RenderContext, Table,
    TextMark,
};
use gpui_kit::base::{Scrollbar, ScrollbarHandle, TextViewStyle};
use gpui_kit::component::input::{Escape, Replace, Search};
use gpui_kit::*;
use regex::Regex;
use unicode_segmentation::UnicodeSegmentation;

use crate::doc::{Change, Selection};
use crate::edit::{LiveDoc, TableKey};
use crate::projection::{Align, Block, BlockKind, Leaf, Marker, Projection, SegKind, Style};
use viewport::{Frame, LiveScroll, Viewport};

actions!(
    live_markdown,
    [
        MoveLeft,
        MoveRight,
        MoveUp,
        MoveDown,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        PageUp,
        PageDown,
        SelectPageUp,
        SelectPageDown,
        MoveHome,
        MoveEnd,
        SelectHome,
        SelectEnd,
        MoveDocStart,
        MoveDocEnd,
        SelectDocStart,
        SelectDocEnd,
        Backspace,
        Delete,
        Enter,
        TabNext,
        TabPrev,
        ToggleBold,
        ToggleItalic,
        Undo,
        Redo,
        SelectAll,
        Copy,
        Cut,
        Paste,
        MoveWordLeft,
        MoveWordRight,
        SelectWordLeft,
        SelectWordRight,
        Indent,
        Outdent,
    ]
);

/// The key context while the view has focus.
pub const KEY_CONTEXT: &str = "LiveMarkdown";
/// A read-only view leaves the editing chords to the app, as CodeMirror's
/// commands decline them there (Ctrl+B, Ctrl+[ and Ctrl+] are app shortcuts
/// too), and Ctrl+[ and Ctrl+] are the list's only with the caret in one.
const EDITABLE: &str = "LiveMarkdown && !read_only";
const IN_LIST: &str = "LiveMarkdown && in_list && !read_only";

/// Document typography, as `.live-markdown` in Tauri's `styles.css`: it
/// belongs to the page, not to the app's UI scale.
const FONT_SIZE: f32 = 16.;
const LINE_HEIGHT: f32 = 1.65;
const CONTENT_WIDTH: f32 = 720.;

/// Binds the view's keys. Called once at start.
pub fn init(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("left", MoveLeft, context),
        KeyBinding::new("right", MoveRight, context),
        KeyBinding::new("up", MoveUp, context),
        KeyBinding::new("down", MoveDown, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("shift-up", SelectUp, context),
        KeyBinding::new("shift-down", SelectDown, context),
        KeyBinding::new("pageup", PageUp, context),
        KeyBinding::new("pagedown", PageDown, context),
        KeyBinding::new("shift-pageup", SelectPageUp, context),
        KeyBinding::new("shift-pagedown", SelectPageDown, context),
        KeyBinding::new("home", MoveHome, context),
        KeyBinding::new("end", MoveEnd, context),
        KeyBinding::new("shift-home", SelectHome, context),
        KeyBinding::new("shift-end", SelectEnd, context),
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("shift-backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("enter", Enter, context),
        KeyBinding::new("tab", TabNext, context),
        KeyBinding::new("shift-tab", TabPrev, context),
        KeyBinding::new("escape", Escape, context),
    ]);
    // The host's find bar, as CodeMirror's search keymap and the source
    // editor bind it; Escape above also closes it.
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-f", Search, context),
        KeyBinding::new("cmd-shift-f", Replace, context),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-f", Search, context),
        KeyBinding::new("ctrl-h", Replace, context),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-z", Undo, context),
        KeyBinding::new("cmd-shift-z", Redo, context),
        KeyBinding::new("cmd-a", SelectAll, context),
        KeyBinding::new("cmd-c", Copy, context),
        KeyBinding::new("cmd-x", Cut, context),
        KeyBinding::new("cmd-v", Paste, context),
        KeyBinding::new("cmd-left", MoveHome, context),
        KeyBinding::new("cmd-right", MoveEnd, context),
        KeyBinding::new("cmd-shift-left", SelectHome, context),
        KeyBinding::new("cmd-shift-right", SelectEnd, context),
        KeyBinding::new("cmd-up", MoveDocStart, context),
        KeyBinding::new("cmd-down", MoveDocEnd, context),
        KeyBinding::new("cmd-shift-up", SelectDocStart, context),
        KeyBinding::new("cmd-shift-down", SelectDocEnd, context),
        KeyBinding::new("alt-left", MoveWordLeft, context),
        KeyBinding::new("alt-right", MoveWordRight, context),
        KeyBinding::new("alt-shift-left", SelectWordLeft, context),
        KeyBinding::new("alt-shift-right", SelectWordRight, context),
        KeyBinding::new("cmd-b", ToggleBold, Some(EDITABLE)),
        KeyBinding::new("cmd-i", ToggleItalic, Some(EDITABLE)),
        KeyBinding::new("cmd-]", Indent, Some(IN_LIST)),
        KeyBinding::new("cmd-[", Outdent, Some(IN_LIST)),
    ]);
    // As in Tauri, where CodeMirror took these first: a focused text field
    // gets first refusal over the app's shortcuts (Ctrl+B, Ctrl+[ and Ctrl+]
    // there toggle the sidebar and go back and forward).
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-z", Undo, context),
        KeyBinding::new("ctrl-shift-z", Redo, context),
        KeyBinding::new("ctrl-y", Redo, context),
        KeyBinding::new("ctrl-a", SelectAll, context),
        KeyBinding::new("ctrl-c", Copy, context),
        KeyBinding::new("ctrl-x", Cut, context),
        KeyBinding::new("ctrl-v", Paste, context),
        KeyBinding::new("ctrl-left", MoveWordLeft, context),
        KeyBinding::new("ctrl-right", MoveWordRight, context),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, context),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, context),
        KeyBinding::new("ctrl-home", MoveDocStart, context),
        KeyBinding::new("ctrl-end", MoveDocEnd, context),
        KeyBinding::new("ctrl-shift-home", SelectDocStart, context),
        KeyBinding::new("ctrl-shift-end", SelectDocEnd, context),
        KeyBinding::new("ctrl-b", ToggleBold, Some(EDITABLE)),
        KeyBinding::new("ctrl-i", ToggleItalic, Some(EDITABLE)),
        KeyBinding::new("ctrl-]", Indent, Some(IN_LIST)),
        KeyBinding::new("ctrl-[", Outdent, Some(IN_LIST)),
    ]);
}

/// The colors and faces the host draws the document with.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveStyle {
    pub sans: SharedString,
    pub mono: SharedString,
    pub background: Hsla,
    pub ink: Hsla,
    pub ink_strong: Hsla,
    pub dim: Hsla,
    pub accent: Hsla,
    pub selection: Hsla,
    pub raised: Hsla,
    pub hairline: Hsla,
    pub border: Hsla,
    pub dark: bool,
}

/// What the view tells its host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveEvent {
    /// The source changed: the host takes the text when
    /// [`LiveView::take_edited`] says so.
    Edited,
    /// A link was ⌘-clicked (Ctrl on Linux): its target, for the host to open
    /// as it opens the preview's links.
    Link(String),
}

/// A scrollbar thumb the view drags, with where along it it is held: the
/// page's, or the wide table's that starts at this source position.
#[derive(Clone, Copy)]
enum Thumb {
    Page(Pixels),
    Table(usize, Pixels),
}

pub struct LiveView {
    live: LiveDoc,
    style: LiveStyle,
    read_only: bool,
    focus: FocusHandle,
    frame: Rc<RefCell<Frame>>,
    /// The x a run of Up and Down keeps to.
    goal_x: Option<Pixels>,
    selecting: bool,
    /// The scrollbar thumb the view took the press for.
    thumb: Option<Thumb>,
    /// The caret moved by key: scroll it into view at the next layout.
    reveal: Rc<Cell<bool>>,
    /// The source position whose cell a wide table scrolls to show at the
    /// next layout.
    follow: Option<usize>,
    scroll: LiveScroll,
    /// The find bar's matcher while it is open: its matches are marked.
    find: Option<Rc<Regex>>,
    /// How far the host's find bar reaches down over the view: a match is
    /// revealed below it.
    top_inset: Pixels,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<LiveEvent> for LiveView {}

impl Focusable for LiveView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl LiveView {
    pub fn new(text: &str, style: LiveStyle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        // Markers reveal only with focus, and the host caches this view.
        let subscriptions = vec![
            cx.on_focus(&focus, window, |_, _, cx| cx.notify()),
            cx.on_blur(&focus, window, |_, _, cx| cx.notify()),
        ];
        let live = LiveDoc::new(text);
        Self {
            live,
            style,
            read_only: false,
            focus,
            frame: Rc::default(),
            goal_x: None,
            selecting: false,
            thumb: None,
            reveal: Rc::default(),
            follow: None,
            scroll: LiveScroll::default(),
            find: None,
            top_inset: px(0.),
            _subscriptions: subscriptions,
        }
    }

    pub fn text(&self) -> &str {
        self.live.text()
    }

    pub fn selection(&self) -> Selection {
        self.live.selection()
    }

    /// Moves on with every change to the text.
    pub fn revision(&self) -> u64 {
        self.live.revision()
    }

    /// Marks the matches of the find bar's `matcher` where the view draws,
    /// the selected one apart; none once the bar closes.
    pub fn set_find(&mut self, matcher: Option<Regex>, cx: &mut Context<Self>) {
        self.find = matcher.map(Rc::new);
        cx.notify();
    }

    /// How far the host's find bar covers the top of the view, so what is
    /// revealed comes into view below it; zero once the bar closes.
    pub fn set_top_inset(&mut self, inset: Pixels, cx: &mut Context<Self>) {
        if self.top_inset != inset {
            self.top_inset = inset;
            cx.notify();
        }
    }

    /// Selects a match of the find bar and scrolls it into view.
    pub fn select_match(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.motion(cx, |l| l.select_match(range));
    }

    /// The find bar's Replace, as one undo step.
    pub fn replace(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        self.edit(cx, |l| l.replace(range, text));
    }

    /// The find bar's Replace All, as one undo step. The view stays where it
    /// is, as CodeMirror's does, wide tables too.
    pub fn replace_all(&mut self, text: &str, cx: &mut Context<Self>) {
        self.edit(cx, |l| l.replace_all(text));
        self.reveal.set(false);
        self.follow = None;
    }

    /// The text changed outside the view. The caret stays where it was in the
    /// unchanged text.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if text == self.live.text() {
            return;
        }
        self.live.reset_text(text);
        cx.notify();
    }

    /// The span of the text changed since the last call, for the host's
    /// buffer. Never for an open IME preedit.
    pub fn take_edited(&mut self) -> Option<Change> {
        self.live.take_edited()
    }

    pub fn set_read_only(&mut self, read_only: bool, cx: &mut Context<Self>) {
        if self.read_only != read_only {
            self.read_only = read_only;
            self.live.set_read_only(read_only);
            cx.notify();
        }
    }

    pub fn set_style(&mut self, style: LiveStyle, cx: &mut Context<Self>) {
        if self.style != style {
            self.style = style;
            cx.notify();
        }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.frame.borrow_mut().wrap_start = None;
        self.reveal.set(true);
        // The pointer too: a click on a cell half shown brings it in, as
        // focusing Tauri's cell does.
        self.follow = Some(self.live.selection().head);
        // Also after a commit by `unmark_text`, which changes no text.
        if self.live.edited() {
            cx.emit(LiveEvent::Edited);
        }
        cx.notify();
    }

    fn motion(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut LiveDoc)) {
        f(&mut self.live);
        self.changed(cx);
    }

    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut LiveDoc)) {
        if self.read_only {
            return;
        }
        f(&mut self.live);
        self.changed(cx);
    }

    fn hit(&self, at: Point<Pixels>) -> Option<usize> {
        self.hit_leaf(at).map(|(_, _, pos, _)| pos)
    }

    /// The segment and leaf under `at`, the source position there, and
    /// whether `at` fell on the character before that position rather than
    /// the one after.
    fn hit_leaf(&self, at: Point<Pixels>) -> Option<(Rc<Projection>, usize, usize, bool)> {
        let frame = self.frame.borrow();
        let mut leaves = Vec::new();
        let laid = frame.segments.iter().flat_map(|p| {
            p.leaves
                .iter()
                .enumerate()
                .map(move |(i, leaf)| (p, i, leaf))
        });
        for (projection, i, leaf) in laid {
            let Some(geometry) = frame.geometry.get(&leaf.key) else {
                continue;
            };
            let bounds = geometry.bounds();
            // A wide table's cell scrolled out of sight takes no click; one
            // showing only its padding (`px_3`) still does.
            let pad = px(12.);
            if let Some(clip) = frame.clip_at(bounds.center().y)
                && (bounds.right() + pad <= clip.left() || bounds.left() - pad >= clip.right())
            {
                continue;
            }
            let distance = |at: Pixels, low: Pixels, high: Pixels| {
                if at < low {
                    low - at
                } else if at > high {
                    at - high
                } else {
                    px(0.)
                }
            };
            let dy = distance(at.y, bounds.top(), bounds.bottom());
            let dx = distance(at.x, bounds.left(), bounds.right());
            leaves.push((dy, dx, projection, i, geometry));
        }
        // Nearest row first, then nearest column: leaves on one visual row differ
        // by a few pixels (inline code is a taller run), so compare rows loosely.
        let nearest_row = leaves.iter().map(|l| l.0).reduce(|a, b| a.min(b))?;
        let &(_, _, projection, i, geometry) = leaves
            .iter()
            .filter(|l| l.0 <= nearest_row + px(8.))
            .min_by(|a, b| {
                (a.1, a.0)
                    .partial_cmp(&(b.1, b.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })?;
        let index = geometry.index_for_position(at);
        let before = geometry.position_for_index(index).x > at.x;
        // The frame can predate an edit that shortened the text.
        let pos = self.live.clamp(projection.from_display(i, index));
        Some((projection.clone(), i, pos, before))
    }

    fn caret_rect(&self) -> Option<Bounds<Pixels>> {
        caret_bounds(&self.frame.borrow(), self.live.selection().head)
    }

    fn vertical(&mut self, down: bool, extend: bool, cx: &mut Context<Self>) {
        // Use this cell's own geometry first, including wrapped lines. Only
        // its first/last visual line hands the arrow to cell navigation.
        let within = {
            let frame = self.frame.borrow();
            frame
                .locate(self.live.selection().head)
                .and_then(|(p, leaf, disp)| {
                    let (_, _, cell) = p.cell_at(self.live.selection().head)?;
                    let geometry = frame.geometry.get(&p.leaves[leaf].key)?;
                    let origin = geometry.position_for_index(disp);
                    let height = geometry.line_height();
                    let goal = self.goal_x.unwrap_or(origin.x);
                    let y = origin.y + if down { height * 1.5 } else { -height * 0.5 };
                    let index = geometry.index_for_position(point(goal, y));
                    let next = geometry.position_for_index(index);
                    let moved = if down {
                        next.y > origin.y
                    } else {
                        next.y < origin.y
                    };
                    let pos = p.from_display(leaf, index);
                    (moved && pos >= cell.from && pos <= cell.limit).then_some((pos, goal))
                })
        };
        if let Some((pos, goal)) = within {
            if extend {
                self.live.extend_to(pos, down);
            } else {
                self.live.set_caret(pos);
            }
            self.changed(cx);
            self.goal_x = Some(goal);
            return;
        }
        let key = if down { TableKey::Down } else { TableKey::Up };
        if self.live.table_key(key) {
            self.changed(cx);
            return;
        }
        let Some(rect) = self.caret_rect() else {
            return;
        };
        let goal = self.goal_x.unwrap_or(rect.origin.x);
        let head = self.live.selection().head;
        let step = rect.size.height / 2.;
        // Probe downward or upward until the hit lands on another visual line:
        // a table row can be several caret heights tall.
        let mut y = rect.origin.y + step;
        for _ in 0..16 {
            y = if down { y + step } else { y - step };
            let Some(pos) = self.hit(point(goal, y)) else {
                return;
            };
            // Judged where the caret would stop: a hit on the bullet beside
            // the caret's own line is the caret's line.
            let stop = self.live.snap(pos);
            if stop == head {
                continue;
            }
            let moved = caret_bounds(&self.frame.borrow(), stop).is_none_or(|c| {
                if down {
                    c.top() > rect.top() + step
                } else {
                    c.top() < rect.top() - step
                }
            });
            if moved {
                if !extend && self.live.enter_table(pos, down) {
                    self.changed(cx);
                    return;
                }
                if extend {
                    self.live.extend_to(pos, down)
                } else {
                    self.live.set_caret(pos)
                }
                self.changed(cx);
                self.goal_x = Some(goal);
                return;
            }
        }
    }

    /// Home and End: the edges of the visual line the caret is on.
    fn line_edge(&mut self, end: bool, extend: bool, cx: &mut Context<Self>) {
        let target = self.visual_edge(end);
        self.motion(cx, |l| match target {
            Some(target) => l.move_to_line_edge(end, extend, target),
            None => l.move_line_edge(end, extend),
        });
        if !end {
            self.frame.borrow_mut().wrap_start = Some(self.live.selection().head);
        }
    }

    fn visual_edge(&self, end: bool) -> Option<usize> {
        let frame = self.frame.borrow();
        let (p, mut leaf, mut disp) = frame.locate(self.live.selection().head)?;
        // An ordered item's number is a leaf of its own beside the text; the
        // two are one line, so Home goes before the number and End past the
        // text, as in Tauri where the number is plain text.
        let number = p
            .blocks
            .iter()
            .find(|b| {
                matches!(
                    b.kind,
                    BlockKind::ListItem {
                        marker: Some(_),
                        ..
                    }
                ) && b.leaves.contains(&leaf)
            })
            .map(|b| b.leaves[0])
            .filter(|&m| p.leaves[m].segs.iter().any(|s| s.kind == SegKind::Text));
        if end && number == Some(leaf) {
            (leaf, disp) = (leaf + 1, 0);
        }
        let geometry = frame.geometry.get(&p.leaves[leaf].key)?;
        let caret = caret_origin(&frame, p, leaf, disp, self.live.selection().head)?;
        let bounds = geometry.bounds();
        let x = if end {
            bounds.right() + px(1.)
        } else {
            bounds.left() - px(1.)
        };
        let mut index =
            geometry.index_for_position(point(x, caret.y + geometry.line_height() / 2.));
        // A wrapped line's end is the next line's start, where the caret
        // would be drawn: stop before the character the line wraps after.
        let text = &p.leaves[leaf].text;
        if end && index < text.len() && geometry.position_for_index(index).y > caret.y {
            index = text[..index]
                .grapheme_indices(true)
                .next_back()
                .map_or(index, |(at, _)| at);
        }
        if !end
            && index == 0
            && let Some(number) = number
        {
            return Some(p.from_display(number, 0));
        }
        Some(p.from_display(leaf, index))
    }

    /// The task box under `at`: where its `[` is.
    fn task_at(&self, at: Point<Pixels>) -> Option<usize> {
        let frame = self.frame.borrow();
        frame
            .segments
            .iter()
            .flat_map(|p| p.blocks.iter().map(move |block| (p, block)))
            .filter_map(|(p, block)| task_box(&frame, p, block))
            .find(|(bounds, ..)| bounds.contains(&at))
            .map(|(_, _, at)| at)
    }

    fn press(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The page's scrollbar, though it is not showing yet, or a wide
        // table's.
        let table = self.frame.borrow().table_bar_at(event.position);
        self.thumb = (self.scroll.on_bar(event.position).map(Thumb::Page))
            .or(table.map(|(start, grab)| Thumb::Table(start, grab)));
        if self.thumb.is_some() {
            return self.drag_thumb(event.position, cx);
        }
        // The box takes the click: the caret stays where it is.
        if let Some(at) = self.task_at(event.position) {
            self.edit(cx, |l| l.toggle_task(at));
            self.reveal.set(false);
            return;
        }
        window.focus(&self.focus, cx);
        let Some((projection, leaf, pos, before)) = self.hit_leaf(event.position) else {
            return;
        };
        if event.modifiers.secondary(cx)
            && let Some(href) = self.live.link_at(pos)
        {
            cx.emit(LiveEvent::Link(href));
            return;
        }
        // A cell a short row lacks is drawn, with no source of its own: any
        // click there puts the caret in it, as a click in Tauri's empty td.
        let missing = projection
            .missing_leaf(leaf)
            .map(|cell| (cell.from, cell.col));
        match event.click_count {
            _ if event.modifiers.shift => self.live.set_head(pos),
            _ if let Some((at, col)) = missing => self.live.set_caret_in_missing(at, col),
            2 => self.live.select_word(pos, before),
            3.. => self.live.select_block(pos),
            _ => self.live.set_caret(pos),
        }
        // A drag extends a single click only: one after a double or triple
        // click would shrink the word or line to the pointer.
        self.selecting = event.click_count <= 1;
        self.changed(cx);
        self.reveal.set(false);
    }

    /// Page Up and Page Down: the caret moves a viewport up or down, and the
    /// view with it.
    fn page(&mut self, down: bool, extend: bool, cx: &mut Context<Self>) {
        let Some(rect) = self.caret_rect() else {
            return;
        };
        if !self.scroll.viewport().intersects(&rect) {
            // Off screen, only the caret's own blocks are laid out: the view
            // goes to it first.
            self.reveal.set(true);
            cx.notify();
            return;
        }
        let page = (self.scroll.viewport().size.height - px(FONT_SIZE * LINE_HEIGHT)).max(px(0.));
        let goal = self.goal_x.unwrap_or(rect.origin.x);
        let y = rect.center().y + if down { page } else { -page };
        let Some(pos) = self.hit(point(goal, y)) else {
            return;
        };
        if extend {
            self.live.extend_to(pos, down)
        } else {
            self.live.set_caret(pos)
        }
        self.scroll.scroll_by(if down { page } else { -page });
        self.changed(cx);
        self.goal_x = Some(goal);
    }

    /// A drag that selects, wherever the pointer is: past the top or bottom,
    /// the view follows it.
    fn drag(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.thumb.is_some() && event.pressed_button == Some(MouseButton::Left) {
            return self.drag_thumb(event.position, cx);
        }
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let Some(pos) = self.hit(event.position) else {
            return;
        };
        self.live.set_head(pos);
        self.changed(cx);
        let viewport = self.scroll.viewport();
        let outside = event.position.y < viewport.top() || event.position.y > viewport.bottom();
        self.reveal.set(outside);
    }

    fn drag_thumb(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        match self.thumb {
            Some(Thumb::Page(grab)) => self.scroll.drag_thumb_to(at.y, grab),
            Some(Thumb::Table(start, grab)) => {
                if let Some(x) = self.frame.borrow().table_x_for(start, at.x, grab) {
                    self.live.table_x().borrow_mut().insert(start, x);
                }
            }
            None => return,
        }
        cx.notify();
    }

    fn release(&mut self) {
        self.selecting = false;
        if let Some(Thumb::Page(_)) = self.thumb.take() {
            self.scroll.end_drag();
        }
    }

    fn copy(&self, cx: &mut Context<Self>) {
        let text = self.live.selected_text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
        }
    }
}

/// A task item's box, drawn over the space its marker leaf leaves for it:
/// where, whether it is ticked, and where its `[` is.
fn task_box(frame: &Frame, p: &Projection, block: &Block) -> Option<(Bounds<Pixels>, bool, usize)> {
    let BlockKind::ListItem {
        marker: Some(Marker::Task { checked, at }),
        ..
    } = block.kind
    else {
        return None;
    };
    let leaf = &p.leaves[block.leaves[0]];
    let space = leaf.segs.last()?.disp.start;
    let geometry = frame.geometry.get(&leaf.key)?;
    let origin = geometry.position_for_index(space);
    let side = px(FONT_SIZE);
    let top = origin.y + (geometry.line_height() - side) / 2.;
    Some((
        Bounds::new(point(origin.x, top), size(side, side)),
        checked,
        at,
    ))
}

/// A checkbox, as the native one Tauri drew with the accent colour.
fn paint_task(
    bounds: Bounds<Pixels>,
    checked: bool,
    read_only: bool,
    s: &LiveStyle,
    window: &mut Window,
) {
    let fade = |color: Hsla| if read_only { color.opacity(0.5) } else { color };
    let (fill, border) = if checked {
        (s.accent, s.accent)
    } else {
        (s.background, s.dim)
    };
    window.paint_quad(quad(
        bounds,
        px(3.),
        fade(fill),
        px(1.),
        fade(border),
        BorderStyle::default(),
    ));
    if !checked {
        return;
    }
    let at = |x: f32, y: f32| bounds.origin + point(bounds.size.width * x, bounds.size.height * y);
    let mut check = PathBuilder::stroke(px(2.));
    check.move_to(at(0.24, 0.52));
    check.line_to(at(0.42, 0.7));
    check.line_to(at(0.76, 0.32));
    if let Ok(path) = check.build() {
        // The page's own colour reads on the accent in either theme.
        window.paint_path(path, s.background);
    }
}

fn caret_bounds(frame: &Frame, head: usize) -> Option<Bounds<Pixels>> {
    let (p, leaf, disp) = frame.locate(head)?;
    let geometry = frame.geometry.get(&p.leaves[leaf].key)?;
    let origin = caret_origin(frame, p, leaf, disp, head)?;
    Some(Bounds::new(origin, size(px(1.5), geometry.caret_height())))
}

/// Where the caret at display offset `disp` of `leaf` is drawn: at a wrap
/// that Home went to, the start of the later line.
fn caret_origin(
    frame: &Frame,
    p: &Projection,
    leaf: usize,
    disp: usize,
    head: usize,
) -> Option<Point<Pixels>> {
    let geometry = frame.geometry.get(&p.leaves[leaf].key)?;
    let origin = geometry.position_for_index(disp);
    if frame.wrap_start == Some(head)
        && let Some(g) = p.leaves[leaf]
            .text
            .get(disp..)
            .and_then(|t| t.graphemes(true).next())
    {
        let next = geometry.position_for_index(disp + g.len());
        if next.y > origin.y {
            return Some(point(geometry.bounds().left(), next.y));
        }
    }
    Some(origin)
}

/// The column and row of the cell holding `head`, as Tauri's ring on the td.
/// gpui-base draws each cell as wide as its column's widest text plus
/// padding (`px_3`, 7px) and shares a table's spare width out to the
/// columns in proportion to those widths, beside or around the cell as the
/// column aligns. `table` is the table's frame, the width of the page; a
/// wide table's is where it shows, and its columns sit at their floors with
/// no spare width.
fn cell_ring(
    frame: &Frame,
    head: usize,
    table: Bounds<Pixels>,
    rem: Pixels,
) -> Option<Bounds<Pixels>> {
    let (p, (t, _, cell)) = frame
        .segments
        .iter()
        .find_map(|p| Some((p, p.cell_at(head)?)))?;
    let cells = &p.tables[t].cells;
    let extent = |same: &dyn Fn(&crate::projection::Cell) -> bool| {
        cells
            .iter()
            .filter(|c| same(c))
            .filter_map(|c| frame.geometry.get(&p.leaves[c.leaf].key))
            .map(|g| g.bounds())
            .reduce(|a, b| a.union(&b))
    };
    let pad = rem * 0.75;
    let columns = cells.iter().filter(|c| c.row == 0).count();
    let boxes = (0..columns)
        .map(|k| extent(&|c| c.col == k).map(|b| (b.left() - pad, b.right() + pad)))
        .collect::<Option<Vec<_>>>()?;
    let row = extent(&|c| c.row == cell.row)?;
    let table = frame.clip_at(row.center().y).unwrap_or(table);
    // Inside the frame's 1px border; a 1px divider follows each column.
    let (left, right) = (table.left() + px(1.), table.right() - px(1.));
    let content = boxes.iter().fold(px(0.), |sum, (l, r)| sum + (*r - *l));
    let spare = right - left - content - px(columns as f32 - 1.);
    let (mut x0, mut x1) = boxes[cell.col];
    if spare > px(1.) {
        let mut x = left;
        for (k, (l, r)) in boxes.iter().enumerate() {
            let width = (*r - *l) * (1. + spare / content);
            if k == cell.col {
                (x0, x1) = (x, x + width);
            }
            x += width + px(1.);
        }
    }
    let (x0, x1) = (x0.max(left), x1.min(right));
    (x0 < x1).then(|| {
        Bounds::from_corners(
            point(x0, row.top() - px(7.)),
            point(x1, row.bottom() + px(7.)),
        )
    })
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
        mark = mark.link(LinkMark::default());
    }
    mark
}

/// Non-overlapping runs over the leaf text, each with its merged style, as
/// `Paragraph` marks expect.
fn merged_marks(leaf: &Leaf) -> Vec<(Range<usize>, TextMark)> {
    let mut points: Vec<usize> = leaf
        .marks
        .iter()
        .flat_map(|(r, _)| [r.start, r.end])
        .collect();
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

/// A heading's size in ems, from `styles.css`.
fn heading_scale(level: u8) -> f32 {
    match level {
        1 => 2.,
        2 => 1.55,
        3 => 1.25,
        4 => 1.125,
        _ => 1.,
    }
}

/// Space above and below a heading, in its own em, as Tauri's stylesheet.
fn heading_padding(level: u8) -> (f32, f32) {
    match level {
        1 => (0.5, 0.2),
        2 => (0.6, 0.2),
        3 => (0.5, 0.15),
        4 => (0.4, 0.),
        _ => (0.25, 0.),
    }
}

/// `wide.rs` measures tables as these draw them: a change to the table's,
/// its head's or its cells' text, or to inline code, must be made there too.
fn text_style(s: &LiveStyle) -> TextViewStyle {
    let strong = s.ink_strong;
    TextViewStyle::default()
        .with_foreground(s.ink)
        .with_muted_foreground(s.dim)
        .with_link(s.accent)
        .with_selection(s.selection)
        .with_code_background(s.raised)
        .with_border(s.border)
        .with_paragraph_gap(rems(0.))
        .with_heading(move |level| {
            StyleRefinement::default()
                .text_size(px(FONT_SIZE * heading_scale(level)))
                .text_color(strong)
                .font_weight(FontWeight::SEMIBOLD)
        })
        .with_table(StyleRefinement::default().rounded(px(8.)).bg(s.background))
        .with_table_head(
            StyleRefinement::default()
                .bg(s.raised)
                .text_color(strong)
                .font_weight(FontWeight::SEMIBOLD),
        )
        .with_inline_code(HighlightStyle {
            background_color: Some(s.raised),
            ..Default::default()
        })
        .with_dark(s.dark)
}

/// `wide` is a wide table's width and how far it is scrolled.
#[allow(clippy::too_many_arguments)]
fn render_block(
    ix: usize,
    block: &Block,
    p: &Projection,
    s: &LiveStyle,
    rcx: &RenderContext,
    wide: Option<(Pixels, Pixels)>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let leaf = |i: usize| paragraph(&p.leaves[block.leaves[i]]);
    let node = match &block.kind {
        BlockKind::Heading(level) => BlockNode::Heading {
            level: *level,
            children: leaf(0),
            span: None,
        },
        BlockKind::Table { rows, aligns } => {
            let rows = rows
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
        // The text; the marker is drawn in its own column below.
        BlockKind::ListItem {
            marker: Some(_), ..
        } => BlockNode::Paragraph(leaf(1)),
        _ => BlockNode::Paragraph(leaf(0)),
    };
    let nodes = [&node];
    let flow = FlowPosition::at(&nodes, 0, true).first(true).last(true);
    let element = node.render(ix, flow, rcx, window, cx);
    let line = px(FONT_SIZE * LINE_HEIGHT);
    let inner = match &block.kind {
        BlockKind::Code => div()
            .px(px(18.))
            .py(px(12.))
            .border_1()
            .border_color(s.hairline)
            .rounded(px(8.))
            .bg(s.raised)
            .font_family(s.mono.clone())
            .text_size(px(FONT_SIZE * 0.875))
            .child(element),
        BlockKind::ListItem {
            depth,
            columns,
            marker,
        } => {
            let indent = px(32. * *depth as f32);
            let width = marker_width(*columns);
            let Some(marker) = marker else {
                // A later paragraph of an item, at the item's text.
                let inner = div().pl(indent + width).min_h(line).child(element);
                return quoted(block, s, inner);
            };
            // The marker column, so wrapped lines hang at the text
            // (`cm-live-list`'s padding and negative text indent).
            let column = div().flex_none().w(width);
            let column = match marker {
                Marker::Bullet => column.text_color(s.dim),
                _ => column.text_right().pr(px(FONT_SIZE * 0.5)),
            };
            let marker = BlockNode::Paragraph(leaf(0)).render_with_id(
                ElementId::NamedInteger("live-marker".into(), ix as u64),
                flow,
                rcx,
                window,
                cx,
            );
            div()
                .pl(indent)
                .flex()
                .child(column.child(marker))
                .child(div().flex_1().min_w_0().child(element))
        }
        // An empty line, so the caret can stop on the rule, with the rule
        // centred in it, as Tauri's `vertical-align: middle`.
        BlockKind::Rule { revealed: false } => div().relative().child(element).child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(line / 2.)
                .h(px(1.))
                .bg(s.hairline),
        ),
        BlockKind::Heading(level) => {
            let (top, bottom) = heading_padding(*level);
            let size = FONT_SIZE * heading_scale(*level);
            div()
                .pt(px(top * size))
                .pb(px(bottom * size))
                .child(element)
        }
        // Room below for the bar `viewport` draws, as Tauri's scrollbar.
        BlockKind::Table { .. } if let Some((track, x)) = wide => div()
            .overflow_hidden()
            .pb(px(viewport::TABLE_BAR))
            .child(div().w(track).ml(-x).child(element)),
        _ => div().child(element),
    };
    // A blank line keeps its height, as an empty line does in the source.
    quoted(block, s, inner.min_h(line))
}

/// A list's marker column: `max(1.5em, Nch)` for an ordered list's `N`
/// columns, 1.5em for a bullet or a task.
fn marker_width(columns: usize) -> Pixels {
    // About a digit's width in the document face.
    px((columns as f32 * 0.6 * FONT_SIZE).max(1.5 * FONT_SIZE))
}

fn quoted(block: &Block, s: &LiveStyle, inner: Div) -> AnyElement {
    if block.quote > 0 {
        div()
            .pl(px(18. * block.quote as f32))
            .border_l_2()
            .border_color(s.border)
            .text_color(s.dim)
            .child(inner)
            .into_any_element()
    } else {
        inner.into_any_element()
    }
}

impl Render for LiveView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        self.live.focused = focused;
        let style = self.style.clone();
        let segments = self.live.segments();
        let sel = self.live.selection();
        let mut context = KeyContext::default();
        context.add(KEY_CONTEXT);
        if self.read_only {
            context.add("read_only");
        }
        let at = segments.partition_point(|s| s.src.end < sel.head);
        let in_list = (segments.iter().skip(at).take(2)).any(|s| {
            let p = &s.projection;
            p.locate(sel.head)
                .and_then(|(leaf, _)| p.blocks.iter().find(|b| b.leaves.contains(&leaf)))
                .is_some_and(|b| matches!(b.kind, BlockKind::ListItem { .. }))
        });
        if in_list {
            context.add("in_list");
        }
        let rcx = RenderContext::new(text_style(&style))
            .with_body_font_size(px(FONT_SIZE))
            .on_link_click(|_, _, _, _| {});
        let root = Viewport {
            segments,
            style: style.clone(),
            rcx,
            // `clamp(20px, 4vw, 48px)`, as `--markdown-gutter`.
            gutter: (window.viewport_size().width * 0.04).clamp(px(20.), px(48.)),
            bottom: window.viewport_size().height * 0.4,
            selection: sel.range(),
            head: sel.head,
            find: self.find.clone(),
            top_inset: self.top_inset,
            marked: self.live.marked().map(|m| m.start),
            show_caret: focused,
            read_only: self.read_only,
            cell_ring: focused && !self.read_only,
            reveal: self.reveal.clone(),
            frame: self.frame.clone(),
            scroll: self.scroll.clone(),
            table_x: self.live.table_x(),
            follow: self.follow.take(),
            view: cx.entity(),
            focus: self.focus.clone(),
        };

        div()
            .id("live-markdown")
            .key_context(context)
            .track_focus(&self.focus)
            .size_full()
            .bg(style.background)
            .text_color(style.ink)
            .font_family(style.sans.clone())
            .text_size(px(FONT_SIZE))
            .line_height(relative(LINE_HEIGHT))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(|this, _: &MoveLeft, _, cx| {
                this.motion(cx, |l| {
                    if !l.table_key(TableKey::Left) {
                        l.move_horizontal(false, false)
                    }
                })
            }))
            .on_action(cx.listener(|this, _: &MoveRight, _, cx| {
                this.motion(cx, |l| {
                    if !l.table_key(TableKey::Right) {
                        l.move_horizontal(true, false)
                    }
                })
            }))
            .on_action(cx.listener(|this, _: &SelectLeft, _, cx| {
                this.motion(cx, |l| l.move_horizontal(false, true))
            }))
            .on_action(cx.listener(|this, _: &SelectRight, _, cx| {
                this.motion(cx, |l| l.move_horizontal(true, true))
            }))
            .on_action(cx.listener(|this, _: &MoveUp, _, cx| this.vertical(false, false, cx)))
            .on_action(cx.listener(|this, _: &MoveDown, _, cx| this.vertical(true, false, cx)))
            .on_action(cx.listener(|this, _: &SelectUp, _, cx| this.vertical(false, true, cx)))
            .on_action(cx.listener(|this, _: &SelectDown, _, cx| this.vertical(true, true, cx)))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| this.page(false, false, cx)))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| this.page(true, false, cx)))
            .on_action(cx.listener(|this, _: &SelectPageUp, _, cx| this.page(false, true, cx)))
            .on_action(cx.listener(|this, _: &SelectPageDown, _, cx| this.page(true, true, cx)))
            .on_action(cx.listener(|this, _: &MoveHome, _, cx| this.line_edge(false, false, cx)))
            .on_action(cx.listener(|this, _: &MoveEnd, _, cx| this.line_edge(true, false, cx)))
            .on_action(cx.listener(|this, _: &SelectHome, _, cx| this.line_edge(false, true, cx)))
            .on_action(cx.listener(|this, _: &SelectEnd, _, cx| this.line_edge(true, true, cx)))
            .on_action(cx.listener(|this, _: &MoveDocStart, _, cx| {
                this.motion(cx, |l| l.move_doc_edge(false, false))
            }))
            .on_action(cx.listener(|this, _: &MoveDocEnd, _, cx| {
                this.motion(cx, |l| l.move_doc_edge(true, false))
            }))
            .on_action(cx.listener(|this, _: &SelectDocStart, _, cx| {
                this.motion(cx, |l| l.move_doc_edge(false, true))
            }))
            .on_action(cx.listener(|this, _: &SelectDocEnd, _, cx| {
                this.motion(cx, |l| l.move_doc_edge(true, true))
            }))
            .on_action(cx.listener(|this, _: &MoveWordLeft, _, cx| {
                this.motion(cx, |l| l.move_word(false, false))
            }))
            .on_action(cx.listener(|this, _: &MoveWordRight, _, cx| {
                this.motion(cx, |l| l.move_word(true, false))
            }))
            .on_action(cx.listener(|this, _: &SelectWordLeft, _, cx| {
                this.motion(cx, |l| l.move_word(false, true))
            }))
            .on_action(cx.listener(|this, _: &SelectWordRight, _, cx| {
                this.motion(cx, |l| l.move_word(true, true))
            }))
            .on_action(
                cx.listener(|this, _: &ToggleBold, _, cx| this.edit(cx, |l| l.toggle_inline("**"))),
            )
            .on_action(
                cx.listener(|this, _: &ToggleItalic, _, cx| {
                    this.edit(cx, |l| l.toggle_inline("*"))
                }),
            )
            .on_action(cx.listener(|this, _: &Indent, _, cx| {
                this.edit(cx, |l| {
                    l.change_list_indent(false);
                })
            }))
            .on_action(cx.listener(|this, _: &Outdent, _, cx| {
                this.edit(cx, |l| {
                    l.change_list_indent(true);
                })
            }))
            .on_action(
                cx.listener(|this, _: &SelectAll, _, cx| this.motion(cx, LiveDoc::select_all)),
            )
            .on_action(cx.listener(|this, _: &Backspace, _, cx| this.edit(cx, LiveDoc::backspace)))
            .on_action(
                cx.listener(|this, _: &Delete, _, cx| this.edit(cx, LiveDoc::delete_forward)),
            )
            .on_action(cx.listener(|this, _: &Enter, _, cx| this.edit(cx, LiveDoc::enter)))
            // Tab moves between cells, or moves list items in and out. Either
            // way it stays in the document rather than moving focus.
            .on_action(cx.listener(|this, _: &TabNext, _, cx| {
                this.edit(cx, |l| {
                    if !l.table_key(TableKey::Tab) {
                        l.change_list_indent(false);
                    }
                })
            }))
            .on_action(cx.listener(|this, _: &TabPrev, _, cx| {
                this.edit(cx, |l| {
                    if !l.table_key(TableKey::ShiftTab) {
                        l.change_list_indent(true);
                    }
                })
            }))
            // Escape leaves a table; elsewhere it is the host's, which closes
            // its find bar with it.
            .on_action(cx.listener(|this, _: &Escape, _, cx| {
                if !this.read_only && this.live.table_key(TableKey::Escape) {
                    this.changed(cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &Undo, _, cx| this.edit(cx, LiveDoc::undo)))
            .on_action(cx.listener(|this, _: &Redo, _, cx| this.edit(cx, LiveDoc::redo)))
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &Cut, _, cx| {
                this.copy(cx);
                this.edit(cx, LiveDoc::delete_selection);
            }))
            .on_action(cx.listener(|this, _: &Paste, _, cx| {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    let text = text.replace("\r\n", "\n").replace('\r', "\n");
                    this.edit(cx, |l| l.insert_text(&text));
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.press(event, window, cx)
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.release()),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.release()),
            )
            .child(root)
            .child(Scrollbar::vertical(&self.scroll))
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

impl EntityInputHandler for LiveView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let text = self.live.text();
        let bytes = to_byte_range(text, &range);
        *adjusted = Some(to_utf16_range(text, bytes.clone()));
        Some(text[bytes].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let sel = self.live.selection();
        Some(UTF16Selection {
            range: to_utf16_range(self.live.text(), sel.range()),
            reversed: sel.head < sel.anchor,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.live
            .marked()
            .map(|r| to_utf16_range(self.live.text(), r))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.live.commit_composition();
        self.changed(cx);
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit(cx, |live| {
            if let Some(range) = range {
                live.select_range(to_byte_range(live.text(), &range));
            }
            live.insert_text(text);
        });
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit(cx, |live| {
            if let Some(range) = range {
                live.select_range(to_byte_range(live.text(), &range));
            }
            // `selected` is in UTF-16 offsets within `new_text`.
            let caret = selected.map(|r| to_byte(new_text, r.end));
            live.set_preedit(new_text, caret);
        });
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let start = to_byte(self.live.text(), range.start);
        // The start's block is laid out only near the screen; the caret's is
        // always, and the input method's window goes beside it.
        let frame = self.frame.borrow();
        caret_bounds(&frame, start).or_else(|| caret_bounds(&frame, self.live.selection().head))
    }

    fn character_index_for_point(
        &mut self,
        at: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let pos = self.hit(at)?;
        Some(utf16_len(&self.live.text()[..pos]))
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
