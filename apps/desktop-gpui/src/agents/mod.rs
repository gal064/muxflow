//! Agents: what each agent on every shown host is doing, the attention it
//! wants, and the actions on it. Port of `features/agents` and the agent half
//! of `app/useAppAgentController.ts`.
//!
//! [`runtime::AgentRuntime`] holds the records, fed by [`start`] from the
//! connection controller: snapshot requests through the backend, pushed frames
//! through [`ConnectionEvent::AgentService`]. The sidebar draws
//! [`view::agents_list`]; [`view::AgentsSurface`] carries the dialogs.

pub mod activation;
pub mod labels;
pub mod list;
pub mod model;
pub mod notify;
pub mod runtime;
pub mod setup;
pub mod sound;
pub mod view;
pub mod wire;
pub mod wiring;

#[cfg(test)]
mod tests;

use std::{rc::Rc, sync::Arc};

use desktop_core::{
    app_state::{AgentSortMode, load_app_state},
    notifications::NativeNotifications,
    platform::AppPaths,
};
use gpui_kit::{App, AppContext, Entity, Global, base::Root};
use serde_json::json;

use crate::{
    app::Core,
    commands::{self, RowSurface},
    connection_ui::{
        backend::TerminalBackend,
        connection_reducer::ConnectionPhase,
        controller::{ConnectionController, ConnectionEvent},
        helper_upgrade::{HelperOperation, HelperUpgradeState},
        host_links::HostLink,
        host_profiles::host_letter,
    },
    terminal::integration::{
        agents::PaneActivity,
        routing::{PaneDestination, resolve_terminal_destination},
    },
};

use list::{AgentListRow, AgentLocation, build_agent_rows, jump_target, unread_count};
use model::EventScope;
use notify::AgentFocus;
use runtime::{AgentRuntime, AgentStatus, BackendTransport, NativeEffects, RuntimeInputs};
use wire::RequestScope;

/// The running runtime and the controller it follows. Absent until [`start`].
pub struct Agents {
    pub runtime: Entity<AgentRuntime>,
    pub controller: Entity<ConnectionController>,
}

impl Global for Agents {}

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    Root::register_plugin::<view::AgentsSurface>(cx, view::AgentsSurface::new);
    cx.set_global(PaneActivity(Rc::new(|profile_id, pane_id, cx| {
        let Some(runtime) = cx
            .try_global::<Agents>()
            .map(|agents| agents.runtime.clone())
        else {
            return;
        };
        let (profile_id, pane_id) = (profile_id.to_owned(), pane_id.to_owned());
        runtime.update(cx, |runtime, cx| {
            runtime.acknowledge_pane(&profile_id, &pane_id, cx)
        });
    })));
    commands::on_command("agents.jumpUnread", cx, |_, cx| jump_to_unread(cx));
}

/// Starts the runtime against the running controller. Called from
/// [`crate::connection_ui::start`], once the controller exists.
pub fn start(
    controller: Entity<ConnectionController>,
    backend: Arc<dyn TerminalBackend>,
    cx: &mut App,
) {
    let core = cx.global::<Core>();
    let app_state = core.app_state.clone();
    let data_dir = core.paths.app_data_dir().ok();
    let activations = activation::ActivationQueue::default();
    let notifications = NativeNotifications::new(Arc::new(activations.clone()), data_dir);
    let effects = Rc::new(NativeEffects {
        notifications,
        app_state: app_state.clone(),
    });
    let saved = load_app_state(&app_state);
    let runtime = cx.new(|_| {
        let mut runtime = AgentRuntime::new(Rc::new(BackendTransport(backend)), effects);
        runtime.setup.persistable = saved.is_ok();
        if let Ok(state) = &saved {
            runtime.setup.decisions = state.host_setup.clone();
        }
        runtime
    });
    cx.set_global(Agents {
        runtime: runtime.clone(),
        controller: controller.clone(),
    });

    cx.subscribe(&controller, {
        let runtime = runtime.clone();
        move |_, event, cx| {
            if let ConnectionEvent::AgentService(frame) = event {
                let scope = EventScope {
                    host_profile_id: frame.profile_id.clone(),
                    server_identity: frame.server_identity.clone(),
                    connection_epoch: frame.terminal_epoch,
                };
                runtime.update(cx, |runtime, cx| {
                    runtime.pushed(scope, &frame.scope, &frame.payload, cx)
                });
            }
        }
    })
    .detach();
    cx.observe(&controller, {
        let runtime = runtime.clone();
        move |controller, cx| sync(&controller, &runtime, cx)
    })
    .detach();
    cx.subscribe(&runtime, {
        let controller = controller.clone();
        move |_, AgentStatus(message), cx| {
            controller.update(cx, |controller, cx| {
                controller.report_status(message.clone(), cx)
            });
        }
    })
    .detach();
    cx.observe(&runtime, |_, cx| publish_row_commands(cx))
        .detach();
    activation::poll(activations, cx);
    sync(&controller, &runtime, cx);
}

