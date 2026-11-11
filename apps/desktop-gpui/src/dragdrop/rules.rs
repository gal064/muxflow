//! What releasing a dragged tab, document or pane on a target would do. Port
//! of `features/dragDrop/dropRules.ts`.
//!
//! Pure, and the same answer the host would give: a move that would empty the
//! workspace it leaves is refused here so the drag never draws a marker for
//! it, and refused again on the host, which is where the guard holds.

use desktop_core::app_state::{AppTabRecord, DockSide};
use tmux_control::{Pane, Session, Window};

use crate::workspaces::actions::{ActionKind, PaneEdge, RelativePosition, TmuxAction};

pub const CLOSES_WORKSPACE: &str = "Moving a workspace's only tab would close the workspace.";
pub const ONE_PANE_TAB_ONLY: &str = "Only a tab with a single pane can become a split.";
pub const NO_DOCUMENT_SPLITS: &str =
    "Drop a file at the left or right edge to open it beside the terminal.";
pub const SAME_HOST_ONLY: &str = "Tabs and agents can only move within one host.";
pub const ALREADY_OPEN: &str = "That file is already open in the workspace.";

/// What is being dragged, and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DragPayload {
    TerminalTab {
        host: String,
        session_id: String,
        window_id: String,
        label: String,
    },
    AppTab {
        host: String,
        session_id: String,
        tab_id: String,
        label: String,
    },
    /// An agent row, or a pane grabbed with the link modifier.
    Pane {
        host: String,
        session_id: String,
        window_id: String,
        pane_id: String,
        label: String,
    },
}

impl DragPayload {
    pub fn host(&self) -> &str {
        match self {
            Self::TerminalTab { host, .. }
            | Self::AppTab { host, .. }
            | Self::Pane { host, .. } => host,
        }
    }

    /// What the ghost shows beside the pointer.
    pub fn label(&self) -> &str {
        match self {
            Self::TerminalTab { label, .. }
            | Self::AppTab { label, .. }
            | Self::Pane { label, .. } => label,
        }
    }

    pub fn is_pane(&self) -> bool {
        matches!(self, Self::Pane { .. })
    }

    pub fn is_app_tab(&self) -> bool {
        matches!(self, Self::AppTab { .. })
    }
}

/// A workspace, on one host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceRef {
    pub host: String,
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropTarget {
    /// `workspace` is the strip's own, for a pane dropped on a tab: it becomes
    /// a tab of that workspace.
    Tab {
        key: String,
        side: RelativePosition,
        workspace: Option<WorkspaceRef>,
    },
    Workspace(WorkspaceRef),
    Pane {
        pane_id: String,
        edge: PaneEdge,
    },
    /// An edge of the workspace, where a document docks beside the terminal.
    Dock(DockSide),
}

/// The active host's topology: every move stays on one tmux server.
pub struct DropContext<'a> {
    pub host: &'a str,
    pub sessions: &'a [Session],
    pub windows: &'a [Window],
    pub panes: &'a [Pane],
    /// This server's documents, so a move never opens one twice in a workspace.
    pub app_tabs: &'a [AppTabRecord],
    /// The document docked beside the terminal in the workspace on screen, and
    /// on which edge.
    pub dock: Option<(&'a str, DockSide)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DropPlan {
    /// A join needs no follow-up focus: the host leaves the window's active
    /// pane alone, unless the pane it moves is that active pane, which keeps
    /// it.
    Action(TmuxAction),
    MoveAppTab {
        tab_id: String,
        target_tab_id: String,
        side: RelativePosition,
    },
    MoveAppTabToWorkspace {
        tab_id: String,
        session: Session,
    },
    DockAppTab {
        tab_id: String,
        side: DockSide,
    },
    /// The docked document back among the tabs: anywhere on its own strip, and
    /// at the gap it was dropped on when that gap is beside another document.
    UndockAppTab {
        tab_id: String,
        place: Option<(String, RelativePosition)>,
    },
    Refuse(&'static str),
}

impl DropContext<'_> {
    fn windows_in(&self, session_id: &str) -> usize {
        self.windows
            .iter()
            .filter(|window| window.session_id == session_id)
            .count()
    }

    fn panes_in(&self, window_id: &str) -> Vec<&Pane> {
        self.panes
            .iter()
            .filter(|pane| pane.window_id == window_id)
            .collect()
    }

    fn session(&self, session_id: &str) -> Option<&Session> {
        self.sessions
            .iter()
            .find(|session| session.id == session_id)
    }

    fn pane(&self, pane_id: &str) -> Option<&Pane> {
        self.panes.iter().find(|pane| pane.id == pane_id)
    }
}

