//! The window body under the title bar: sidebar, workspace and right panel,
//! their widths and how they collapse. [`Chrome`] is the seam the shell renders.
//!
//! Saved preferences are read once from `app_state` and written back through
//! `Chrome::save`, which never overlaps itself and always ends with the newest
//! value: a save started while one runs just replaces what the runner picks up
//! next. It runs on the backend's runtime, so a save outlives this entity.
//!
//! Layout notes: the rails are plain flex children with Kit `resize_handle`
//! bands. Kit's `DockArea` sizes docks in `Pixels` only, so it could not follow
//! UI scale and would bring tab-group chrome Muxflow does not have; Kit's
//! `Sidebar` is one scrolling list, and Muxflow's sidebar is two independently
//! scrolling sections split by a divider. Tabs use Kit's unstyled base `Tabs`,
//! because Kit's `TabBar` sizes tabs in fixed `Pixels`.

pub mod divider;
pub mod layout;
mod right_panel;
pub use right_panel::PanelBodies;
mod sidebar;
pub(crate) use sidebar::phase_color;
mod tab_strip;
mod title_content;
mod workspace;

use std::sync::{Arc, Mutex};

use desktop_core::{
    app_state::{self, DockSide},
    platform,
};
use gpui_kit::{
    AnyView, App, AppContext, Bounds, Context, Entity, InteractiveElement, IntoElement,
    ParentElement, Pixels, Point, Render, Styled, WeakEntity, Window, div, prelude::FluentBuilder,
};
use tokio::task::JoinHandle;

use crate::{
    app::Core,
    commands,
    connection_ui::ConnectionUi,
    terminal::surface::TerminalSurface,
    theme,
    ui::chrome::{
        divider::{Divider, DividerDrag},
        layout::{Prefs, Rails},
        right_panel::RightPanel,
        sidebar::Sidebar,
        title_content::TitleContent,
        workspace::WorkspaceArea,
    },
    workspaces::Workspaces,
};

/// Called once from [`crate::features::init`].
pub fn init(_cx: &mut App) {}

pub struct Chrome {
    this: WeakEntity<Self>,
    prefs: Prefs,
    drag: Option<(Divider, f32)>,
    /// `shell.tabsInTitleBar`, which Settings sets and this follows.
    tabs_in_title_bar: bool,
    /// (id, label) of each right-panel segment, built-ins first.
    surfaces: Vec<(String, String)>,
    workspaces: Entity<Workspaces>,
    saves: Arc<Mutex<SaveQueue>>,
    /// Set once a real connection is running.
    terminal: Option<Entity<TerminalSurface>>,
}

/// The newest preferences waiting to be written, and whether a writer runs.
#[derive(Default)]
struct SaveQueue {
    pending: Option<Prefs>,
    running: bool,
    /// The writer while one runs, so quitting can wait for it.
    runner: Option<JoinHandle<()>>,
}

