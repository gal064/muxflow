//! The workspaces and the shell around them: the sidebar's rows across every
//! shown host, the tab strip, navigation between workspaces, windows and panes,
//! creating, renaming, moving and closing them, Back and Forward, and the
//! commands for all of it. Port of `features/workspaces`, `features/shell`
//! (what Phase 2 had not) and `features/tmux`, with the `app/` hooks behind
//! them (`useShellNavigation`, `useTmuxActionPerformer`, `useBulkTabClose`,
//! `useFocusHistoryNavigation`, `useWorkspaceCreate`).
//!
//! [`Workspaces`] is one entity per window, made by the chrome. It owns no
//! topology: the connection controller holds every host's link and is the one
//! in-memory owner of the app tabs and workspace records. What the shell shows
//! is derived from those on every read ([`Workspaces::view`]).
//!
//! The agents feature joins through [`agents::AgentShell`].

pub mod actions;
pub mod agents;
mod commands;
mod dialog;
pub mod focus_history;
pub mod model;
pub mod navigation;
pub mod rows;
#[cfg(test)]
mod tests;

use std::{
    cell::Cell,
    collections::HashMap,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use desktop_core::{
    app_state::{AppTabRecord, DockSide, PersistedAppState, update_app_state},
    connection::ConnectionSpec,
    platform,
};
use futures::FutureExt as _;
use gpui_kit::{
    App, AppContext, AsyncApp, Context, Entity, FocusHandle, Global, Subscription, Task,
    WeakEntity, Window as GpuiWindow,
};
use serde_json::json;
use tmux_control::{Pane, Session, TmuxSnapshot, Window};

use crate::{
    app::Core,
    connection_ui::{
        ConnectionUi,
        connection_reducer::ConnectionPhase,
        controller::ConnectionController,
        host_links::{HostLink, OptimisticWindow, session_windows},
        host_profiles::host_letter,
        host_scope::{HostScopeToken, same_host_connection},
    },
};

use actions::{
    ActionKind, ActionResult, Precondition, RECONCILE_POLL_MS, SCOPE_CHANGED, TmuxAction,
    request_reconciled,
};
use agents::{AgentPresence, AgentShell};
use focus_history::{Direction, FocusHistory, FocusPoint};
use model::{
    CombinedTab, PendingTab, TabKind, UNAVAILABLE_UNTIL_LIVE, WAITING_FOR_HOST, WindowAgentInfo,
    app_key, app_tabs_for_workspace, combine_workspace_tabs, retire_pending_tab, select_app_tab,
    should_surface_authoritative_terminal, terminal_key, workspace_ui_record,
};
use navigation::{Coordinator, Finish, Location, Outcome, transition_plan, window_for_session};
use rows::{HostRowSource, WorkspaceRow, host_workspace_rows, merge_host_rows};

pub use commands::{Target, run as run_command};

/// Called once from [`crate::features::init`]. The agents feature fills the
/// seam later; until then every row and tab says nothing about agents.
pub fn init(cx: &mut App) {
    cx.set_global(AgentShell::default());
}

/// The window's shell, for the agents feature and anything else that needs to
/// move it. `None` before the first window.
pub fn current(cx: &App) -> Option<Entity<Workspaces>> {
    cx.try_global::<CurrentWorkspaces>()
        .and_then(|current| current.0.upgrade())
}

struct CurrentWorkspaces(WeakEntity<Workspaces>);

impl Global for CurrentWorkspaces {}

/// A host round trip, smoothed. Port of `features/shell/hostLatency.ts`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Latency {
    pub milliseconds: f64,
    at: Instant,
}

/// A reading older than this says nothing about the link now.
const LATENCY_STALE_AFTER: Duration = Duration::from_secs(60);
const LATENCY_SMOOTHING: f64 = 0.3;

fn smoothed_latency(previous: Option<Latency>, sample: f64, now: Instant) -> Latency {
    let previous =
        previous.filter(|previous| now.duration_since(previous.at) <= LATENCY_STALE_AFTER);
    Latency {
        milliseconds: previous.map_or(sample, |previous| {
            previous.milliseconds + (sample - previous.milliseconds) * LATENCY_SMOOTHING
        }),
        at: now,
    }
}

/// One shown host, as the host menu and the New workspace dialog list it.
#[derive(Clone, Debug, PartialEq)]
pub struct HostEntry {
    pub profile_id: String,
    pub label: String,
    pub letter: String,
    pub phase: ConnectionPhase,
    pub can_mutate: bool,
    pub shown: bool,
    pub active: bool,
    pub transport: &'static str,
}

/// What the shell shows right now, derived from the active host's link and the
/// app tabs.
#[derive(Clone, Default)]
pub struct ShellView {
    pub host: String,
    pub host_label: String,
    pub scope: Option<HostScopeToken>,
    pub can_mutate: bool,
    pub server: Option<String>,
    pub snapshot: Arc<TmuxSnapshot>,
    pub session: Option<Session>,
    pub window: Option<Window>,
    /// The workspace's windows in tab order.
    pub windows: Vec<Window>,
    /// The panes of the window on screen.
    pub panes: Vec<Pane>,
    pub active_pane: Option<Pane>,
    pub app_tabs: Vec<AppTabRecord>,
    /// Every workspace's documents, for Back and Forward across workspaces.
    pub app_state: Arc<PersistedAppState>,
    pub selected_app_tab: Option<AppTabRecord>,
    pub docked: Option<(AppTabRecord, DockSide)>,
    pub tabs: Vec<CombinedTab>,
    pub active_key: Option<String>,
}

impl ShellView {
    pub fn tab(&self, key: &str) -> Option<&CombinedTab> {
        self.tabs.iter().find(|tab| tab.key == key)
    }

    pub fn dock_side(&self) -> DockSide {
        self.docked
            .as_ref()
            .map_or(DockSide::Right, |(_, side)| *side)
    }
}

/// How an action reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Feedback {
    /// A refusal is said, and success is acknowledged.
    Visible,
    /// Following focus and other navigation the user did not ask about.
    Silent,
}

/// What a move commits once it lands.
#[derive(Clone, Debug)]
struct Intent {
    key: String,
    host: String,
    scope: HostScopeToken,
    destination: Location,
    whole_workspace: bool,
    /// Focused once the window is reached.
    pane: Option<String>,
    /// The guard this move put up, if it was painted before the host answered.
    optimistic: Option<OptimisticWindow>,
    /// Where the host was when the move began, for planning an optimistic move
    /// and for putting a refused one back.
    origin: Option<Location>,
    /// A document reasserted over the terminal location, or none.
    app_tab: Option<String>,
    /// Its place among the user's moves: one asked for later, a create among
    /// them, keeps this one from committing.
    seq: u64,
}

/// A document chosen while a remote move was still in flight: tmux will land
/// that move after the document is up, and its window change must not take the
/// document down.
struct ProtectedAppTab {
    app_tab_id: String,
    scope: HostScopeToken,
    session_id: String,
    through: Option<u64>,
}

/// A selection on a host that is being activated, made once it has spoken.
struct HostSelection {
    host: String,
    session_id: String,
    pane_id: Option<String>,
}

/// A workspace asked for on another host, created once that host is active.
struct PendingCreate {
    host: String,
    name: String,
    scope: HostScopeToken,
}

