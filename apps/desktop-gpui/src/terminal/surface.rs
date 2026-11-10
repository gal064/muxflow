//! The tiled area that holds a window's panes, measures itself and tells tmux
//! how big a client that is worth. Port of `app/TerminalWorkspaceSurface.tsx`.

use std::rc::Rc;
use std::time::Instant;

use desktop_core::app_state::load_app_state;
use gpui_kit::{
    AnyElement, AnyView, App, AppContext, Bounds, Context, CursorStyle, DispatchPhase, Entity,
    FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render, Role,
    SharedString, Size, StatefulInteractiveElement, StyleRefinement, Styled, Subscription, Task,
    WeakEntity, Window, accesskit::Orientation, canvas, div, prelude::FluentBuilder, px, relative,
};
use serde_json::json;
use terminal_engine::GridSize;
use tmux_control::{Pane, TmuxSnapshot, Window as TmuxWindow};

use crate::{
    app::Core,
    commands,
    connection_ui::{controller::ConnectionController, host_links::HostLink},
    fonts,
    theme::{self, metrics::ui},
};

use super::cell_metrics::{self, BoxChrome, CellMetrics, PixelBox, cells_for_box};
use super::client_size::{
    CLIENT_RESIZE_DEBOUNCE, ClientResize, ResizeContext, ResizeOutcome, RetryDecision,
    window_cell_size,
};
use super::integration::{
    self,
    agents::PaneAttention,
    frames::{
        AFTER_GRAB_QUIET, Axis, DRAG_THRESHOLD_PX, HOVER_FOCUS_DELAY, KEY_RESIZE_CELLS, PaneEdge,
        divider_key, join_action, pane_edge_at, resize_action, resize_cells_from_pixels,
    },
};
use super::layout::{PaneRect, WindowGrid, mounted_panes, pane_rect, separators, window_grid};
use super::link::{TerminalLink, lock};
use super::pane::TerminalPane;
use super::reading::is_link_activation;

/// A pane's box and grid, the last time tmux described it.
struct Mounted {
    id: SharedString,
    pane: Entity<TerminalPane>,
    geometry: Pane,
    /// The right and bottom dividers, which take arrow keys.
    dividers: [FocusHandle; 2],
    _focused: Subscription,
}

#[derive(Clone, PartialEq)]
struct SurfaceTarget {
    profile_id: String,
    client_id: Option<String>,
    session_id: Option<String>,
    window_id: Option<String>,
    window_name: Option<String>,
    zoomed: bool,
    can_mutate: bool,
    server_identity: Option<String>,
}

fn active_window<'a>(snapshot: &'a TmuxSnapshot, link: &HostLink) -> Option<&'a TmuxWindow> {
    let session = link
        .active_session_id
        .as_deref()
        .or_else(|| snapshot.sessions.first().map(|session| session.id.as_str()))?;
    let in_session = || {
        snapshot
            .windows
            .iter()
            .filter(move |window| window.session_id == session)
    };
    link.active_window_id
        .as_deref()
        .and_then(|id| in_session().find(|window| window.id == id))
        .or_else(|| in_session().find(|window| window.active))
        .or_else(|| in_session().next())
}

fn box_chrome(metrics: &CellMetrics) -> BoxChrome {
    BoxChrome {
        horizontal: 0.,
        vertical: 0.,
        scrollbar: f32::from(metrics.scrollbar_width),
    }
}

/// A usable grid: what tmux says if it says something a terminal can show,
/// otherwise the box's own measurement, as `reconcilePaneGrid` falls back to.
pub fn reconcile_pane_grid(tmux: GridSize, measured: Option<GridSize>) -> Option<GridSize> {
    if tmux.columns >= 2 && tmux.rows >= 2 {
        Some(tmux)
    } else {
        measured.filter(|size| size.columns >= 2 && size.rows >= 2)
    }
}

pub struct TerminalSurface {
    this: WeakEntity<Self>,
    controller: Entity<ConnectionController>,
    link: Option<Entity<TerminalLink>>,
    panes: Vec<Mounted>,
    target: Option<SurfaceTarget>,
    grid: WindowGrid,
    actual: Option<GridSize>,
    metrics: Option<Rc<CellMetrics>>,
    surface: Option<Size<Pixels>>,
    resize: ClientResize,
    font_size: f32,
    /// Runs after a change settles or a retry waits. Replaced, not stacked.
    resize_timer: Option<Task<()>>,
    take_timer: Option<Task<()>>,
    pending_focus: bool,
    active_pane: Option<String>,
    terminal_visible: bool,
    /// Where the surface's top-left corner is in the window, for hit-testing
    /// a grabbed pane's drop.
    origin: Point<Pixels>,
    /// A divider being dragged. It resizes once, when released.
    divider: Option<DividerGrab>,
    /// A pane grabbed with the link modifier, and when the last one ended.
    grab: Option<PaneGrab>,
    grab_ended: Option<Instant>,
    /// The pane the pointer rests on, and the timer that focuses it.
    hover: Option<(SharedString, Task<()>)>,
    _subscriptions: Vec<Subscription>,
}

