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
pub(crate) mod tests;

use std::{rc::Rc, sync::Arc};

use desktop_core::{
    app_state::{AgentSortMode, load_app_state},
    notifications::NativeNotifications,
    platform::AppPaths,
};
use gpui_kit::{App, AppContext, Entity, Global, base::Root};

use crate::{
    app::Core,
    commands::{self, RowSurface},
    connection_ui::{
        backend::TerminalBackend,
        connection_reducer::ConnectionPhase,
        controller::{ConnectionController, ConnectionEvent},
        helper_upgrade::{HelperOperation, HelperUpgradeState},
        host_links::HostLink,
    },
    terminal::integration::{
        agents::PaneActivity,
        routing::{PaneDestination, resolve_terminal_destination},
    },
};

use crate::workspaces::agents::{AgentPresence, AgentShell, RowAgent, WindowAgents};
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
        notifications: notifications.clone(),
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
    // Settings asks this notifier, the one whose clicks are routed, and offers
    // this runtime's setup.
    crate::settings::set_notifier(notifications.clone(), cx);
    publish_agent_setup(&runtime, cx);
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
    cx.observe(&runtime, |_, cx| {
        publish_shell(cx);
        publish_row_commands(cx);
    })
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
    // Read with the scopes, so each window set is the one its generation names.
    let windows = controller
        .links()
        .map(|link| {
            let ids = link.host_state.snapshot.windows.iter();
            (
                link.profile_id.clone(),
                ids.map(|window| window.id.clone()).collect(),
            )
        })
        .collect();
    RuntimeInputs {
        scopes,
        shown: controller
            .links()
            .map(|link| link.profile_id.clone())
            .collect(),
        focus,
        windows,
    }
}