pub struct Workspaces {
    controller: Option<Entity<ConnectionController>>,
    /// The sidebar filter's state, mirrored from the chrome's preferences.
    pinned_only: bool,
    pending: Option<(String, PendingTab)>,
    next_key: u64,
    /// Bumped by every move the user asks for; a create's ack moves the shell
    /// only if nothing was asked for after it.
    intents: u64,
    history: FocusHistory,
    history_host: String,
    last_point: Option<FocusPoint>,
    /// The point a Back or Forward is on its way to; arriving there is not a
    /// new visit.
    expected: Option<FocusPoint>,
    coordinator: Coordinator<Intent>,
    flight_scope: Option<HostScopeToken>,
    latency: Option<Latency>,
    latency_client: Option<String>,
    /// The window tmux reported active, per host, server and workspace.
    authoritative_windows: HashMap<String, String>,
    protected_app_tab: Option<ProtectedAppTab>,
    host_selection: Option<HostSelection>,
    pending_create: Option<PendingCreate>,
    bulk_closing: bool,
    context: Option<commands::ContextFields>,
    /// The workspace list and the tab strip take focus as one stop each; the
    /// arrows then move a cursor through their items.
    /// The window's terminal, set by the chrome that owns it.
    pub terminal: Option<WeakEntity<crate::terminal::surface::TerminalSurface>>,
    pub list_focus: FocusHandle,
    pub strip_focus: FocusHandle,
    pub list_cursor: Option<usize>,
    pub strip_cursor: Option<usize>,
    /// The rows' and tabs' order the cursors were set against: a cursor is a
    /// place in that order, which a move or a close changes under it.
    cursor_order: u64,
    _subscriptions: Vec<Subscription>,
}

impl Workspaces {
    pub fn new(pinned_only: bool, cx: &mut App) -> Entity<Self> {
        let controller = cx.try_global::<ConnectionUi>().map(|ui| ui.0.clone());
        let entity = cx.new(|cx: &mut Context<Self>| {
            let mut subscriptions = vec![cx.observe_global::<AgentShell>(|this, cx| {
                this.publish_context(cx);
                cx.notify();
            })];
            if let Some(controller) = &controller {
                subscriptions
                    .push(cx.observe(controller, |this, _, cx| this.controller_changed(cx)));
            }
            Self {
                controller,
                pinned_only,
                pending: None,
                next_key: 0,
                intents: 0,
                history: FocusHistory::default(),
                history_host: String::new(),
                last_point: None,
                expected: None,
                coordinator: Coordinator::default(),
                flight_scope: None,
                latency: None,
                latency_client: None,
                authoritative_windows: HashMap::new(),
                protected_app_tab: None,
                host_selection: None,
                pending_create: None,
                bulk_closing: false,
                context: None,
                // Tab stops, as Tauri's row buttons and roving tablist are.
                terminal: None,
                list_focus: cx.focus_handle().tab_stop(true),
                strip_focus: cx.focus_handle().tab_stop(true),
                list_cursor: None,
                strip_cursor: None,
                cursor_order: 0,
                _subscriptions: subscriptions,
            }
        });
        cx.set_global(CurrentWorkspaces(entity.downgrade()));
        commands::register(&entity.downgrade(), cx);
        entity.update(cx, |this, cx| this.controller_changed(cx));
        entity
    }

    pub fn controller(&self) -> Option<&Entity<ConnectionController>> {
        self.controller.as_ref()
    }

    pub fn set_pinned_only(&mut self, pinned_only: bool, cx: &mut Context<Self>) {
        if self.pinned_only != pinned_only {
            self.pinned_only = pinned_only;
            // The filter reorders the list under the cursor.
            self.list_cursor = None;
            self.publish_context(cx);
            cx.notify();
        }
    }

    pub fn latency(&self) -> Option<f64> {
        self.latency
            .filter(|latency| latency.at.elapsed() <= LATENCY_STALE_AFTER)
            .map(|latency| latency.milliseconds)
    }

    // Reading.

    pub fn view(&self, cx: &App) -> ShellView {
        let Some(controller) = &self.controller else {
            return ShellView::default();
        };
        let controller = controller.read(cx);
        let host = controller.active_profile_id();
        let host_label = controller.active_host_label();
        let Some(link) = controller.active_link() else {
            return ShellView {
                host,
                host_label,
                ..ShellView::default()
            };
        };
        let state = &link.host_state;
        let snapshot = state.snapshot.clone();
        let session = link
            .active_session_id
            .as_deref()
            .and_then(|id| state.session(id))
            .cloned();
        let windows: Vec<Window> = session.as_ref().map_or_else(Vec::new, |session| {
            session_windows(&snapshot.windows, &session.id)
                .into_iter()
                .cloned()
                .collect()
        });
        let window = link
            .active_window_id
            .as_deref()
            .and_then(|id| windows.iter().find(|window| window.id == id))
            .or_else(|| windows.iter().find(|window| window.active))
            .or_else(|| windows.first())
            .cloned();
        let mut panes: Vec<Pane> = window.as_ref().map_or_else(Vec::new, |window| {
            snapshot
                .panes
                .iter()
                .filter(|pane| pane.window_id == window.id)
                .cloned()
                .collect()
        });
        panes.sort_by_key(|pane| pane.index);
        let active_pane = panes
            .iter()
            .find(|pane| pane.active)
            .or_else(|| panes.first())
            .cloned();
        let app_state = controller.app_state().unwrap_or_default();
        let server = state.server_identity.clone();
        let (app_tabs, record) = match &session {
            Some(session) => (
                app_tabs_for_workspace(&app_state, &host, server.as_deref(), &session.id)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>(),
                workspace_ui_record(&app_state, &host, server.as_deref(), &session.id).cloned(),
            ),
            None => (Vec::new(), None),
        };
        let find = |id: &Option<String>| {
            id.as_ref()
                .and_then(|id| app_tabs.iter().find(|tab| tab.id == *id))
                .cloned()
        };
        let selected_app_tab = record
            .as_ref()
            .and_then(|record| find(&record.selected_app_tab_id));
        let docked = record.as_ref().and_then(|record| {
            find(&record.docked_app_tab_id)
                .map(|tab| (tab, record.dock_side.unwrap_or(DockSide::Right)))
        });
        let pending = self
            .pending
            .as_ref()
            .filter(|(pending_host, pending)| {
                *pending_host == host
                    && pending.session_id == session.as_ref().map(|s| s.id.clone())
            })
            .map(|(_, pending)| pending);
        let agents = agents::agent_shell(cx);
        let window_refs: Vec<&Window> = windows.iter().collect();
        let tab_refs: Vec<&AppTabRecord> = app_tabs.iter().collect();
        let tabs = combine_workspace_tabs(
            &window_refs,
            &tab_refs,
            |window_id| {
                let info = agents.and_then(|agents| agents.window(&host, window_id));
                WindowAgentInfo {
                    attention: info
                        .and_then(|info| info.attention.as_ref().map(|(state, _)| *state)),
                    presence: info.map_or(AgentPresence::Unknown, |info| info.presence),
                }
            },
            pending,
        );
        let active_key = selected_app_tab
            .as_ref()
            .map(|tab| app_key(&tab.id))
            .or_else(|| window.as_ref().map(|window| terminal_key(&window.id)));
        ShellView {
            scope: Some(link.scope()),
            can_mutate: state.can_mutate,
            server,
            snapshot,
            session,
            window,
            windows,
            panes,
            active_pane,
            app_tabs,
            app_state,
            selected_app_tab,
            docked,
            tabs,
            active_key,
            host,
            host_label,
        }
    }