/// A pane held by the link modifier. Nothing moves until it is dropped on
/// another pane's edge; the press alone is no drag, so the pane under it gets
/// no click.
struct PaneGrab {
    pane_id: SharedString,
    origin: Point<Pixels>,
    moving: bool,
    target: Option<(SharedString, PaneEdge)>,
}

struct DividerGrab {
    pane_id: String,
    axis: Axis,
    origin: Point<Pixels>,
    /// The pane's length and cells along the axis, to turn pixels into cells.
    pane_pixels: f32,
    pane_cells: u16,
}

/// The setting, off unless the user turned it on.
fn focus_follows_mouse(cx: &App) -> bool {
    load_app_state(&cx.global::<Core>().app_state)
        .is_ok_and(|state| state.shell.focus_follows_mouse)
}

impl TerminalSurface {
    pub fn new(
        controller: Entity<ConnectionController>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let store = cx.global::<Core>().app_state.clone();
        let saved = load_app_state(&store)
            .ok()
            .and_then(|state| state.shell.terminal_font_size)
            .map_or(theme::metrics::TERM_FONT_SIZE, f32::from);
        cx.new(|cx: &mut Context<Self>| {
            let observe = cx.observe_in(&controller, window, |surface, _, window, cx| {
                surface.sync(window, cx);
            });
            // A key anywhere in the app may take the client size back. Intercepted
            // ahead of dispatch: a pane's key handler stops propagation.
            let keys = {
                let this = cx.entity().downgrade();
                cx.intercept_keystrokes(move |event, _, cx| {
                    let escape = event.keystroke.key == "escape";
                    let ended = this
                        .update(cx, |surface, cx| {
                            surface.request_take(cx);
                            escape && surface.cancel_grab(cx)
                        })
                        .unwrap_or(false);
                    // Escape ended a pane drag, and is spent on it.
                    if ended {
                        cx.stop_propagation();
                    }
                })
            };
            let uploads = cx.observe_global::<integration::upload::Uploads>(|_, cx| cx.notify());
            let attention = cx.observe_global::<PaneAttention>(|_, cx| cx.notify());
            let mut surface = Self {
                this: cx.entity().downgrade(),
                controller,
                link: None,
                panes: Vec::new(),
                target: None,
                grid: WindowGrid {
                    width: 1,
                    height: 1,
                },
                actual: None,
                metrics: None,
                surface: None,
                resize: ClientResize::default(),
                font_size: saved,
                resize_timer: None,
                take_timer: None,
                pending_focus: false,
                active_pane: None,
                terminal_visible: true,
                origin: Point::default(),
                divider: None,
                grab: None,
                grab_ended: None,
                hover: None,
                _subscriptions: vec![observe, keys, uploads, attention],
            };
            surface.sync(window, cx);
            surface
        })
    }