pub(crate) fn sync(
    controller: &Entity<ConnectionController>,
    runtime: &Entity<AgentRuntime>,
    cx: &mut App,
) {
    let (inputs, label, allowed, helper_install, persistable) = {
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
            // A reset of unreadable saved state makes answers recordable again.
            controller.app_state_recovery().is_none(),
        )
    };
    runtime.update(cx, |runtime, cx| {
        runtime.setup.persistable = persistable;
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

/// Publishes "Set up agent status…" to Settings: its state, and the prompt.
fn publish_agent_setup(runtime: &Entity<AgentRuntime>, cx: &mut App) {
    crate::settings::set_agent_setup(
        crate::settings::AgentSetup {
            status: Rc::new({
                let runtime = runtime.downgrade();
                move |cx| {
                    runtime
                        .upgrade()
                        .map(|runtime| setup_status(runtime.read(cx)))
                        .unwrap_or_default()
                }
            }),
            set_up: Rc::new({
                let runtime = runtime.downgrade();
                move |_, cx| {
                    runtime
                        .update(cx, |runtime, cx| runtime.offer_setup(cx))
                        .ok();
                }
            }),
        },
        cx,
    );
}

/// What Settings says about "Set up agent status…" for the host on screen.
fn setup_status(runtime: &AgentRuntime) -> crate::settings::AgentSetupStatus {
    let active = &runtime.inputs.focus.host_profile_id;
    crate::settings::AgentSetupStatus {
        available: runtime.setup_offerable(),
        reports: wiring::host_hook_wiring(runtime.adapters(active)).reports,
        connected: runtime.active_scope().is_some(),
    }
}

/// How many agents a workspace row lists before it counts the rest.
const WORKSPACE_ROW_AGENT_LIMIT: usize = 3;

/// What the workspace rows and the tab strip show, rebuilt whole from the
/// runtime's records. The runtime keeps only each shown host's current
/// connection and server, so a reconnect leaves nothing stale behind here.
fn publish_shell(cx: &mut App) {
    let Some(runtime) = cx
        .try_global::<Agents>()
        .map(|agents| agents.runtime.clone())
    else {
        return;
    };
    let shell = agent_shell_from(runtime.read(cx));
    if cx.try_global::<AgentShell>() != Some(&shell) {
        cx.set_global(shell);
    }
}

/// A window with an agent is `Present`. One without is `Absent` only when the
/// host's agent snapshot was asked for against the topology on screen now and
/// covered that window, and no agent lacks a window to prove it is elsewhere;
/// otherwise it is left out, which the shell reads as `Unknown`.
pub(crate) fn agent_shell_from(runtime: &AgentRuntime) -> AgentShell {
    let mut shell = AgentShell::default();
    for host in &runtime.inputs.shown {
        let adapters = runtime.adapters(host);
        // Loudest first, so each row lists its loudest few.
        let agents = runtime.agents(host);
        let rollups = model::derive_rollups(agents.iter().copied());
        for agent in agents.iter().filter(|agent| !agent.session_id.is_empty()) {
            let state = model::display_state(agent);
            let row = shell
                .workspaces
                .entry((host.clone(), agent.session_id.clone()))
                .or_default();
            if row.agents.len() < WORKSPACE_ROW_AGENT_LIMIT {
                row.agents.push(RowAgent {
                    id: agent.id.clone(),
                    adapter_id: agent.adapter_id.clone(),
                    name: labels::agent_session_label(agent, adapters),
                    state,
                });
            } else {
                row.overflow += 1;
            }
            if list::needs_attention(state) {
                row.unread += 1;
            }
        }
        for ((_, session), row) in shell
            .workspaces
            .iter_mut()
            .filter(|((owner, _), _)| owner == host)
        {
            let rollup = rollups.by_workspace.get(session);
            row.working = rollup.is_some_and(|rollup| rollup.working > 0);
            row.state = rollup.and_then(|rollup| rollup.state);
        }
        for (window, rollup) in rollups.by_window {
            shell.windows.insert(
                (host.clone(), window),
                WindowAgents {
                    attention: rollup.state.zip(rollup.adapter_id),
                    presence: AgentPresence::Present,
                },
            );
        }
        if let Some((covered, generation)) = runtime.topology_authority(host) {
            shell.generations.insert(host.clone(), generation);
            let unmapped = agents.iter().any(|agent| {
                agent.session_id.is_empty()
                    || agent.window_id.is_empty()
                    || agent.pane_id.is_empty()
            });
            for window in covered.iter().filter(|_| !unmapped) {
                shell
                    .windows
                    .entry((host.clone(), window.clone()))
                    .or_insert(WindowAgents {
                        attention: None,
                        presence: AgentPresence::Absent,
                    });
            }
        }
    }
    shell
}

/// Every shown host's agents as list rows, and the subset the section lists
/// under the pinned-only filter. The bell and the jump read the whole list.
/// The workspace order, the host names and the filter are the sidebar's own.
pub fn rows(
    cx: &App,
    mode: AgentSortMode,
    pinned_only: bool,
) -> (Vec<AgentListRow>, Vec<AgentListRow>) {
    let Some(agents) = cx.try_global::<Agents>() else {
        return (Vec::new(), Vec::new());
    };
    let (controller, runtime) = (agents.controller.read(cx), agents.runtime.read(cx));
    let workspaces = crate::workspaces::current(cx);
    let workspaces = workspaces.as_ref().map(|workspaces| workspaces.read(cx));
    let order = workspaces.map_or_else(Vec::new, |workspaces| workspaces.rows(false, cx));
    let hosts = workspaces.map_or_else(Vec::new, |workspaces| workspaces.hosts(cx));
    // The host's letter is drawn only when more than one host is shown.
    let letters = hosts.iter().filter(|host| host.shown).count() > 1;
    let locate = |agent: &model::AgentRecord| {
        let window = controller.link(&agent.host_profile_id).and_then(|link| {
            link.host_state
                .snapshot
                .windows
                .iter()
                .find(|window| window.id == agent.window_id)
        });
        let position = order.iter().position(|row| {
            row.host_profile_id == agent.host_profile_id && row.session.id == agent.session_id
        });
        let host = hosts
            .iter()
            .find(|host| host.profile_id == agent.host_profile_id);
        AgentLocation {
            workspace_order: position.unwrap_or(usize::MAX),
            workspace_name: if agent.session_name.is_empty() {
                "unknown workspace".into()
            } else {
                agent.session_name.clone()
            },
            host_label: host
                .map_or_else(|| agent.host_profile_id.clone(), |host| host.label.clone()),
            host_letter: host
                .filter(|_| letters)
                .map(|host| host.letter.clone())
                .unwrap_or_default(),
            tab_index: window.map(|window| window.index),
            workspace_pinned: position.is_some_and(|index| order[index].pinned),
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
        // The workspaces the sidebar lists: pinned ones and the one on screen.
        let visible = workspaces.map_or_else(Vec::new, |workspaces| workspaces.rows(true, cx));
        all.iter()
            .filter(|row| {
                visible.iter().any(|workspace| {
                    workspace.host_profile_id == row.agent.host_profile_id
                        && workspace.session.id == row.agent.session_id
                })
            })
            .cloned()
            .collect()
    } else {
        all.clone()
    };
    (all, listed)
}

/// Every row, unfiltered: what the bell counts and the jump searches. They
/// rank by `compare_agents`, a total order, so the sort mode does not matter.
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
    let Err(reason) = open_pane(&agent.host_profile_id, &agent.pane_id, cx) else {
        return;
    };
    // On another host the click was also a move there: it still lands, on
    // the agent's workspace.
    let elsewhere = cx.try_global::<Agents>().is_some_and(|agents| {
        agents.controller.read(cx).active_profile_id() != agent.host_profile_id
    });
    let workspaces = crate::workspaces::current(cx);
    if let Some(workspaces) = workspaces.filter(|_| elsewhere && !agent.session_id.is_empty()) {
        report(
            cx,
            format!(
                "Agent {}'s pane is no longer available: {reason}; opening its workspace.",
                agent.display_name
            ),
        );
        workspaces.update(cx, |workspaces, cx| {
            workspaces.select_on_host(&agent.host_profile_id, &agent.session_id, None, cx)
        });
        return;
    }
    report(
        cx,
        format!(
            "Agent destination {} is no longer available: {reason}.",
            agent.display_name
        ),
    );
}

/// Puts `pane_id` on screen through the shell's navigation: its host made
/// active, its workspace and window selected, and the pane focused. Errors
/// with why the pane is not there.
pub fn open_pane(profile_id: &str, pane_id: &str, cx: &mut App) -> Result<(), String> {
    let Some(agents) = cx.try_global::<Agents>() else {
        return Err("agents are not running".into());
    };
    {
        let controller = agents.controller.read(cx);
        let link = controller
            .link(profile_id)
            .ok_or("its host is no longer shown")?;
        if let PaneDestination::Unavailable(reason) =
            resolve_terminal_destination(&link.host_state.snapshot.panes, pane_id)
        {
            return Err(reason);
        }
    }
    let workspaces = crate::workspaces::current(cx).ok_or("no window is open")?;
    workspaces.update(cx, |workspaces, cx| {
        workspaces.select_pane(profile_id, pane_id, cx)
    });
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
        if let Err(reason) = open_pane(&pending.profile_id, &pending.pane_id, cx) {
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
