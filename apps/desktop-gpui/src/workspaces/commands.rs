//! The shell's commands and Quick Open source. Port of
//! `features/shell/useShellCommands.ts` for the commands this module owns, and
//! of `useBulkTabClose`.
//!
//! An ambient command (a key, the palette, the menu bar) acts on what is on
//! screen. A targeted one (a context menu, a close button) names its subject
//! and the host connection it was read from; a subject that is gone, or a host
//! that has reconnected since, resolves to nothing rather than to whatever is
//! selected behind it.

use std::{rc::Rc, sync::Arc, time::Duration};

use desktop_core::app_state::{AppTabRecord, DockSide};
use gpui_kit::{App, Context, Entity, Task, WeakEntity, Window};
use tmux_control::{Pane, Session, TmuxSnapshot, Window as TmuxWindow};

use crate::{
    commands::{
        self,
        dialogs::{
            Confirmation, TextPromptRequest, open_confirmation, open_text_prompt, tmux_confirmation,
        },
        quick_open::{
            FileSearchResults, QuickOpenItem, QuickOpenKind, QuickOpenSnapshot, QuickOpenSource,
            QuickOpenTab, QuickOpenWorkspace,
        },
        registry::{CommandContext, CommandId, command_by_id, selection_index},
    },
    connection_ui::host_scope::{HostScopeToken, same_host_connection},
};

use super::{
    Feedback, Workspaces,
    actions::{ActionKind, TmuxAction, close_precondition},
    agents::{AgentPresence, agent_shell},
    dialog,
    focus_history::Direction,
    model::{
        CombinedTab, TabKind, bulk_close_complete_status, bulk_close_outcome_status, close_app_tab,
        dock_app_tab, ordered_sessions, pin_app_tab, reorder_app_tab, selectable_tabs,
        strip_agent_status_glyphs, undock_app_tab,
    },
    navigation::{PaneDirection, adjacent_pane, relative_window_reorder_action},
};

/// The subject a context menu or a close button names.
#[derive(Clone, Debug)]
pub enum Target {
    Session(HostScopeToken, String),
    Window(HostScopeToken, String),
    AppTab(HostScopeToken, String),
    Pane(HostScopeToken, String),
}

impl Target {
    fn scope(&self) -> &HostScopeToken {
        match self {
            Self::Session(scope, _)
            | Self::Window(scope, _)
            | Self::AppTab(scope, _)
            | Self::Pane(scope, _) => scope,
        }
    }
}

/// The commands this module answers.
const SHELL_COMMANDS: &[CommandId] = &[
    "session.new",
    "session.rename",
    "session.moveLeft",
    "session.moveRight",
    "session.close",
    "tab.previous",
    "tab.next",
    "window.new",
    "window.rename",
    "window.moveLeft",
    "window.moveRight",
    "window.close",
    "pane.splitRight",
    "pane.splitDown",
    "pane.focusLeft",
    "pane.focusRight",
    "pane.focusUp",
    "pane.focusDown",
    "pane.resizeLeft",
    "pane.resizeRight",
    "pane.resizeUp",
    "pane.resizeDown",
    "pane.zoom",
    "pane.close",
    "focus.back",
    "focus.forward",
    "focus.workspaces",
    "focus.tabs",
    "view.toggleDock",
    "workspace.select1",
    "workspace.select2",
    "workspace.select3",
    "workspace.select4",
    "workspace.select5",
    "workspace.select6",
    "workspace.select7",
    "workspace.select8",
    "workspace.select9",
    "tab.select1",
    "tab.select2",
    "tab.select3",
    "tab.select4",
    "tab.select5",
    "tab.select6",
    "tab.select7",
    "tab.select8",
    "tab.select9",
];

/// How long a protected bulk close waits for agent status to catch up with
/// each close before it stops.
const PRESENCE_WAIT: Duration = Duration::from_secs(2);
const PRESENCE_POLL: Duration = Duration::from_millis(16);