/// The window on screen for a link: the remembered one, else tmux's own.
fn active_window_id(link: &HostLink) -> Option<(String, String)> {
    let snapshot = &link.host_state.snapshot;
    let session = link
        .active_session_id
        .clone()
        .or_else(|| snapshot.sessions.first().map(|session| session.id.clone()))?;
    let in_session = || {
        snapshot
            .windows
            .iter()
            .filter(|window| window.session_id == session)
    };
    let window = link
        .active_window_id
        .as_deref()
        .and_then(|id| in_session().find(|window| window.id == id))
        .or_else(|| in_session().find(|window| window.active))
        .or_else(|| in_session().next())?;
    Some((session, window.id.clone()))
}

/// What the runtime is told about the hosts.
pub fn inputs(controller: &ConnectionController) -> RuntimeInputs {
    let scopes = controller
        .links()
        .filter_map(|link| {
            let state = &link.host_state;
            let (Some(client_id), Some(server_identity)) =
                (&link.client_id, &state.server_identity)
            else {
                return None;
            };
            (state.can_mutate && link.terminal_epoch != 0).then(|| RequestScope {
                client_id: client_id.clone(),
                host_profile_id: link.profile_id.clone(),
                server_identity: server_identity.clone(),
                topology_generation: state.generation,
                connection_epoch: link.terminal_epoch,
            })
        })
        .collect();
    let mut focus = AgentFocus {
        host_profile_id: controller.active_profile_id(),
        // Read again when a transition is decided, as the app's focus is.
        terminal_visible: true,
        ..AgentFocus::default()
    };
    if let Some(link) = controller.active_link() {
        focus.server_identity = link.host_state.server_identity.clone().unwrap_or_default();
        if let Some((session, window)) = active_window_id(link) {
            focus.pane_id = link
                .host_state
                .snapshot
                .panes
                .iter()
                .find(|pane| pane.window_id == window && pane.active)
                .map(|pane| pane.id.clone())
                .unwrap_or_default();
            focus.session_id = session;
            focus.window_id = window;
        }
    }
    RuntimeInputs {
        scopes,
        shown: controller
            .links()
            .map(|link| link.profile_id.clone())
            .collect(),
        focus,
    }
}

fn sync(controller: &Entity<ConnectionController>, runtime: &Entity<AgentRuntime>, cx: &mut App) {
    let (inputs, label, allowed, helper_install) = {
        let controller = controller.read(cx);
        let helper = controller.helper();
        (
            inputs(controller),
            controller.active_host_label(),
            // Another host-level consent question owns the modal lane.
            !helper.owns_host_setup_lane() && !controller.app_recovery_modal_open(),
            matches!(
                helper,
                HelperUpgradeState::Upgrading {
                    operation: HelperOperation::Install,
                    ..
                }
            )
            .then(|| controller.active_profile_id()),
        )
    };
    runtime.update(cx, |runtime, cx| {
        runtime.setup.host_label = label;
        runtime.setup.allowed = allowed;
        // An install names agent status as part of what it sets up, so it
        // answers the agent question for that host once it reconnects.
        if helper_install.is_some() {
            runtime.setup.auto_setup = helper_install;
        }
        runtime.sync(inputs, cx);
    });
    activation::resume_pending(cx);
    navigate_pending(cx);
}

/// The host's letter, drawn only when more than one host is shown.
fn host_marks(controller: &ConnectionController) -> Vec<(String, String, String)> {
    let shown = controller.links().count();
    controller
        .links()
        .map(|link| {
            let profile = controller
                .profiles()
                .iter()
                .find(|profile| profile.id == link.profile_id);
            let label =
                profile.map_or_else(|| link.profile_id.clone(), |profile| profile.label.clone());
            let letter = match profile {
                Some(profile) if shown > 1 => host_letter(profile),
                _ => String::new(),
            };
            (link.profile_id.clone(), label, letter)
        })
        .collect()
}

/// The sidebar's workspace order across hosts, as `mergeHostRows`: every host's
/// pinned workspaces, then every host's unpinned ones, each in tmux's order.
fn workspace_order(controller: &ConnectionController) -> Vec<(String, String, bool)> {
    let mut order = Vec::new();
    for pinned in [true, false] {
        for link in controller.links() {
            let mut sessions: Vec<_> = link
                .host_state
                .snapshot
                .sessions
                .iter()
                .filter(|session| session.pinned == pinned)
                .collect();
            sessions.sort_by_key(|session| session.order);
            order.extend(
                sessions
                    .into_iter()
                    .map(|session| (link.profile_id.clone(), session.id.clone(), pinned)),
            );
        }
    }
    order
}

