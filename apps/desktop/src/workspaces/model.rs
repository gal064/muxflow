//! The shell's model of one workspace: its strip of terminal and document tabs,
//! the bulk closes over that strip, and the document-tab edits saved in
//! `app_state`. Port of `features/shell/model.ts`, less what belongs to files
//! and Git (Phase 5) and to app-tab recovery (`connection_ui::app_recovery`).
//!
//! Every edit takes the state by `&mut` and says whether it changed anything,
//! which is what [`ConnectionController::update_app_state`] wants.
//!
//! [`ConnectionController::update_app_state`]:
//! crate::connection_ui::controller::ConnectionController::update_app_state

use desktop_core::app_state::{
    AppTabKind, AppTabRecord, DockSide, PersistedAppState, WorkspaceUiRecord,
};
use tmux_control::{Session, Window};

use super::agents::AgentPresence;
use crate::agents::model::DisplayState;

/// The performer's acknowledgement of an action the host took. Routine:
/// [`crate::notices`] keeps it off the screen.
pub const WAITING_FOR_HOST: &str = "Waiting for authoritative tmux state…";
pub const UNAVAILABLE_UNTIL_LIVE: &str =
    "This action is unavailable until the authoritative connection is live.";

#[derive(Clone, Debug, PartialEq)]
pub enum TabKind {
    Terminal {
        window_id: String,
        zoomed: bool,
        /// The host's flag; a document has no window to pin.
        pinned: bool,
        attention: Option<DisplayState>,
        presence: AgentPresence,
    },
    App {
        tab_id: String,
        app_kind: AppTabKind,
        resource: String,
        preview: bool,
    },
    /// A create still in flight. See [`PendingTab`].
    Pending,
}

/// One tab in the strip, in the strip's own order.
#[derive(Clone, Debug, PartialEq)]
pub struct CombinedTab {
    pub key: String,
    pub title: String,
    /// In tmux window order or document order, not the pinned display order:
    /// a move changes the window index or the document order.
    pub can_move_left: bool,
    pub can_move_right: bool,
    pub kind: TabKind,
}

impl CombinedTab {
    pub fn is_pending(&self) -> bool {
        matches!(self.kind, TabKind::Pending)
    }

    pub fn window_id(&self) -> Option<&str> {
        match &self.kind {
            TabKind::Terminal { window_id, .. } => Some(window_id),
            _ => None,
        }
    }

    pub fn app_tab_id(&self) -> Option<&str> {
        match &self.kind {
            TabKind::App { tab_id, .. } => Some(tab_id),
            _ => None,
        }
    }
}

pub fn terminal_key(window_id: &str) -> String {
    format!("terminal:{window_id}")
}

pub fn app_key(tab_id: &str) -> String {
    format!("app:{tab_id}")
}

/// A tab asked for that the host has not created yet. Creating is a round
/// trip, and a click with no visible effect reads as a dropped click. It is a
/// picture of the request, never reconciled into the real tab: it retires once
/// a snapshot names its window, or when the request fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingTab {
    pub key: u64,
    /// Unset until a new workspace's ack names it: there is no strip to draw it
    /// in before that.
    pub session_id: Option<String>,
    /// Set by the ack. The placeholder outlives the ack, which is not the
    /// snapshot, so the strip does not blink empty in between.
    pub window_id: Option<String>,
    pub title: String,
}

fn pending_still_open(pending: &PendingTab, windows: &[&Window]) -> bool {
    pending
        .window_id
        .as_ref()
        .is_none_or(|id| !windows.iter().any(|window| &window.id == id))
}

/// The placeholder's one-way retirement. Fed back in, it is a latch: once a
/// snapshot names the window it is gone, and closing that window later does
/// not bring back a placeholder with nothing behind it.
pub fn retire_pending_tab(pending: Option<PendingTab>, windows: &[&Window]) -> Option<PendingTab> {
    pending.filter(|pending| pending_still_open(pending, windows))
}

/// The one ordering rule every pinned surface reads: pinned items lead, and
/// both halves keep their order.
pub fn pinned_first<T>(items: Vec<T>, pinned: impl Fn(&T) -> bool) -> Vec<T> {
    let (mut leading, rest): (Vec<T>, Vec<T>) = items.into_iter().partition(|item| pinned(item));
    leading.extend(rest);
    leading
}

/// Sessions in presentation order: the host's order, then name, then id.
pub fn ordered_sessions(sessions: &[Session]) -> Vec<&Session> {
    let mut ordered: Vec<&Session> = sessions.iter().collect();
    ordered.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    ordered
}