pub(super) fn register(this: &WeakEntity<Workspaces>, cx: &mut App) {
    for id in SHELL_COMMANDS {
        let this = this.clone();
        commands::on_command(id, cx, move |window, cx| {
            if let Some(this) = this.upgrade() {
                run(&this, id, None, window, cx);
            }
        });
    }
    commands::set_quick_open_source(Rc::new(Source(this.clone())), cx);
}

/// The part of [`CommandContext`] the shell reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ContextFields {
    can_mutate: bool,
    can_create_workspace: bool,
    has_session: bool,
    has_window: bool,
    has_pane: bool,
    has_tab: bool,
    can_move_session_up: bool,
    can_move_session_down: bool,
    can_move_tab_left: bool,
    can_move_tab_right: bool,
    pinned_only: bool,
    can_toggle_dock: bool,
}

impl ContextFields {
    fn apply(&self, context: &mut CommandContext) {
        context.can_mutate = self.can_mutate;
        context.can_create_workspace = self.can_create_workspace;
        context.has_session = self.has_session;
        context.has_window = self.has_window;
        context.has_pane = self.has_pane;
        context.has_tab = self.has_tab;
        context.can_move_session_up = self.can_move_session_up;
        context.can_move_session_down = self.can_move_session_down;
        context.can_move_tab_left = self.can_move_tab_left;
        context.can_move_tab_right = self.can_move_tab_right;
        context.pinned_only = self.pinned_only;
        context.can_toggle_dock = self.can_toggle_dock;
    }
}

/// What a command acts on, read from one host.
struct Subject {
    host: String,
    scope: HostScopeToken,
    snapshot: Arc<TmuxSnapshot>,
    session: Option<Session>,
    window: Option<TmuxWindow>,
    /// The subject window's workspace, in tab order.
    windows: Vec<TmuxWindow>,
    app_tab: Option<AppTabRecord>,
    pane: Option<Pane>,
    ambient: bool,
}

impl Workspaces {
    pub(super) fn publish_context(&mut self, cx: &mut Context<Self>) {
        let view = self.view(cx);
        let selected = view.selected_app_tab.as_ref();
        let sessions = ordered_sessions(&view.snapshot.sessions);
        let position = view
            .session
            .as_ref()
            .and_then(|session| sessions.iter().position(|s| s.id == session.id));
        let windows: Vec<&TmuxWindow> = view.windows.iter().collect();
        let can_move_tab = |left: bool| match selected {
            Some(tab) => view
                .tab(&super::model::app_key(&tab.id))
                .is_some_and(|tab| {
                    if left {
                        tab.can_move_left
                    } else {
                        tab.can_move_right
                    }
                }),
            None => view.window.as_ref().is_some_and(|window| {
                relative_window_reorder_action(&windows, &window.id, left).is_some()
            }),
        };
        let fields = ContextFields {
            can_mutate: view.can_mutate,
            can_create_workspace: self
                .hosts(cx)
                .iter()
                .any(|host| host.shown && host.can_mutate),
            has_session: view.session.is_some(),
            has_window: view.window.is_some() && selected.is_none(),
            has_pane: view.active_pane.is_some() && selected.is_none(),
            has_tab: view.window.is_some() || selected.is_some(),
            can_move_session_up: position.is_some_and(|index| index > 0),
            can_move_session_down: position.is_some_and(|index| index + 1 < sessions.len()),
            can_move_tab_left: can_move_tab(true),
            can_move_tab_right: can_move_tab(false),
            pinned_only: self.pinned_only,
            can_toggle_dock: selected.is_some() || view.docked.is_some(),
        };
        if self.context.as_ref() != Some(&fields) {
            commands::update_context(cx, |context| fields.apply(context));
            self.context = Some(fields);
        }
    }

