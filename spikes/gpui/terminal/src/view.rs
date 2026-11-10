//! GPUI views: the root workspace (positions panes from the real tmux layout)
//! and one view per pane (focus, key input, IME).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::*;
use spike_shared::fonts::MONO;

use crate::control::{Connection, Signal};
use crate::element::{CellMetrics, ShapedSeg, TerminalElement, cell_metrics};
use crate::feed::{Pane, PaneRect, Session};
use crate::grid::{DARK, RowCache};
use crate::probe::Probe;

pub const FONT_SIZE: f32 = 13.0;
pub const PADDING: f32 = 4.0;

/// One presented frame's cost, recorded from the first `render` of the root to
/// the deferred effect that runs after `Window::present`.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameRec {
    /// Microseconds since the log started, at present return.
    pub at_us: u64,
    /// Microseconds since the log started, when the frame's build began. Frames
    /// are started by the frame tick, so this is the cadence to judge coalescing
    /// by; `at_us` also moves with how long each present took.
    pub begin_us: u64,
    /// Root render start to the end of paint: GPUI's CPU side of the frame.
    pub build_us: u64,
    /// End of paint to `Window::present` returning: renderer and swapchain.
    pub present_us: u64,
    pub pane_views_rendered: u32,
    pub panes_prepainted: u32,
    pub rows_damaged: u32,
    pub rows_reshaped: u32,
    pub rows_reused: u32,
    /// Built inside GPUI's `dispatch_key_event`, which draws a dirty window
    /// before routing the key. No `present` follows this draw: the scene waits
    /// for the next tick, so `present_us` here is not a present time.
    pub key_event_draw: bool,
    pub term_copy_us: u64,
    pub shape_us: u64,
}

#[derive(Default)]
struct FrameInner {
    build_start: Option<Instant>,
    paint_end: Option<Instant>,
    current: FrameRec,
    records: Vec<FrameRec>,
}

/// A condition the frame hook checks, to time a resize or a switch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Measure {
    Resize { cols: u16, rows: u16 },
    Window { window: String },
    Pane { pane: String },
}

struct PendingMeasure {
    measure: Measure,
    ready: bool,
    done_at: Option<Instant>,
}

pub struct FrameLog {
    epoch: Instant,
    inner: Mutex<FrameInner>,
    measure: Mutex<Option<PendingMeasure>>,
    in_key_dispatch: std::sync::atomic::AtomicBool,
}

impl Default for FrameLog {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            inner: Mutex::default(),
            in_key_dispatch: std::sync::atomic::AtomicBool::new(false),
            measure: Mutex::new(None),
        }
    }
}

impl FrameLog {
    fn begin(&self) {
        let mut inner = self.inner.lock().unwrap();
        if inner.build_start.is_none() {
            inner.build_start = Some(Instant::now());
            inner.current.key_event_draw = self.in_key_dispatch.load(Ordering::Relaxed);
        }
    }

    /// Bracket a key dispatch so a draw GPUI runs inside it is tagged.
    pub fn set_in_key_dispatch(&self, value: bool) {
        self.in_key_dispatch.store(value, Ordering::Relaxed);
    }

    pub fn frame_begin(&self) -> Option<Instant> {
        self.inner.lock().unwrap().build_start
    }

    fn with(&self, f: impl FnOnce(&mut FrameRec)) {
        f(&mut self.inner.lock().unwrap().current);
    }

    pub fn note_pane_render(&self) {
        self.with(|c| c.pane_views_rendered += 1);
    }

    pub fn note_pane_prepaint(
        &self,
        damaged: usize,
        stats: crate::grid::CacheStats,
        copy: Duration,
        shape: Duration,
    ) {
        self.with(|c| {
            c.panes_prepainted += 1;
            c.rows_damaged += damaged as u32;
            c.rows_reshaped += stats.reshaped as u32;
            c.rows_reused += stats.reused as u32;
            c.term_copy_us += copy.as_micros() as u64;
            c.shape_us += shape.as_micros() as u64;
        });
    }

