//! Tabs left behind by a replaced tmux server. Port of `app/appRecovery.ts` and
//! the four workspace-identity functions of `features/shell/model.ts`.

use std::collections::{HashMap, HashSet};

use desktop_core::app_state::PersistedAppState;
use tmux_control::Session;

use super::host_scope::{HostScopeToken, same_host_connection};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppRecoveryPhase {
    Offered,
    ConfirmingDiscard,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppRecoveryState {
    pub phase: AppRecoveryPhase,
    pub count: usize,
    pub previous_server_identity: String,
    pub scope: HostScopeToken,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppRecoveryAction {
    Offer {
        count: usize,
        previous_server_identity: String,
        scope: HostScopeToken,
    },
    ConfirmDiscard,
    CancelDiscard,
    ReconcileScope(HostScopeToken),
    Clear,
}

/// One reducer owns both the offer and its modal; no independent open flag.
pub fn app_recovery_reducer(state: &mut Option<AppRecoveryState>, action: AppRecoveryAction) {
    match action {
        AppRecoveryAction::Offer {
            count,
            previous_server_identity,
            scope,
        } => {
            *state = Some(AppRecoveryState {
                phase: AppRecoveryPhase::Offered,
                count,
                previous_server_identity,
                scope,
            });
        }
        AppRecoveryAction::ConfirmDiscard => {
            if let Some(state) = state {
                state.phase = AppRecoveryPhase::ConfirmingDiscard;
            }
        }
        AppRecoveryAction::CancelDiscard => {
            if let Some(state) = state {
                state.phase = AppRecoveryPhase::Offered;
            }
        }
        AppRecoveryAction::ReconcileScope(scope) => {
            if state
                .as_ref()
                .is_some_and(|current| !same_host_connection(&current.scope, &scope))
            {
                *state = None;
            }
        }
        AppRecoveryAction::Clear => *state = None,
    }
}

/// Drops records for sessions the current server no longer has and refreshes
/// renamed ones. Returns whether anything changed.
pub fn reconcile_workspace_identity(
    state: &mut PersistedAppState,
    host_profile_id: &str,
    server_identity: Option<&str>,
    sessions: &[Session],
) -> bool {
    let session = |id: &str| sessions.iter().find(|session| session.id == id);
    let mut changed = false;
    state.app_tabs.retain_mut(|tab| {
        if tab.host_profile_id != host_profile_id
            || Some(tab.server_identity.as_str()) != server_identity
        {
            return true;
        }
        let Some(exact) = session(&tab.session_id) else {
            changed = true;
            return false;
        };
        if tab.session_name != exact.name {
            tab.session_name = exact.name.clone();
            changed = true;
        }
        true
    });
    state.workspace_ui.retain_mut(|item| {
        if item.host_profile_id != host_profile_id
            || Some(item.server_identity.as_str()) != server_identity
        {
            return true;
        }
        let Some(exact) = session(&item.session_id) else {
            changed = true;
            return false;
        };
        if item.session_name != exact.name {
            item.session_name = exact.name.clone();
            changed = true;
        }
        true
    });
    changed
}

pub fn recoverable_app_tab_count(
    state: &PersistedAppState,
    host_profile_id: &str,
    previous_server_identity: &str,
    sessions: &[Session],
) -> usize {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for session in sessions {
        *counts.entry(session.name.as_str()).or_default() += 1;
    }
    state
        .app_tabs
        .iter()
        .filter(|tab| {
            tab.host_profile_id == host_profile_id
                && tab.server_identity == previous_server_identity
                && counts.get(tab.session_name.as_str()) == Some(&1)
        })
        .count()
}

pub fn recover_app_tabs_from_previous_server(
    state: &mut PersistedAppState,
    host_profile_id: &str,
    previous_server_identity: &str,
    current_server_identity: &str,
    sessions: &[Session],
) {
    let mut unique: HashMap<&str, &Session> = HashMap::new();
    let mut ambiguous: HashSet<&str> = HashSet::new();
    for session in sessions {
        if unique.insert(session.name.as_str(), session).is_some() {
            ambiguous.insert(session.name.as_str());
        }
    }
    for name in &ambiguous {
        unique.remove(name);
    }
    for tab in &mut state.app_tabs {
        if tab.host_profile_id != host_profile_id || tab.server_identity != previous_server_identity
        {
            continue;
        }
        if let Some(session) = unique.get(tab.session_name.as_str()) {
            tab.server_identity = current_server_identity.to_owned();
            tab.session_id = session.id.clone();
            // Root tokens are bound to the previous tmux server and inode: a
            // rebound tab must reacquire one from the new active pane.
            tab.root_path = None;
            tab.root_token = None;
        }
    }
    for item in &mut state.workspace_ui {
        if item.host_profile_id != host_profile_id
            || item.server_identity != previous_server_identity
        {
            continue;
        }
        if let Some(session) = unique.get(item.session_name.as_str()) {
            item.server_identity = current_server_identity.to_owned();
            item.session_id = session.id.clone();
        }
    }
}

pub fn discard_server_app_state(
    state: &mut PersistedAppState,
    host_profile_id: &str,
    server_identity: &str,
) {
    state.app_tabs.retain(|tab| {
        tab.host_profile_id != host_profile_id || tab.server_identity != server_identity
    });
    state.workspace_ui.retain(|item| {
        item.host_profile_id != host_profile_id || item.server_identity != server_identity
    });
}

#[cfg(test)]
mod tests {
    use desktop_core::app_state::{AppTabKind, AppTabRecord, WorkspaceUiRecord};

    use super::*;

    fn scope(epoch: u64, generation: u64) -> HostScopeToken {
        HostScopeToken {
            host_profile_id: "remote".into(),
            connection_key: "ssh:remote".into(),
            connection_epoch: epoch,
            server_identity: Some("server-b".into()),
            generation,
        }
    }

    fn offered() -> Option<AppRecoveryState> {
        let mut state = None;
        app_recovery_reducer(
            &mut state,
            AppRecoveryAction::Offer {
                count: 2,
                previous_server_identity: "server-a".into(),
                scope: scope(1, 1),
            },
        );
        state
    }

    #[test]
    fn opens_and_cancels_the_discard_confirmation_from_the_offer() {
        let mut state = offered();
        app_recovery_reducer(&mut state, AppRecoveryAction::ConfirmDiscard);
        assert_eq!(
            state.as_ref().unwrap().phase,
            AppRecoveryPhase::ConfirmingDiscard
        );
        app_recovery_reducer(&mut state, AppRecoveryAction::CancelDiscard);
        assert_eq!(state.as_ref().unwrap().phase, AppRecoveryPhase::Offered);
    }

    #[test]
    fn keeps_the_offer_across_a_generation_and_drops_it_with_the_connection() {
        let mut state = offered();
        app_recovery_reducer(&mut state, AppRecoveryAction::ReconcileScope(scope(1, 9)));
        assert!(state.is_some());
        app_recovery_reducer(&mut state, AppRecoveryAction::ReconcileScope(scope(2, 9)));
        assert_eq!(state, None);

        // The confirmation goes with its offer, and a new offer starts unconfirmed.
        let mut confirming = offered();
        app_recovery_reducer(&mut confirming, AppRecoveryAction::ConfirmDiscard);
        app_recovery_reducer(
            &mut confirming,
            AppRecoveryAction::ReconcileScope(scope(2, 9)),
        );
        assert_eq!(confirming, None);
    }

    fn session(id: &str, name: &str) -> Session {
        Session {
            id: id.into(),
            name: name.into(),
            window_count: 1,
            attached_clients: 0,
            order: 0,
            pinned: false,
        }
    }

    fn tab(host: &str, server: &str, session_id: &str, session_name: &str) -> AppTabRecord {
        AppTabRecord {
            id: format!("{host}-{server}-{session_id}"),
            host_profile_id: host.into(),
            server_identity: server.into(),
            session_id: session_id.into(),
            session_name: session_name.into(),
            kind: AppTabKind::File,
            resource: "/repo/a.ts".into(),
            root_path: Some("/repo".into()),
            root_token: Some("token".into()),
            preview: None,
            view_mode: None,
            git_repository_id: None,
            git_path: None,
            git_original_path: None,
            git_target: None,
            git_status_generation: None,
            git_source_generation: None,
            title: "a.ts".into(),
            order: 0,
        }
    }

    fn ui(host: &str, server: &str, session_id: &str, session_name: &str) -> WorkspaceUiRecord {
        WorkspaceUiRecord {
            host_profile_id: host.into(),
            server_identity: server.into(),
            session_id: session_id.into(),
            session_name: session_name.into(),
            selected_app_tab_id: None,
            docked_app_tab_id: None,
            dock_side: None,
        }
    }

    fn state() -> PersistedAppState {
        PersistedAppState {
            app_tabs: vec![
                tab("remote", "server-a", "$1", "work"),
                tab("remote", "server-a", "$2", "twice"),
                tab("other", "server-a", "$1", "work"),
            ],
            workspace_ui: vec![ui("remote", "server-a", "$1", "work")],
            ..PersistedAppState::default()
        }
    }

    #[test]
    fn counts_only_tabs_of_this_host_and_server_with_a_unique_name() {
        let sessions = [
            session("$7", "work"),
            session("$8", "twice"),
            session("$9", "twice"),
        ];
        assert_eq!(
            recoverable_app_tab_count(&state(), "remote", "server-a", &sessions),
            1
        );
        assert_eq!(
            recoverable_app_tab_count(&state(), "remote", "server-z", &sessions),
            0
        );
    }

    #[test]
    fn rebinds_by_unique_name_and_drops_the_root_capability() {
        let sessions = [
            session("$7", "work"),
            session("$8", "twice"),
            session("$9", "twice"),
        ];
        let mut value = state();
        recover_app_tabs_from_previous_server(
            &mut value, "remote", "server-a", "server-b", &sessions,
        );
        let rebound = &value.app_tabs[0];
        assert_eq!(rebound.server_identity, "server-b");
        assert_eq!(rebound.session_id, "$7");
        assert_eq!(rebound.root_path, None);
        assert_eq!(rebound.root_token, None);
        // Ambiguous: stays bound to the old server.
        assert_eq!(value.app_tabs[1].server_identity, "server-a");
        // Another host is untouched.
        assert_eq!(value.app_tabs[2], state().app_tabs[2]);
        assert_eq!(value.workspace_ui[0].server_identity, "server-b");
        assert_eq!(value.workspace_ui[0].session_id, "$7");
    }

    #[test]
    fn discards_only_the_replaced_servers_records_for_this_host() {
        let mut value = state();
        discard_server_app_state(&mut value, "remote", "server-a");
        assert_eq!(value.app_tabs.len(), 1);
        assert_eq!(value.app_tabs[0].host_profile_id, "other");
        assert!(value.workspace_ui.is_empty());
    }

    #[test]
    fn reconciles_the_current_server_only_and_reports_no_change_honestly() {
        let mut value = state();
        let before = value.clone();
        assert!(!reconcile_workspace_identity(
            &mut value,
            "remote",
            None,
            &[]
        ));
        assert!(!reconcile_workspace_identity(
            &mut value,
            "remote",
            Some("server-b"),
            &[]
        ));
        assert_eq!(value, before);

        let sessions = [session("$1", "renamed")];
        assert!(reconcile_workspace_identity(
            &mut value,
            "remote",
            Some("server-a"),
            &sessions
        ));
        assert_eq!(value.app_tabs.len(), 2);
        assert_eq!(value.app_tabs[0].session_name, "renamed");
        assert_eq!(value.workspace_ui[0].session_name, "renamed");
    }
}
