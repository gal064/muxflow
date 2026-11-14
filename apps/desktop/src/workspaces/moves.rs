//! Carrying out a drop, or a move chosen from an agent's menu. Port of
//! `app/usePaneMoves.ts`.
//!
//! Both ask [`dragdrop::rules::plan_drop`] against the host on screen, so a menu
//! item and a drop onto the same place always mean the same move. The plan is
//! made when the pointer is released, against the latest topology: a drag
//! outlives many snapshots.

use gpui_kit::{App, Context, WeakEntity};
use tmux_control::Pane;

use super::{
    Feedback, Workspaces,
    actions::{ActionKind, PaneEdge, TmuxAction},
    current,
    model::{
        dock_app_tab, move_app_tab_to, move_app_tab_to_workspace, ordered_sessions, undock_app_tab,
    },
};
use crate::{
    agents::labels::strip_agent_status_glyphs,
    dragdrop::rules::{DragPayload, DropContext, DropPlan, DropTarget, WorkspaceRef, plan_drop},
};

/// What releasing `payload` on `target` would do, against the shell now.
pub fn plan_for(payload: &DragPayload, target: &DropTarget, cx: &App) -> Option<DropPlan> {
    current(cx)?.read(cx).drop_plan(payload, target, cx)
}

/// Releases `payload` on `target`: does what the plan says, or says why not.
pub fn drop_on(payload: &DragPayload, target: &DropTarget, cx: &mut App) {
    let Some(workspaces) = current(cx) else {
        return;
    };
    workspaces.update(cx, |workspaces, cx| {
        let Some(plan) = workspaces.drop_plan(payload, target, cx) else {
            return;
        };
        workspaces.run_plan(plan, false, cx);
    });
}

/// The pane on screen, when `action` carries it somewhere else: its window
/// moved to another workspace, or it broke out into a tab of its own. A join is
/// not here: the host keeps focus on the pane it had inside the window.
pub fn pane_on_screen_moved_by<'a>(
    action: &TmuxAction,
    active: Option<&'a Pane>,
) -> Option<&'a Pane> {
    let active = active?;
    match action.kind {
        ActionKind::MoveWindow if action.window_id == active.window_id => Some(active),
        ActionKind::BreakPane if action.pane_id == active.id => Some(active),
        _ => None,
    }
}

/// The moves an agent's menu offers.
pub struct AgentMoves {
    /// Splits the agent's pane beside the pane on screen.
    pub bring_here: Option<DropPlan>,
    /// Workspaces the pane can go to, each as a tab of its own there.
    pub workspaces: Vec<(WorkspaceRef, String)>,
}

impl Workspaces {
    /// The drag payload of a tab in the strip. A terminal tab moves the host's
    /// window, so it drags only while the host can be changed.
    pub fn tab_payload(&self, key: &str, cx: &App) -> Option<DragPayload> {
        let view = self.view(cx);
        let session = view.session.as_ref()?;
        let tab = view.tab(key)?;
        let label = tab.title.clone();
        match &tab.kind {
            super::model::TabKind::Terminal { window_id, .. } => {
                view.can_mutate.then(|| DragPayload::TerminalTab {
                    host: view.host.clone(),
                    session_id: session.id.clone(),
                    window_id: window_id.clone(),
                    label,
                })
            }
            super::model::TabKind::App { tab_id, .. } => Some(DragPayload::AppTab {
                host: view.host.clone(),
                session_id: session.id.clone(),
                tab_id: tab_id.clone(),
                label,
            }),
            super::model::TabKind::Pending => None,
        }
    }

    /// A pane, dragged by its agent row or grabbed in the terminal. Only on the
    /// host on screen: every move is planned against its topology.
    pub fn pane_payload(
        &self,
        host: &str,
        pane_id: &str,
        label: Option<&str>,
        cx: &App,
    ) -> Option<DragPayload> {
        let view = self.view(cx);
        if !view.can_mutate || view.host != host {
            return None;
        }
        let pane = view.snapshot.panes.iter().find(|pane| pane.id == pane_id)?;
        let window = view
            .snapshot
            .windows
            .iter()
            .find(|window| window.id == pane.window_id);
        let label = match (label, window) {
            (Some(label), _) => label.to_owned(),
            (None, Some(window)) => strip_agent_status_glyphs(&window.name).to_owned(),
            (None, None) => pane.id.clone(),
        };
        Some(DragPayload::Pane {
            host: view.host.clone(),
            session_id: pane.session_id.clone(),
            window_id: pane.window_id.clone(),
            pane_id: pane.id.clone(),
            label,
        })
    }