    /// Follows the controller: the host, session and window on screen, and the
    /// panes in that window.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let controller = self.controller.read(cx);
        let active = controller.active_link().cloned();
        let link_entity = active
            .as_ref()
            .and_then(|link| controller.terminal_link(&link.profile_id));
        let (target, wanted): (Option<SurfaceTarget>, Vec<Pane>) = match active.as_ref() {
            Some(link) => {
                let snapshot = link.host_state.snapshot.clone();
                let window_id = active_window(&snapshot, link).map(|window| window.id.clone());
                let (zoomed, window_name, session_id) = window_id
                    .as_deref()
                    .and_then(|id| snapshot.windows.iter().find(|window| window.id == id))
                    .map_or((false, None, None), |window| {
                        (
                            window.zoomed,
                            Some(window.name.clone()),
                            Some(window.session_id.clone()),
                        )
                    });
                let panes = window_id.as_deref().map_or(Vec::new(), |id| {
                    mounted_panes(&snapshot.panes, id, zoomed)
                        .into_iter()
                        .cloned()
                        .collect()
                });
                self.actual = window_id
                    .as_deref()
                    .and_then(|id| window_cell_size(&snapshot.panes, id));
                self.grid = window_grid(&panes.iter().collect::<Vec<_>>());
                (
                    Some(SurfaceTarget {
                        profile_id: link.profile_id.clone(),
                        client_id: link.client_id.clone(),
                        session_id,
                        window_id,
                        window_name,
                        zoomed,
                        can_mutate: link.host_state.can_mutate,
                        server_identity: link.host_state.server_identity.clone(),
                    }),
                    panes,
                )
            }
            None => (None, Vec::new()),
        };
        let unchanged_link = self.link.as_ref().map(Entity::entity_id)
            == link_entity.as_ref().map(Entity::entity_id);
        if !unchanged_link {
            self.unmount_all(cx);
            self.link = link_entity;
        }
        let target_changed = self.target != target;
        if self.target.as_ref().and_then(|t| t.client_id.as_ref())
            != target.as_ref().and_then(|t| t.client_id.as_ref())
        {
            self.resize.reset_for_new_client();
        }
        self.target = target;
        let active_pane = wanted
            .iter()
            .find(|pane| pane.active)
            .map(|pane| pane.id.clone());
        if self.active_pane != active_pane {
            self.active_pane = active_pane;
            self.pending_focus = true;
        }
        self.apply_panes(wanted, window, cx);
        if target_changed {
            self.schedule_resize(cx);
        }
        // Pane commands, paste among them, need a pane to act on. Written only
        // when it changes: every write tells the palette to look again.
        let has_pane = !self.panes.is_empty();
        if cx
            .try_global::<commands::Commands>()
            .is_some_and(|commands| commands.context().has_pane != has_pane)
        {
            commands::update_context(cx, |context| context.has_pane = has_pane);
        }
        cx.notify();
    }

    /// An app tab covers the terminal, or stops covering it. Coming back is a
    /// new claim on the client size.
    pub fn set_terminal_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.terminal_visible == visible {
            return;
        }
        self.terminal_visible = visible;
        if visible {
            self.schedule_resize(cx);
        } else {
            self.resize.take();
            self.resize_timer = None;
            self.take_timer = None;
        }
    }

    fn unmount_all(&mut self, cx: &mut Context<Self>) {
        let Some(link) = self.link.clone() else {
            return;
        };
        link.update(cx, |link, _| {
            for mounted in self.panes.drain(..) {
                link.unmount(&mounted.id);
            }
        });
    }

    /// Mounts what tmux added, unmounts what it took away, and passes each
    /// pane tmux's grid: the box only ever decides the client size.
    fn apply_panes(&mut self, wanted: Vec<Pane>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(link) = self.link.clone() else {
            return;
        };
        let metrics = self.metrics_or_default(window);
        let measured = self.pane_measurements();
        let surface = self.surface_box();
        let grid = self.grid;
        let zoomed = self.target.as_ref().is_some_and(|t| t.zoomed);
        let previous_ids: Vec<SharedString> = self
            .panes
            .iter()
            .map(|mounted| mounted.id.clone())
            .collect();
        let wanted_ids: Vec<&str> = wanted.iter().map(|pane| pane.id.as_str()).collect();
        for gone in previous_ids
            .iter()
            .filter(|id| !wanted_ids.contains(&id.as_ref()))
        {
            link.update(cx, |link, _| link.unmount(gone.as_ref()));
        }
        self.panes
            .retain(|mounted| wanted_ids.contains(&mounted.id.as_ref()));
        let probes = link.read(cx).probes();
        let now = Instant::now();
        for geometry in wanted {
            let tmux = GridSize {
                columns: usize::from(geometry.width),
                rows: usize::from(geometry.height),
            };
            let measured_size = measured(&geometry, grid, surface, zoomed);
            let grid_size = reconcile_pane_grid(tmux, measured_size);
            if grid_size == Some(tmux) {
                lock(&probes)
                    .mismatch
                    .note_grids(&geometry.id, tmux, measured_size, now);
            } else {
                lock(&probes).mismatch.clear(&geometry.id);
            }
            if let Some(existing) = self
                .panes
                .iter_mut()
                .find(|mounted| mounted.id.as_ref() == geometry.id)
            {
                existing.geometry = geometry;
                if let Some(size) = grid_size {
                    link.read(cx).set_grid(existing.id.as_ref(), size);
                }
                continue;
            }
            let Some(size) = grid_size else {
                continue;
            };
            let id: SharedString = geometry.id.clone().into();
            let pane_metrics = metrics.clone();
            let pane_link = link.clone();
            let pane_probes = probes.clone();
            let pane = cx.new(|cx| {
                TerminalPane::new(id.clone(), pane_link, pane_metrics, pane_probes, window, cx)
            });
            link.update(cx, |link, _| link.mount(&id, &pane, size));
            self.pending_focus = true;
            let focused = {
                let id = id.clone();
                let handle = pane.read(cx).focus_handle(cx);
                cx.on_focus(&handle, window, move |surface, _, cx| {
                    surface.pane_focused(&id, cx);
                })
            };
            self.panes.push(Mounted {
                id,
                pane,
                geometry,
                dividers: [
                    cx.focus_handle().tab_stop(true),
                    cx.focus_handle().tab_stop(true),
                ],
                _focused: focused,
            });
        }
        let several = self.panes.len() > 1;
        for mounted in &self.panes {
            let active = several && mounted.geometry.active;
            mounted
                .pane
                .update(cx, |pane, cx| pane.set_active(active, cx));
        }
    }

    fn pane_measurements(
        &self,
    ) -> impl Fn(&Pane, WindowGrid, Option<PixelBox>, bool) -> Option<GridSize> + use<> {
        let metrics = self.metrics.clone();
        move |geometry, grid, surface, zoomed| {
            let surface = surface?;
            let metrics = metrics.as_ref()?;
            let rect = pane_rect(geometry, grid, surface, zoomed);
            cells_for_box(
                PixelBox {
                    width: rect.width,
                    height: rect.height,
                },
                metrics.cell(),
                box_chrome(metrics),
            )
        }
    }

    fn surface_box(&self) -> Option<PixelBox> {
        self.surface.map(|surface| PixelBox {
            width: f32::from(surface.width),
            height: f32::from(surface.height),
        })
    }

    fn metrics_or_default(&mut self, window: &mut Window) -> Rc<CellMetrics> {
        if self.metrics.is_none() {
            self.metrics = Some(Rc::new(self.measure(window)));
        }
        self.metrics.clone().expect("just set")
    }

    fn measure(&self, window: &mut Window) -> CellMetrics {
        let font_size = ui(self.font_size).to_pixels(window.rem_size());
        cell_metrics::measure(window, fonts::MONO, font_size)
    }

    fn measured_surface(
        &mut self,
        measured: Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.surface == Some(measured) {
            return;
        }
        self.surface = Some(measured);
        self.apply_panes_for_new_box(window, cx);
        self.schedule_resize(cx);
        cx.notify();
    }

    /// The box moved: every pane's measured size is a new fact for the probe.
    fn apply_panes_for_new_box(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted: Vec<Pane> = self
            .panes
            .iter()
            .map(|mounted| mounted.geometry.clone())
            .collect();
        self.apply_panes(wanted, window, cx);
    }

    fn resize_context(&self) -> ResizeContext {
        let actual = self.actual;
        let target = self.target.as_ref();
        ResizeContext {
            client_id: target.and_then(|t| t.client_id.clone()),
            session_id: target.and_then(|t| t.session_id.clone()),
            window_id: target.and_then(|t| t.window_id.clone()),
            can_mutate: target.is_some_and(|t| t.can_mutate),
            surface: self.surface_box(),
            cell: self.metrics.as_ref().map(|metrics| metrics.cell()),
            chrome: self
                .metrics
                .as_deref()
                .map_or_else(BoxChrome::default, box_chrome),
            actual,
            terminal_visible: self.terminal_visible,
        }
    }

    fn schedule_resize(&mut self, cx: &mut Context<Self>) {
        let timer = cx.background_executor().timer(CLIENT_RESIZE_DEBOUNCE);
        self.resize_timer = Some(cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |this, cx| this.send_resize(cx)).ok();
        }));
    }

    fn send_resize(&mut self, cx: &mut Context<Self>) {
        let context = self.resize_context();
        match self.resize.evaluate(&context, Instant::now()) {
            ResizeOutcome::Nothing => {}
            ResizeOutcome::Report(message) => self.report(message, cx),
            ResizeOutcome::Send(request) => {
                let Some(link) = self.link.clone() else {
                    return;
                };
                let answer = link.read(cx).backend().resize_client(
                    request.client_id.clone(),
                    request.size.columns as u16,
                    request.size.rows as u16,
                    request.session_id.clone(),
                    request.window_id.clone(),
                );
                cx.spawn(async move |this, cx| {
                    let outcome = answer.await;
                    this.update(cx, |this, cx| match outcome {
                        Ok(()) => this.resize.landed(),
                        Err(error) => this.resize_failed(request, error, cx),
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    fn resize_failed(
        &mut self,
        request: super::client_size::ResizeRequest,
        error: String,
        cx: &mut Context<Self>,
    ) {
        let context = self.resize_context();
        match self.resize.failed(&request, &context, error) {
            RetryDecision::RetryAfter(delay) => {
                let timer = cx.background_executor().timer(delay);
                self.resize_timer = Some(cx.spawn(async move |this, cx| {
                    timer.await;
                    this.update(cx, |this, cx| this.send_resize(cx)).ok();
                }));
            }
            RetryDecision::Report(message) => self.report(message, cx),
            RetryDecision::Nothing => {}
        }
    }

    fn request_take(&mut self, cx: &mut Context<Self>) {
        if self.take_timer.is_some() {
            return;
        }
        let context = self.resize_context();
        if !self.resize.take_due(&context, Instant::now()) {
            return;
        }
        let timer = cx.background_executor().timer(CLIENT_RESIZE_DEBOUNCE);
        self.take_timer = Some(cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |this, cx| {
                this.take_timer = None;
                let context = this.resize_context();
                if this.resize.take_due(&context, Instant::now()) {
                    this.resize.take();
                    this.send_resize(cx);
                }
            })
            .ok();
        }));
    }

    fn report(&mut self, message: String, cx: &mut Context<Self>) {
        self.controller
            .update(cx, |controller, cx| controller.report_status(message, cx));
    }

    /// Sends one tmux action for the window on screen, reporting a refusal
    /// unless `quiet`. Nothing is sent while the host cannot be changed.
    ///
    /// Guarded by the server alone, with no topology generation: a focus or a
    /// resize names its pane, which is the whole of what it needs to be right,
    /// and a generation that moves under a dragged divider is no reason to
    /// refuse it.
    fn tmux_action(&mut self, mut action: serde_json::Value, quiet: bool, cx: &mut Context<Self>) {
        let (Some(link), Some(target)) = (self.link.clone(), self.target.as_ref()) else {
            return;
        };
        if !target.can_mutate {
            return;
        }
        let (Some(client_id), Some(server)) =
            (target.client_id.clone(), target.server_identity.clone())
        else {
            return;
        };
        action["expected_server_identity"] = server.into();
        let answer = link.read(cx).backend().tmux_action(client_id, action);
        cx.spawn(async move |this, cx| {
            if let Err(error) = answer.await
                && !quiet
            {
                this.update(cx, |this, cx| this.report(error, cx)).ok();
            }
        })
        .detach();
    }

    /// A pane took the keyboard: a click, hover or a keyboard move. tmux's
    /// active pane follows, as it does when the Tauri app's pane is focused.
    fn pane_focused(&mut self, pane_id: &str, cx: &mut Context<Self>) {
        let Some(mounted) = self.panes.iter().find(|mounted| mounted.id == pane_id) else {
            return;
        };
        if mounted.geometry.active {
            return;
        }
        let action = json!({
            "kind": "focusPane",
            "session_id": mounted.geometry.session_id,
            "window_id": mounted.geometry.window_id,
            "pane_id": pane_id,
        });
        // Following focus is navigation, which the Tauri app did silently.
        self.tmux_action(action, true, cx);
    }

    fn begin_divider_drag(&mut self, pane_id: &str, axis: Axis, origin: Point<Pixels>) {
        if !self.target.as_ref().is_some_and(|target| target.can_mutate) {
            return;
        }
        let Some((mounted, surface)) = self
            .panes
            .iter()
            .find(|mounted| mounted.id == pane_id)
            .zip(self.surface_box())
        else {
            return;
        };
        let zoomed = self.target.as_ref().is_some_and(|target| target.zoomed);
        let rect = pane_rect(&mounted.geometry, self.grid, surface, zoomed);
        let (pane_pixels, pane_cells) = match axis {
            Axis::Horizontal => (rect.width, mounted.geometry.width),
            Axis::Vertical => (rect.height, mounted.geometry.height),
        };
        self.divider = Some(DividerGrab {
            pane_id: pane_id.to_owned(),
            axis,
            origin,
            pane_pixels,
            pane_cells,
        });
    }

    fn end_divider_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(grab) = self.divider.take() else {
            return;
        };
        let delta = f32::from(match grab.axis {
            Axis::Horizontal => position.x - grab.origin.x,
            Axis::Vertical => position.y - grab.origin.y,
        });
        if delta.abs() < DRAG_THRESHOLD_PX {
            return;
        }
        let cells = resize_cells_from_pixels(delta, grab.pane_pixels, grab.pane_cells);
        self.tmux_action(
            resize_action(&grab.pane_id, grab.axis, delta >= 0., cells),
            false,
            cx,
        );
    }

    /// Arrow keys along a focused divider's axis move it two cells.
    fn divider_key(&mut self, pane_id: &str, axis: Axis, key: &str, cx: &mut Context<Self>) {
        let Some(forward) = divider_key(axis, key) else {
            return;
        };
        cx.stop_propagation();
        self.tmux_action(
            resize_action(pane_id, axis, forward, KEY_RESIZE_CELLS),
            false,
            cx,
        );
    }

    /// A press with the link modifier grabs the pane, unless it is on a link,
    /// which that modifier opens. Claimed before the pane sees it, so no
    /// selection starts and a program reading the mouse gets no click. The
    /// grab does not focus the pane.
    fn press_frame(
        &mut self,
        pane_id: &SharedString,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left
            || !is_link_activation(&event.modifiers)
            || !self.target.as_ref().is_some_and(|target| target.can_mutate)
        {
            return;
        }
        let Some(mounted) = self.panes.iter().find(|mounted| mounted.id == *pane_id) else {
            return;
        };
        if integration::link_at(mounted.pane.read(cx), event.position) {
            return;
        }
        cx.stop_propagation();
        self.hover = None;
        self.grab = Some(PaneGrab {
            pane_id: pane_id.clone(),
            origin: event.position,
            moving: false,
            target: None,
        });
    }

    /// Follows a grab's pointer. True while a grab holds the pointer.
    fn move_grab(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) -> bool {
        // A divider release this window never saw: no later click may resize.
        if event.pressed_button.is_none() {
            self.divider = None;
        }
        let Some(grab) = self.grab.as_ref() else {
            return false;
        };
        // A release this gesture never saw: a later plain move is no drag.
        if event.pressed_button != Some(MouseButton::Left) {
            self.grab = None;
            cx.notify();
            return false;
        }
        let moving = grab.moving
            || (event.position - grab.origin).magnitude() >= f64::from(DRAG_THRESHOLD_PX);
        let target = if moving {
            self.drop_target(event.position)
        } else {
            None
        };
        let grab = self.grab.as_mut().expect("checked above");
        if grab.moving != moving || grab.target != target {
            grab.moving = moving;
            grab.target = target;
            cx.notify();
        }
        true
    }

    /// The pane edge under a window position, never the grabbed pane's own.
    fn drop_target(&self, position: Point<Pixels>) -> Option<(SharedString, PaneEdge)> {
        let surface = self.surface_box()?;
        let zoomed = self.target.as_ref().is_some_and(|target| target.zoomed);
        let x = f32::from(position.x - self.origin.x);
        let y = f32::from(position.y - self.origin.y);
        let grabbed = self.grab.as_ref().map(|grab| &grab.pane_id);
        self.panes
            .iter()
            .filter(|mounted| Some(&mounted.id) != grabbed)
            .find_map(|mounted| {
                let rect = pane_rect(&mounted.geometry, self.grid, surface, zoomed);
                // Half-open, so a split boundary belongs to one pane.
                let inside = x >= rect.left
                    && x < rect.left + rect.width
                    && y >= rect.top
                    && y < rect.top + rect.height;
                let edge = inside
                    .then(|| {
                        pane_edge_at((x - rect.left) / rect.width, (y - rect.top) / rect.height)
                    })
                    .flatten()?;
                Some((mounted.id.clone(), edge))
            })
    }

    /// Drops a grabbed pane. True if a grab held the pointer.
    fn end_grab(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some(grab) = self.grab.take() else {
            return false;
        };
        if grab.moving {
            self.grab_ended = Some(Instant::now());
            cx.notify();
            if let Some(action) = self
                .drop_target(position)
                .and_then(|(target, edge)| join_action(&grab.pane_id, &target, edge))
            {
                self.tmux_action(action, false, cx);
            }
        }
        true
    }

    /// Escape: ends a grab that is moving, and says whether it did.
    fn cancel_grab(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.grab.as_ref().is_some_and(|grab| grab.moving) {
            return false;
        }
        self.grab = None;
        self.grab_ended = Some(Instant::now());
        cx.notify();
        true
    }

    /// Port of `usePaneHoverFocus`. Every check is repeated when the timer
    /// fires, because the delay is exactly when a dialog can open.
    fn hover_moved(
        &mut self,
        pane_id: &SharedString,
        pressed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Decided once per entry into a pane, as a pointer enter would be.
        if self.hover.as_ref().is_some_and(|(id, _)| id == pane_id) {
            return;
        }
        // A held button is a selection or a drag crossing panes, and the
        // pane a grab was just dropped beside is no hover either: such an
        // entry focuses nothing until the pointer leaves and comes back.
        let after_grab = self
            .grab_ended
            .is_some_and(|ended| ended.elapsed() < AFTER_GRAB_QUIET);
        let task = if pressed || self.divider.is_some() || self.grab.is_some() || after_grab {
            Task::ready(())
        } else {
            let timer = cx.background_executor().timer(HOVER_FOCUS_DELAY);
            let id = pane_id.clone();
            cx.spawn_in(window, async move |this, cx| {
                timer.await;
                let _ = this.update_in(cx, |surface, window, cx| {
                    surface.hover_rested(&id, window, cx);
                });
            })
        };
        self.hover = Some((pane_id.clone(), task));
    }

    fn hover_rested(
        &mut self,
        pane_id: &SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.hover.as_ref().is_some_and(|(id, _)| id == pane_id)
            || self.panes.len() < 2
            || !window.is_window_active()
            || !focus_follows_mouse(cx)
        {
            return;
        }
        let handles: Vec<FocusHandle> = self
            .panes
            .iter()
            .map(|mounted| mounted.pane.read(cx).focus_handle(cx))
            .collect();
        let Some(target) = self
            .panes
            .iter()
            .position(|mounted| mounted.id == *pane_id)
            .map(|index| handles[index].clone())
        else {
            return;
        };
        // Never taken from anything but another pane or a divider between
        // them: a field, a dialog or a menu keeps the keyboard while the
        // pointer drifts over a split.
        let ours = |focused: &FocusHandle| {
            handles.contains(focused)
                || self
                    .panes
                    .iter()
                    .any(|mounted| mounted.dividers.contains(focused))
        };
        let elsewhere = window.focused(cx).is_some_and(|focused| !ours(&focused));
        if target.is_focused(window) || elsewhere {
            return;
        }
        window.focus(&target, cx);
    }

    fn hover_left(&mut self, pane_id: &SharedString) {
        if self.hover.as_ref().is_some_and(|(id, _)| id == pane_id) {
            self.hover = None;
        }
    }

    fn refresh_metrics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fresh = Rc::new(self.measure(window));
        if self.metrics.as_ref() == Some(&fresh) {
            return;
        }
        self.metrics = Some(fresh.clone());
        for mounted in &self.panes {
            mounted
                .pane
                .update(cx, |pane, cx| pane.set_metrics(fresh.clone(), cx));
        }
        self.schedule_resize(cx);
    }
}

impl Render for TerminalSurface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh_metrics(window, cx);
        // Focus moves between panes with tmux's active pane, but is not taken
        // from anything else that holds it.
        let surface_holds_focus = self
            .panes
            .iter()
            .any(|mounted| mounted.pane.read(cx).focus_handle(cx).is_focused(window));
        if self.pending_focus && (surface_holds_focus || window.focused(cx).is_none()) {
            let active = self
                .panes
                .iter()
                .find(|mounted| mounted.geometry.active)
                .or_else(|| self.panes.first());
            if let Some(mounted) = active {
                window.focus(&mounted.pane.read(cx).focus_handle(cx), cx);
                self.pending_focus = false;
            }
        }
        let palette = theme::palette(cx);
        let label: SharedString = self
            .target
            .as_ref()
            .and_then(|target| target.window_name.clone())
            .map_or_else(
                || "Terminal".to_owned(),
                |name| format!("Terminal tab {name}"),
            )
            .into();
        let surface = self.surface_box();
        let zoomed = self.target.as_ref().is_some_and(|target| target.zoomed);
        let grid = self.grid;
        let grab_target = self
            .grab
            .as_ref()
            .filter(|grab| grab.moving)
            .and_then(|grab| grab.target.clone());
        let drop_edge = |id: &SharedString| {
            grab_target
                .as_ref()
                .filter(|(target, _)| target == id)
                .map(|(_, edge)| *edge)
        };
        let frames: Vec<_> = surface.map_or(Vec::new(), |surface| {
            self.panes
                .iter()
                .map(|mounted| {
                    let PaneRect {
                        left,
                        top,
                        width,
                        height,
                    } = pane_rect(&mounted.geometry, grid, surface, zoomed);
                    let (right, bottom) =
                        separators(&mounted.geometry, grid, self.panes.len() > 1, zoomed);
                    let id = mounted.id.clone();
                    let wants_attention = self.target.as_ref().is_some_and(|target| {
                        cx.try_global::<PaneAttention>()
                            .is_some_and(|attention| attention.wants(&target.profile_id, &id))
                    });
                    // Measured pixels of this surface, already scaled.
                    div()
                        .id(SharedString::from(format!("pane-frame-{id}")))
                        .absolute()
                        .left(px(left))
                        .top(px(top))
                        .w(px(width))
                        .h(px(height))
                        .overflow_hidden()
                        .on_mouse_move(cx.listener({
                            let id = id.clone();
                            move |surface, event: &MouseMoveEvent, window, cx| {
                                let pressed = event.pressed_button.is_some();
                                surface.hover_moved(&id, pressed, window, cx);
                            }
                        }))
                        // Leaving the pane, wherever to, ends its entry.
                        .on_hover(cx.listener({
                            let id = id.clone();
                            move |surface, hovered: &bool, _, _| {
                                if !*hovered {
                                    surface.hover_left(&id);
                                }
                            }
                        }))
                        .child(
                            div()
                                .id("pane-grip")
                                .size_full()
                                .capture_any_mouse_down(cx.listener({
                                    let id = id.clone();
                                    move |surface, event: &MouseDownEvent, _, cx| {
                                        surface.press_frame(&id, event, cx);
                                    }
                                }))
                                .child(
                                    AnyView::from(mounted.pane.clone())
                                        .cached(StyleRefinement::default().size_full()),
                                ),
                        )
                        .when_some(drop_edge(&id), |frame, edge| {
                            frame.child(drop_hint(edge, cx))
                        })
                        // An agent in this pane wants a human.
                        .when(wants_attention, |frame| {
                            frame.child(
                                div()
                                    .absolute()
                                    .inset(ui(2.))
                                    .border(ui(2.5))
                                    .border_color(palette.accent)
                                    .rounded(ui(6.))
                                    .opacity(0.9),
                            )
                        })
                        .when(right, |frame| {
                            frame.child(self.divider_handle(mounted, Axis::Horizontal, cx))
                        })
                        .when(bottom, |frame| {
                            frame.child(self.divider_handle(mounted, Axis::Vertical, cx))
                        })
                })
                .collect()
        });
        let measure = {
            let this = self.this.clone();
            canvas(
                move |bounds: Bounds<Pixels>, window, cx| {
                    let this = this.clone();
                    let handle = window.window_handle();
                    // Layout is under way; the measurement may change state.
                    cx.defer(move |cx| {
                        let _ = handle.update(cx, |_, window, cx| {
                            this.update(cx, |surface, cx| {
                                surface.origin = bounds.origin;
                                surface.measured_surface(bounds.size, window, cx)
                            })
                            .ok();
                        });
                    });
                },
                // Anywhere in the window, as a press outside the surface may
                // take the client size back too.
                {
                    let this = self.this.clone();
                    move |_, _, window, _| {
                        let pressed = this.clone();
                        window.on_mouse_event(move |_: &MouseDownEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture {
                                pressed
                                    .update(cx, |surface, cx| surface.request_take(cx))
                                    .ok();
                            }
                        });
                        // A divider or a grabbed pane is released wherever
                        // the pointer is by then. A grab holds the pointer the
                        // way pointer capture does: nothing under it sees the
                        // gesture.
                        let released = this.clone();
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                            if phase != DispatchPhase::Capture {
                                return;
                            }
                            let grabbed = released
                                .update(cx, |surface, cx| {
                                    surface.end_divider_drag(event.position, cx);
                                    surface.end_grab(event.position, cx)
                                })
                                .unwrap_or(false);
                            if grabbed {
                                cx.stop_propagation();
                            }
                        });
                        let moved = this.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase != DispatchPhase::Capture {
                                return;
                            }
                            let grabbed = moved
                                .update(cx, |surface, cx| surface.move_grab(event, cx))
                                .unwrap_or(false);
                            if grabbed {
                                cx.stop_propagation();
                            }
                        });
                    }
                },
            )
            .absolute()
            .left_0()
            .top_0()
            .size_full()
        };
        let empty = self.target.is_some()
            && self
                .target
                .as_ref()
                .is_some_and(|target| target.window_id.is_some())
            && self.panes.is_empty();
        div()
            .id("terminal-surface")
            .role(Role::Group)
            .aria_label(label)
            .size_full()
            .bg(palette.term_bg)
            .p(ui(4.))
            .child(
                div()
                    .id("terminal-window")
                    .relative()
                    .size_full()
                    .child(measure)
                    .children(frames)
                    .when(empty, |window| {
                        window.child(
                            div()
                                .text_size(ui(theme::metrics::TYPE_SECONDARY))
                                .text_color(palette.chrome_dim)
                                .child("No tmux panes in this terminal window."),
                        )
                    })
                    .children(integration::upload_list(cx)),
            )
    }
}