    fn subject(&self, target: Option<&Target>, cx: &App) -> Option<Subject> {
        let Some(target) = target else {
            let view = self.view(cx);
            return Some(Subject {
                host: view.host,
                scope: view.scope?,
                snapshot: view.snapshot,
                session: view.session,
                window: view.window,
                windows: view.windows,
                app_tab: view.selected_app_tab,
                pane: view.active_pane,
                ambient: true,
            });
        };
        let scope = target.scope();
        let link = self.link(&scope.host_profile_id, cx)?;
        if !same_host_connection(scope, &link.scope()) {
            return None;
        }
        let snapshot = link.host_state.snapshot.clone();
        let mut subject = Subject {
            host: link.profile_id.clone(),
            scope: link.scope(),
            snapshot: snapshot.clone(),
            session: None,
            window: None,
            windows: Vec::new(),
            app_tab: None,
            pane: None,
            ambient: false,
        };
        match target {
            Target::Session(_, id) => subject.session = link.host_state.session(id).cloned(),
            Target::Window(_, id) => {
                subject.window = snapshot
                    .windows
                    .iter()
                    .find(|window| window.id == *id)
                    .cloned();
                if let Some(window) = &subject.window {
                    subject.windows = super::session_windows(&snapshot.windows, &window.session_id)
                        .into_iter()
                        .cloned()
                        .collect();
                }
            }
            Target::AppTab(_, id) => {
                let state = self.controller.as_ref()?.read(cx).app_state().ok()?;
                subject.app_tab = state
                    .app_tabs
                    .iter()
                    .find(|tab| {
                        tab.id == *id
                            && tab.host_profile_id == link.profile_id
                            && Some(&tab.server_identity) == scope.server_identity.as_ref()
                    })
                    .cloned();
            }
            Target::Pane(_, id) => {
                subject.pane = snapshot.panes.iter().find(|pane| pane.id == *id).cloned()
            }
        }
        Some(subject)
    }

    /// Steps through the strip, wrapping; the docked document is never the
    /// active tab, so stepping onto it would leave the next step where it was.
    fn select_relative_tab(&mut self, step: isize, cx: &mut Context<Self>) {
        let view = self.view(cx);
        let docked = view
            .docked
            .as_ref()
            .map(|(tab, _)| super::model::app_key(&tab.id));
        let cycle: Vec<&CombinedTab> = selectable_tabs(&view.tabs)
            .into_iter()
            .filter(|tab| Some(&tab.key) != docked.as_ref())
            .collect();
        if cycle.is_empty() {
            return;
        }
        let len = cycle.len() as isize;
        let index = cycle
            .iter()
            .position(|tab| Some(&tab.key) == view.active_key.as_ref())
            .map_or(0, |index| (index as isize + step).rem_euclid(len));
        let key = cycle[index as usize].key.clone();
        self.select_tab(&key, cx);
    }

    fn select_tab_by_index(&mut self, index: usize, cx: &mut Context<Self>) {
        let view = self.view(cx);
        if let Some(key) = selectable_tabs(&view.tabs)
            .get(index)
            .map(|tab| tab.key.clone())
        {
            self.select_tab(&key, cx);
        }
    }

    fn select_workspace_by_index(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.rows(true, cx).get(index) {
            self.select_row(&row.clone(), cx);
        }
    }

    fn focus_direction(&mut self, direction: PaneDirection, cx: &mut Context<Self>) {
        let view = self.view(cx);
        let Some(active) = &view.active_pane else {
            return;
        };
        let panes: Vec<&Pane> = view.panes.iter().collect();
        if let Some(target) = adjacent_pane(&panes, active, direction) {
            let (host, pane) = (view.host.clone(), target.id.clone());
            self.select_pane(&host, &pane, cx);
        }
    }

    /// Docks the selected document beside the terminal, or returns the docked
    /// one to the strip.
    pub fn toggle_dock(&mut self, cx: &mut Context<Self>) {
        let view = self.view(cx);
        let (Some(session), Some(server)) = (view.session.clone(), view.server.clone()) else {
            return;
        };
        let side = view.dock_side();
        let (host, selected) = (view.host.clone(), view.selected_app_tab.map(|tab| tab.id));
        self.update_app_state(
            move |state| match &selected {
                Some(id) => dock_app_tab(state, &host, &server, &session, id, side),
                None => {
                    undock_app_tab(state, &host, &server, &session);
                    true
                }
            },
            cx,
        );
    }