    /// What `payload` dropped on `target` would do, against the host on screen.
    pub fn drop_plan(
        &self,
        payload: &DragPayload,
        target: &DropTarget,
        cx: &App,
    ) -> Option<DropPlan> {
        let view = self.view(cx);
        let app_tabs: Vec<_> = view
            .app_state
            .app_tabs
            .iter()
            .filter(|tab| {
                tab.host_profile_id == view.host
                    && Some(&tab.server_identity) == view.server.as_ref()
            })
            .cloned()
            .collect();
        let dock = view
            .docked
            .as_ref()
            .map(|(tab, side)| (tab.id.as_str(), *side));
        plan_drop(
            payload,
            target,
            &DropContext {
                host: &view.host,
                sessions: &view.snapshot.sessions,
                windows: &view.snapshot.windows,
                panes: &view.snapshot.panes,
                app_tabs: &app_tabs,
                dock,
            },
        )
    }

    /// Does what a plan says. `from_menu`: a menu hands focus back to the row it
    /// was opened from, so after a move that leaves the view alone the keyboard
    /// goes back to the pane on screen, where it was before the right-click. A
    /// drag restores focus itself.
    pub fn run_plan(&mut self, plan: DropPlan, from_menu: bool, cx: &mut Context<Self>) {
        let view = self.view(cx);
        match plan {
            DropPlan::Action(action) => {
                // Focus stays on what is on screen. A move of anything else
                // leaves it alone (a join never takes focus, and tmux re-picks
                // only the source window's or workspace's view), but a move of
                // the pane or tab on screen takes it along, to wherever the host
                // says it landed. A document covering the terminal is what is on
                // screen, and stays.
                let terminal_on_screen = view.selected_app_tab.is_none();
                let followed = terminal_on_screen
                    .then(|| pane_on_screen_moved_by(&action, view.active_pane.as_ref()))
                    .flatten()
                    .cloned();
                let from_session = view.session.as_ref().map(|session| session.id.clone());
                let host = view.host.clone();
                let task = self.perform(&host, action, None, Feedback::Visible, cx);
                cx.spawn(async move |this, cx| {
                    let result = task.await.ok();
                    this.update(cx, |this, cx| {
                        let now = this.view(cx);
                        // Somewhere else by the time it lands: the user has
                        // moved on.
                        if now.session.as_ref().map(|session| &session.id) != from_session.as_ref()
                        {
                            return;
                        }
                        match (followed, result) {
                            (Some(pane), Some(result))
                                if result.session_id.is_some() && result.window_id.is_some() =>
                            {
                                this.select_pane_at(
                                    &pane.id,
                                    result.session_id.as_deref().unwrap_or_default(),
                                    result.window_id.as_deref().unwrap_or_default(),
                                    cx,
                                );
                            }
                            _ if from_menu && now.selected_app_tab.is_none() => {
                                this.focus_active_terminal(cx);
                            }
                            _ => {}
                        }
                    })
                    .ok();
                })
                .detach();
            }
            DropPlan::MoveAppTab {
                tab_id,
                target_tab_id,
                side,
            } => {
                let Some(session) = view.session.clone() else {
                    return;
                };
                let (host, server) = (view.host.clone(), view.server.clone());
                self.update_app_state(
                    move |state| {
                        move_app_tab_to(
                            state,
                            &host,
                            server.as_deref(),
                            &session.id,
                            &tab_id,
                            &target_tab_id,
                            side == super::actions::RelativePosition::After,
                        )
                    },
                    cx,
                );
            }
            DropPlan::DockAppTab { tab_id, side } => {
                let (Some(session), Some(server)) = (view.session.clone(), view.server.clone())
                else {
                    return;
                };
                let host = view.host.clone();
                self.update_app_state(
                    move |state| dock_app_tab(state, &host, &server, &session, &tab_id, side),
                    cx,
                );
            }
            DropPlan::UndockAppTab { tab_id, place } => {
                let (Some(session), Some(server)) = (view.session.clone(), view.server.clone())
                else {
                    return;
                };
                let host = view.host.clone();
                self.update_app_state(
                    move |state| {
                        undock_app_tab(state, &host, &server, &session);
                        if let Some((target, side)) = &place {
                            move_app_tab_to(
                                state,
                                &host,
                                Some(&server),
                                &session.id,
                                &tab_id,
                                target,
                                *side == super::actions::RelativePosition::After,
                            );
                        }
                        true
                    },
                    cx,
                );
            }
            DropPlan::MoveAppTabToWorkspace { tab_id, session } => {
                let (host, server) = (view.host.clone(), view.server.clone());
                self.update_app_state(
                    move |state| {
                        move_app_tab_to_workspace(
                            state,
                            &host,
                            server.as_deref(),
                            &tab_id,
                            &session,
                        )
                    },
                    cx,
                );
            }
            DropPlan::Refuse(reason) => self.report(reason, cx),
        }
    }