/// Every shown host's agents as list rows, and the subset the section lists
/// under the pinned-only filter. The bell and the jump read the whole list.
pub fn rows(
    cx: &App,
    mode: AgentSortMode,
    pinned_only: bool,
) -> (Vec<AgentListRow>, Vec<AgentListRow>) {
    let Some(agents) = cx.try_global::<Agents>() else {
        return (Vec::new(), Vec::new());
    };
    let (controller, runtime) = (agents.controller.read(cx), agents.runtime.read(cx));
    let marks = host_marks(controller);
    let order = workspace_order(controller);
    // The pinned-only filter keeps the workspace on screen, on the active host only.
    let on_screen = controller
        .active_link()
        .and_then(active_window_id)
        .map(|(session, _)| (controller.active_profile_id(), session));
    let visible: Vec<_> = order
        .iter()
        .filter(|(host, session, pinned)| {
            *pinned
                || on_screen
                    .as_ref()
                    .is_some_and(|(active, id)| active == host && id == session)
        })
        .collect();
    let locate = |agent: &model::AgentRecord| {
        let link = controller.link(&agent.host_profile_id);
        let snapshot = link.map(|link| &link.host_state.snapshot);
        let window = snapshot.and_then(|snapshot| {
            snapshot
                .windows
                .iter()
                .find(|window| window.id == agent.window_id)
        });
        let session = snapshot.and_then(|snapshot| {
            snapshot
                .sessions
                .iter()
                .find(|session| session.id == agent.session_id)
        });
        let (label, letter) = marks
            .iter()
            .find(|(id, ..)| *id == agent.host_profile_id)
            .map_or_else(
                || (agent.host_profile_id.clone(), String::new()),
                |(_, label, letter)| (label.clone(), letter.clone()),
            );
        AgentLocation {
            workspace_order: order
                .iter()
                .position(|(host, session, _)| {
                    *host == agent.host_profile_id && *session == agent.session_id
                })
                .unwrap_or(usize::MAX),
            workspace_name: if agent.session_name.is_empty() {
                "unknown workspace".into()
            } else {
                agent.session_name.clone()
            },
            host_label: label,
            host_letter: letter,
            tab_index: window.map(|window| window.index),
            workspace_pinned: session.is_some_and(|session| session.pinned),
            tab_pinned: window.is_some_and(|window| window.pinned),
        }
    };
    let routable = |agent: &model::AgentRecord| {
        !agent.pane_id.is_empty()
            && controller.link(&agent.host_profile_id).is_some_and(|link| {
                link.host_state
                    .snapshot
                    .panes
                    .iter()
                    .any(|pane| pane.id == agent.pane_id)
            })
    };
    let all = build_agent_rows(
        runtime.all_agents(),
        locate,
        routable,
        mode,
        runtime.now_ms(),
    );
    let listed = if pinned_only {
        all.iter()
            .filter(|row| {
                visible.iter().any(|(host, session, _)| {
                    *host == row.agent.host_profile_id && *session == row.agent.session_id
                })
            })
            .cloned()
            .collect()
    } else {
        all.clone()
    };
    (all, listed)
}

/// Every shown host's rows, unfiltered, in the saved order: what the bell
/// counts and the jump searches.
/// Every row, unfiltered. The bell and the jump rank by `compare_agents`,
/// a total order, so the list's sort mode does not matter here.
fn all_rows(cx: &App) -> Vec<AgentListRow> {
    rows(cx, AgentSortMode::Status, false).0
}

/// The title bar's bell: agents waiting on a human, and whether the jump
/// has one it can reach. A waiting agent whose pane is gone still counts.
pub fn bell(cx: &App) -> (usize, bool) {
    let all = all_rows(cx);
    (unread_count(&all), jump_target(&all).is_some())
}

/// The jump shortcut and the bell: the loudest reachable agent that wants you.
pub fn jump_to_unread(cx: &mut App) {
    let all = all_rows(cx);
    match jump_target(&all) {
        Some(row) => open_row(row, cx),
        None => report(cx, "No agent is waiting on you.".into()),
    }
}

pub(crate) fn report(cx: &mut App, message: String) {
    if let Some(controller) = cx
        .try_global::<Agents>()
        .map(|agents| agents.controller.clone())
    {
        controller.update(cx, |controller, cx| controller.report_status(message, cx));
    }
}

/// A row clicked, or the jump: the agent's exact pane, on its own host.
pub fn open_row(row: &AgentListRow, cx: &mut App) {
    let agent = &row.agent;
    if agent.pane_id.is_empty() {
        return report(
            cx,
            format!(
                "Agent {} has no exact pane match; navigation is unavailable.",
                agent.display_name
            ),
        );
    }
    let source = format!("Agent {}", agent.display_name);
    if let Err(reason) = open_pane(&agent.host_profile_id, &agent.pane_id, &source, cx) {
        report(
            cx,
            format!(
                "Agent destination {} is no longer available: {reason}.",
                agent.display_name
            ),
        );
    }
}