impl Chrome {
    pub fn new(window: &mut Window, cx: &mut App) -> Entity<Self> {
        let store = cx.global::<Core>().app_state.clone();
        // A broken file shows defaults; `app_state` keeps it write-frozen.
        let state = app_state::load_app_state(&store).unwrap_or_default();
        let prefs = Prefs::from_shell(&state.shell);
        let workspaces = Workspaces::new(prefs.pinned_only, cx);
        let chrome = cx.new(|cx| {
            cx.observe(&workspaces, |chrome: &mut Self, _, cx| {
                chrome.sync_terminal_visible(cx);
                cx.notify();
            })
            .detach();
            // Settings changes these two live.
            cx.observe_global::<crate::settings::Settings>(|chrome: &mut Self, cx| {
                let prefs = crate::settings::get(cx);
                chrome.tabs_in_title_bar = prefs.tabs_in_title_bar;
                chrome.surfaces = layout::panel_surfaces(Some(&prefs.enabled_panel_extensions));
                cx.notify();
            })
            .detach();
            // A layout changed just before quitting is still written: the
            // writer runs on the backend's runtime, which quitting would stop.
            cx.on_app_quit(|chrome: &mut Self, _| {
                let runner = chrome.saves.lock().unwrap().runner.take();
                async move {
                    if let Some(runner) = runner {
                        runner.await.ok();
                    }
                }
            })
            .detach();
            Self {
                this: cx.entity().downgrade(),
                prefs,
                drag: None,
                tabs_in_title_bar: crate::settings::get(cx).tabs_in_title_bar,
                surfaces: layout::panel_surfaces(Some(
                    &crate::settings::get(cx).enabled_panel_extensions,
                )),
                workspaces,
                saves: Arc::default(),
                terminal: None,
            }
        });
        if let Some(controller) = cx.try_global::<ConnectionUi>().map(|ui| ui.0.clone()) {
            let surface = TerminalSurface::new(controller, window, cx);
            chrome.update(cx, |chrome, cx| {
                let weak = surface.downgrade();
                chrome
                    .workspaces
                    .update(cx, |workspaces, _| workspaces.terminal = Some(weak));
                chrome.terminal = Some(surface);
                chrome.sync_terminal_visible(cx);
            });
        }
        register_commands(&chrome.downgrade(), cx);
        chrome
    }

    /// The title bar's content. A press that reaches the bar starts a window
    /// drag, so an interactive child calls `cx.stop_propagation()` on left
    /// mouse-down, as `WindowControls` does.
    pub fn title_bar_content(&self) -> impl IntoElement + use<> {
        TitleContent(self.this.clone())
    }

    pub fn tabs_in_title_bar(&self) -> bool {
        self.tabs_in_title_bar
    }

    pub fn workspaces(&self) -> &Entity<Workspaces> {
        &self.workspaces
    }

    /// The committed preferences, or a drag's preview of them.
    fn displayed(&self) -> Prefs {
        let mut prefs = self.prefs.clone();
        if let Some((divider, value)) = self.drag {
            set_divider(&mut prefs, divider, value);
        }
        prefs
    }

    /// A CSS pixel in this window's `Pixels`, which is the UI scale.
    fn css_unit(window: &Window) -> f32 {
        window.rem_size().as_f32() / 16.
    }

    fn window_width(window: &Window) -> f32 {
        // Rounded: at a scale like 1.2 the division lands a hair under an exact
        // breakpoint, and a rail would hide one pixel early.
        (window.viewport_size().width.as_f32() / Self::css_unit(window) * 100.).round() / 100.
    }

    /// What the rails do right now, drag preview included.
    pub fn rails(&self, window: &Window) -> Rails {
        layout::rails(&self.displayed(), Self::window_width(window))
    }

    fn committed_rails(&self, window: &Window) -> Rails {
        layout::rails(&self.prefs, Self::window_width(window))
    }

    fn dock_side(&self, cx: &App) -> DockSide {
        self.workspaces.read(cx).view(cx).dock_side()
    }