    pub fn dock_tab(&mut self, tab_id: &str, side: Option<DockSide>, cx: &mut Context<Self>) {
        let view = self.view(cx);
        let (Some(session), Some(server)) = (view.session.clone(), view.server.clone()) else {
            return;
        };
        let (host, tab, side) = (
            view.host.clone(),
            tab_id.to_owned(),
            side.unwrap_or(view.dock_side()),
        );
        self.update_app_state(
            move |state| dock_app_tab(state, &host, &server, &session, &tab, side),
            cx,
        );
    }

    /// A preview tab stops being disposable once the user commits to it.
    pub fn pin_document(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        let (host, id) = (self.view(cx).host, tab_id.to_owned());
        self.update_app_state(move |state| pin_app_tab(state, &host, &id), cx);
    }

    pub fn undock(&mut self, cx: &mut Context<Self>) {
        let view = self.view(cx);
        let (Some(session), Some(server)) = (view.session.clone(), view.server.clone()) else {
            return;
        };
        let host = view.host.clone();
        self.update_app_state(
            move |state| {
                undock_app_tab(state, &host, &server, &session);
                true
            },
            cx,
        );
    }

    /// Closes one document. Closing the one on screen goes back to what was
    /// showing before it, else to a neighbouring document, else to the
    /// terminal; a bulk close and a docked layout skip both.
    pub fn close_document(&mut self, tab_id: &str, single: bool, cx: &mut Context<Self>) {
        let view = self.view(cx);
        let Some(tab) = view.app_tabs.iter().find(|tab| tab.id == tab_id).cloned() else {
            return;
        };
        let host = view.host.clone();
        let on_screen = view
            .selected_app_tab
            .as_ref()
            .is_some_and(|selected| selected.id == tab_id);
        if single && view.docked.is_none() && on_screen && !self.back_from_closing(tab_id, cx) {
            let neighbours: Vec<&AppTabRecord> = view
                .app_tabs
                .iter()
                .filter(|other| other.id != tab_id)
                .collect();
            let neighbour = neighbours
                .iter()
                .rev()
                .find(|other| other.order < tab.order)
                .or(neighbours.first());
            if let Some(neighbour) = neighbour {
                let neighbour = neighbour.id.clone();
                self.select_app_tab(&tab.session_id, Some(&neighbour), cx);
            }
        }
        let id = tab_id.to_owned();
        self.update_app_state(move |state| close_app_tab(state, &host, &id), cx);
    }

