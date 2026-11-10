//! One tmux pane on screen. Port of the view half of `TerminalPane.tsx`.
//!
//! Exists before the engine has mounted it, so a pane appears as soon as tmux
//! names it. The engine thread owns the grid; a pane only ever paints it, and
//! says what it wants through commands.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::term::TermMode;
use gpui_kit::{
    Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, Point, Render, Role, SharedString, Size, StatefulInteractiveElement,
    Styled, Task, Window, div, prelude::FluentBuilder,
};
use terminal_engine::{GridSize, PaneTerminal, TerminalReadingState};

use crate::theme::{self, metrics::ui};

use super::cell_metrics::CellMetrics;
use super::element::{ShapedSegment, TerminalElement};
use super::grid::{CacheStats, RowCache, RowSpec};
use super::input::{self, PaneInput};
use super::integration::{self, PaneIntegration};
use super::link::{SharedProbes, TerminalLink};
use super::probes::PanePerf;
use super::reading::{self, PaneReading};

/// xterm's blink period.
const CURSOR_BLINK: Duration = Duration::from_millis(600);

/// Where the last paint put the grid, for mapping a pointer to a cell.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PaintedGrid {
    pub origin: Point<Pixels>,
    pub cell: Size<Pixels>,
}

pub struct TerminalPane {
    pane_id: SharedString,
    /// Scopes this pane's paint gate by host: `%1` exists on every server.
    gate_key: Option<SharedString>,
    link: Entity<TerminalLink>,
    probes: SharedProbes,
    focus: FocusHandle,
    terminal: Option<Arc<PaneTerminal>>,
    pub(super) metrics: Rc<CellMetrics>,
    pub(super) cache: Rc<RefCell<RowCache<ShapedSegment>>>,
    /// Set by anything that changes a row without damaging the grid: a mount, a
    /// resize, a theme change, a new highlight. The next paint rebuilds all rows.
    pub(super) repaint_all: Rc<Cell<bool>>,
    pub(super) painted_at: Rc<Cell<Option<PaintedGrid>>>,
    pub(super) dirty_since: Rc<Cell<Option<Instant>>>,
    /// Set once a reveal is answered; the next paint releases the gate.
    pub(super) note_painted: Rc<Cell<bool>>,
    pub(super) perf: Rc<RefCell<PanePerf>>,
    pub(super) last_stats: Rc<Cell<CacheStats>>,
    revealed: bool,
    /// tmux's active pane, when there are several to tell apart.
    pub(super) active: bool,
    cursor_on: bool,
    last_output: Option<Instant>,
    _blink: Option<Task<()>>,
    reading_state: TerminalReadingState,
    renderer_diagnostic: Option<String>,
    seed_diagnostic: Option<String>,
    /// T3.4's state on this pane.
    pub input: PaneInput,
    /// T3.5's state on this pane.
    pub reading: PaneReading,
    /// T3.6's state on this pane.
    pub integration: PaneIntegration,
    _subscriptions: Vec<gpui_kit::Subscription>,
}