/// A workspace's document tabs, in their order. None without a server
/// identity: a tab belongs to one tmux server.
pub fn app_tabs_for_workspace<'a>(
    state: &'a PersistedAppState,
    host: &str,
    server: Option<&str>,
    session_id: &str,
) -> Vec<&'a AppTabRecord> {
    let Some(server) = server else {
        return Vec::new();
    };
    let mut tabs: Vec<&AppTabRecord> = state
        .app_tabs
        .iter()
        .filter(|tab| {
            tab.host_profile_id == host
                && tab.server_identity == server
                && tab.session_id == session_id
        })
        .collect();
    tabs.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.id.cmp(&right.id))
    });
    tabs
}

/// What the strip knows about the agents in one window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowAgentInfo {
    pub attention: Option<DisplayState>,
    pub presence: AgentPresence,
}

/// The strip: pinned terminal tabs, the other terminal tabs in window order,
/// the documents, and the placeholder last, so it never pushes the tabs a
/// waiting pointer is aimed at.
pub fn combine_workspace_tabs(
    windows: &[&Window],
    app_tabs: &[&AppTabRecord],
    agents: impl Fn(&str) -> WindowAgentInfo,
    pending: Option<&PendingTab>,
) -> Vec<CombinedTab> {
    let mut ordered_windows = windows.to_vec();
    ordered_windows.sort_by(|left, right| {
        left.index
            .cmp(&right.index)
            .then_with(|| left.id.cmp(&right.id))
    });
    let window_count = ordered_windows.len();
    let terminals = ordered_windows.iter().enumerate().map(|(index, window)| {
        let info = agents(&window.id);
        CombinedTab {
            key: terminal_key(&window.id),
            // Independent of agent authority: animated titles move the topology
            // before the agent snapshot does, and tying the two together made
            // the title flicker between the glyph and the stripped name.
            title: crate::agents::labels::strip_agent_status_glyphs(&window.name).to_owned(),
            can_move_left: index > 0,
            can_move_right: index + 1 < window_count,
            kind: TabKind::Terminal {
                window_id: window.id.clone(),
                zoomed: window.zoomed,
                pinned: window.pinned,
                attention: info.attention,
                presence: info.presence,
            },
        }
    });
    let mut ordered_tabs = app_tabs.to_vec();
    ordered_tabs.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.id.cmp(&right.id))
    });
    let tab_count = ordered_tabs.len();
    let documents = ordered_tabs
        .iter()
        .enumerate()
        .map(|(index, tab)| CombinedTab {
            key: app_key(&tab.id),
            title: tab.title.clone(),
            can_move_left: index > 0,
            can_move_right: index + 1 < tab_count,
            kind: TabKind::App {
                tab_id: tab.id.clone(),
                app_kind: tab.kind,
                resource: tab.resource.clone(),
                preview: tab.preview == Some(true),
            },
        });
    let mut strip = pinned_first(terminals.chain(documents).collect(), |tab| {
        matches!(tab.kind, TabKind::Terminal { pinned: true, .. })
    });
    if let Some(pending) = pending.filter(|pending| pending_still_open(pending, windows)) {
        strip.push(CombinedTab {
            key: format!("pending:{}", pending.key),
            title: pending.title.clone(),
            can_move_left: false,
            can_move_right: false,
            kind: TabKind::Pending,
        });
    }
    strip
}

/// The tabs Ctrl+1…9 address and the strip numbers: everything but the
/// placeholder.
pub fn selectable_tabs(tabs: &[CombinedTab]) -> Vec<&CombinedTab> {
    tabs.iter().filter(|tab| !tab.is_pending()).collect()
}

/// Everything but the anchor, in the strip's order. An anchor the strip no
/// longer holds closes nothing, rather than everything.
pub fn tabs_to_close_others(tabs: &[CombinedTab], anchor: &str) -> Vec<CombinedTab> {
    if !tabs.iter().any(|tab| tab.key == anchor) {
        return Vec::new();
    }
    tabs.iter()
        .filter(|tab| !tab.is_pending() && tab.key != anchor)
        .cloned()
        .collect()
}

/// Everything after the anchor in the strip's own order.
pub fn tabs_to_close_right(tabs: &[CombinedTab], anchor: &str) -> Vec<CombinedTab> {
    let Some(position) = tabs.iter().position(|tab| tab.key == anchor) else {
        return Vec::new();
    };
    tabs[position + 1..]
        .iter()
        .filter(|tab| !tab.is_pending())
        .cloned()
        .collect()
}

/// Documents, and terminal tabs known to hold no agent. A terminal whose agents
/// are not known is kept: idle and unknown agents are protected as much as
/// working ones.
pub fn tabs_to_close_non_agent(tabs: &[CombinedTab]) -> Vec<CombinedTab> {
    tabs.iter()
        .filter(|tab| match &tab.kind {
            TabKind::App { .. } => true,
            TabKind::Terminal { presence, .. } => *presence == AgentPresence::Absent,
            TabKind::Pending => false,
        })
        .cloned()
        .collect()
}