    /// Closes a set of tabs, read under `scope`. Terminals go first, each
    /// rechecked against agent status when agents are protected; the
    /// documents go only once every terminal did.
    pub fn close_tab_set(
        &mut self,
        scope: Option<HostScopeToken>,
        tabs: Vec<CombinedTab>,
        protect_agents: bool,
        cx: &mut Context<Self>,
    ) {
        if self.bulk_closing || tabs.is_empty() {
            return;
        }
        let Some(scope) = scope else {
            return;
        };
        let host = scope.host_profile_id.clone();
        // The ids in the set are this connection's; on another server the
        // same ids can name other windows.
        if !self.scope_current(&host, &scope, cx) {
            self.report(
                "Closing those tabs was cancelled because its host scope changed.",
                cx,
            );
            return;
        }
        let Some(server) = scope.server_identity.clone() else {
            self.report(
                "Closing those tabs was cancelled because the terminal layout changed.",
                cx,
            );
            return;
        };
        self.bulk_closing = true;
        cx.spawn(async move |this, cx| {
            let terminals: Vec<String> = tabs.iter().filter_map(|tab| tab.window_id().map(str::to_owned)).collect();
            let (mut closed, mut failed, mut skipped) = (0, 0, 0);
            let current = |cx: &mut gpui_kit::AsyncApp| {
                this.read_with(cx, |this, cx| this.scope_current(&host, &scope, cx)).unwrap_or(false)
            };
            let finish = |cx: &mut gpui_kit::AsyncApp| {
                this.update(cx, |this, _| this.bulk_closing = false).ok();
            };
            for (index, window_id) in terminals.iter().enumerate() {
                let found = this
                    .read_with(cx, |this, cx| {
                        let window = this
                            .link(&host, cx)?
                            .host_state
                            .snapshot
                            .windows
                            .iter()
                            .find(|window| window.id == *window_id)
                            .cloned()?;
                        let absent = agent_shell(cx)
                            .and_then(|agents| agents.window(&host, window_id))
                            .is_some_and(|agents| agents.presence == AgentPresence::Absent);
                        Some((window, absent))
                    })
                    .ok()
                    .flatten();
                let Some((window, absent)) = found else {
                    continue;
                };
                if protect_agents && !absent {
                    skipped += 1;
                    continue;
                }
                let action = TmuxAction {
                    confirmed: true,
                    ..TmuxAction::window(ActionKind::CloseWindow, &window.session_id, &window.id)
                };
                let Ok(task) = this.update(cx, |this, cx| {
                    this.perform(&host, action, Some(close_precondition(&server)), Feedback::Silent, cx)
                }) else {
                    return;
                };
                let result = task.await;
                if !current(cx) {
                    return finish(cx);
                }
                let Ok(result) = result else {
                    failed += 1;
                    continue;
                };
                closed += 1;
                if protect_agents && index + 1 < terminals.len() {
                    let started = std::time::Instant::now();
                    let caught_up = |cx: &mut gpui_kit::AsyncApp| {
                        cx.update(|cx| {
                            agent_shell(cx).is_some_and(|agents| {
                                agents.current_through(&host, result.topology_generation)
                            })
                        })
                    };
                    while current(cx) && !caught_up(cx) && started.elapsed() < PRESENCE_WAIT {
                        cx.background_executor().timer(PRESENCE_POLL).await;
                    }
                    if !current(cx) {
                        return finish(cx);
                    }
                    if !caught_up(cx) {
                        this.update(cx, |this, cx| {
                            this.report(
                                "Stopped closing tabs because current agent status was unavailable; remaining terminals were left open.",
                                cx,
                            );
                        })
                        .ok();
                        return finish(cx);
                    }
                }
            }
            this.update(cx, |this, cx| {
                if let Some(outcome) = bulk_close_outcome_status(closed, failed, skipped) {
                    this.report(outcome, cx);
                }
                if failed > 0 || skipped > 0 || !this.scope_current(&host, &scope, cx) {
                    return;
                }
                // By id on the set's host: the workspace on screen may be
                // another by now. Only what was still open is counted.
                let mut documents = 0;
                this.update_app_state(
                    |state| {
                        for tab in &tabs {
                            if let TabKind::App { tab_id, .. } = &tab.kind
                                && close_app_tab(state, &host, tab_id)
                            {
                                documents += 1;
                            }
                        }
                        documents > 0
                    },
                    cx,
                );
                if let Some(complete) = bulk_close_complete_status(closed + documents) {
                    this.report(complete, cx);
                }
            })
            .ok();
            finish(cx);
        })
        .detach();
    }
}