    /// The command behind the sidebar button and `view.toggleSidebar`.
    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.prefs.sidebar_collapsed = !self.prefs.sidebar_collapsed;
        self.save(cx);
    }

    /// The command behind the panel button and `view.togglePanel`.
    pub fn toggle_panel(&mut self, cx: &mut Context<Self>) {
        self.prefs.panel_open = !self.prefs.panel_open;
        self.save(cx);
    }

    /// Asks for a surface: opens the panel on it, as `view.showFiles` and its
    /// siblings do. Not a toggle.
    pub fn show_surface(&mut self, surface: &str, cx: &mut Context<Self>) {
        self.prefs.panel_open = true;
        self.prefs.panel_surface = surface.to_owned();
        self.save(cx);
    }

    /// `workspaces.showPinnedOnly` and `workspaces.showAll`, and the filter
    /// toggle in the sidebar.
    pub fn set_pinned_only(&mut self, pinned_only: bool, cx: &mut Context<Self>) {
        self.prefs.pinned_only = pinned_only;
        self.workspaces.update(cx, |workspaces, cx| {
            workspaces.set_pinned_only(pinned_only, cx)
        });
        self.save(cx);
    }

    fn next_agent_sort(&mut self, cx: &mut Context<Self>) {
        self.prefs.agent_sort = layout::next_sort_mode(self.prefs.agent_sort);
        self.save(cx);
    }

    /// An app tab covers the terminal: the surface stops sizing the client.
    fn sync_terminal_visible(&mut self, cx: &mut Context<Self>) {
        let visible = self.workspaces.read(cx).view(cx).selected_app_tab.is_none();
        if let Some(terminal) = self.terminal.clone() {
            terminal.update(cx, |surface, cx| surface.set_terminal_visible(visible, cx));
        }
    }

    pub(crate) fn drag_to(
        &mut self,
        divider: Divider,
        pointer: Point<Pixels>,
        bounds: Bounds<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let unit = Self::css_unit(window);
        let rails = self.committed_rails(window);
        let value = match divider {
            Divider::Sidebar => ((pointer.x - bounds.left()).as_f32() / unit)
                .clamp(layout::SIDEBAR_MIN_WIDTH, rails.sidebar_max),
            Divider::Panel => ((bounds.right() - pointer.x).as_f32() / unit)
                .clamp(layout::PANEL_MIN_WIDTH, rails.panel_max),
            Divider::Agents => ((bounds.bottom() - pointer.y) / bounds.size.height)
                .clamp(layout::AGENTS_RATIO_MIN, layout::AGENTS_RATIO_MAX),
            Divider::Dock => layout::dock_fraction_at(
                bounds.size.width.as_f32() / unit,
                (pointer.x - bounds.left()).as_f32() / unit,
                self.dock_side(cx),
            ),
        };
        self.drag = Some((divider, value));
        cx.notify();
    }

    pub(crate) fn end_drag(&mut self, window: &Window, cx: &mut Context<Self>) {
        if let Some((divider, value)) = self.drag.take() {
            self.commit(divider, value, window, cx);
        }
    }

    pub(crate) fn nudge(
        &mut self,
        divider: Divider,
        delta: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        // From the width on screen, which the window may have capped.
        let rails = self.committed_rails(window);
        let current = match divider {
            Divider::Sidebar => rails.sidebar_width,
            Divider::Panel => rails.panel_width,
            Divider::Agents => self.prefs.agents_ratio,
            Divider::Dock => self.prefs.dock_fraction,
        };
        self.commit(divider, current + delta, window, cx);
    }

    /// Stores a divider's value through the same caps a drag gets.
    fn commit(&mut self, divider: Divider, value: f32, window: &Window, cx: &mut Context<Self>) {
        let width = Self::window_width(window);
        let rails = self.committed_rails(window);
        let value = match divider {
            Divider::Sidebar => {
                layout::sidebar_width_for_window(value.min(rails.sidebar_max), width)
            }
            Divider::Panel => layout::panel_width_for_window(value.min(rails.panel_max), width),
            Divider::Agents => layout::clamped_agents_ratio(Some(value)),
            Divider::Dock => layout::clamped_dock_fraction(Some(value)),
        };
        set_divider(&mut self.prefs, divider, value);
        self.save(cx);
    }

    /// Writes [`Prefs`] over the saved state, one write at a time.
    fn save(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        {
            let mut queue = self.saves.lock().unwrap();
            queue.pending = Some(self.prefs.clone());
            if queue.running {
                return;
            }
            queue.running = true;
        }
        let saves = self.saves.clone();
        let store = cx.global::<Core>().app_state.clone();
        let runner = platform::spawn(async move {
            loop {
                let next = {
                    let mut queue = saves.lock().unwrap();
                    let next = queue.pending.take();
                    queue.running = next.is_some();
                    next
                };
                let Some(prefs) = next else {
                    return;
                };
                let result = app_state::update_app_state(&store, |state| {
                    prefs.apply_to(&mut state.shell);
                })
                .await;
                if let Err(error) = result {
                    eprintln!("muxflow: could not save the shell layout: {error}");
                }
            }
        });
        self.saves.lock().unwrap().runner = Some(runner);
    }
}