/// A set holding a tmux window needs the write permission a single close does.
pub fn takes_terminals(targets: &[CombinedTab]) -> bool {
    targets
        .iter()
        .any(|tab| matches!(tab.kind, TabKind::Terminal { .. }))
}

/// The one rule that greys out a bulk close, for the toolbar and the menu alike.
pub fn bulk_close_disabled(targets: &[CombinedTab], can_mutate: bool) -> bool {
    targets.is_empty() || (takes_terminals(targets) && !can_mutate)
}

/// What a bulk close says when it did not close everything, as one counted
/// sentence. `None` when every tab closed.
pub fn bulk_close_outcome_status(closed: usize, failed: usize, skipped: usize) -> Option<String> {
    if failed == 0 && skipped == 0 {
        return None;
    }
    let attempted = closed + failed + skipped;
    let mut survivors = Vec::new();
    if failed > 0 {
        survivors.push(format!("{failed} could not be closed"));
    }
    if skipped > 0 {
        let verb = if skipped == 1 { "was" } else { "were" };
        survivors.push(format!("{skipped} still had an agent and {verb} left open"));
    }
    let noun = if attempted == 1 { "tab" } else { "tabs" };
    Some(format!(
        "Closed {closed} of {attempted} {noun}; {}.",
        survivors.join("; ")
    ))
}

/// The receipt for a close that asked nothing first. `None` when nothing closed.
pub fn bulk_close_complete_status(closed: usize) -> Option<String> {
    (closed > 0).then(|| {
        format!(
            "Closed {closed} {}.",
            if closed == 1 { "tab" } else { "tabs" }
        )
    })
}

pub fn workspace_ui_record<'a>(
    state: &'a PersistedAppState,
    host: &str,
    server: Option<&str>,
    session_id: &str,
) -> Option<&'a WorkspaceUiRecord> {
    let server = server?;
    state.workspace_ui.iter().find(|record| {
        record.host_profile_id == host
            && record.server_identity == server
            && record.session_id == session_id
    })
}

/// Rewrites one workspace's record, creating it when it has none yet.
fn update_workspace_ui(
    state: &mut PersistedAppState,
    host: &str,
    server: &str,
    session: &Session,
    update: impl FnOnce(&mut WorkspaceUiRecord),
) {
    let index = state.workspace_ui.iter().position(|record| {
        record.host_profile_id == host
            && record.server_identity == server
            && record.session_id == session.id
    });
    let index = index.unwrap_or_else(|| {
        state.workspace_ui.push(WorkspaceUiRecord {
            host_profile_id: host.to_owned(),
            server_identity: server.to_owned(),
            session_id: session.id.clone(),
            session_name: session.name.clone(),
            selected_app_tab_id: None,
            docked_app_tab_id: None,
            dock_side: None,
        });
        state.workspace_ui.len() - 1
    });
    let record = &mut state.workspace_ui[index];
    record.session_name = session.name.clone();
    update(record);
}

/// Puts a document on screen, or the terminal back with `None`. While a
/// document is docked, the next one replaces it in the side column instead of
/// covering the terminal; asking for the docked one changes nothing.
pub fn select_app_tab(
    state: &mut PersistedAppState,
    host: &str,
    server: &str,
    session: &Session,
    tab_id: Option<&str>,
) {
    update_workspace_ui(state, host, server, session, |record| {
        match (tab_id, record.docked_app_tab_id.as_deref()) {
            (Some(id), Some(docked)) if id == docked => {}
            (Some(id), Some(_)) => {
                record.selected_app_tab_id = None;
                record.docked_app_tab_id = Some(id.to_owned());
            }
            (id, _) => record.selected_app_tab_id = id.map(str::to_owned),
        }
    });
}

/// Shows one document beside the terminal. Docking another returns the previous
/// one to the strip, docking the selected one gives the main column back to the
/// terminal, and a document docked on purpose stops being a preview.
pub fn dock_app_tab(
    state: &mut PersistedAppState,
    host: &str,
    server: &str,
    session: &Session,
    tab_id: &str,
    side: DockSide,
) -> bool {
    let Some(index) = state.app_tabs.iter().position(|tab| {
        tab.host_profile_id == host
            && tab.server_identity == server
            && tab.session_id == session.id
            && tab.id == tab_id
    }) else {
        return false;
    };
    if state.app_tabs[index].preview == Some(true) {
        state.app_tabs[index].preview = None;
    }
    update_workspace_ui(state, host, server, session, |record| {
        if record.selected_app_tab_id.as_deref() == Some(tab_id) {
            record.selected_app_tab_id = None;
        }
        record.docked_app_tab_id = Some(tab_id.to_owned());
        record.dock_side = Some(side);
    });
    true
}