/// Runs one of the shell's commands, ambient or against `target`.
pub fn run(
    this: &Entity<Workspaces>,
    id: CommandId,
    target: Option<Target>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(definition) = command_by_id(id) else {
        return;
    };
    if let Some(index) = selection_index(id, "workspace.select") {
        this.update(cx, |this, cx| this.select_workspace_by_index(index - 1, cx));
        return;
    }
    if let Some(index) = selection_index(id, "tab.select") {
        this.update(cx, |this, cx| this.select_tab_by_index(index - 1, cx));
        return;
    }
    let direction = match id {
        "pane.focusLeft" => Some(PaneDirection::Left),
        "pane.focusRight" => Some(PaneDirection::Right),
        "pane.focusUp" => Some(PaneDirection::Up),
        "pane.focusDown" => Some(PaneDirection::Down),
        _ => None,
    };
    if let Some(direction) = direction {
        this.update(cx, |this, cx| this.focus_direction(direction, cx));
        return;
    }
    match id {
        "session.new" => return dialog::open(this, window, cx),
        "tab.previous" => return this.update(cx, |this, cx| this.select_relative_tab(-1, cx)),
        "tab.next" => return this.update(cx, |this, cx| this.select_relative_tab(1, cx)),
        "focus.back" => return this.update(cx, |this, cx| this.step_history(Direction::Back, cx)),
        "focus.forward" => {
            return this.update(cx, |this, cx| this.step_history(Direction::Forward, cx));
        }
        "focus.workspaces" => return this.update(cx, |this, cx| this.focus_list(window, cx)),
        "focus.tabs" => return this.update(cx, |this, cx| this.focus_strip(window, cx)),
        "view.toggleDock" => return this.update(cx, |this, cx| this.toggle_dock(cx)),
        _ => {}
    }
    let Some(subject) = this.read(cx).subject(target.as_ref(), cx) else {
        return;
    };
    if id == "window.close"
        && let Some(tab) = &subject.app_tab
    {
        // No notice: the tab leaving the strip is the whole message.
        let tab = tab.id.clone();
        this.update(cx, |this, cx| this.close_document(&tab, true, cx));
        return;
    }
    if definition.destructive {
        close(this, id, definition.title, subject, window, cx);
        return;
    }
    let host = subject.host.clone();
    let send = |this: &Entity<Workspaces>, action: TmuxAction, cx: &mut App| {
        this.update(cx, |this, cx| this.send(&host, action, None, cx));
    };
    match id {
        "session.rename" => {
            let Some(session) = subject.session else {
                return;
            };
            let (this, scope) = (this.downgrade(), subject.scope);
            open_text_prompt(
                TextPromptRequest {
                    title: "Rename workspace".into(),
                    label: "Workspace name".into(),
                    initial_value: session.name.clone(),
                    submit: Rc::new(move |name, _, cx| {
                        let action = TmuxAction {
                            name,
                            ..TmuxAction::session(ActionKind::RenameSession, &session.id)
                        };
                        rename(
                            &this,
                            &scope,
                            action,
                            "Workspace rename was cancelled because its host scope changed.",
                            cx,
                        );
                    }),
                },
                window,
                cx,
            );
        }
        "session.moveLeft" | "session.moveRight" => {
            let Some(session) = subject.session else {
                return;
            };
            let ordered = ordered_sessions(&subject.snapshot.sessions);
            let Some(current) = ordered.iter().position(|s| s.id == session.id) else {
                return;
            };
            let index = if id == "session.moveLeft" {
                current.checked_sub(1)
            } else {
                Some(current + 1)
            };
            if let Some(index) = index.filter(|index| *index < ordered.len()) {
                let action = TmuxAction {
                    index: index as u32,
                    ..TmuxAction::session(ActionKind::ReorderSession, &session.id)
                };
                send(this, action, cx);
            }
        }
        "window.new" => {
            if let Some(session) = subject.session {
                this.update(cx, |this, cx| this.create_window(&session.id, cx));
            }
        }
        "window.rename" => {
            let Some(target) = subject.window else {
                return;
            };
            let (this, scope) = (this.downgrade(), subject.scope);
            open_text_prompt(
                TextPromptRequest {
                    title: "Rename terminal tab".into(),
                    label: "Tab name".into(),
                    // As the tab shows it: a saved rename locks, so a ticker
                    // frame left in the field would be frozen into the name.
                    initial_value: strip_agent_status_glyphs(&target.name),
                    submit: Rc::new(move |name, _, cx| {
                        let action = TmuxAction {
                            name,
                            ..TmuxAction::window(
                                ActionKind::RenameWindow,
                                &target.session_id,
                                &target.id,
                            )
                        };
                        rename(
                            &this,
                            &scope,
                            action,
                            "Terminal-tab rename was cancelled because its host scope changed.",
                            cx,
                        );
                    }),
                },
                window,
                cx,
            );
        }
        "window.moveLeft" | "window.moveRight" => {
            let left = id == "window.moveLeft";
            if let Some(tab) = subject.app_tab {
                let Some(server) = subject.scope.server_identity.clone() else {
                    return;
                };
                this.update(cx, |this, cx| {
                    this.update_app_state(
                        move |state| {
                            reorder_app_tab(
                                state,
                                &host,
                                Some(&server),
                                &tab.session_id,
                                &tab.id,
                                left,
                            )
                        },
                        cx,
                    );
                });
                return;
            }
            let Some(target) = subject.window else {
                return;
            };
            let windows: Vec<&TmuxWindow> = subject.windows.iter().collect();
            if let Some(action) = relative_window_reorder_action(&windows, &target.id, left) {
                send(this, action, cx);
            }
        }
        "pane.splitRight" | "pane.splitDown" => {
            if let Some(pane) = subject.pane {
                let kind = if id == "pane.splitRight" {
                    ActionKind::SplitPaneRight
                } else {
                    ActionKind::SplitPaneDown
                };
                let action = TmuxAction {
                    split_size: 50,
                    ..TmuxAction::pane(kind, &pane.session_id, &pane.window_id, &pane.id)
                };
                send(this, action, cx);
            }
        }
        "pane.resizeLeft" | "pane.resizeRight" | "pane.resizeUp" | "pane.resizeDown" => {
            if let Some(pane) = subject.pane {
                let kind = match id {
                    "pane.resizeLeft" => ActionKind::ResizePaneLeft,
                    "pane.resizeRight" => ActionKind::ResizePaneRight,
                    "pane.resizeUp" => ActionKind::ResizePaneUp,
                    _ => ActionKind::ResizePaneDown,
                };
                let action = TmuxAction {
                    pane_id: pane.id,
                    resize_cells: 2,
                    ..TmuxAction::new(kind)
                };
                send(this, action, cx);
            }
        }
        "pane.zoom" => {
            if let Some(pane) = subject.pane {
                let zoomed = subject
                    .snapshot
                    .windows
                    .iter()
                    .find(|window| window.id == pane.window_id)
                    .is_some_and(|window| window.zoomed);
                let action = TmuxAction {
                    pane_id: pane.id,
                    window_id: pane.window_id,
                    zoomed: !zoomed,
                    ..TmuxAction::new(ActionKind::ZoomPane)
                };
                send(this, action, cx);
            }
        }
        _ => {}
    }
}