    /// What an agent's menu offers to do with its pane, as plans to run.
    pub fn agent_moves(&self, host: &str, pane_id: &str, cx: &App) -> Option<AgentMoves> {
        let payload = self.pane_payload(host, pane_id, None, cx)?;
        let view = self.view(cx);
        let action_for = |target: DropTarget| {
            self.drop_plan(&payload, &target, cx)
                .filter(|plan| matches!(plan, DropPlan::Action(_)))
        };
        let pane = view.snapshot.panes.iter().find(|pane| pane.id == pane_id);
        let workspaces = ordered_sessions(&view.snapshot.sessions)
            .into_iter()
            .filter_map(|session| {
                let target = WorkspaceRef {
                    host: view.host.clone(),
                    session_id: session.id.clone(),
                };
                action_for(DropTarget::Workspace(target.clone()))?;
                // Its own workspace is offered only to split it out into a tab
                // there.
                let name = if pane.is_some_and(|pane| pane.session_id == session.id) {
                    format!("{} (as its own tab)", session.name)
                } else {
                    session.name.clone()
                };
                Some((target, name))
            })
            .collect();
        // From another tab only: an agent already beside you is already here.
        let bring_here = view
            .active_pane
            .as_ref()
            .filter(|active| {
                view.selected_app_tab.is_none()
                    && pane.is_some_and(|pane| pane.window_id != active.window_id)
            })
            .and_then(|active| {
                action_for(DropTarget::Pane {
                    pane_id: active.id.clone(),
                    edge: PaneEdge::Right,
                })
            });
        Some(AgentMoves {
            bring_here,
            workspaces,
        })
    }

    /// Moves `pane` to `workspace` from an agent's menu.
    pub fn move_agent_to_workspace(
        &mut self,
        host: &str,
        pane_id: &str,
        workspace: WorkspaceRef,
        cx: &mut Context<Self>,
    ) {
        let Some(payload) = self.pane_payload(host, pane_id, None, cx) else {
            return;
        };
        if let Some(plan @ DropPlan::Action(_)) =
            self.drop_plan(&payload, &DropTarget::Workspace(workspace), cx)
        {
            self.run_plan(plan, true, cx);
        }
    }

    /// Splits `pane` beside the pane on screen, from an agent's menu.
    pub fn bring_agent_here(&mut self, host: &str, pane_id: &str, cx: &mut Context<Self>) {
        let Some(moves) = self.agent_moves(host, pane_id, cx) else {
            return;
        };
        if let Some(plan) = moves.bring_here {
            self.run_plan(plan, true, cx);
        }
    }

    /// The keyboard back in the pane on screen.
    fn focus_active_terminal(&mut self, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminal.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        if let Some(window) = cx.active_window() {
            window
                .update(cx, |_, window, cx| {
                    terminal.update(cx, |terminal, cx| terminal.focus_terminal(window, cx));
                })
                .ok();
        }
    }
}