/// Returns the docked document to the strip, selected so it stays on screen.
pub fn undock_app_tab(state: &mut PersistedAppState, host: &str, server: &str, session: &Session) {
    update_workspace_ui(state, host, server, session, |record| {
        if let Some(docked) = record.docked_app_tab_id.take() {
            record.selected_app_tab_id = Some(docked);
        }
        record.dock_side = None;
    });
}

/// Forgets `tab_id` wherever a workspace on `host` selected or docked it.
fn forget_app_tab(state: &mut PersistedAppState, host: &str, tab_id: &str) {
    for record in state
        .workspace_ui
        .iter_mut()
        .filter(|record| record.host_profile_id == host)
    {
        if record.selected_app_tab_id.as_deref() == Some(tab_id) {
            record.selected_app_tab_id = None;
        } else if record.docked_app_tab_id.as_deref() == Some(tab_id) {
            record.docked_app_tab_id = None;
            record.dock_side = None;
        }
    }
}

pub fn close_app_tab(state: &mut PersistedAppState, host: &str, tab_id: &str) -> bool {
    let before = state.app_tabs.len();
    state
        .app_tabs
        .retain(|tab| tab.host_profile_id != host || tab.id != tab_id);
    forget_app_tab(state, host, tab_id);
    state.app_tabs.len() != before
}

/// A preview tab stops being disposable once the user commits to it.
pub fn pin_app_tab(state: &mut PersistedAppState, host: &str, tab_id: &str) -> bool {
    match state
        .app_tabs
        .iter_mut()
        .find(|tab| tab.host_profile_id == host && tab.id == tab_id && tab.preview == Some(true))
    {
        Some(tab) => {
            tab.preview = None;
            true
        }
        None => false,
    }
}

fn renumber(state: &mut PersistedAppState, host: &str, ids: &[String]) {
    for tab in state
        .app_tabs
        .iter_mut()
        .filter(|tab| tab.host_profile_id == host)
    {
        if let Some(order) = ids.iter().position(|id| *id == tab.id) {
            tab.order = order as u32;
        }
    }
}

/// Swaps a document tab with its neighbour in the workspace's own order.
pub fn reorder_app_tab(
    state: &mut PersistedAppState,
    host: &str,
    server: Option<&str>,
    session_id: &str,
    tab_id: &str,
    left: bool,
) -> bool {
    let mut ids: Vec<String> = app_tabs_for_workspace(state, host, server, session_id)
        .iter()
        .map(|tab| tab.id.clone())
        .collect();
    let Some(current) = ids.iter().position(|id| id == tab_id) else {
        return false;
    };
    let Some(target) = (if left {
        current.checked_sub(1)
    } else {
        Some(current + 1)
    })
    .filter(|target| *target < ids.len()) else {
        return false;
    };
    ids.swap(current, target);
    renumber(state, host, &ids);
    true
}

/// Puts one document tab before or after another in the same workspace.
pub fn move_app_tab_to(
    state: &mut PersistedAppState,
    host: &str,
    server: Option<&str>,
    session_id: &str,
    tab_id: &str,
    target_id: &str,
    after: bool,
) -> bool {
    let mut ids: Vec<String> = app_tabs_for_workspace(state, host, server, session_id)
        .iter()
        .map(|tab| tab.id.clone())
        .collect();
    if tab_id == target_id
        || !ids.iter().any(|id| id == tab_id)
        || !ids.iter().any(|id| id == target_id)
    {
        return false;
    }
    ids.retain(|id| id != tab_id);
    let at = ids.iter().position(|id| id == target_id).unwrap_or(0) + usize::from(after);
    ids.insert(at, tab_id.to_owned());
    renumber(state, host, &ids);
    true
}

/// Moves a document tab to the end of another workspace on the same server. It
/// arrives pinned (a workspace has one preview slot), and the source forgets it.
/// Refused when the destination already has that document open.
pub fn move_app_tab_to_workspace(
    state: &mut PersistedAppState,
    host: &str,
    server: Option<&str>,
    tab_id: &str,
    target: &Session,
) -> bool {
    let Some(server) = server else {
        return false;
    };
    let Some(index) = state.app_tabs.iter().position(|tab| {
        tab.host_profile_id == host && tab.server_identity == server && tab.id == tab_id
    }) else {
        return false;
    };
    let moving = &state.app_tabs[index];
    if moving.session_id == target.id {
        return false;
    }
    let destination = app_tabs_for_workspace(state, host, Some(server), &target.id);
    if destination
        .iter()
        .any(|tab| tab.kind == moving.kind && tab.resource == moving.resource)
    {
        return false;
    }
    let order = destination
        .iter()
        .map(|tab| tab.order + 1)
        .max()
        .unwrap_or(0);
    let tab = &mut state.app_tabs[index];
    tab.session_id = target.id.clone();
    tab.session_name = target.name.clone();
    tab.order = order;
    tab.preview = None;
    forget_app_tab(state, host, tab_id);
    true
}