/// A rename's answer, against the host it was asked of.
fn rename(
    this: &WeakEntity<Workspaces>,
    scope: &HostScopeToken,
    action: TmuxAction,
    cancelled: &str,
    cx: &mut App,
) {
    this.update(cx, |this, cx| {
        if !this.scope_current(&scope.host_profile_id, scope, cx) {
            this.report(cancelled, cx);
            return;
        }
        this.send(&scope.host_profile_id, action, None, cx);
    })
    .ok();
}

/// The destructive commands. A terminal tab and a pane close on the spot:
/// their disappearance is the confirmation. A workspace takes every window in
/// it, so it asks. Either way the host gets `confirmed` and a close
/// precondition, stamped here.
fn close(
    this: &Entity<Workspaces>,
    id: CommandId,
    title: &str,
    subject: Subject,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(server) = subject.scope.server_identity.clone() else {
        return;
    };
    // The ambient Close closes the focused pane while a tab is split; the tab
    // menu's Close tab names the tab and stays whole-tab.
    let pane_first = id == "window.close"
        && subject.ambient
        && subject.pane.is_some()
        && subject.window.as_ref().is_some_and(|window| {
            subject
                .snapshot
                .panes
                .iter()
                .filter(|pane| pane.window_id == window.id)
                .count()
                > 1
        });
    let id = if pane_first { "pane.close" } else { id };
    let (action, confirm) = match (id, &subject.session, &subject.window, &subject.pane) {
        ("session.close", Some(session), _, _) => (
            TmuxAction::session(ActionKind::CloseSession, &session.id),
            Some(format!(
                "workspace “{}” and all of its windows",
                session.name
            )),
        ),
        ("window.close", _, Some(window), _) => (
            TmuxAction::window(ActionKind::CloseWindow, &window.session_id, &window.id),
            None,
        ),
        ("pane.close", _, _, Some(pane)) => (
            TmuxAction::pane(
                ActionKind::ClosePane,
                &pane.session_id,
                &pane.window_id,
                &pane.id,
            ),
            None,
        ),
        _ => return,
    };
    let action = TmuxAction {
        confirmed: true,
        ..action
    };
    let precondition = close_precondition(&server);
    let Some(label) = confirm else {
        this.update(cx, |this, cx| {
            this.send(&subject.host, action, Some(precondition), cx)
        });
        return;
    };
    let (heading, detail) = tmux_confirmation(title, &label);
    let (this, scope) = (this.downgrade(), subject.scope);
    open_confirmation(
        Confirmation {
            title: heading.into(),
            detail: detail.into(),
            confirm_label: "Close permanently".into(),
            destructive: true,
            on_confirm: Rc::new(move |_, cx| {
                let (action, precondition) = (action.clone(), precondition.clone());
                this.update(cx, |this, cx| {
                    // Against the host the row named, which need not be the
                    // one on screen by the time the dialog is answered.
                    if !this.scope_current(&scope.host_profile_id, &scope, cx) {
                        this.report(
                            format!(
                                "Closing {label} was cancelled because its host connection changed."
                            ),
                            cx,
                        );
                        return;
                    }
                    this.send(&scope.host_profile_id, action, Some(precondition), cx);
                })
                .ok();
            }),
        },
        window,
        cx,
    );
}