/// Gives the registry's view commands, and so their shortcuts, this chrome.
fn register_commands(chrome: &WeakEntity<Chrome>, cx: &mut App) {
    fn bind(
        id: &'static str,
        chrome: &WeakEntity<Chrome>,
        cx: &mut App,
        action: impl Fn(&mut Chrome, &mut Context<Chrome>) + 'static,
    ) {
        let chrome = chrome.clone();
        commands::on_command(id, cx, move |_, cx| {
            chrome.update(cx, |chrome, cx| action(chrome, cx)).ok();
        });
    }
    bind("view.toggleSidebar", chrome, cx, Chrome::toggle_sidebar);
    bind("agents.toggleSort", chrome, cx, Chrome::next_agent_sort);
    bind("workspaces.showPinnedOnly", chrome, cx, |chrome, cx| {
        chrome.set_pinned_only(true, cx)
    });
    bind("workspaces.showAll", chrome, cx, |chrome, cx| {
        chrome.set_pinned_only(false, cx)
    });
    bind("view.togglePanel", chrome, cx, Chrome::toggle_panel);
    for (id, surface) in [
        ("view.showFiles", "files"),
        ("view.showGit", "git"),
        ("view.showPorts", "ports"),
    ] {
        bind(id, chrome, cx, move |chrome, cx| {
            chrome.show_surface(surface, cx)
        });
    }
}

fn set_divider(prefs: &mut Prefs, divider: Divider, value: f32) {
    match divider {
        Divider::Sidebar => prefs.sidebar_width = value,
        Divider::Panel => prefs.panel_width = value,
        Divider::Agents => prefs.agents_ratio = value,
        Divider::Dock => prefs.dock_fraction = value,
    }
}

impl Render for Chrome {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rails = self.rails(window);
        let displayed = self.displayed();
        let chrome = self.this.clone();
        let surface = layout::resolved_surface(&self.surfaces, &displayed.panel_surface).to_owned();
        div()
            .id("workspace-shell")
            .relative()
            .flex()
            .size_full()
            .overflow_hidden()
            .bg(theme::palette(cx).chrome_bg)
            // First, so that on every move it follows the pointer before any
            // target offers itself.
            .child(crate::dragdrop::listeners())
            .on_drag_move::<DividerDrag>({
                let chrome = chrome.clone();
                move |event, window, cx| {
                    let DividerDrag(divider) = *event.drag(cx);
                    if divider == Divider::Dock {
                        return;
                    }
                    chrome
                        .update(cx, |chrome, cx| {
                            chrome.drag_to(divider, event.event.position, event.bounds, window, cx)
                        })
                        .ok();
                }
            })
            .when(self.drag.is_some(), |this| {
                this.child(divider::drag_release(chrome.clone()))
            })
            .when(rails.sidebar_open, |this| {
                this.child(Sidebar {
                    chrome: chrome.clone(),
                    width: rails.sidebar_width,
                    agents_ratio: displayed.agents_ratio,
                    new_workspace_here: self.tabs_in_title_bar,
                    pinned_only: displayed.pinned_only,
                    agent_sort: displayed.agent_sort,
                    workspaces: self.workspaces.clone(),
                })
            })
            .child(WorkspaceArea {
                chrome: chrome.clone(),
                workspaces: self.workspaces.clone(),
                dock_fraction: displayed.dock_fraction,
                tabs_in_title_bar: self.tabs_in_title_bar,
                terminal: self.terminal.clone().map(AnyView::from),
            })
            .when(rails.panel_open, |this| {
                this.child(RightPanel {
                    chrome,
                    width: rails.panel_width,
                    surfaces: self.surfaces.clone(),
                    surface,
                })
            })
            .children(crate::dragdrop::ghost(cx))
    }
}