/// Whether a host-side window change should uncover the terminal under a
/// document: only a real switch inside the same workspace. When the old window
/// left the workspace (moved or closed), tmux picking another is not someone
/// switching tabs, and the document stays.
pub fn should_surface_authoritative_terminal(
    app_tab_selected: bool,
    previous_session: Option<&str>,
    previous_window: Option<&str>,
    next_session: Option<&str>,
    next_window: Option<&str>,
    previous_window_remains: bool,
) -> bool {
    app_tab_selected
        && previous_session.is_some()
        && previous_session == next_session
        && previous_window.is_some()
        && next_window.is_some()
        && previous_window != next_window
        && previous_window_remains
}

#[cfg(test)]
pub(crate) mod fixtures {
    use desktop_core::app_state::{AppTabKind, AppTabRecord, PersistedAppState, WorkspaceUiRecord};
    use tmux_control::{Pane, Session, Window};

    pub fn session(id: &str, name: &str, order: u32) -> Session {
        Session {
            id: id.into(),
            name: name.into(),
            window_count: 1,
            attached_clients: 0,
            order,
            pinned: false,
        }
    }

    pub fn window(id: &str, session_id: &str, index: u32, name: &str, active: bool) -> Window {
        Window {
            id: id.into(),
            session_id: session_id.into(),
            index,
            name: name.into(),
            active,
            layout: String::new(),
            zoomed: false,
            pinned: false,
        }
    }

    pub fn pane(
        id: &str,
        window_id: &str,
        session_id: &str,
        (left, top, width, height): (u16, u16, u16, u16),
    ) -> Pane {
        Pane {
            id: id.into(),
            session_id: session_id.into(),
            window_id: window_id.into(),
            index: 0,
            active: false,
            width,
            height,
            left,
            top,
            current_path: String::new(),
            current_command: String::new(),
            pane_pid: 0,
            start_command: String::new(),
            title: String::new(),
        }
    }

    pub fn tab(id: &str, session_id: &str, order: u32) -> AppTabRecord {
        AppTabRecord {
            id: id.into(),
            host_profile_id: "local".into(),
            server_identity: "server-a".into(),
            session_id: session_id.into(),
            session_name: if session_id == "$1" { "one" } else { "two" }.into(),
            kind: AppTabKind::File,
            resource: format!("/repo/{id}.rs"),
            root_path: None,
            root_token: None,
            preview: None,
            view_mode: None,
            git_repository_id: None,
            git_path: None,
            git_original_path: None,
            git_target: None,
            git_status_generation: None,
            git_source_generation: None,
            title: format!("{id}.rs"),
            order,
        }
    }

    /// Two documents in workspace `$1` on `server-a`.
    pub fn state() -> PersistedAppState {
        PersistedAppState {
            app_tabs: vec![tab("b", "$1", 1), tab("a", "$1", 0)],
            workspace_ui: vec![WorkspaceUiRecord {
                host_profile_id: "local".into(),
                server_identity: "server-a".into(),
                session_id: "$1".into(),
                session_name: "one".into(),
                selected_app_tab_id: None,
                docked_app_tab_id: None,
                dock_side: None,
            }],
            ..PersistedAppState::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fixtures::*, *};

    fn unknown(_: &str) -> WindowAgentInfo {
        WindowAgentInfo {
            attention: None,
            presence: AgentPresence::Unknown,
        }
    }

    fn keys(tabs: &[CombinedTab]) -> Vec<&str> {
        tabs.iter().map(|tab| tab.key.as_str()).collect()
    }

    fn strip(
        windows: &[Window],
        state: &PersistedAppState,
        pending: Option<&PendingTab>,
    ) -> Vec<CombinedTab> {
        let windows: Vec<&Window> = windows.iter().collect();
        let tabs = app_tabs_for_workspace(state, "local", Some("server-a"), "$1");
        combine_workspace_tabs(&windows, &tabs, unknown, pending)
    }

    fn ui(state: &PersistedAppState) -> &WorkspaceUiRecord {
        &state.workspace_ui[0]
    }

    #[test]
    fn orders_sessions_by_the_hosts_order_then_name() {
        let sessions = [
            session("$2", "two", 1),
            session("$1", "one", 0),
            session("$3", "a", 1),
        ];
        let ids: Vec<&str> = ordered_sessions(&sessions)
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, ["$1", "$3", "$2"]);
    }