impl TerminalPane {
    pub fn new(
        pane_id: SharedString,
        link: Entity<TerminalLink>,
        metrics: Rc<CellMetrics>,
        probes: SharedProbes,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus(&focus, window, Self::focus_changed),
            cx.on_blur(&focus, window, Self::focus_changed),
            cx.observe_window_activation(window, |_, _, cx| cx.notify()),
            // A row's colors are resolved when it is built.
            cx.observe_global::<theme::Appearance>(|pane, cx| pane.repaint_all(cx)),
        ];
        Self {
            pane_id,
            gate_key: None,
            link,
            probes,
            focus,
            terminal: None,
            metrics,
            cache: Rc::default(),
            repaint_all: Rc::new(Cell::new(true)),
            painted_at: Rc::default(),
            dirty_since: Rc::default(),
            note_painted: Rc::default(),
            perf: Rc::default(),
            last_stats: Rc::default(),
            revealed: false,
            active: false,
            cursor_on: true,
            last_output: None,
            _blink: None,
            reading_state: TerminalReadingState::Live,
            renderer_diagnostic: None,
            seed_diagnostic: None,
            input: PaneInput::default(),
            reading: PaneReading::default(),
            integration: PaneIntegration::default(),
            _subscriptions: subscriptions,
        }
    }

    pub fn pane_id(&self) -> &SharedString {
        &self.pane_id
    }

    pub fn link(&self) -> &Entity<TerminalLink> {
        &self.link
    }

    pub fn terminal(&self) -> Option<&Arc<PaneTerminal>> {
        self.terminal.as_ref()
    }

    pub fn reading_state(&self) -> TerminalReadingState {
        self.reading_state
    }

    /// What the last paint did: how many rows it shaped against how many it
    /// reused from the cache.
    pub fn last_paint_stats(&self) -> CacheStats {
        self.last_stats.get()
    }

    /// One cached row, as last painted.
    pub fn row_spec(&self, row: usize) -> Option<RowSpec> {
        let cache = self.cache.borrow();
        Some(cache.rows.get(row)?.as_ref()?.spec.clone())
    }

    pub fn painted_at(&self) -> Option<PaintedGrid> {
        self.painted_at.get()
    }

    pub fn gate_key(&self) -> Option<&SharedString> {
        self.gate_key.as_ref()
    }

    pub fn metrics(&self) -> Rc<CellMetrics> {
        self.metrics.clone()
    }

    pub fn probes(&self) -> SharedProbes {
        self.probes.clone()
    }

    /// True while the program in the pane reads the mouse, which decides
    /// whether `input` or `reading` owns a mouse or wheel event. See [`super`].
    pub fn mouse_reporting(&self) -> bool {
        self.terminal
            .as_ref()
            .is_some_and(|terminal| terminal.lock().mode().intersects(TermMode::MOUSE_MODE))
    }

    /// Sends bytes to the program in this pane. Fails while the pane has no
    /// live client.
    pub fn send_input(&self, bytes: Vec<u8>, cx: &mut Context<Self>) -> Result<(), String> {
        let link = self.link.clone();
        let pane_id = self.pane_id.clone();
        link.update(cx, |link, _| {
            link.note_input();
            let client_id = link
                .client_id()
                .ok_or("this terminal has no live client")?
                .to_owned();
            link.backend()
                .send_input(client_id, pane_id.to_string(), bytes)
        })
    }

    /// Asks the next paint to rebuild every row: for changes the grid's damage
    /// cannot see, such as a new selection.
    pub fn repaint_all(&self, cx: &mut Context<Self>) {
        self.repaint_all.set(true);
        cx.notify();
    }

    pub fn set_metrics(&mut self, metrics: Rc<CellMetrics>, cx: &mut Context<Self>) {
        if self.metrics != metrics {
            self.metrics = metrics;
            // A row spec holds no font, so equal specs would keep the old shaping.
            *self.cache.borrow_mut() = RowCache::default();
            self.repaint_all(cx);
        }
    }

    pub(super) fn on_mounted(
        &mut self,
        terminal: Arc<PaneTerminal>,
        gate_key: SharedString,
        cx: &mut Context<Self>,
    ) {
        // `Mounted` is a full paint; after it only `Dirty` wakes this pane.
        self.terminal = Some(terminal);
        self.gate_key = Some(gate_key);
        self.repaint_all.set(true);
        cx.notify();
    }

    pub(super) fn on_dirty(&mut self, cx: &mut Context<Self>) {
        if self.dirty_since.get().is_none() {
            self.dirty_since.set(Some(Instant::now()));
        }
        // Output keeps the cursor solid, so a busy terminal does not flicker it.
        self.cursor_on = true;
        self.last_output = Some(Instant::now());
        cx.notify();
    }

    pub(super) fn on_revealed(&mut self, cx: &mut Context<Self>) {
        self.revealed = true;
        self.note_painted.set(true);
        cx.notify();
    }

    /// The engine reveals a pane once per mount, so no later reveal can release
    /// a paint gate armed for it.
    pub(super) fn revealed(&self) -> bool {
        self.revealed
    }

    pub(super) fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            cx.notify();
        }
    }

    pub(super) fn on_grid_applied(&mut self, _size: GridSize, cx: &mut Context<Self>) {
        self.repaint_all.set(true);
        cx.notify();
    }

    pub(super) fn on_reading_changed(
        &mut self,
        state: TerminalReadingState,
        cx: &mut Context<Self>,
    ) {
        self.reading_state = state;
        cx.notify();
    }

    pub(super) fn set_renderer_diagnostic(
        &mut self,
        message: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.renderer_diagnostic = message;
        cx.notify();
    }

    pub(super) fn set_seed_diagnostic(&mut self, message: Option<String>, cx: &mut Context<Self>) {
        // These are capability limits on supported tmux versions: the pane says
        // so in the event stream without covering output the user cannot act on.
        self.seed_diagnostic = message.filter(|text| !text.starts_with("tmux does not expose "));
        cx.notify();
    }

    fn focus_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cursor_on = true;
        self._blink = self.focus.is_focused(window).then(|| {
            cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor().timer(CURSOR_BLINK).await;
                    let toggled = this.update_in(cx, |pane, window, cx| {
                        // A steady cursor has nothing to redraw.
                        let blinks = pane
                            .terminal
                            .as_ref()
                            .is_some_and(|terminal| terminal.lock().cursor_style().blinking);
                        if !window.is_window_active() || !blinks {
                            pane.cursor_on = true;
                            return;
                        }
                        let blink_hidden = pane
                            .last_output
                            .is_some_and(|output| output.elapsed() < CURSOR_BLINK);
                        pane.cursor_on = blink_hidden || !pane.cursor_on;
                        cx.notify();
                    });
                    if toggled.is_err() {
                        return;
                    }
                }
            })
        });
        cx.notify();
    }

    fn diagnostic(&self) -> Option<String> {
        let parts: Vec<&str> = [
            self.seed_diagnostic.as_deref(),
            self.renderer_diagnostic.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
    }

    pub(super) fn cursor_on(&self) -> bool {
        self.cursor_on
    }
}

impl Focusable for TerminalPane {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx);
        let pane = cx.entity();
        let focused = self.focus.is_focused(window) && window.is_window_active();
        let label = format!("Terminal pane {}", self.pane_id);
        let focus = self.focus.clone();
        let root = div()
            .id(SharedString::from(format!(
                "terminal-pane-{}",
                self.pane_id
            )))
            .role(Role::Terminal)
            .aria_label(SharedString::from(label))
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(palette.term_bg)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.focus(&focus, cx);
            })
            .child(TerminalElement::new(pane.clone(), focused))
            .when_some(self.diagnostic(), |root, text| {
                root.child(
                    div()
                        .id("renderer-diagnostic")
                        .role(Role::Status)
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .px(ui(8.))
                        .py(ui(4.))
                        .text_size(ui(theme::metrics::TYPE_SECONDARY))
                        .bg(palette.danger_wash)
                        .text_color(palette.danger_ink)
                        .child(text),
                )
            });
        let root = input::attach(root, &pane, window, cx);
        let root = reading::attach(root, &pane, window, cx);
        let root = integration::attach(root, &pane, window, cx);
        root.children(reading::children(&pane, window, cx))
            .children(integration::children(&pane, window, cx))
    }
}