/// Puts `pane_id` on screen: its host made active, its window selected, and
/// tmux told to focus it. Errors with why the pane is not there.
pub fn open_pane(
    profile_id: &str,
    pane_id: &str,
    source: &str,
    cx: &mut App,
) -> Result<(), String> {
    let Some(agents) = cx.try_global::<Agents>() else {
        return Err("agents are not running".into());
    };
    let (controller, runtime) = (agents.controller.clone(), agents.runtime.clone());
    let (pane, client_id, server) = {
        let controller = controller.read(cx);
        let link = controller
            .link(profile_id)
            .ok_or("its host is no longer shown")?;
        let pane = match resolve_terminal_destination(&link.host_state.snapshot.panes, pane_id) {
            PaneDestination::Target(pane) => pane.clone(),
            PaneDestination::Unavailable(reason) => return Err(reason),
        };
        (
            pane,
            link.client_id.clone(),
            link.host_state.server_identity.clone(),
        )
    };
    controller.update(cx, |controller, cx| {
        controller.select_window(profile_id, &pane.session_id, &pane.window_id, cx);
    });
    let (Some(client_id), Some(server)) = (client_id, server) else {
        return Ok(());
    };
    let action = json!({
        "kind": "focusPane",
        "session_id": pane.session_id,
        "window_id": pane.window_id,
        "pane_id": pane.id,
        "expected_server_identity": server,
    });
    let answer = runtime.read(cx).transport().tmux_action(client_id, action);
    let source = source.to_owned();
    cx.spawn(async move |cx| {
        if let Err(error) = answer.await {
            cx.update(|cx| report(cx, format!("{source} could not be focused: {error}")));
        }
    })
    .detach();
    Ok(())
}

/// A pane the host said it opened but whose topology has not arrived yet.
pub struct PendingOpen {
    pub profile_id: String,
    pub pane_id: String,
    pub source: String,
    pub expected_by: u64,
}

/// How long a branch's pane is waited for before saying so.
pub const BRANCH_PANE_WAIT_MS: u64 = 5_000;

#[derive(Default)]
struct PendingNavigation(Option<PendingOpen>);

impl Global for PendingNavigation {}

pub fn open_when_present(pending: PendingOpen, cx: &mut App) {
    let wait = pending.expected_by;
    cx.set_global(PendingNavigation(Some(pending)));
    navigate_pending(cx);
    if cx.default_global::<PendingNavigation>().0.is_some() {
        let now = cx
            .try_global::<Agents>()
            .map_or(0, |agents| agents.runtime.read(cx).now_ms());
        let delay = std::time::Duration::from_millis(wait.saturating_sub(now));
        cx.spawn(async move |cx| {
            cx.background_executor().timer(delay).await;
            cx.update(navigate_pending);
        })
        .detach();
    }
}

fn navigate_pending(cx: &mut App) {
    let Some(pending) = cx.default_global::<PendingNavigation>().0.take() else {
        return;
    };
    let now = cx
        .try_global::<Agents>()
        .map_or(0, |agents| agents.runtime.read(cx).now_ms());
    let present = cx.try_global::<Agents>().is_some_and(|agents| {
        agents
            .controller
            .read(cx)
            .link(&pending.profile_id)
            .is_some_and(|link| {
                link.host_state.phase == ConnectionPhase::Connected
                    && link
                        .host_state
                        .snapshot
                        .panes
                        .iter()
                        .any(|pane| pane.id == pending.pane_id)
            })
    });
    if present {
        if let Err(reason) = open_pane(&pending.profile_id, &pending.pane_id, &pending.source, cx) {
            report(
                cx,
                format!(
                    "{}'s pane is no longer available: {reason}.",
                    pending.source
                ),
            );
        }
    } else if now >= pending.expected_by {
        report(
            cx,
            format!(
                "{} has not appeared. The agent may have no saved conversation yet.",
                pending.source
            ),
        );
    } else {
        cx.set_global(PendingNavigation(Some(pending)));
    }
}

/// The palette's agent row commands, for the row the active pane represents.
fn publish_row_commands(cx: &mut App) {
    let (rows, _) = rows(cx, AgentSortMode::Status, false);
    let selected = cx.try_global::<Agents>().and_then(|agents| {
        let focus = &agents.runtime.read(cx).inputs.focus;
        list::selected_agent_id_for_pane(&rows, Some(&focus.pane_id), Some(&focus.host_profile_id))
            .map(str::to_owned)
    });
    let source = selected
        .and_then(|id| rows.iter().find(|row| row.agent.id == id).cloned())
        .map(|row| view::row_command_source(row, cx));
    commands::publish_row_commands(RowSurface::Agents, source, cx);
}