    #[test]
    fn combines_terminal_tabs_with_ordered_documents() {
        let windows = [
            window("@2", "$1", 2, "second", false),
            window("@1", "$1", 1, "first", true),
        ];
        let combined = strip(&windows, &state(), None);
        assert_eq!(
            keys(&combined),
            ["terminal:@1", "terminal:@2", "app:a", "app:b"]
        );
        assert!(!combined[1].can_move_right);
        assert!(!combined[2].can_move_left);
        // Nothing for a tab without a server identity.
        assert!(app_tabs_for_workspace(&state(), "local", None, "$1").is_empty());
    }

    #[test]
    fn leads_the_strip_with_the_windows_the_host_pinned() {
        let mut windows = [
            window("@1", "$1", 1, "first", true),
            window("@2", "$1", 2, "second", false),
            window("@3", "$1", 3, "third", false),
        ];
        windows[1].pinned = true;
        windows[2].pinned = true;
        let combined = strip(&windows, &state(), None);
        assert_eq!(
            keys(&combined),
            [
                "terminal:@2",
                "terminal:@3",
                "terminal:@1",
                "app:a",
                "app:b"
            ]
        );
        // The flags follow the window index, not the pinned order.
        assert!(!combined[2].can_move_left);
    }

    #[test]
    fn shows_a_placeholder_last_and_retires_it_for_good() {
        let before = [window("@1", "$1", 1, "first", true)];
        let after = [
            window("@1", "$1", 1, "first", true),
            window("@2", "$1", 2, "second", false),
        ];
        let pending = PendingTab {
            key: 1,
            session_id: Some("$1".into()),
            window_id: None,
            title: "New window".into(),
        };
        assert_eq!(
            keys(&strip(
                &before,
                &PersistedAppState::default(),
                Some(&pending)
            )),
            ["terminal:@1", "pending:1"]
        );
        let acked = PendingTab {
            window_id: Some("@2".into()),
            ..pending
        };
        // The ack is not the snapshot: still shown.
        let before_refs: Vec<&Window> = before.iter().collect();
        let after_refs: Vec<&Window> = after.iter().collect();
        assert_eq!(
            retire_pending_tab(Some(acked.clone()), &before_refs),
            Some(acked.clone())
        );
        assert_eq!(
            keys(&strip(&after, &PersistedAppState::default(), Some(&acked))),
            ["terminal:@1", "terminal:@2"]
        );
        let retired = retire_pending_tab(Some(acked), &after_refs);
        assert_eq!(retired, None);
        assert_eq!(retire_pending_tab(retired, &before_refs), None);
    }

    #[test]
    fn reports_a_bulk_close_that_left_survivors() {
        assert_eq!(bulk_close_outcome_status(7, 0, 0), None);
        assert_eq!(bulk_close_outcome_status(0, 0, 0), None);
        assert_eq!(
            bulk_close_outcome_status(5, 2, 0).as_deref(),
            Some("Closed 5 of 7 tabs; 2 could not be closed.")
        );
        assert_eq!(
            bulk_close_outcome_status(0, 1, 0).as_deref(),
            Some("Closed 0 of 1 tab; 1 could not be closed.")
        );
        assert_eq!(
            bulk_close_outcome_status(0, 0, 1).as_deref(),
            Some("Closed 0 of 1 tab; 1 still had an agent and was left open.")
        );
        assert_eq!(
            bulk_close_outcome_status(3, 1, 2).as_deref(),
            Some(
                "Closed 3 of 6 tabs; 1 could not be closed; 2 still had an agent and were left open."
            )
        );
        assert_eq!(bulk_close_complete_status(0), None);
        assert_eq!(
            bulk_close_complete_status(1).as_deref(),
            Some("Closed 1 tab.")
        );
        assert_eq!(
            bulk_close_complete_status(4).as_deref(),
            Some("Closed 4 tabs.")
        );
    }

    #[test]
    fn takes_bulk_closes_from_the_strips_own_order() {
        let windows = [
            window("@1", "$1", 1, "first", true),
            window("@2", "$1", 2, "second", false),
        ];
        let pending = PendingTab {
            key: 1,
            session_id: Some("$1".into()),
            window_id: None,
            title: "New window".into(),
        };
        let combined = strip(&windows, &state(), Some(&pending));
        assert_eq!(
            keys(&combined),
            ["terminal:@1", "terminal:@2", "app:a", "app:b", "pending:1"]
        );
        assert_eq!(
            keys(&tabs_to_close_others(&combined, "terminal:@1")),
            ["terminal:@2", "app:a", "app:b"]
        );
        assert_eq!(keys(&tabs_to_close_right(&combined, "app:a")), ["app:b"]);
        assert_eq!(
            keys(&tabs_to_close_others(&combined, "app:a")),
            ["terminal:@1", "terminal:@2", "app:b"]
        );
        assert!(tabs_to_close_right(&combined, "app:b").is_empty());
        assert!(tabs_to_close_others(&combined[..1], "terminal:@1").is_empty());
        assert!(tabs_to_close_others(&combined, "terminal:@9").is_empty());
        assert!(tabs_to_close_right(&combined, "terminal:@9").is_empty());
        assert!(bulk_close_disabled(
            &tabs_to_close_others(&combined, "app:a"),
            false
        ));
        assert!(!bulk_close_disabled(
            &tabs_to_close_right(&combined, "app:a"),
            false
        ));
    }