    /// Every shown host, in order, with the active one among them.
    pub fn hosts(&self, cx: &App) -> Vec<HostEntry> {
        let Some(controller) = &self.controller else {
            return Vec::new();
        };
        let controller = controller.read(cx);
        let active = controller.active_profile_id();
        let mut hosts: Vec<HostEntry> = controller
            .profiles()
            .iter()
            .map(|profile| {
                let link = controller.link(&profile.id);
                HostEntry {
                    profile_id: profile.id.clone(),
                    label: profile.label.clone(),
                    letter: host_letter(profile),
                    phase: link.map_or(ConnectionPhase::Disconnected, |link| link.host_state.phase),
                    can_mutate: link.is_some_and(|link| link.host_state.can_mutate),
                    shown: link.is_some(),
                    active: profile.id == active,
                    transport: transport(&profile.connection),
                }
            })
            .collect();
        // A host connected from the form without a saved profile.
        if !hosts.iter().any(|host| host.active)
            && let Some(link) = controller.active_link()
        {
            let label = controller.active_host_label();
            hosts.insert(
                0,
                HostEntry {
                    profile_id: active.clone(),
                    letter: label
                        .chars()
                        .next()
                        .map(|c| c.to_uppercase().collect())
                        .unwrap_or_default(),
                    label,
                    phase: link.host_state.phase,
                    can_mutate: link.host_state.can_mutate,
                    shown: true,
                    active: true,
                    transport: transport(&link.connection),
                },
            );
        }
        hosts
    }

    /// Every shown host's rows, merged; filtered when the sidebar shows pinned
    /// workspaces only.
    pub fn rows(&self, filtered: bool, cx: &App) -> Vec<WorkspaceRow> {
        let Some(controller) = &self.controller else {
            return Vec::new();
        };
        let controller_state = controller.read(cx);
        let hosts = self.hosts(cx);
        let active = controller_state.active_profile_id();
        let show_letters = controller_state.links().count() > 1;
        let agents = agents::agent_shell(cx);
        let per_host = controller_state
            .links()
            .map(|link| {
                let host = hosts.iter().find(|host| host.profile_id == link.profile_id);
                let source = HostRowSource {
                    host_profile_id: &link.profile_id,
                    letter: host.map_or("", |host| host.letter.as_str()),
                    label: host.map_or(link.profile_id.as_str(), |host| host.label.as_str()),
                    phase: link.host_state.phase,
                    can_mutate: link.host_state.can_mutate,
                    scope: link.scope(),
                    snapshot: &link.host_state.snapshot,
                    active_session_id: (link.profile_id == active)
                        .then_some(link.active_session_id.as_deref())
                        .flatten(),
                };
                host_workspace_rows(&source, show_letters, |host, session| {
                    agents
                        .and_then(|agents| agents.workspace(host, session))
                        .cloned()
                })
            })
            .collect();
        let rows = merge_host_rows(per_host);
        if filtered && self.pinned_only {
            let session = controller_state
                .active_link()
                .and_then(|link| link.active_session_id.clone());
            rows::pinned_only_rows(&rows, &active, session.as_deref())
        } else {
            rows
        }
    }

    pub fn can_go(&self, direction: Direction, cx: &App) -> bool {
        let view = self.view(cx);
        self.history
            .has_step(direction, |point| point_exists(&view, point))
    }

    fn link(&self, host: &str, cx: &App) -> Option<HostLink> {
        self.controller.as_ref()?.read(cx).link(host).cloned()
    }

    fn scope_current(&self, host: &str, scope: &HostScopeToken, cx: &App) -> bool {
        self.link(host, cx)
            .is_some_and(|link| same_host_connection(scope, &link.scope()))
    }

    fn report(&self, message: impl Into<String>, cx: &mut App) {
        if let Some(controller) = &self.controller {
            let message = message.into();
            controller.update(cx, |controller, cx| controller.report_status(message, cx));
        }
    }

    /// Changes the app tabs or workspace records, through their one owner.
    pub fn update_app_state(
        &self,
        change: impl FnOnce(&mut PersistedAppState) -> bool,
        cx: &mut App,
    ) {
        if let Some(controller) = &self.controller {
            controller.update(cx, |controller, cx| {
                controller.update_app_state(change, cx);
                cx.notify();
            });
        }
    }

    // Following the controller.

    fn controller_changed(&mut self, cx: &mut Context<Self>) {
        let view = self.view(cx);
        let scope = view.scope.clone();
        // A different connection: what was in flight belongs to the old one.
        let same = match (&self.flight_scope, &scope) {
            (Some(old), Some(new)) => same_host_connection(old, new),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.coordinator.invalidate();
            self.protected_app_tab = None;
            self.pending = None;
            self.flight_scope = scope.clone();
        }
        let client = self
            .controller
            .as_ref()
            .and_then(|controller| controller.read(cx).active_link())
            .and_then(|link| link.client_id.clone());
        if client != self.latency_client {
            self.latency_client = client;
            self.latency = None;
        }
        self.retire_pending(cx);
        let order = {
            let mut hasher = std::hash::DefaultHasher::new();
            for row in self.rows(true, cx) {
                std::hash::Hash::hash(&row.key, &mut hasher);
            }
            for tab in &view.tabs {
                std::hash::Hash::hash(&tab.key, &mut hasher);
            }
            std::hash::Hasher::finish(&hasher)
        };
        if order != self.cursor_order {
            self.cursor_order = order;
            self.list_cursor = None;
            self.strip_cursor = None;
        }
        self.surface_terminal_on_host_switch(&view, cx);
        self.record_history(&view);
        self.finish_host_handoffs(&view, cx);
        self.publish_context(cx);
        cx.notify();
    }

    fn retire_pending(&mut self, cx: &App) {
        let Some((host, pending)) = self.pending.take() else {
            return;
        };
        let Some(link) = self.link(&host, cx) else {
            return;
        };
        let windows: Vec<&Window> = match &pending.session_id {
            Some(session) => session_windows(&link.host_state.snapshot.windows, session),
            None => Vec::new(),
        };
        self.pending = retire_pending_tab(Some(pending), &windows).map(|pending| (host, pending));
    }

    /// Another client switching windows in this workspace uncovers the
    /// terminal under a document, as the Tauri shell does.
    fn surface_terminal_on_host_switch(&mut self, view: &ShellView, cx: &mut Context<Self>) {
        let (Some(session), Some(server)) = (&view.session, &view.server) else {
            return;
        };
        let generation = view.scope.as_ref().map_or(0, |scope| scope.generation);
        let authoritative = view
            .windows
            .iter()
            .find(|window| window.active)
            .map(|w| w.id.clone());
        let key = format!("{}\0{server}\0{}", view.host, session.id);
        let previous = self.authoritative_windows.get(&key).cloned();
        let protected = match &self.protected_app_tab {
            Some(protection)
                if view
                    .scope
                    .as_ref()
                    .is_some_and(|scope| same_host_connection(&protection.scope, scope)) =>
            {
                if protection.session_id == session.id
                    && protection
                        .through
                        .is_some_and(|through| generation >= through)
                {
                    self.protected_app_tab = None;
                }
                true
            }
            _ => false,
        };
        if !protected
            && should_surface_authoritative_terminal(
                view.selected_app_tab.is_some(),
                Some(&session.id),
                previous.as_deref(),
                Some(&session.id),
                authoritative.as_deref(),
                previous.as_ref().is_some_and(|previous| {
                    view.windows.iter().any(|window| window.id == *previous)
                }),
            )
        {
            let (host, server, session) = (view.host.clone(), server.clone(), session.clone());
            self.update_app_state(
                move |state| {
                    select_app_tab(state, &host, &server, &session, None);
                    true
                },
                cx,
            );
        }
        if let Some(authoritative) = authoritative {
            self.authoritative_windows.insert(key, authoritative);
        }
    }