    fn end_paint(&self) {
        self.inner.lock().unwrap().paint_end = Some(Instant::now());
    }

    /// Runs in the deferred effect after `present` returned.
    fn presented(&self, now: Instant) {
        let mut inner = self.inner.lock().unwrap();
        let (Some(start), Some(paint_end)) = (inner.build_start.take(), inner.paint_end.take())
        else {
            return;
        };
        let mut record = std::mem::take(&mut inner.current);
        record.at_us = now.duration_since(self.epoch).as_micros() as u64;
        record.begin_us = start.duration_since(self.epoch).as_micros() as u64;
        record.build_us = paint_end.saturating_duration_since(start).as_micros() as u64;
        record.present_us = now.saturating_duration_since(paint_end).as_micros() as u64;
        inner.records.push(record);
    }

    pub fn take_frames(&self) -> Vec<FrameRec> {
        std::mem::take(&mut self.inner.lock().unwrap().records)
    }

    pub fn us_since_epoch(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.epoch).as_micros() as u64
    }

    pub fn arm_measure(&self, measure: Measure) {
        *self.measure.lock().unwrap() = Some(PendingMeasure {
            measure,
            ready: false,
            done_at: None,
        });
    }

    fn set_measure_ready(&self, satisfied: impl FnOnce(&Measure) -> bool) {
        let mut guard = self.measure.lock().unwrap();
        if let Some(pending) = guard.as_mut()
            && !pending.ready
            && pending.done_at.is_none()
            && satisfied(&pending.measure)
        {
            pending.ready = true;
        }
    }

    fn measure_presented(&self, now: Instant) {
        let mut guard = self.measure.lock().unwrap();
        if let Some(pending) = guard.as_mut()
            && pending.ready
            && pending.done_at.is_none()
        {
            pending.done_at = Some(now);
        }
    }

    /// `None` until the armed measurement's frame was presented.
    pub fn measure_done_at(&self) -> Option<Instant> {
        self.measure
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|pending| pending.done_at)
    }

    pub fn disarm_measure(&self) {
        *self.measure.lock().unwrap() = None;
    }
}

/// Everything the views and the bench driver share.
pub struct Shared {
    pub session: Arc<Session>,
    pub probe: Arc<Probe>,
    pub log: Arc<FrameLog>,
    pub conn: Connection,
    /// Whether pane views are embedded with GPUI's view cache (`.cached()`).
    pub view_cache: bool,
    /// Diagnostic: keep feeding every `Term` but draw no panes, to separate the
    /// cost of parsing from the cost of building and presenting frames.
    pub paint_panes: bool,
}

pub struct PaneView {
    shared: Arc<Shared>,
    pane: Arc<Pane>,
    focus: FocusHandle,
    cache: Rc<RefCell<RowCache<ShapedSeg>>>,
    metrics: Rc<CellMetrics>,
    cursor_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    preedit: String,
}

impl PaneView {
    fn new(
        shared: Arc<Shared>,
        pane: Arc<Pane>,
        metrics: Rc<CellMetrics>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            shared,
            pane,
            focus: cx.focus_handle(),
            cache: Rc::default(),
            metrics,
            cursor_bounds: Rc::default(),
            preedit: String::new(),
        }
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub fn preedit(&self) -> &str {
        &self.preedit
    }

    /// The single choke point for bytes leaving the view: the probe's t0 is
    /// taken here, immediately before the channel to the writer thread.
    fn send_input(&mut self, bytes: Vec<u8>) {
        self.shared.probe.note_dispatch(&self.pane.id);
        self.shared.conn.send_input(&self.pane.id, bytes);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if (event.keystroke.modifiers.control && matches!(event.keystroke.key.as_str(), "1" | "2")) || matches!(event.keystroke.key.as_str(), "f5" | "f6") {
            let mut windows: Vec<_> = self.shared.session.layouts.lock().unwrap().keys().cloned().collect();
            windows.sort();
            if let Some(w) = windows.get(if matches!(event.keystroke.key.as_str(), "1" | "f5") { 0 } else { 1 }) { self.shared.conn.select_window(w); }
            cx.stop_propagation(); return;
        }
        let app_cursor = self
            .pane
            .term
            .lock()
            .mode()
            .contains(alacritty_terminal::term::TermMode::APP_CURSOR);
        if let Some(bytes) = encode_key(&event.keystroke, app_cursor) {
            self.send_input(bytes);
            cx.stop_propagation();
        }
        // Plain text falls through to the platform's IME path, which calls
        // `replace_text_in_range`, so typing and composition share one path.
    }