    #[test]
    fn closes_all_and_only_tabs_known_to_hold_no_agent() {
        let windows = [
            window("@1", "$1", 1, "agent", true),
            window("@2", "$1", 2, "empty", false),
            window("@3", "$1", 3, "unknown", false),
        ];
        let refs: Vec<&Window> = windows.iter().collect();
        let state = state();
        let tabs = app_tabs_for_workspace(&state, "local", Some("server-a"), "$1");
        let combined = combine_workspace_tabs(
            &refs,
            &tabs,
            |id| WindowAgentInfo {
                attention: (id == "@1").then_some(DisplayState::Idle),
                presence: match id {
                    "@1" => AgentPresence::Present,
                    "@2" => AgentPresence::Absent,
                    _ => AgentPresence::Unknown,
                },
            },
            None,
        );
        assert_eq!(
            keys(&tabs_to_close_non_agent(&combined)),
            ["terminal:@2", "app:a", "app:b"]
        );
        // Until the agents are known, only documents.
        let unknown_strip = combine_workspace_tabs(&refs, &tabs, unknown, None);
        assert_eq!(
            keys(&tabs_to_close_non_agent(&unknown_strip)),
            ["app:a", "app:b"]
        );
    }

    #[test]
    fn persists_selection_closes_only_documents_and_reorders_peers() {
        let one = session("$1", "one", 0);
        let mut state = state();
        select_app_tab(&mut state, "local", "server-a", &one, Some("a"));
        assert_eq!(ui(&state).selected_app_tab_id.as_deref(), Some("a"));
        assert!(reorder_app_tab(
            &mut state,
            "local",
            Some("server-a"),
            "$1",
            "a",
            false
        ));
        let ids = |state: &PersistedAppState| -> Vec<String> {
            app_tabs_for_workspace(state, "local", Some("server-a"), "$1")
                .iter()
                .map(|tab| tab.id.clone())
                .collect()
        };
        assert_eq!(ids(&state), ["b", "a"]);
        assert!(!reorder_app_tab(
            &mut state,
            "local",
            Some("server-a"),
            "$1",
            "a",
            false
        ));
        assert!(close_app_tab(&mut state, "local", "a"));
        assert_eq!(ids(&state), ["b"]);
        assert_eq!(ui(&state).selected_app_tab_id, None);
    }

    #[test]
    fn drops_a_document_beside_a_peer_and_moves_one_to_another_workspace_pinned() {
        let ids = |state: &PersistedAppState, session: &str| -> Vec<String> {
            app_tabs_for_workspace(state, "local", Some("server-a"), session)
                .iter()
                .map(|tab| tab.id.clone())
                .collect()
        };
        let mut state = state();
        assert!(move_app_tab_to(
            &mut state,
            "local",
            Some("server-a"),
            "$1",
            "a",
            "b",
            true
        ));
        assert_eq!(ids(&state, "$1"), ["b", "a"]);
        assert!(move_app_tab_to(
            &mut state,
            "local",
            Some("server-a"),
            "$1",
            "a",
            "b",
            false
        ));
        assert_eq!(ids(&state, "$1"), ["a", "b"]);
        assert!(!move_app_tab_to(
            &mut state,
            "local",
            Some("server-a"),
            "$1",
            "a",
            "a",
            true
        ));

        let one = session("$1", "one", 0);
        let two = session("$2", "two", 1);
        state
            .app_tabs
            .iter_mut()
            .find(|tab| tab.id == "a")
            .unwrap()
            .preview = Some(true);
        select_app_tab(&mut state, "local", "server-a", &one, Some("a"));
        let mut doubled = state.clone();
        assert!(move_app_tab_to_workspace(
            &mut state,
            "local",
            Some("server-a"),
            "a",
            &two
        ));
        let moved = state.app_tabs.iter().find(|tab| tab.id == "a").unwrap();
        assert_eq!(
            (
                moved.session_id.as_str(),
                moved.session_name.as_str(),
                moved.order,
                moved.preview
            ),
            ("$2", "two", 0, None)
        );
        assert_eq!(ids(&state, "$1"), ["b"]);
        assert_eq!(ui(&state).selected_app_tab_id, None);
        assert!(!move_app_tab_to_workspace(
            &mut state,
            "local",
            Some("server-a"),
            "b",
            &one
        ));
        // One tab per document in a workspace.
        let mut copy = doubled.app_tabs[0].clone();
        copy.id = "b2".into();
        copy.session_id = "$2".into();
        doubled.app_tabs.push(copy);
        assert!(!move_app_tab_to_workspace(
            &mut doubled,
            "local",
            Some("server-a"),
            "b",
            &two
        ));
    }