    fn record_history(&mut self, view: &ShellView) {
        // A different host is a different tmux server, with its own `$0`.
        if view.host != self.history_host {
            self.history.clear();
            self.history_host = view.host.clone();
            self.last_point = None;
        }
        if let Some(session) = &view.session {
            let point = FocusPoint {
                session_id: session.id.clone(),
                window_id: view.window.as_ref().map(|window| window.id.clone()),
                app_tab_id: view.selected_app_tab.as_ref().map(|tab| tab.id.clone()),
            };
            if self.last_point.as_ref() != Some(&point) {
                self.last_point = Some(point.clone());
                // The keyboard cursors start again from what is now active, as
                // Tauri's roving tab stop does: one left on an old row or tab
                // would send Enter somewhere the user no longer is.
                self.list_cursor = None;
                self.strip_cursor = None;
                let expected = self.expected.take();
                if !expected.is_some_and(|expected| landed_at(&expected, &point)) {
                    self.history.visit(point);
                }
            }
        }
        self.history.prune(|point| point_exists(view, point));
    }

    fn finish_host_handoffs(&mut self, view: &ShellView, cx: &mut Context<Self>) {
        if let Some(pending) = self.pending_create.take() {
            if pending.host != view.host {
                self.report(
                    "Workspace creation was cancelled because another host was selected.",
                    cx,
                );
            } else if view.server.is_none() {
                self.pending_create = Some(pending);
            } else if !view
                .scope
                .as_ref()
                .is_some_and(|scope| same_host_connection(&pending.scope, scope))
                || !view.can_mutate
            {
                self.report(
                    "Workspace creation was cancelled because its host connection changed.",
                    cx,
                );
            } else {
                self.create_workspace(pending.name, cx);
            }
        }
        if let Some(selection) = self.host_selection.take() {
            if selection.host != view.host {
                return;
            }
            // A host that has not spoken has nothing to select on yet.
            if view.server.is_none() {
                self.host_selection = Some(selection);
                return;
            }
            match selection.pane_id {
                Some(pane) => self.select_pane(&selection.host, &pane, cx),
                None => self.select_session(&selection.session_id, cx),
            }
        }
    }

    // Navigation.

    /// A sidebar row or a Quick Open workspace, on whichever host it is.
    pub fn select_row(&mut self, row: &WorkspaceRow, cx: &mut Context<Self>) {
        let active = self.view(cx).host;
        if row.host_profile_id == active {
            self.select_session(&row.session.id, cx);
            return;
        }
        self.select_on_host(&row.host_profile_id, &row.session.id, None, cx);
    }

    /// Activates `host` and selects there once it has spoken.
    pub fn select_on_host(
        &mut self,
        host: &str,
        session_id: &str,
        pane_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(controller) = self.controller.clone() else {
            return;
        };
        let known = {
            let controller = controller.read(cx);
            controller.link(host).is_some() || controller.profiles().iter().any(|p| p.id == host)
        };
        if !known {
            return;
        }
        self.begin_intent(true);
        self.host_selection = Some(HostSelection {
            host: host.to_owned(),
            session_id: session_id.to_owned(),
            pane_id,
        });
        controller.update(cx, |controller, cx| controller.activate_host(host, cx));
        self.controller_changed(cx);
    }

