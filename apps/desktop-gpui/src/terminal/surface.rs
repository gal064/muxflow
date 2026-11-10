//! The tiled area that holds a window's panes, measures itself and tells tmux
//! how big a client that is worth. Port of `app/TerminalWorkspaceSurface.tsx`.

use std::rc::Rc;
use std::time::Instant;

use desktop_core::app_state::load_app_state;
use gpui_kit::{
    AnyView, App, AppContext, Bounds, Context, DispatchPhase, Entity, Focusable,
    InteractiveElement, IntoElement, MouseDownEvent, ParentElement, Pixels, Render, Role,
    SharedString, Size, StatefulInteractiveElement, StyleRefinement, Styled, Subscription, Task,
    WeakEntity, Window, canvas, div, prelude::FluentBuilder, px,
};
use terminal_engine::GridSize;
use tmux_control::{Pane, TmuxSnapshot, Window as TmuxWindow};

use crate::{
    app::Core,
    connection_ui::{controller::ConnectionController, host_links::HostLink},
    fonts,
    theme::{self, metrics::ui},
};

use super::cell_metrics::{self, BoxChrome, CellMetrics, PixelBox, cells_for_box};
use super::client_size::{
    CLIENT_RESIZE_DEBOUNCE, ClientResize, ResizeContext, ResizeOutcome, RetryDecision,
    window_cell_size,
};
use super::layout::{PaneRect, WindowGrid, mounted_panes, pane_rect, separators, window_grid};
use super::link::{TerminalLink, lock};
use super::pane::TerminalPane;

/// A pane's box and grid, the last time tmux described it.
struct Mounted {
    id: SharedString,
    pane: Entity<TerminalPane>,
    geometry: Pane,
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
    _subscriptions: Vec<Subscription>,
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
                cx.intercept_keystrokes(move |_, _, cx| {
                    this.update(cx, |surface, cx| surface.request_take(cx)).ok();
                })
            };
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
                _subscriptions: vec![observe, keys],
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
            self.panes.push(Mounted { id, pane, geometry });
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
                    // Measured pixels of this surface, already scaled.
                    div()
                        .absolute()
                        .left(px(left))
                        .top(px(top))
                        .w(px(width))
                        .h(px(height))
                        .overflow_hidden()
                        .child(
                            AnyView::from(mounted.pane.clone())
                                .cached(StyleRefinement::default().size_full()),
                        )
                        .when(right, |frame| {
                            frame.child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .right_0()
                                    .h_full()
                                    .w(ui(1.))
                                    .bg(palette.chrome_hairline),
                            )
                        })
                        .when(bottom, |frame| {
                            frame.child(
                                div()
                                    .absolute()
                                    .bottom_0()
                                    .left_0()
                                    .w_full()
                                    .h(ui(1.))
                                    .bg(palette.chrome_hairline),
                            )
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
                        window.on_mouse_event(move |_: &MouseDownEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture {
                                this.update(cx, |surface, cx| surface.request_take(cx)).ok();
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
                    }),
            )
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