fn join(pane_id: &str, target_pane_id: &str, target: &DropTarget) -> Option<DropPlan> {
    let DropTarget::Pane { edge, .. } = target else {
        return None;
    };
    Some(DropPlan::Action(TmuxAction {
        pane_id: pane_id.to_owned(),
        target_pane_id: target_pane_id.to_owned(),
        pane_edge: Some(*edge),
        ..TmuxAction::new(ActionKind::JoinPane)
    }))
}

fn move_window(window: &Window, target_session_id: &str) -> DropPlan {
    DropPlan::Action(TmuxAction {
        target_session_id: target_session_id.to_owned(),
        ..TmuxAction::window(ActionKind::MoveWindow, &window.session_id, &window.id)
    })
}

fn terminal_tab_window_id(key: &str) -> Option<&str> {
    key.strip_prefix("terminal:")
}

fn app_tab_id(key: &str) -> Option<&str> {
    key.strip_prefix("app:")
}

/// What releasing `payload` on `dropped` would do, or `None` when it would do
/// nothing (dropped on itself, or onto a group it does not belong to).
pub fn plan_drop(
    payload: &DragPayload,
    dropped: &DropTarget,
    context: &DropContext,
) -> Option<DropPlan> {
    if payload.host() != context.host {
        return Some(DropPlan::Refuse(SAME_HOST_ONLY));
    }
    // A pane dropped on a tab is dropped on the strip: it becomes a tab of the
    // strip's workspace, appended, the same move as dropping it on that row.
    let target = match (payload, dropped) {
        (
            DragPayload::Pane { .. },
            DropTarget::Tab {
                workspace: Some(workspace),
                ..
            },
        ) => DropTarget::Workspace(workspace.clone()),
        _ => dropped.clone(),
    };
    if let DropTarget::Workspace(workspace) = &target
        && workspace.host != context.host
    {
        return Some(DropPlan::Refuse(SAME_HOST_ONLY));
    }
    let target_pane = match &target {
        DropTarget::Pane { pane_id, .. } => context.pane(pane_id),
        _ => None,
    };

    match payload {
        DragPayload::TerminalTab { window_id, .. } => {
            let window = context.windows.iter().find(|item| item.id == *window_id)?;
            match &target {
                DropTarget::Tab { key, side, .. } => {
                    let target_window_id = terminal_tab_window_id(key)?;
                    if target_window_id == window.id {
                        return None;
                    }
                    let target_window = context
                        .windows
                        .iter()
                        .find(|item| item.id == target_window_id)
                        .filter(|item| item.session_id == window.session_id)?;
                    // Pinned tabs are drawn first whatever their window order,
                    // so a drop across that boundary would land somewhere other
                    // than its marker.
                    if target_window.pinned != window.pinned {
                        return None;
                    }
                    Some(DropPlan::Action(TmuxAction {
                        target_window_id: target_window_id.to_owned(),
                        relative_position: Some(*side),
                        ..TmuxAction::window(
                            ActionKind::ReorderWindow,
                            &window.session_id,
                            &window.id,
                        )
                    }))
                }
                DropTarget::Workspace(workspace) => {
                    if workspace.session_id == window.session_id
                        || context.session(&workspace.session_id).is_none()
                    {
                        return None;
                    }
                    if context.windows_in(&window.session_id) == 1 {
                        return Some(DropPlan::Refuse(CLOSES_WORKSPACE));
                    }
                    Some(move_window(window, &workspace.session_id))
                }
                _ => {
                    let target_pane = target_pane.filter(|pane| pane.window_id != window.id)?;
                    let panes = context.panes_in(&window.id);
                    if panes.len() != 1 {
                        return Some(DropPlan::Refuse(ONE_PANE_TAB_ONLY));
                    }
                    if context.windows_in(&window.session_id) == 1 {
                        return Some(DropPlan::Refuse(CLOSES_WORKSPACE));
                    }
                    join(&panes[0].id, &target_pane.id, &target)
                }
            }
        }
        DragPayload::AppTab {
            session_id, tab_id, ..
        } => {
            let docked = context.dock.is_some_and(|(id, _)| id == tab_id);
            match &target {
                DropTarget::Dock(side) => {
                    if docked && context.dock.is_some_and(|(_, current)| current == *side) {
                        return None;
                    }
                    Some(DropPlan::DockAppTab {
                        tab_id: tab_id.clone(),
                        side: *side,
                    })
                }
                DropTarget::Tab { key, side, .. } => {
                    let target_tab_id = app_tab_id(key);
                    if docked {
                        return Some(DropPlan::UndockAppTab {
                            tab_id: tab_id.clone(),
                            place: target_tab_id
                                .filter(|target| target != tab_id)
                                .map(|target| (target.to_owned(), *side)),
                        });
                    }
                    let target_tab_id = target_tab_id.filter(|target| target != tab_id)?;
                    Some(DropPlan::MoveAppTab {
                        tab_id: tab_id.clone(),
                        target_tab_id: target_tab_id.to_owned(),
                        side: *side,
                    })
                }
                DropTarget::Workspace(workspace) => {
                    let destination = context.session(&workspace.session_id);
                    if docked && destination.is_some_and(|session| session.id == *session_id) {
                        return Some(DropPlan::UndockAppTab {
                            tab_id: tab_id.clone(),
                            place: None,
                        });
                    }
                    let destination = destination.filter(|session| session.id != *session_id)?;
                    let moving = context.app_tabs.iter().find(|tab| tab.id == *tab_id);
                    if moving.is_some_and(|moving| {
                        context.app_tabs.iter().any(|tab| {
                            tab.session_id == destination.id
                                && tab.kind == moving.kind
                                && tab.resource == moving.resource
                        })
                    }) {
                        return Some(DropPlan::Refuse(ALREADY_OPEN));
                    }
                    Some(DropPlan::MoveAppTabToWorkspace {
                        tab_id: tab_id.clone(),
                        session: destination.clone(),
                    })
                }
                DropTarget::Pane { .. } => {
                    target_pane.map(|_| DropPlan::Refuse(NO_DOCUMENT_SPLITS))
                }
            }
        }
        DragPayload::Pane { pane_id, .. } => {
            // The record can trail the topology; where the pane is *now* decides.
            let pane = context.pane(pane_id)?;
            let alone = context.panes_in(&pane.window_id).len() == 1;
            let empties_workspace = alone && context.windows_in(&pane.session_id) == 1;
            match &target {
                DropTarget::Pane { .. } => {
                    let target_pane = target_pane.filter(|target| target.id != pane.id)?;
                    // Inside its own window a join rearranges the panes and
                    // empties nothing.
                    if target_pane.window_id != pane.window_id && empties_workspace {
                        return Some(DropPlan::Refuse(CLOSES_WORKSPACE));
                    }
                    join(&pane.id, &target_pane.id, &target)
                }
                DropTarget::Workspace(workspace) => {
                    context.session(&workspace.session_id)?;
                    // Alone in its window, the agent already is a tab: move
                    // that tab.
                    if alone {
                        if workspace.session_id == pane.session_id {
                            return None;
                        }
                        if empties_workspace {
                            return Some(DropPlan::Refuse(CLOSES_WORKSPACE));
                        }
                        return Some(DropPlan::Action(TmuxAction {
                            target_session_id: workspace.session_id.clone(),
                            ..TmuxAction::window(
                                ActionKind::MoveWindow,
                                &pane.session_id,
                                &pane.window_id,
                            )
                        }));
                    }
                    // Sharing a window, it gets a tab of its own there, in its
                    // own workspace too, which is how a split becomes a tab.
                    Some(DropPlan::Action(TmuxAction {
                        pane_id: pane.id.clone(),
                        target_session_id: workspace.session_id.clone(),
                        ..TmuxAction::new(ActionKind::BreakPane)
                    }))
                }
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspaces::model::fixtures::{pane, session, tab, window};

    const LOCAL: &str = "local";

    /// `$1` holds `@1` (`%1`, `%2` split) and `@2` (`%3`); `$2` holds only `@3`
    /// (`%4`), so it is the workspace a move must not empty.
    struct Topology {
        sessions: Vec<Session>,
        windows: Vec<Window>,
        panes: Vec<Pane>,
        app_tabs: Vec<AppTabRecord>,
        dock: Option<(String, DockSide)>,
    }

    fn topology() -> Topology {
        Topology {
            sessions: vec![session("$1", "one", 0), session("$2", "two", 1)],
            windows: vec![
                window("@1", "$1", 0, "@1", false),
                window("@2", "$1", 1, "@2", false),
                window("@3", "$2", 0, "@3", false),
            ],
            panes: vec![
                pane("%1", "@1", "$1", (0, 0, 40, 24)),
                pane("%2", "@1", "$1", (40, 0, 40, 24)),
                pane("%3", "@2", "$1", (0, 0, 80, 24)),
                pane("%4", "@3", "$2", (0, 0, 80, 24)),
            ],
            app_tabs: vec![{
                let mut readme = tab("readme", "$1", 0);
                readme.resource = "/r/README.md".into();
                readme
            }],
            dock: None,
        }
    }

    fn plan(topology: &Topology, payload: &DragPayload, target: DropTarget) -> Option<DropPlan> {
        let context = DropContext {
            host: LOCAL,
            sessions: &topology.sessions,
            windows: &topology.windows,
            panes: &topology.panes,
            app_tabs: &topology.app_tabs,
            dock: topology
                .dock
                .as_ref()
                .map(|(id, side)| (id.as_str(), *side)),
        };
        plan_drop(payload, &target, &context)
    }

    fn terminal_tab(window_id: &str, session_id: &str) -> DragPayload {
        DragPayload::TerminalTab {
            host: LOCAL.into(),
            session_id: session_id.into(),
            window_id: window_id.into(),
            label: window_id.into(),
        }
    }

    fn agent(pane_id: &str) -> DragPayload {
        DragPayload::Pane {
            host: LOCAL.into(),
            session_id: String::new(),
            window_id: String::new(),
            pane_id: pane_id.into(),
            label: pane_id.into(),
        }
    }

    fn app_tab() -> DragPayload {
        DragPayload::AppTab {
            host: LOCAL.into(),
            session_id: "$1".into(),
            tab_id: "readme".into(),
            label: "README.md".into(),
        }
    }

    fn workspace(session_id: &str) -> DropTarget {
        DropTarget::Workspace(WorkspaceRef {
            host: LOCAL.into(),
            session_id: session_id.into(),
        })
    }

    fn tab_target(key: &str, side: RelativePosition) -> DropTarget {
        DropTarget::Tab {
            key: key.into(),
            side,
            workspace: None,
        }
    }

    fn strip(key: &str, session_id: &str) -> DropTarget {
        DropTarget::Tab {
            key: key.into(),
            side: RelativePosition::After,
            workspace: Some(WorkspaceRef {
                host: LOCAL.into(),
                session_id: session_id.into(),
            }),
        }
    }

    fn edge(pane_id: &str, edge: PaneEdge) -> DropTarget {
        DropTarget::Pane {
            pane_id: pane_id.into(),
            edge,
        }
    }

    fn action(plan: Option<DropPlan>) -> TmuxAction {
        match plan {
            Some(DropPlan::Action(action)) => action,
            other => panic!("expected an action, got {other:?}"),
        }
    }

    fn join_of(pane_id: &str, target: &str, edge: PaneEdge) -> DropPlan {
        DropPlan::Action(TmuxAction {
            pane_id: pane_id.into(),
            target_pane_id: target.into(),
            pane_edge: Some(edge),
            ..TmuxAction::new(ActionKind::JoinPane)
        })
    }

    fn break_of(pane_id: &str, session: &str) -> DropPlan {
        DropPlan::Action(TmuxAction {
            pane_id: pane_id.into(),
            target_session_id: session.into(),
            ..TmuxAction::new(ActionKind::BreakPane)
        })
    }

    fn move_of(session: &str, window: &str, target: &str) -> DropPlan {
        DropPlan::Action(TmuxAction {
            target_session_id: target.into(),
            ..TmuxAction::window(ActionKind::MoveWindow, session, window)
        })
    }

    #[test]
    fn reorders_a_terminal_tab_against_another_in_its_workspace() {
        let topology = topology();
        let reorder = action(plan(
            &topology,
            &terminal_tab("@2", "$1"),
            tab_target("terminal:@1", RelativePosition::Before),
        ));
        assert_eq!(reorder.kind, ActionKind::ReorderWindow);
        assert_eq!(
            (
                reorder.session_id.as_str(),
                reorder.window_id.as_str(),
                reorder.target_window_id.as_str(),
                reorder.relative_position
            ),
            ("$1", "@2", "@1", Some(RelativePosition::Before))
        );
        let after = |key: &str| {
            plan(
                &topology,
                &terminal_tab("@2", "$1"),
                tab_target(key, RelativePosition::After),
            )
        };
        assert_eq!(after("terminal:@2"), None);
        assert_eq!(after("app:readme"), None);
    }

    #[test]
    fn keeps_a_reorder_inside_the_pinned_group_or_outside_it() {
        let mut topology = topology();
        topology.windows[0].pinned = true;
        let target = |key: &str| tab_target(key, RelativePosition::Before);
        assert_eq!(
            plan(&topology, &terminal_tab("@2", "$1"), target("terminal:@1")),
            None
        );
        assert_eq!(
            plan(&topology, &terminal_tab("@1", "$1"), target("terminal:@2")),
            None
        );
    }

    #[test]
    fn moves_a_terminal_tab_to_another_workspace_unless_it_is_the_only_tab() {
        let topology = topology();
        assert_eq!(
            plan(&topology, &terminal_tab("@2", "$1"), workspace("$2")),
            Some(move_of("$1", "@2", "$2"))
        );
        assert_eq!(
            plan(&topology, &terminal_tab("@2", "$1"), workspace("$1")),
            None
        );
        assert_eq!(
            plan(&topology, &terminal_tab("@3", "$2"), workspace("$1")),
            Some(DropPlan::Refuse(CLOSES_WORKSPACE))
        );
    }

    #[test]
    fn turns_only_a_single_pane_terminal_tab_into_a_split() {
        let topology = topology();
        assert_eq!(
            plan(
                &topology,
                &terminal_tab("@2", "$1"),
                edge("%4", PaneEdge::Left)
            ),
            Some(join_of("%3", "%4", PaneEdge::Left))
        );
        assert_eq!(
            plan(
                &topology,
                &terminal_tab("@1", "$1"),
                edge("%4", PaneEdge::Left)
            ),
            Some(DropPlan::Refuse(ONE_PANE_TAB_ONLY))
        );
        assert_eq!(
            plan(
                &topology,
                &terminal_tab("@3", "$2"),
                edge("%1", PaneEdge::Top)
            ),
            Some(DropPlan::Refuse(CLOSES_WORKSPACE))
        );
        assert_eq!(
            plan(
                &topology,
                &terminal_tab("@1", "$1"),
                edge("%2", PaneEdge::Top)
            ),
            None
        );
    }

    #[test]
    fn keeps_document_tabs_local_and_out_of_splits() {
        let mut topology = topology();
        assert_eq!(
            plan(
                &topology,
                &app_tab(),
                tab_target("app:notes", RelativePosition::After)
            ),
            Some(DropPlan::MoveAppTab {
                tab_id: "readme".into(),
                target_tab_id: "notes".into(),
                side: RelativePosition::After
            })
        );
        assert_eq!(
            plan(
                &topology,
                &app_tab(),
                tab_target("terminal:@1", RelativePosition::After)
            ),
            None
        );
        assert_eq!(
            plan(&topology, &app_tab(), workspace("$2")),
            Some(DropPlan::MoveAppTabToWorkspace {
                tab_id: "readme".into(),
                session: topology.sessions[1].clone()
            })
        );
        assert_eq!(plan(&topology, &app_tab(), workspace("$1")), None);
        let mut copy = tab("readme-2", "$2", 0);
        copy.resource = "/r/README.md".into();
        topology.app_tabs.push(copy);
        assert_eq!(
            plan(&topology, &app_tab(), workspace("$2")),
            Some(DropPlan::Refuse(ALREADY_OPEN))
        );
        assert_eq!(
            plan(&topology, &app_tab(), edge("%1", PaneEdge::Left)),
            Some(DropPlan::Refuse(NO_DOCUMENT_SPLITS))
        );
        assert_eq!(
            plan(&topology, &app_tab(), edge("%9", PaneEdge::Left)),
            None
        );
    }

    #[test]
    fn docks_a_document_at_a_workspace_edge_and_a_drop_back_on_its_strip_undocks_it() {
        let mut topology = topology();
        assert_eq!(
            plan(&topology, &app_tab(), DropTarget::Dock(DockSide::Right)),
            Some(DropPlan::DockAppTab {
                tab_id: "readme".into(),
                side: DockSide::Right
            })
        );
        topology.dock = Some(("readme".into(), DockSide::Right));
        // Its own edge changes nothing; the other edge moves it there.
        assert_eq!(
            plan(&topology, &app_tab(), DropTarget::Dock(DockSide::Right)),
            None
        );
        assert_eq!(
            plan(&topology, &app_tab(), DropTarget::Dock(DockSide::Left)),
            Some(DropPlan::DockAppTab {
                tab_id: "readme".into(),
                side: DockSide::Left
            })
        );
        // Anywhere on its own strip undocks, at the gap when the gap is beside
        // a document.
        assert_eq!(
            plan(
                &topology,
                &app_tab(),
                tab_target("app:notes", RelativePosition::Before)
            ),
            Some(DropPlan::UndockAppTab {
                tab_id: "readme".into(),
                place: Some(("notes".into(), RelativePosition::Before))
            })
        );
        let plain = Some(DropPlan::UndockAppTab {
            tab_id: "readme".into(),
            place: None,
        });
        assert_eq!(
            plan(
                &topology,
                &app_tab(),
                tab_target("terminal:@1", RelativePosition::After)
            ),
            plain
        );
        assert_eq!(plan(&topology, &app_tab(), workspace("$1")), plain);
        // Another workspace is still a move, which forgets the dock on the way.
        assert!(matches!(
            plan(&topology, &app_tab(), workspace("$2")),
            Some(DropPlan::MoveAppTabToWorkspace { .. })
        ));
        // Terminal tabs and agents do not dock.
        assert_eq!(
            plan(
                &topology,
                &terminal_tab("@2", "$1"),
                DropTarget::Dock(DockSide::Left)
            ),
            None
        );
        assert_eq!(
            plan(&topology, &agent("%2"), DropTarget::Dock(DockSide::Left)),
            None
        );
    }

    #[test]
    fn joins_an_agents_pane_beside_another_windows_pane_judged_by_where_it_is_now() {
        let topology = topology();
        assert_eq!(
            plan(&topology, &agent("%2"), edge("%3", PaneEdge::Bottom)),
            Some(join_of("%2", "%3", PaneEdge::Bottom))
        );
        assert_eq!(
            plan(&topology, &agent("%4"), edge("%1", PaneEdge::Left)),
            Some(DropPlan::Refuse(CLOSES_WORKSPACE))
        );
        assert_eq!(
            plan(&topology, &agent("%9"), edge("%1", PaneEdge::Left)),
            None
        );
    }

    #[test]
    fn rearranges_a_pane_inside_its_own_window_but_never_onto_itself() {
        let topology = topology();
        assert_eq!(
            plan(&topology, &agent("%2"), edge("%1", PaneEdge::Left)),
            Some(join_of("%2", "%1", PaneEdge::Left))
        );
        assert_eq!(
            plan(&topology, &agent("%2"), edge("%2", PaneEdge::Left)),
            None
        );
    }

    #[test]
    fn makes_a_pane_dropped_on_the_tab_strip_a_tab_of_the_strips_workspace() {
        let topology = topology();
        // Sharing a window: broken out into a tab of its own.
        assert_eq!(
            plan(&topology, &agent("%2"), strip("terminal:@2", "$1")),
            Some(break_of("%2", "$1"))
        );
        // Already a tab of this workspace: nothing to do.
        assert_eq!(
            plan(&topology, &agent("%3"), strip("terminal:@1", "$1")),
            None
        );
        // A tab of another workspace: that tab moves here.
        assert_eq!(
            plan(&topology, &agent("%3"), strip("terminal:@3", "$2")),
            Some(move_of("$1", "@2", "$2"))
        );
        assert_eq!(
            plan(&topology, &agent("%4"), strip("terminal:@1", "$1")),
            Some(DropPlan::Refuse(CLOSES_WORKSPACE))
        );
        // A tab dropped on a tab is still a reorder, whatever strip it is in.
        let reorder = action(plan(
            &topology,
            &terminal_tab("@2", "$1"),
            strip("terminal:@1", "$1"),
        ));
        assert_eq!(reorder.kind, ActionKind::ReorderWindow);
        assert_eq!(reorder.relative_position, Some(RelativePosition::After));
        // Without a strip's workspace, a pane on a tab is no drop at all.
        assert_eq!(
            plan(
                &topology,
                &agent("%2"),
                tab_target("terminal:@2", RelativePosition::After)
            ),
            None
        );
    }

    #[test]
    fn moves_an_agent_alone_in_its_window_as_that_window_and_breaks_out_one_that_shares() {
        let topology = topology();
        assert_eq!(
            plan(&topology, &agent("%3"), workspace("$2")),
            Some(move_of("$1", "@2", "$2"))
        );
        assert_eq!(plan(&topology, &agent("%3"), workspace("$1")), None);
        assert_eq!(
            plan(&topology, &agent("%2"), workspace("$2")),
            Some(break_of("%2", "$2"))
        );
        // Its own workspace: the split becomes a tab there.
        assert_eq!(
            plan(&topology, &agent("%2"), workspace("$1")),
            Some(break_of("%2", "$1"))
        );
        assert_eq!(
            plan(&topology, &agent("%4"), workspace("$1")),
            Some(DropPlan::Refuse(CLOSES_WORKSPACE))
        );
        // A workspace the host no longer has is no drop.
        assert_eq!(plan(&topology, &agent("%2"), workspace("$9")), None);
    }

    #[test]
    fn never_moves_anything_between_hosts() {
        let topology = topology();
        let elsewhere = DropTarget::Workspace(WorkspaceRef {
            host: "remote".into(),
            session_id: "$2".into(),
        });
        assert_eq!(
            plan(&topology, &terminal_tab("@2", "$1"), elsewhere),
            Some(DropPlan::Refuse(SAME_HOST_ONLY))
        );
        let remote_agent = DragPayload::Pane {
            host: "remote".into(),
            session_id: String::new(),
            window_id: String::new(),
            pane_id: "%2".into(),
            label: String::new(),
        };
        assert_eq!(
            plan(&topology, &remote_agent, edge("%3", PaneEdge::Left)),
            Some(DropPlan::Refuse(SAME_HOST_ONLY))
        );
    }
}