    pub fn select_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let seq = self.begin_intent(true);
        let view = self.view(cx);
        let Some(scope) = view.scope.clone() else {
            return;
        };
        if !view.can_mutate {
            self.commit_local(&view.host, session_id, None, None, cx);
            self.report(
                "Viewing the last known workspace. Writes remain frozen.",
                cx,
            );
            return;
        }
        let origin = view.session.as_ref().map(|session| Location {
            session_id: session.id.clone(),
            window_id: view.window.as_ref().map(|window| window.id.clone()),
        });
        let already_there = view
            .session
            .as_ref()
            .is_some_and(|session| session.id == session_id);
        let window_id = window_for_session(
            &view.snapshot.windows,
            session_id,
            origin
                .as_ref()
                .and_then(|origin| origin.window_id.as_deref()),
        );
        let optimistic = (!already_there).then(|| OptimisticWindow {
            session_id: session_id.to_owned(),
            window_id: window_id.clone(),
            through_generation: None,
        });
        let intent = Intent {
            key: format!("session:{session_id}"),
            host: view.host.clone(),
            scope,
            destination: Location {
                session_id: session_id.to_owned(),
                window_id: None,
            },
            whole_workspace: true,
            pane: None,
            optimistic: optimistic.clone(),
            origin,
            app_tab: None,
            seq,
        };
        // Painted first: the guard names the new workspace, and set while the
        // old one is still active it would be released at once.
        if let Some(guard) = optimistic {
            self.commit_local(&view.host, session_id, window_id.as_deref(), None, cx);
            self.set_guard(&view.host, Some(guard), cx);
        }
        self.navigate(intent, already_there, cx);
    }

    pub fn select_window(&mut self, window_id: &str, cx: &mut Context<Self>) {
        let seq = self.begin_intent(true);
        let view = self.view(cx);
        let Some(scope) = view.scope.clone() else {
            return;
        };
        let Some(target) = view
            .snapshot
            .windows
            .iter()
            .find(|window| window.id == window_id)
            .cloned()
        else {
            return;
        };
        if !view.can_mutate {
            self.commit_local(&view.host, &target.session_id, Some(&target.id), None, cx);
            self.report(
                "Viewing the last known terminal tab. Writes remain frozen.",
                cx,
            );
            return;
        }
        let origin = view.session.as_ref().map(|session| Location {
            session_id: session.id.clone(),
            window_id: view.window.as_ref().map(|window| window.id.clone()),
        });
        let already_there = view
            .window
            .as_ref()
            .is_some_and(|window| window.id == window_id)
            && view
                .session
                .as_ref()
                .is_some_and(|session| session.id == target.session_id);
        let optimistic = (!already_there).then(|| OptimisticWindow {
            session_id: target.session_id.clone(),
            window_id: Some(target.id.clone()),
            through_generation: None,
        });
        let intent = Intent {
            key: format!("window:{}:{}", target.session_id, target.id),
            host: view.host.clone(),
            scope,
            destination: Location {
                session_id: target.session_id.clone(),
                window_id: Some(target.id.clone()),
            },
            whole_workspace: false,
            pane: None,
            optimistic: optimistic.clone(),
            origin,
            app_tab: None,
            seq,
        };
        if let Some(guard) = optimistic {
            self.commit_local(&view.host, &target.session_id, Some(&target.id), None, cx);
            self.set_guard(&view.host, Some(guard), cx);
        }
        self.navigate(intent, already_there, cx);
    }

    /// Goes to a pane's workspace and window and focuses it, committed once the
    /// host has moved. On another host, that host is activated first.
    pub fn select_pane(&mut self, host: &str, pane_id: &str, cx: &mut Context<Self>) {
        let seq = self.begin_intent(true);
        let view = self.view(cx);
        if host != view.host {
            let Some(pane) = self.link(host, cx).and_then(|link| {
                link.host_state
                    .snapshot
                    .panes
                    .iter()
                    .find(|p| p.id == pane_id)
                    .cloned()
            }) else {
                return;
            };
            self.select_on_host(host, &pane.session_id, Some(pane_id.to_owned()), cx);
            return;
        }
        let Some(scope) = view.scope.clone() else {
            return;
        };
        let Some(pane) = view
            .snapshot
            .panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .cloned()
        else {
            self.report("Pane destination is no longer available.", cx);
            return;
        };
        if !view.can_mutate {
            self.commit_local(
                &view.host,
                &pane.session_id,
                Some(&pane.window_id),
                None,
                cx,
            );
            return;
        }
        let intent = Intent {
            key: format!("pane:{}:{}:{}", pane.session_id, pane.window_id, pane.id),
            host: view.host.clone(),
            scope,
            destination: Location {
                session_id: pane.session_id.clone(),
                window_id: Some(pane.window_id.clone()),
            },
            whole_workspace: false,
            pane: Some(pane.id.clone()),
            optimistic: None,
            origin: None,
            app_tab: None,
            seq,
        };
        self.navigate(intent, false, cx);
    }

    /// Puts a document over the terminal, or the terminal back with `None`.
    /// Local: it paints at once, and when a remote move is still in flight the
    /// terminal location is reasserted after it.
    pub fn select_app_tab(
        &mut self,
        session_id: &str,
        app_tab: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let seq = self.begin_intent(app_tab.is_none());
        let view = self.view(cx);
        let (Some(scope), Some(server)) = (view.scope.clone(), view.server.clone()) else {
            return;
        };
        let Some(session) = view
            .snapshot
            .sessions
            .iter()
            .find(|s| s.id == session_id)
            .cloned()
        else {
            return;
        };
        let window_id = if view.session.as_ref().is_some_and(|s| s.id == session_id) {
            view.window.as_ref().map(|window| window.id.clone())
        } else {
            window_for_session(&view.snapshot.windows, session_id, None)
        };
        let tab = app_tab.map(str::to_owned);
        {
            let (host, tab) = (view.host.clone(), tab.clone());
            self.update_app_state(
                move |state| {
                    select_app_tab(state, &host, &server, &session, tab.as_deref());
                    true
                },
                cx,
            );
        }
        if !self.coordinator.has_flight() {
            return;
        }
        if let Some(tab) = &tab {
            self.protected_app_tab = Some(ProtectedAppTab {
                app_tab_id: tab.clone(),
                scope: scope.clone(),
                session_id: session_id.to_owned(),
                through: None,
            });
        }
        let intent = Intent {
            key: match &tab {
                Some(tab) => format!("app:{session_id}:{tab}"),
                None => format!(
                    "window:{session_id}:{}",
                    window_id.clone().unwrap_or_default()
                ),
            },
            host: view.host.clone(),
            scope,
            destination: Location {
                session_id: session_id.to_owned(),
                window_id,
            },
            whole_workspace: false,
            pane: None,
            optimistic: None,
            origin: None,
            app_tab: tab.or(Some(String::new())),
            seq,
        };
        let _ = self.coordinator.navigate(intent.key.clone(), intent);
    }

    /// A tab in the strip: a terminal selects its window; a document covers
    /// the terminal, unless it is the docked one, which is already on screen.
    pub fn select_tab(&mut self, key: &str, cx: &mut Context<Self>) {
        let view = self.view(cx);
        let Some(tab) = view.tab(key) else {
            return;
        };
        match &tab.kind {
            TabKind::Terminal { window_id, .. } => {
                let window_id = window_id.clone();
                // Choosing the tab is looking at its agent.
                if let Some(pane) = view
                    .snapshot
                    .panes
                    .iter()
                    .find(|pane| pane.window_id == window_id && pane.active)
                {
                    crate::terminal::integration::agents::note_activity(&view.host, &pane.id, cx);
                }
                self.select_window(&window_id, cx);
            }
            TabKind::App { tab_id, .. } => {
                if view
                    .docked
                    .as_ref()
                    .is_some_and(|(docked, _)| docked.id == *tab_id)
                {
                    return;
                }
                if let Some(session) = &view.session {
                    let (session, tab_id) = (session.id.clone(), tab_id.clone());
                    self.select_app_tab(&session, Some(&tab_id), cx);
                }
            }
            TabKind::Pending => {}
        }
    }

    /// A new move by the user. A move to a terminal also ends any document's
    /// protection: the terminal is what the user asked for now.
    fn begin_intent(&mut self, terminal: bool) -> u64 {
        self.intents += 1;
        if terminal {
            self.protected_app_tab = None;
        }
        self.intents
    }

    /// A create supersedes the move still waiting behind the flight, as it
    /// does in Tauri, and nothing will now release the guard that move
    /// painted: it is released here, unless a later paint replaced it.
    fn supersede_queued(&mut self, cx: &mut App) {
        if let Some(queued) = self.coordinator.drop_queued()
            && let Some(guard) = &queued.optimistic
        {
            let held = self.link(&queued.host, cx).is_some_and(|link| {
                link.optimistic_window.as_ref().is_some_and(|held| {
                    held.session_id == guard.session_id && held.window_id == guard.window_id
                })
            });
            if held {
                self.set_guard(&queued.host, None, cx);
            }
        }
    }

    fn set_guard(&self, host: &str, guard: Option<OptimisticWindow>, cx: &mut App) {
        if let Some(controller) = &self.controller {
            controller.update(cx, |controller, cx| {
                controller.set_optimistic_window(host, guard, cx)
            });
        }
    }

    /// Puts the shell on `session` and `window`, and the terminal over any
    /// document there (or `app_tab` back over it).
    fn commit_local(
        &mut self,
        host: &str,
        session_id: &str,
        window_id: Option<&str>,
        app_tab: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(controller) = self.controller.clone() else {
            return;
        };
        controller.update(cx, |controller, cx| {
            controller.set_active_session(host, Some(session_id.to_owned()), cx);
            if let Some(window_id) = window_id {
                controller.set_active_window(host, Some(window_id.to_owned()), cx);
            }
        });
        let view = self.view(cx);
        let (Some(server), Some(session)) = (
            view.server.clone(),
            view.snapshot
                .sessions
                .iter()
                .find(|s| s.id == session_id)
                .cloned(),
        ) else {
            return;
        };
        let current = controller.read(cx).app_state().ok().and_then(|state| {
            workspace_ui_record(&state, host, Some(&server), session_id)
                .and_then(|record| record.selected_app_tab_id.clone())
        });
        if current.as_deref() == app_tab {
            return;
        }
        let (host, app_tab) = (host.to_owned(), app_tab.map(str::to_owned));
        self.update_app_state(
            move |state| {
                select_app_tab(state, &host, &server, &session, app_tab.as_deref());
                true
            },
            cx,
        );
    }

    fn commit(&mut self, intent: &Intent, cx: &mut Context<Self>) {
        if !self.scope_current(&intent.host, &intent.scope, cx) {
            return;
        }
        let app_tab = intent.app_tab.as_deref().filter(|tab| !tab.is_empty());
        self.commit_local(
            &intent.host,
            &intent.destination.session_id,
            intent.destination.window_id.as_deref(),
            app_tab,
            cx,
        );
    }

    fn navigate(&mut self, intent: Intent, already_there: bool, cx: &mut Context<Self>) {
        let Some((id, intent)) = self.coordinator.navigate(intent.key.clone(), intent) else {
            return;
        };
        if already_there {
            self.commit(&intent, cx);
            self.coordinator.finish(id);
            return;
        }
        self.start(id, intent, None, cx);
    }

    fn start(
        &mut self,
        id: u64,
        intent: Intent,
        predecessor: Option<Outcome>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let outcome = fly(&this, id, &intent, predecessor, cx).await;
            this.update(cx, |this, cx| this.land(id, intent, outcome, cx))
                .ok();
        })
        .detach();
    }

    /// Where the move is planned from: the predecessor's result, else where the
    /// host was before an optimistic paint, else where the shell is.
    fn plan(
        &self,
        intent: &Intent,
        predecessor: Option<&Outcome>,
        cx: &App,
    ) -> Option<(navigation::Plan, Outcome)> {
        let link = self.link(&intent.host, cx)?;
        if !same_host_connection(&intent.scope, &link.scope()) {
            return None;
        }
        let snapshot = &link.host_state.snapshot;
        let from = intent.origin.clone().or_else(|| {
            link.active_session_id.clone().map(|session_id| Location {
                session_id,
                window_id: link.active_window_id.clone(),
            })
        });
        let target = &intent.destination;
        let session_already_active = from
            .as_ref()
            .is_some_and(|from| from.session_id == target.session_id);
        let window_already_active = target.window_id.as_ref().is_some_and(|id| {
            snapshot
                .windows
                .iter()
                .any(|w| w.id == *id && w.session_id == target.session_id && w.active)
        });
        let current = match (predecessor, &from) {
            (None, Some(from)) => Some(Outcome::Partial {
                location: from.clone(),
                generation: link.host_state.generation,
            }),
            _ => None,
        };
        let plan = transition_plan(
            predecessor.or(current.as_ref()),
            target,
            intent.whole_workspace,
            window_already_active,
            session_already_active,
        );
        let start = predecessor.cloned().unwrap_or(Outcome::Unknown);
        Some((plan, start))
    }

    fn land(&mut self, id: u64, intent: Intent, outcome: Outcome, cx: &mut Context<Self>) {
        // A flight from a connection since replaced must not touch a guard or
        // a protection with the same ids on the new one.
        let current = self.scope_current(&intent.host, &intent.scope, cx);
        if let Some(guard) = intent.optimistic.as_ref().filter(|_| current) {
            let owned = self.link(&intent.host, cx).is_some_and(|link| {
                link.optimistic_window.as_ref().is_some_and(|held| {
                    held.session_id == guard.session_id && held.window_id == guard.window_id
                })
            });
            match outcome.generation() {
                // Held until a snapshot has caught up: the ack is not the
                // snapshot.
                Some(generation) if owned => self.set_guard(
                    &intent.host,
                    Some(OptimisticWindow {
                        through_generation: Some(generation),
                        ..guard.clone()
                    }),
                    cx,
                ),
                Some(_) => {}
                None if owned => {
                    // Put back actively: a refused switch changes nothing on the
                    // host, so no snapshot may come to correct the shell.
                    self.set_guard(&intent.host, None, cx);
                    if let Some(origin) = intent
                        .origin
                        .clone()
                        .filter(|_| self.scope_current(&intent.host, &intent.scope, cx))
                    {
                        let windows = self
                            .link(&intent.host, cx)
                            .map(|link| link.host_state.snapshot.windows.clone())
                            .unwrap_or_default();
                        let window = window_for_session(
                            &windows,
                            &origin.session_id,
                            origin.window_id.as_deref(),
                        );
                        if let Some(controller) = self.controller.clone() {
                            controller.update(cx, |controller, cx| {
                                controller.set_active_session(
                                    &intent.host,
                                    Some(origin.session_id.clone()),
                                    cx,
                                );
                                controller.set_active_window(&intent.host, window, cx);
                            });
                        }
                    }
                }
                None => {}
            }
        }
        if let Some(protection) = &mut self.protected_app_tab
            && current
            && intent.app_tab.as_deref() == Some(protection.app_tab_id.as_str())
        {
            match outcome {
                Outcome::Reached { generation, .. } => protection.through = Some(generation),
                _ => self.protected_app_tab = None,
            }
        }
        let reached = matches!(outcome, Outcome::Reached { .. });
        match self.coordinator.finish(id) {
            Finish::Latest if reached && intent.seq == self.intents => self.commit(&intent, cx),
            Finish::SameDestination(newer) if reached && newer.seq == self.intents => {
                self.commit(&newer, cx)
            }
            Finish::Next(next_id, next) => self.start(next_id, next, Some(outcome), cx),
            _ => {}
        }
    }

    // Actions.

    /// Sends one action to `host` through the reconciling request, saying
    /// what the user needs to hear. `Err` carries why it did not happen.
    fn perform(
        &mut self,
        host: &str,
        action: TmuxAction,
        captured: Option<Precondition>,
        feedback: Feedback,
        cx: &mut Context<Self>,
    ) -> Task<Result<ActionResult, String>> {
        let (Some(controller), Some(link)) = (self.controller.clone(), self.link(host, cx)) else {
            return Task::ready(Err(UNAVAILABLE_UNTIL_LIVE.into()));
        };
        let scope = link.scope();
        let (Some(client), true, Some(_)) = (
            link.client_id.clone(),
            link.host_state.can_mutate,
            scope.server_identity.clone(),
        ) else {
            if feedback == Feedback::Visible {
                self.report(UNAVAILABLE_UNTIL_LIVE, cx);
            }
            return Task::ready(Err(UNAVAILABLE_UNTIL_LIVE.into()));
        };
        let backend = controller.read(cx).backend();
        let measured = host == controller.read(cx).active_profile_id();
        let host = host.to_owned();
        // Each round trip is timed on its own, refused or not: a reconciling
        // retry's wait is not the host's latency.
        let round_trip = Rc::new(Cell::new(None::<f64>));
        cx.spawn(async move |this, cx| {
            let current = {
                let (controller, cx, host) = (controller.downgrade(), cx.clone(), host.clone());
                move || controller.read_with(&cx, |c, _| c.link(&host).map(HostLink::scope)).ok().flatten()
            };
            let executor = cx.background_executor().clone();
            let result = request_reconciled(
                &action,
                &scope,
                captured,
                |wire| {
                    let (started, round_trip) = (Instant::now(), round_trip.clone());
                    backend.tmux_action(client.clone(), wire).inspect(move |_| {
                        round_trip.set(Some(started.elapsed().as_secs_f64() * 1000.));
                    })
                },
                current,
                || executor.timer(Duration::from_millis(RECONCILE_POLL_MS)).map(|_| ()),
            )
            .await;
            this.update(cx, |this, cx| {
                let still = this.scope_current(&host, &scope, cx);
                if let (true, Some(elapsed)) = (measured, round_trip.get()) {
                    this.latency = Some(smoothed_latency(this.latency, elapsed, Instant::now()));
                }
                match &result {
                    Ok(_) => {
                        if still && feedback == Feedback::Visible {
                            this.report(WAITING_FOR_HOST, cx);
                        }
                    }
                    // A reconnect under the action is not a refusal.
                    Err(error) if error == SCOPE_CHANGED => {}
                    Err(error) => {
                        backend.record_incident(
                            "action.refused",
                            json!({ "action": action.kind.wire_name(), "error": error.chars().take(200).collect::<String>() }),
                        );
                        if still && feedback == Feedback::Visible {
                            this.report(error.clone(), cx);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
            result
        })
    }

    /// Fire and forget, with the refusal said.
    fn send(
        &mut self,
        host: &str,
        action: TmuxAction,
        captured: Option<Precondition>,
        cx: &mut Context<Self>,
    ) {
        self.perform(host, action, captured, Feedback::Visible, cx)
            .detach();
    }

    fn new_pending(&mut self, host: &str, session_id: Option<String>, title: String) -> u64 {
        self.next_key += 1;
        self.pending = Some((
            host.to_owned(),
            PendingTab {
                key: self.next_key,
                session_id,
                window_id: None,
                title,
            },
        ));
        self.next_key
    }

    /// Withdraws the placeholder only if it is still the one `key` put up: a
    /// slow create failing must not take down a newer one's.
    fn withdraw_pending(&mut self, key: u64) {
        if self
            .pending
            .as_ref()
            .is_some_and(|(_, pending)| pending.key == key)
        {
            self.pending = None;
        }
    }

    fn upgrade_pending(&mut self, key: u64, session_id: Option<String>, window_id: Option<String>) {
        if let Some((_, pending)) = self
            .pending
            .as_mut()
            .filter(|(_, pending)| pending.key == key)
        {
            pending.session_id = session_id.or(pending.session_id.take());
            pending.window_id = window_id;
        }
    }

    pub fn create_window(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let view = self.view(cx);
        let Some(scope) = view.scope.clone() else {
            return;
        };
        let host = view.host.clone();
        let intent = self.begin_intent(true);
        self.supersede_queued(cx);
        let key = self.new_pending(&host, Some(session_id.to_owned()), "New window".into());
        cx.notify();
        let task = self.perform(
            &host,
            TmuxAction::session(ActionKind::CreateWindow, session_id),
            None,
            Feedback::Visible,
            cx,
        );
        let session_id = session_id.to_owned();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let created = result
                    .ok()
                    .filter(|_| this.scope_current(&host, &scope, cx));
                let Some(created) = created.filter(|created| created.window_id.is_some()) else {
                    this.withdraw_pending(key);
                    cx.notify();
                    return;
                };
                let session = created.session_id.clone().unwrap_or(session_id);
                // Upgraded, not withdrawn: the ack names the window, but the
                // snapshot holding it has not arrived.
                this.upgrade_pending(key, Some(session.clone()), created.window_id.clone());
                if this.intents == intent {
                    this.commit_local(&host, &session, created.window_id.as_deref(), None, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Creates a workspace on the active host with that host's defaults. The
    /// startup command is sent once, here, to the pane the host named: no
    /// reconnect or reselection has a path back to it.
    pub fn create_workspace(&mut self, name: String, cx: &mut Context<Self>) {
        let view = self.view(cx);
        let Some(scope) = view.scope.clone() else {
            return;
        };
        let host = view.host.clone();
        let defaults = crate::settings::get(cx).defaults_for(&host);
        let intent = self.begin_intent(true);
        self.supersede_queued(cx);
        let title = if name.is_empty() {
            "New session".to_owned()
        } else {
            name.clone()
        };
        let key = self.new_pending(&host, None, title);
        cx.notify();
        let action = TmuxAction {
            name,
            directory: defaults.directory.unwrap_or_default(),
            // Born pinned while the list shows pinned only, or it would drop
            // out of the sidebar on the first switch away.
            pinned: self.pinned_only,
            ..TmuxAction::new(ActionKind::CreateSession)
        };
        let task = self.perform(&host, action, None, Feedback::Visible, cx);
        let startup = defaults
            .startup_command
            .filter(|command| !command.is_empty());
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let created = result
                    .ok()
                    .filter(|_| this.scope_current(&host, &scope, cx));
                let Some((created, session)) =
                    created.and_then(|c| c.session_id.clone().map(|s| (c, s)))
                else {
                    this.withdraw_pending(key);
                    cx.notify();
                    return;
                };
                this.upgrade_pending(key, Some(session.clone()), created.window_id.clone());
                if let (Some(command), Some(pane)) = (startup, created.pane_id.clone()) {
                    this.send_startup(&host, &pane, command, cx);
                }
                if this.intents == intent {
                    this.commit_local(&host, &session, created.window_id.as_deref(), None, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn send_startup(&self, host: &str, pane: &str, command: String, cx: &mut App) {
        let Some(controller) = &self.controller else {
            return;
        };
        let Some(client) = self.link(host, cx).and_then(|link| link.client_id) else {
            return;
        };
        // Shell text, sent verbatim: escaping it would change the command.
        let sent = controller.read(cx).backend().send_input(
            client,
            pane.to_owned(),
            format!("{command}\n").into_bytes(),
        );
        if let Err(error) = sent {
            self.report(
                format!("Workspace created, but the startup command could not be sent: {error}"),
                cx,
            );
        }
    }

    /// The New workspace dialog's answer.
    pub fn submit_new_workspace(&mut self, name: String, host: &str, cx: &mut Context<Self>) {
        let hosts = self.hosts(cx);
        let Some(link) = self
            .link(host, cx)
            .filter(|link| link.host_state.can_mutate)
        else {
            self.report("This action is unavailable until the selected host's authoritative connection is live.", cx);
            return;
        };
        if !hosts
            .iter()
            .any(|entry| entry.profile_id == host && entry.can_mutate)
        {
            self.report("This action is unavailable until the selected host's authoritative connection is live.", cx);
            return;
        }
        let view = self.view(cx);
        if host == view.host {
            if !view
                .scope
                .as_ref()
                .is_some_and(|scope| same_host_connection(scope, &link.scope()))
            {
                self.report(
                    "Workspace creation was cancelled because its host connection changed.",
                    cx,
                );
                return;
            }
            self.create_workspace(name, cx);
            return;
        }
        self.pending_create = Some(PendingCreate {
            host: host.to_owned(),
            name,
            scope: link.scope(),
        });
        if let Some(controller) = self.controller.clone() {
            controller.update(cx, |controller, cx| controller.activate_host(host, cx));
        }
        self.controller_changed(cx);
    }

    pub fn toggle_session_pin(&mut self, row: &WorkspaceRow, cx: &mut Context<Self>) {
        if !self.scope_current(&row.host_profile_id, &row.scope, cx) {
            return;
        }
        let action = TmuxAction {
            pinned: !row.session.pinned,
            ..TmuxAction::session(ActionKind::SetPinned, &row.session.id)
        };
        self.send(&row.host_profile_id.clone(), action, None, cx);
    }

    pub fn toggle_window_pin(&mut self, window_id: &str, cx: &mut Context<Self>) {
        let view = self.view(cx);
        let (Some(session), Some(window)) = (
            view.session.clone(),
            view.windows
                .iter()
                .find(|window| window.id == window_id)
                .cloned(),
        ) else {
            return;
        };
        let action = TmuxAction {
            pinned: !window.pinned,
            ..TmuxAction::window(ActionKind::SetPinned, &session.id, &window.id)
        };
        self.send(&view.host, action, None, cx);
    }

    pub fn toggle_host_shown(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        let Some(controller) = self.controller.clone() else {
            return;
        };
        let Some((letter, shown)) = controller
            .read(cx)
            .profiles()
            .iter()
            .find(|profile| profile.id == profile_id)
            .map(|profile| {
                (
                    profile.letter.clone(),
                    controller.read(cx).link(&profile.id).is_some(),
                )
            })
        else {
            return;
        };
        controller.update(cx, |controller, cx| {
            controller.save_profile_fields(profile_id, letter, !shown, cx)
        });
    }

    // The keyboard.

    /// `focus.workspaces`: the list, with its cursor on the active row.
    pub fn focus_list(&mut self, window: &mut GpuiWindow, cx: &mut Context<Self>) {
        let rows = self.rows(true, cx);
        self.list_cursor = rows
            .iter()
            .position(|row| row.active)
            .or((!rows.is_empty()).then_some(0));
        self.list_focus.focus(window, cx);
        cx.notify();
    }

    /// The list's keyboard cursor: where it was left, else the active row, so
    /// focus arriving by Tab starts where the user is.
    pub fn list_position(&self, cx: &App) -> Option<usize> {
        self.list_cursor
            .or_else(|| self.rows(true, cx).iter().position(|row| row.active))
    }

    /// The strip's keyboard cursor, else the active tab.
    pub fn strip_position(&self, cx: &App) -> Option<usize> {
        self.strip_cursor.or_else(|| {
            let view = self.view(cx);
            view.tabs
                .iter()
                .position(|tab| Some(&tab.key) == view.active_key.as_ref())
        })
    }

    /// `focus.tabs`: the strip, with its cursor on the active tab.
    pub fn focus_strip(&mut self, window: &mut GpuiWindow, cx: &mut Context<Self>) {
        let view = self.view(cx);
        self.strip_cursor = view
            .tabs
            .iter()
            .position(|tab| Some(&tab.key) == view.active_key.as_ref())
            .or((!view.tabs.is_empty()).then_some(0));
        self.strip_focus.focus(window, cx);
        cx.notify();
    }

    // Back and Forward.

    pub fn step_history(&mut self, direction: Direction, cx: &mut Context<Self>) {
        let view = self.view(cx);
        let Some(point) = self
            .history
            .step(direction, |point| point_exists(&view, point))
        else {
            self.report(
                match direction {
                    Direction::Back => "Nothing earlier to go back to.",
                    Direction::Forward => "Nothing later to go forward to.",
                },
                cx,
            );
            return;
        };
        self.navigate_to(point, &view, cx);
    }

    /// Where a closing document hands focus, if history knows somewhere.
    fn back_from_closing(&mut self, app_tab_id: &str, cx: &mut Context<Self>) -> bool {
        let view = self.view(cx);
        let Some(point) = self
            .history
            .back_from_closing(app_tab_id, |point| point_exists(&view, point))
        else {
            return false;
        };
        self.navigate_to(point, &view, cx);
        true
    }

    fn navigate_to(&mut self, point: FocusPoint, view: &ShellView, cx: &mut Context<Self>) {
        self.expected = Some(point.clone());
        let active_session = view.session.as_ref().map(|session| session.id.as_str());
        let active_window = view.window.as_ref().map(|window| window.id.as_str());
        if let Some(app_tab) = &point.app_tab_id {
            if Some(point.session_id.as_str()) != active_session {
                self.select_session(&point.session_id, cx);
            }
            self.select_app_tab(&point.session_id, Some(app_tab), cx);
        } else if Some(point.session_id.as_str()) != active_session {
            self.select_session(&point.session_id, cx);
        } else if let Some(window) = point
            .window_id
            .as_deref()
            .filter(|window| Some(*window) != active_window)
        {
            self.select_window(window, cx);
        } else if view.selected_app_tab.is_some() {
            self.select_app_tab(&point.session_id, None, cx);
        } else {
            self.expected = None;
        }
    }
}

/// Runs one move's writes. A two-step move stops after the first when a newer
/// move is waiting.
async fn fly(
    this: &WeakEntity<Workspaces>,
    id: u64,
    intent: &Intent,
    predecessor: Option<Outcome>,
    cx: &mut AsyncApp,
) -> Outcome {
    let Ok(Some((plan, start))) =
        this.read_with(cx, |this, cx| this.plan(intent, predecessor.as_ref(), cx))
    else {
        return Outcome::Unknown;
    };
    let server = intent.scope.server_identity.clone().unwrap_or_default();
    let target = &intent.destination;
    let mut generation = start.generation();
    let mut applied = match start {
        Outcome::Reached { location, .. } | Outcome::Partial { location, .. } => Some(location),
        Outcome::Unknown => None,
    };
    let stamp = |generation: Option<u64>| {
        generation.map(|generation| Precondition {
            server_identity: server.clone(),
            generation,
        })
    };
    // Pane and document moves pass through workspaces and windows on the way;
    // only a move to a workspace or window speaks.
    let moving = if intent.pane.is_some() || intent.app_tab.is_some() {
        Feedback::Silent
    } else {
        Feedback::Visible
    };
    let step = async |action: TmuxAction,
                      captured: Option<Precondition>,
                      feedback: Feedback,
                      cx: &mut AsyncApp| {
        let task = this.update(cx, |this, cx| {
            this.perform(&intent.host, action, captured, feedback, cx)
        });
        match task {
            Ok(task) => task.await.ok(),
            Err(_) => None,
        }
    };
    if plan.select_session {
        let action = TmuxAction::session(ActionKind::SelectSession, &target.session_id);
        let Some(result) = step(action, stamp(generation), moving, cx).await else {
            return Outcome::Unknown;
        };
        generation = Some(result.topology_generation);
        applied = Some(Location {
            session_id: target.session_id.clone(),
            window_id: None,
        });
        let current = this
            .read_with(cx, |this, _| this.coordinator.is_current(id))
            .unwrap_or(false);
        if !current && plan.select_window {
            return Outcome::Partial {
                location: applied.unwrap_or_else(|| target.clone()),
                generation: result.topology_generation,
            };
        }
    }
    if plan.select_window
        && let Some(window) = &target.window_id
    {
        let action = TmuxAction::window(ActionKind::SelectWindow, &target.session_id, window);
        let Some(result) = step(action, stamp(generation), moving, cx).await else {
            return Outcome::Unknown;
        };
        generation = Some(result.topology_generation);
        applied = Some(target.clone());
    }
    let current = this
        .read_with(cx, |this, _| this.coordinator.is_current(id))
        .unwrap_or(false);
    if let (Some(pane), Some(window), true) = (&intent.pane, &target.window_id, current) {
        let action = TmuxAction::pane(ActionKind::FocusPane, &target.session_id, window, pane);
        let Some(result) = step(action, stamp(generation), Feedback::Silent, cx).await else {
            return Outcome::Unknown;
        };
        generation = Some(result.topology_generation);
    }
    let generation = match generation {
        Some(generation) => generation,
        None => this
            .read_with(cx, |this, cx| {
                this.link(&intent.host, cx)
                    .map_or(0, |link| link.host_state.generation)
            })
            .unwrap_or(0),
    };
    if !current && applied.as_ref().is_some_and(|applied| applied != target) {
        return Outcome::Partial {
            location: applied.unwrap_or_else(|| target.clone()),
            generation,
        };
    }
    Outcome::Reached {
        location: target.clone(),
        generation,
    }
}

/// Whether a traversal landed where it meant to: a document by its id (the
/// window under it may have changed), a terminal by its window.
fn landed_at(expected: &FocusPoint, actual: &FocusPoint) -> bool {
    expected.session_id == actual.session_id
        && expected.app_tab_id == actual.app_tab_id
        && (expected.app_tab_id.is_some() || expected.window_id == actual.window_id)
}

fn point_exists(view: &ShellView, point: &FocusPoint) -> bool {
    view.snapshot
        .sessions
        .iter()
        .any(|session| session.id == point.session_id)
        && point
            .window_id
            .as_ref()
            .is_none_or(|id| view.snapshot.windows.iter().any(|window| window.id == *id))
        && point.app_tab_id.as_ref().is_none_or(|id| {
            app_tabs_for_workspace(
                &view.app_state,
                &view.host,
                view.server.as_deref(),
                &point.session_id,
            )
            .iter()
            .any(|tab| tab.id == *id)
        })
}

fn transport(connection: &ConnectionSpec) -> &'static str {
    match connection {
        ConnectionSpec::Local => "local",
        ConnectionSpec::Ssh { .. } => "ssh",
    }
}

/// Gives the keyboard to the terminal after a choice made in the sidebar,
/// the strip or the New workspace dialog.
pub fn focus_terminal(workspaces: &Entity<Workspaces>, window: &mut GpuiWindow, cx: &mut App) {
    if let Some(terminal) = workspaces
        .read(cx)
        .terminal
        .as_ref()
        .and_then(WeakEntity::upgrade)
    {
        terminal.update(cx, |terminal, cx| terminal.focus_terminal(window, cx));
    }
}

/// Remembers the host the New workspace dialog last picked.
fn remember_new_workspace_host(host: &str, cx: &App) {
    let store = cx.global::<Core>().app_state.clone();
    let host = host.to_owned();
    platform::spawn(async move {
        let saved = update_app_state(&store, move |state| {
            state.shell.new_workspace_host_profile_id = Some(host);
        })
        .await;
        if let Err(error) = saved {
            eprintln!("muxflow: could not save the new workspace host: {error}");
        }
    });
}

/// Moves a list cursor by a key, as the Tauri lists move focus: arrows step,
/// Home and End jump. `None` when the key is not one of those.
pub fn moved_cursor(cursor: Option<usize>, len: usize, key: &str, vertical: bool) -> Option<usize> {
    let last = len.checked_sub(1)?;
    let current = cursor.unwrap_or(0).min(last);
    let (back, forward) = if vertical {
        ("up", "down")
    } else {
        ("left", "right")
    };
    match key {
        "home" => Some(0),
        "end" => Some(last),
        key if key == back => Some(current.saturating_sub(1)),
        key if key == forward => Some((current + 1).min(last)),
        _ => None,
    }
}