    fn on_mouse_down(&mut self, _: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.shared.conn.select_pane(&self.pane.id);
    }
}

impl Render for PaneView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.shared.log.note_pane_render();
        let focused = self.focus.is_focused(window);
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .child(TerminalElement {
                view: cx.entity(),
                pane: self.pane.clone(),
                shared: self.shared.clone(),
                focus: self.focus.clone(),
                cache: self.cache.clone(),
                metrics: self.metrics.clone(),
                cursor_bounds: self.cursor_bounds.clone(),
                preedit: SharedString::from(self.preedit.clone()),
                focused,
            })
    }
}

/// Encode the keys that bypass text input. Anything else (printable keys) goes
/// through the input handler so IME composition and plain typing are one path.
pub fn encode_key(keystroke: &Keystroke, app_cursor: bool) -> Option<Vec<u8>> {
    let modifiers = keystroke.modifiers;
    if modifiers.platform {
        return None;
    }
    let plain = |seq: &[u8]| Some(seq.to_vec());
    let arrow = |letter: u8| {
        let mut seq = vec![0x1b, if app_cursor { b'O' } else { b'[' }];
        seq.push(letter);
        Some(seq)
    };
    match keystroke.key.as_str() {
        "enter" => plain(b"\r"),
        "tab" if modifiers.shift => plain(b"\x1b[Z"),
        "tab" => plain(b"\t"),
        "backspace" => plain(b"\x7f"),
        "escape" => plain(b"\x1b"),
        "up" => arrow(b'A'),
        "down" => arrow(b'B'),
        "right" => arrow(b'C'),
        "left" => arrow(b'D'),
        "home" => plain(b"\x1b[H"),
        "end" => plain(b"\x1b[F"),
        "pageup" => plain(b"\x1b[5~"),
        "pagedown" => plain(b"\x1b[6~"),
        "delete" => plain(b"\x1b[3~"),
        key if modifiers.control && key.len() == 1 => {
            let byte = key.as_bytes()[0];
            match byte {
                b'a'..=b'z' => Some(vec![byte - b'a' + 1]),
                b'@' => Some(vec![0]),
                b'[' => Some(vec![0x1b]),
                b'\\' => Some(vec![0x1c]),
                b']' => Some(vec![0x1d]),
                b'^' => Some(vec![0x1e]),
                b'_' => Some(vec![0x1f]),
                b'/' => Some(vec![0x1f]),
                _ => None,
            }
        }
        key if modifiers.alt && !modifiers.control && key.len() == 1 => {
            let mut seq = vec![0x1b];
            seq.push(key.as_bytes()[0]);
            Some(seq)
        }
        _ => None,
    }
}