/// The half of a pane a grabbed pane would fill: `.pane-drop-hint`.
fn drop_hint(edge: PaneEdge, cx: &App) -> AnyElement {
    let palette = theme::palette(cx);
    let hint = div()
        .absolute()
        .top(ui(6.))
        .bottom(ui(6.))
        .left(ui(6.))
        .right(ui(6.))
        .border_2()
        .border_dashed()
        .border_color(palette.accent)
        .rounded(ui(6.))
        .bg(palette.accent_wash);
    match edge {
        PaneEdge::Left => hint.right(relative(0.5)),
        PaneEdge::Right => hint.left(relative(0.5)),
        PaneEdge::Top => hint.bottom(relative(0.5)),
        PaneEdge::Bottom => hint.top(relative(0.5)),
    }
    .into_any_element()
}

impl TerminalSurface {
    /// Port of the `divider-handle` in `TerminalWorkspaceSurface.tsx`: the
    /// hairline on a pane's right or bottom edge, dragged or moved by arrow keys.
    fn divider_handle(&self, mounted: &Mounted, axis: Axis, cx: &mut Context<Self>) -> AnyElement {
        let palette = theme::palette(cx);
        let id = mounted.id.clone();
        let (focus, name, orientation, cursor, share) = match axis {
            Axis::Horizontal => (
                &mounted.dividers[0],
                "horizontally",
                Orientation::Vertical,
                CursorStyle::ResizeLeftRight,
                f64::from(mounted.geometry.width) / self.grid.width.max(1) as f64,
            ),
            Axis::Vertical => (
                &mounted.dividers[1],
                "vertically",
                Orientation::Horizontal,
                CursorStyle::ResizeUpDown,
                f64::from(mounted.geometry.height) / self.grid.height.max(1) as f64,
            ),
        };
        let line = div().absolute().bg(palette.chrome_hairline);
        let line = match axis {
            Axis::Horizontal => line.top_0().right_0().h_full().w(ui(1.)),
            Axis::Vertical => line.bottom_0().left_0().w_full().h(ui(1.)),
        };
        let handle = div()
            .id(SharedString::from(format!("divider-{name}-{id}")))
            .role(Role::Splitter)
            .aria_label(SharedString::from(format!("Resize {id} {name}")))
            .aria_orientation(orientation)
            .aria_numeric_value((share * 100.).round())
            .track_focus(focus)
            .focus_visible(move |style| style.bg(palette.accent_wash))
            .absolute()
            // The grip over the pane's edge is the divider's alone: a press
            // there is neither a selection nor a pane grab.
            .occlude()
            .cursor(cursor)
            .child(line)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let id = id.clone();
                    move |surface, event: &MouseDownEvent, _, cx| {
                        surface.begin_divider_drag(&id, axis, event.position);
                        // The pane under the grip must not start a selection.
                        cx.stop_propagation();
                    }
                }),
            )
            .on_key_down(cx.listener(move |surface, event: &KeyDownEvent, _, cx| {
                surface.divider_key(&id, axis, &event.keystroke.key, cx);
            }));
        match axis {
            Axis::Horizontal => handle.top_0().right_0().h_full().w(ui(4.)),
            Axis::Vertical => handle.bottom_0().left_0().w_full().h(ui(4.)),
        }
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(columns: usize, rows: usize) -> GridSize {
        GridSize { columns, rows }
    }

    #[test]
    fn tmux_grid_wins_over_what_the_box_measured() {
        assert_eq!(
            reconcile_pane_grid(grid(49, 14), Some(grid(50, 15))),
            Some(grid(49, 14)),
        );
    }

    #[test]
    fn the_box_counts_only_when_tmux_reports_no_usable_grid() {
        assert_eq!(
            reconcile_pane_grid(grid(0, 0), Some(grid(50, 15))),
            Some(grid(50, 15)),
        );
        assert_eq!(reconcile_pane_grid(grid(0, 0), Some(grid(1, 15))), None);
        assert_eq!(reconcile_pane_grid(grid(0, 0), None), None);
    }
}