    #[test]
    fn docks_one_document_beside_the_terminal_never_as_the_selection_too() {
        let one = session("$1", "one", 0);
        let mut state = state();
        state
            .app_tabs
            .iter_mut()
            .find(|tab| tab.id == "a")
            .unwrap()
            .preview = Some(true);
        select_app_tab(&mut state, "local", "server-a", &one, Some("a"));
        assert!(dock_app_tab(
            &mut state,
            "local",
            "server-a",
            &one,
            "a",
            DockSide::Right
        ));
        assert_eq!(ui(&state).docked_app_tab_id.as_deref(), Some("a"));
        assert_eq!(ui(&state).selected_app_tab_id, None);
        assert_eq!(
            state
                .app_tabs
                .iter()
                .find(|tab| tab.id == "a")
                .unwrap()
                .preview,
            None
        );

        let docked = state.clone();
        let mut switched = docked.clone();
        select_app_tab(&mut switched, "local", "server-a", &one, Some("b"));
        assert_eq!(ui(&switched).docked_app_tab_id.as_deref(), Some("b"));
        assert_eq!(ui(&switched).selected_app_tab_id, None);
        let mut same = docked.clone();
        select_app_tab(&mut same, "local", "server-a", &one, Some("a"));
        assert_eq!(same, docked);
        let mut terminal = docked.clone();
        select_app_tab(&mut terminal, "local", "server-a", &one, None);
        assert_eq!(ui(&terminal).docked_app_tab_id.as_deref(), Some("a"));

        let mut both = self::fixtures::state();
        select_app_tab(&mut both, "local", "server-a", &one, Some("b"));
        dock_app_tab(&mut both, "local", "server-a", &one, "a", DockSide::Right);
        assert_eq!(ui(&both).selected_app_tab_id.as_deref(), Some("b"));
        dock_app_tab(&mut both, "local", "server-a", &one, "b", DockSide::Left);
        assert_eq!(ui(&both).docked_app_tab_id.as_deref(), Some("b"));
        assert_eq!(ui(&both).dock_side, Some(DockSide::Left));
        assert_eq!(ui(&both).selected_app_tab_id, None);
        assert!(!dock_app_tab(
            &mut both,
            "local",
            "server-a",
            &one,
            "missing",
            DockSide::Left
        ));

        undock_app_tab(&mut both, "local", "server-a", &one);
        assert_eq!(ui(&both).selected_app_tab_id.as_deref(), Some("b"));
        assert_eq!(
            (ui(&both).docked_app_tab_id.as_deref(), ui(&both).dock_side),
            (None, None)
        );

        let mut closed = docked.clone();
        close_app_tab(&mut closed, "local", "a");
        assert_eq!(
            (
                ui(&closed).docked_app_tab_id.as_deref(),
                ui(&closed).dock_side
            ),
            (None, None)
        );
    }

    #[test]
    fn pins_a_preview_tab_once() {
        let mut state = state();
        assert!(!pin_app_tab(&mut state, "local", "a"));
        state.app_tabs[0].preview = Some(true);
        let id = state.app_tabs[0].id.clone();
        assert!(pin_app_tab(&mut state, "local", &id));
        assert_eq!(state.app_tabs[0].preview, None);
    }

    #[test]
    fn surfaces_a_terminal_only_for_a_real_switch() {
        let surface = should_surface_authoritative_terminal;
        assert!(surface(
            true,
            Some("$1"),
            Some("@1"),
            Some("$1"),
            Some("@2"),
            true
        ));
        assert!(!surface(
            true,
            Some("$1"),
            Some("@1"),
            Some("$2"),
            Some("@2"),
            true
        ));
        assert!(!surface(
            true,
            Some("$1"),
            Some("@1"),
            Some("$1"),
            Some("@1"),
            true
        ));
        assert!(!surface(
            false,
            Some("$1"),
            Some("@1"),
            Some("$1"),
            Some("@2"),
            true
        ));
        assert!(!surface(true, None, None, Some("$1"), Some("@1"), true));
        assert!(!surface(
            true,
            Some("$1"),
            Some("@1"),
            Some("$1"),
            Some("@2"),
            false
        ));
    }
}