impl EntityInputHandler for PaneView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let utf16: Vec<u16> = self.preedit.encode_utf16().collect();
        let end = range.end.min(utf16.len());
        let start = range.start.min(end);
        *adjusted_range = Some(start..end);
        Some(String::from_utf16_lossy(&utf16[start..end]))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let len = self.preedit.encode_utf16().count();
        Some(UTF16Selection {
            range: len..len,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        (!self.preedit.is_empty()).then(|| 0..self.preedit.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.preedit.clear();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.preedit.clear();
        if !text.is_empty() {
            self.send_input(text.as_bytes().to_vec());
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.preedit = new_text.to_string();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.cursor_bounds.get()
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

pub struct Workspace {
    shared: Arc<Shared>,
    views: HashMap<String, Entity<PaneView>>,
    metrics: Option<Rc<CellMetrics>>,
    sent_size: Option<(u16, u16)>,
    focused: Option<String>,
}

impl Workspace {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            views: HashMap::new(),
            metrics: None,
            sent_size: None,
            focused: None,
        }
    }

    pub fn cells_for(&self, width: f32, height: f32) -> (u16, u16) {
        let metrics = self.metrics.as_ref().expect("metrics ready after first frame");
        cells_in(width, height, metrics)
    }

    /// Screen rectangle of a pane's cell, in window pixels. The window sits at
    /// the origin of the private display, so this is also a root rectangle.
    pub fn cell_rect(&self, window: &str, pane: &str, col: usize, row: usize) -> Option<(i16, i16, u16, u16)> {
        let metrics = self.metrics.as_ref()?;
        let rects = self.shared.session.panes_in_window(window);
        let rect = rects.iter().find(|r| r.pane == pane)?;
        let (w, h) = (f32::from(metrics.cell_w), f32::from(metrics.cell_h));
        let x = PADDING + (rect.left as f32 + col as f32) * w;
        let y = PADDING + (rect.top as f32 + row as f32) * h;
        Some((x as i16, y as i16, w as u16, h as u16))
    }

    pub fn focused_pane(&self) -> Option<&str> {
        self.focused.as_deref()
    }

    pub fn view_for(&self, pane: &str) -> Option<Entity<PaneView>> {
        self.views.get(pane).cloned()
    }

    fn metrics(&mut self, window: &Window) -> Rc<CellMetrics> {
        self.metrics
            .get_or_insert_with(|| Rc::new(cell_metrics(window, MONO, px(FONT_SIZE))))
            .clone()
    }

    fn active_rects(&self) -> (Option<String>, Vec<PaneRect>) {
        let active = self.shared.session.active_window.lock().unwrap().clone();
        let rects = active
            .as_ref()
            .map(|window| self.shared.session.panes_in_window(window))
            .unwrap_or_default();
        (active, rects)
    }

    /// Create views for new panes and move focus to the window's active pane.
    /// Run on wake and before the first frame.
    pub fn sync_topology(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let metrics = self.metrics(window);
        let (active, rects) = self.active_rects();
        for rect in &rects {
            if !self.views.contains_key(&rect.pane) {
                let pane = self.shared.session.pane(&rect.pane);
                let shared = self.shared.clone();
                let metrics = metrics.clone();
                let view = cx.new(|cx| PaneView::new(shared, pane, metrics, cx));
                self.views.insert(rect.pane.clone(), view);
            }
        }
        let Some(active) = active else {
            return;
        };
        let target = self
            .shared
            .session
            .active_pane
            .lock()
            .unwrap()
            .get(&active)
            .cloned()
            .or_else(|| rects.first().map(|r| r.pane.clone()));
        if let Some(target) = target
            && self.focused.as_ref() != Some(&target)
            && let Some(view) = self.views.get(&target)
        {
            view.read(cx).focus_handle().focus(window, cx);
            self.focused = Some(target);
        }
    }

    /// Whether `window` is fully sized and laid out at `cols`x`rows`.
    fn layout_settled(&self, window: &str, cols: u16, rows: u16) -> bool {
        let rects = self.shared.session.panes_in_window(window);
        let (mut max_x, mut max_y) = (0u16, 0u16);
        for rect in &rects {
            max_x = max_x.max(rect.left + rect.cols);
            max_y = max_y.max(rect.top + rect.rows);
            let pane = self.shared.session.pane(&rect.pane);
            let size = pane.size();
            if size.cols != rect.cols as usize || size.rows != rect.rows as usize {
                return false;
            }
        }
        !rects.is_empty() && max_x == cols && max_y == rows
    }

    fn evaluate_measure(&self, active: Option<&str>, viewport_cells: (u16, u16)) {
        self.shared.log.set_measure_ready(|measure| match measure {
            Measure::Resize { cols, rows } => {
                viewport_cells == (*cols, *rows)
                    && self.sent_size == Some((*cols, *rows))
                    && active.is_some_and(|window| self.layout_settled(window, *cols, *rows))
            }
            Measure::Window { window } => active == Some(window.as_str()),
            Measure::Pane { pane } => self.focused.as_deref() == Some(pane.as_str()),
        });
    }
}

fn cells_in(width: f32, height: f32, metrics: &CellMetrics) -> (u16, u16) {
    let cols = ((width - PADDING * 2.0) / f32::from(metrics.cell_w)) as u16;
    let rows = ((height - PADDING * 2.0) / f32::from(metrics.cell_h)) as u16;
    (cols.max(2), rows.max(2))
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.shared.log.begin();
        self.sync_topology(window, cx);
        let metrics = self.metrics(window);
        let viewport = window.viewport_size();
        let (cols, rows) = cells_in(f32::from(viewport.width), f32::from(viewport.height), &metrics);
        if self.sent_size != Some((cols, rows)) {
            self.shared.conn.resize_client(cols, rows);
            self.sent_size = Some((cols, rows));
        }
        let (active, rects) = self.active_rects();
        self.evaluate_measure(active.as_deref(), (cols, rows));

        let mut children: Vec<AnyElement> = Vec::with_capacity(rects.len());
        for rect in rects.iter().filter(|_| self.shared.paint_panes) {
            let Some(view) = self.views.get(&rect.pane) else {
                continue;
            };
            let x = PADDING + rect.left as f32 * f32::from(metrics.cell_w);
            let y = PADDING + rect.top as f32 * f32::from(metrics.cell_h);
            let w = rect.cols as f32 * f32::from(metrics.cell_w);
            let h = rect.rows as f32 * f32::from(metrics.cell_h);
            let content: AnyElement = if self.shared.view_cache {
                view.clone()
                    .cached(StyleRefinement::default().size_full())
                    .into_any_element()
            } else {
                view.clone().into_any_element()
            };
            let is_active = self.focused.as_deref() == Some(rect.pane.as_str());
            let mut pane_box = div()
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(w))
                .h(px(h))
                .child(content);
            if is_active {
                pane_box = pane_box.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .size_full()
                        .border_1()
                        .border_color(rgba(0x7aa6da55)),
                );
            }
            children.push(pane_box.into_any_element());
        }

        let log = self.shared.log.clone();
        let probe = self.shared.probe.clone();
        div()
            .relative()
            .size_full()
            .bg(rgb(DARK.bg))
            .children(children)
            .child(
                // Painted last in the frame: closes the build timer and queues
                // a deferred effect, which GPUI runs when the frame's window
                // update finishes, i.e. right after `Window::present`.
                canvas(
                    |_, _, _| {},
                    move |_, _, _, cx| {
                        log.end_paint();
                        let (log, probe) = (log.clone(), probe.clone());
                        cx.defer(move |_| {
                            let now = Instant::now();
                            log.presented(now);
                            log.measure_presented(now);
                            probe.on_present(now);
                        });
                    },
                )
                .absolute()
                .size(px(0.)),
            )
    }
}

/// Spawn the foreground task that turns feed-thread wakes into `notify` calls.
/// GPUI itself coalesces notifies into one paint per frame; this keeps the
/// cross-thread hop to one wake per burst.
pub fn spawn_ui_pump(
    handle: AnyWindowHandle,
    workspace: Entity<Workspace>,
    shared: Arc<Shared>,
    wake: Arc<Signal>,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        loop {
            wake.wait().await;
            let shared = shared.clone();
            let workspace = workspace.clone();
            let result = handle.update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    if shared.session.topology_dirty.swap(false, Ordering::AcqRel) {
                        workspace.sync_topology(window, cx);
                        cx.notify();
                    }
                    for view in workspace.views.values() {
                        let pane = view.read(cx).pane.clone();
                        if pane.dirty.swap(false, Ordering::AcqRel) {
                            view.update(cx, |_, cx| cx.notify());
                        }
                    }
                })
            });
            if result.is_err() || shared.session.exited.load(Ordering::Acquire) {
                break;
            }
        }
    })
    .detach();
}