/// Quick Open's workspaces and tabs. Files arrive with the file features.
struct Source(WeakEntity<Workspaces>);

impl QuickOpenSource for Source {
    fn snapshot(&self, cx: &App) -> QuickOpenSnapshot {
        let Some(this) = self.0.upgrade() else {
            return QuickOpenSnapshot::default();
        };
        let this = this.read(cx);
        let view = this.view(cx);
        let workspaces = this
            .rows(false, cx)
            .into_iter()
            .map(|row| QuickOpenWorkspace {
                key: format!("workspace:{}", row.key),
                detail: [row.path.clone().unwrap_or_default(), row.host_label.clone()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · "),
                active: row.active,
                letter: (!row.letter.is_empty()).then(|| row.letter.clone()),
                name: row.session.name,
            })
            .collect();
        let tabs = selectable_tabs(&view.tabs)
            .into_iter()
            .map(|tab| {
                let file = match &tab.kind {
                    TabKind::App {
                        app_kind, resource, ..
                    } => {
                        use desktop_core::app_state::AppTabKind;
                        matches!(app_kind, AppTabKind::File | AppTabKind::Markdown)
                            .then(|| resource.clone())
                    }
                    _ => None,
                };
                QuickOpenTab {
                    key: tab.key.clone(),
                    title: tab.title.clone(),
                    is_file: file.is_some(),
                    file_path: match &tab.kind {
                        TabKind::App { resource, .. } => Some(resource.clone()),
                        _ => None,
                    },
                }
            })
            .collect();
        QuickOpenSnapshot {
            workspaces,
            tabs,
            active_tab_key: view.active_key.clone(),
            search_key: None,
            cwd: view
                .active_pane
                .as_ref()
                .map(|pane| pane.current_path.clone()),
            host_label: view.host_label,
        }
    }

    fn search_files(
        &self,
        _query: String,
        _cx: &mut App,
    ) -> Task<Result<FileSearchResults, String>> {
        Task::ready(Ok(FileSearchResults {
            matches: Vec::new(),
            complete: true,
        }))
    }

    fn choose(&self, item: &QuickOpenItem, _window: &mut Window, cx: &mut App) {
        let Some(this) = self.0.upgrade() else {
            return;
        };
        match &item.kind {
            QuickOpenKind::Workspace(workspace) => this.update(cx, |this, cx| {
                let key = workspace
                    .key
                    .strip_prefix("workspace:")
                    .unwrap_or(&workspace.key);
                if let Some(row) = this.rows(false, cx).into_iter().find(|row| row.key == key) {
                    this.select_row(&row, cx);
                }
            }),
            QuickOpenKind::Tab(tab) => this.update(cx, |this, cx| this.select_tab(&tab.key, cx)),
            QuickOpenKind::File(_) => {}
        }
    }
}
