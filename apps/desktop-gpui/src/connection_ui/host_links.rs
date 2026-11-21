//! One link per shown host. Port of `state/hostLinks.ts`.

use std::collections::HashMap;

use desktop_core::connection::{ConnectionSpec, profiles::HostProfile};
use tmux_control::{Session, Window};

use super::{
    connection_reducer::{HostAction, NormalizedHostState, connection_reducer},
    host_profiles::{helper_connection_key, host_profile_id},
    host_scope::HostScopeToken,
};

/// One shown host: the bridge kept to it, and what it has said so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostLink {
    pub profile_id: String,
    pub connection: ConnectionSpec,
    /// A bump restarts this host's bridge. Never repeats for a profile in one
    /// process.
    pub connection_epoch: u64,
    pub client_id: Option<String>,
    /// Native generation epoch of the live bridge; 0 before the first one.
    pub terminal_epoch: u64,
    pub host_state: NormalizedHostState,
    /// Remembered per host so switching back lands where the user left.
    pub active_session_id: Option<String>,
    pub active_window_id: Option<String>,
    /// A switch the shell has painted ahead of the host. See
    /// [`OptimisticWindow`].
    pub optimistic_window: Option<OptimisticWindow>,
    pub detail: String,
}

/// A window or workspace switch committed locally while its request is still in
/// flight. Port of `OptimisticWindowSwitch` from `app/windowSelection.ts`.
///
/// Every snapshot in that gap still names the old window as active and would
/// snap the shell back to it, so while this is held its window wins. It is
/// released once a snapshot at or past `through_generation` arrives (the host
/// has seen the switch), or once another workspace is selected. Until the
/// select action answers there is no generation to wait for, and it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimisticWindow {
    pub session_id: String,
    /// Unset where the workspace's windows were not known yet.
    pub window_id: Option<String>,
    pub through_generation: Option<u64>,
}

impl HostLink {
    fn empty(profile_id: String, connection: ConnectionSpec, connection_epoch: u64) -> Self {
        Self {
            profile_id,
            connection,
            connection_epoch,
            client_id: None,
            terminal_epoch: 0,
            host_state: NormalizedHostState::default(),
            active_session_id: None,
            active_window_id: None,
            optimistic_window: None,
            detail: String::new(),
        }
    }

    pub fn scope(&self) -> HostScopeToken {
        HostScopeToken {
            host_profile_id: self.profile_id.clone(),
            connection_key: helper_connection_key(&self.connection),
            connection_epoch: self.connection_epoch,
            server_identity: self.host_state.server_identity.clone(),
            generation: self.host_state.generation,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostLinksState {
    pub order: Vec<String>,
    pub by_profile_id: HashMap<String, HostLink>,
    /// One counter for every host keeps an epoch from repeating for a profile
    /// that is shown, hidden and shown again.
    pub next_epoch: u64,
}

impl HostLinksState {
    pub fn link(&self, profile_id: &str) -> Option<&HostLink> {
        self.by_profile_id.get(profile_id)
    }
}

/// A host to keep a link to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShownHostProfile {
    pub profile_id: String,
    pub connection: ConnectionSpec,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostLinksAction {
    Sync(Vec<ShownHostProfile>),
    Add {
        profile_id: String,
        connection: ConnectionSpec,
    },
    Remove {
        profile_id: String,
    },
    Reconnect {
        profile_id: String,
    },
    ReconnectAll,
    Client {
        profile_id: String,
        client_id: Option<String>,
    },
    TerminalEpoch {
        profile_id: String,
        terminal_epoch: u64,
    },
    Detail {
        profile_id: String,
        detail: String,
    },
    /// A snapshot that describes sessions also re-resolves the remembered
    /// session, for every host, since that is where activating it will land.
    Host {
        profile_id: String,
        action: HostAction,
    },
    ActiveSession {
        profile_id: String,
        session_id: Option<String>,
    },
    /// Sets the window as is, with no resolution: a local switch while the
    /// host cannot be changed has nothing to reconcile against.
    ActiveWindow {
        profile_id: String,
        window_id: Option<String>,
    },
    Optimistic {
        profile_id: String,
        guard: Option<OptimisticWindow>,
    },
}

fn same_connection(left: &ConnectionSpec, right: &ConnectionSpec) -> bool {
    left == right
}

fn add_link(state: &mut HostLinksState, profile_id: String, connection: ConnectionSpec) {
    if state.by_profile_id.contains_key(&profile_id) {
        return;
    }
    state.order.push(profile_id.clone());
    state.by_profile_id.insert(
        profile_id.clone(),
        HostLink::empty(profile_id, connection, state.next_epoch),
    );
    state.next_epoch += 1;
}

fn remove_link(state: &mut HostLinksState, profile_id: &str) {
    if state.by_profile_id.remove(profile_id).is_some() {
        state.order.retain(|id| id != profile_id);
    }
}

fn bump_epoch(state: &mut HostLinksState, profile_id: &str) {
    if let Some(link) = state.by_profile_id.get_mut(profile_id) {
        link.connection_epoch = state.next_epoch;
        state.next_epoch += 1;
        // A switch in flight on the old connection lands nowhere now, so its
        // guard would hold the shell against tmux with nothing to release it.
        link.optimistic_window = None;
    }
}

/// Makes the link set exactly `hosts`, in that order; surviving links keep
/// their state.
pub fn sync_host_links(state: &mut HostLinksState, hosts: &[ShownHostProfile]) {
    let stale: Vec<String> = state
        .order
        .iter()
        .filter(|id| !hosts.iter().any(|host| &host.profile_id == *id))
        .cloned()
        .collect();
    for profile_id in stale {
        remove_link(state, &profile_id);
    }
    for host in hosts {
        add_link(state, host.profile_id.clone(), host.connection.clone());
        if let Some(link) = state.by_profile_id.get_mut(&host.profile_id)
            && !same_connection(&link.connection, &host.connection)
        {
            link.connection = host.connection.clone();
        }
    }
    state.order = hosts.iter().map(|host| host.profile_id.clone()).collect();
}

pub fn host_links_reducer(state: &mut HostLinksState, action: HostLinksAction) {
    match action {
        HostLinksAction::Sync(hosts) => sync_host_links(state, &hosts),
        HostLinksAction::Add {
            profile_id,
            connection,
        } => add_link(state, profile_id, connection),
        HostLinksAction::Remove { profile_id } => remove_link(state, &profile_id),
        HostLinksAction::Reconnect { profile_id } => bump_epoch(state, &profile_id),
        HostLinksAction::ReconnectAll => {
            for profile_id in state.order.clone() {
                bump_epoch(state, &profile_id);
            }
        }
        HostLinksAction::Client {
            profile_id,
            client_id,
        } => {
            if let Some(link) = state.by_profile_id.get_mut(&profile_id) {
                link.client_id = client_id;
            }
        }
        HostLinksAction::TerminalEpoch {
            profile_id,
            terminal_epoch,
        } => {
            if let Some(link) = state.by_profile_id.get_mut(&profile_id) {
                link.terminal_epoch = terminal_epoch;
            }
        }
        HostLinksAction::Detail { profile_id, detail } => {
            if let Some(link) = state.by_profile_id.get_mut(&profile_id) {
                link.detail = detail;
            }
        }
        HostLinksAction::Host { profile_id, action } => {
            let Some(link) = state.by_profile_id.get_mut(&profile_id) else {
                return;
            };
            let described = match &action {
                HostAction::Snapshot(action) => action.snapshot.clone(),
                _ => None,
            };
            let previous_name = link
                .active_session_id
                .as_deref()
                .and_then(|id| link.host_state.session(id))
                .map(|session| session.name.clone());
            connection_reducer(&mut link.host_state, action);
            if let Some(world) = described {
                link.active_session_id = resolve_selected_session(
                    &world.sessions,
                    link.active_session_id.as_deref(),
                    previous_name.as_deref(),
                )
                .map(|session| session.id.clone());
            }
            settle_active_window(link);
        }
        HostLinksAction::ActiveSession {
            profile_id,
            session_id,
        } => {
            if let Some(link) = state.by_profile_id.get_mut(&profile_id) {
                link.active_session_id = session_id;
                settle_active_window(link);
            }
        }
        HostLinksAction::ActiveWindow {
            profile_id,
            window_id,
        } => {
            if let Some(link) = state.by_profile_id.get_mut(&profile_id) {
                link.active_window_id = window_id;
            }
        }
        HostLinksAction::Optimistic { profile_id, guard } => {
            if let Some(link) = state.by_profile_id.get_mut(&profile_id) {
                link.optimistic_window = guard;
                settle_active_window(link);
            }
        }
    }
}

/// The window the shell shows in the link's workspace, after anything that can
/// change it: the guard is released once the host caught up, then the window
/// is resolved. Port of the active-window effect in
/// `useAppConnectionController.ts`.
fn settle_active_window(link: &mut HostLink) {
    let session = link.active_session_id.clone();
    if let Some(guard) = &link.optimistic_window {
        let settled = guard
            .through_generation
            .is_some_and(|through| link.host_state.generation >= through);
        if settled || Some(&guard.session_id) != session.as_ref() {
            link.optimistic_window = None;
        }
    }
    let preferred = link
        .optimistic_window
        .as_ref()
        .and_then(|guard| guard.window_id.clone());
    let windows = session.as_deref().map_or_else(Vec::new, |session| {
        session_windows(&link.host_state.snapshot.windows, session)
    });
    link.active_window_id = resolve_active_window_id(
        &windows,
        link.active_window_id.as_deref(),
        preferred.as_deref(),
    )
    .map(str::to_owned);
}

/// One workspace's windows in tab order.
pub fn session_windows<'a>(windows: &'a [Window], session_id: &str) -> Vec<&'a Window> {
    let mut owned: Vec<&Window> = windows
        .iter()
        .filter(|window| window.session_id == session_id)
        .collect();
    owned.sort_by(|left, right| {
        left.index
            .cmp(&right.index)
            .then_with(|| left.id.cmp(&right.id))
    });
    owned
}

/// `resolveActiveWindowId` from `app/windowSelection.ts`: the host's active
/// window, unless a switch still in flight names one that exists; then the
/// current one, then the first.
pub fn resolve_active_window_id<'a>(
    windows: &[&'a Window],
    current: Option<&str>,
    optimistic: Option<&str>,
) -> Option<&'a str> {
    if let Some(window) = optimistic.and_then(|id| windows.iter().find(|window| window.id == id)) {
        return Some(&window.id);
    }
    windows
        .iter()
        .find(|window| window.active)
        .or_else(|| {
            windows
                .iter()
                .find(|window| Some(window.id.as_str()) == current)
        })
        .or_else(|| windows.first())
        .map(|window| window.id.as_str())
}

/// `resolveSelectedSession` from `features/shell/model.ts`: the session id, then
/// the remembered name, then the first in presentation order.
pub fn resolve_selected_session<'a>(
    sessions: &'a [Session],
    current_id: Option<&str>,
    previous_name: Option<&str>,
) -> Option<&'a Session> {
    let mut ordered: Vec<&Session> = sessions.iter().collect();
    ordered.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    ordered
        .iter()
        .find(|session| Some(session.id.as_str()) == current_id)
        .or_else(|| {
            ordered
                .iter()
                .find(|session| Some(session.name.as_str()) == previous_name)
        })
        .or_else(|| ordered.first())
        .copied()
}

/// Every saved profile the user checked, in profile order, with the active host
/// always among them under the live connection spec.
pub fn shown_host_profiles(
    profiles: &[HostProfile],
    active: &ConnectionSpec,
) -> Vec<ShownHostProfile> {
    let active_id = host_profile_id(active);
    let mut shown: Vec<ShownHostProfile> = profiles
        .iter()
        .filter_map(|profile| {
            if profile.id == active_id {
                Some(ShownHostProfile {
                    profile_id: active_id.clone(),
                    connection: active.clone(),
                })
            } else {
                profile.shown.then(|| ShownHostProfile {
                    profile_id: profile.id.clone(),
                    connection: profile.connection.clone(),
                })
            }
        })
        .collect();
    if !shown.iter().any(|host| host.profile_id == active_id) {
        shown.insert(
            0,
            ShownHostProfile {
                profile_id: active_id,
                connection: active.clone(),
            },
        );
    }
    shown
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tmux_control::TmuxSnapshot;

    use super::{super::connection_reducer::SnapshotAction, *};
    use crate::connection_ui::connection_reducer::ConnectionPhase;

    fn local() -> ConnectionSpec {
        ConnectionSpec::Local
    }

    fn remote(profile_id: &str, target: &str) -> ConnectionSpec {
        ConnectionSpec::Ssh {
            profile_id: profile_id.into(),
            target: target.into(),
            config_path: None,
        }
    }

    fn host(profile_id: &str, connection: ConnectionSpec) -> ShownHostProfile {
        ShownHostProfile {
            profile_id: profile_id.into(),
            connection,
        }
    }

    fn two_hosts() -> HostLinksState {
        let mut state = HostLinksState::default();
        host_links_reducer(
            &mut state,
            HostLinksAction::Sync(vec![
                host("local", local()),
                host("qa", remote("qa", "qa-host")),
            ]),
        );
        state
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

    fn world(sessions: &[(&str, &str)]) -> Arc<TmuxSnapshot> {
        Arc::new(TmuxSnapshot {
            sessions: sessions
                .iter()
                .map(|(id, name)| session(id, name))
                .collect(),
            windows: Vec::new(),
            panes: Vec::new(),
        })
    }

    fn snapshot_action(
        profile_id: &str,
        snapshot: Arc<TmuxSnapshot>,
        generation: u64,
    ) -> HostLinksAction {
        HostLinksAction::Host {
            profile_id: profile_id.into(),
            action: HostAction::Snapshot(SnapshotAction {
                snapshot: Some(snapshot),
                sequence: 0,
                generation: Some(generation),
                server_identity: "srv".into(),
            }),
        }
    }

    #[test]
    fn follows_the_shown_hosts_each_link_born_empty_under_its_own_epoch() {
        let state = two_hosts();
        assert_eq!(state.order, ["local", "qa"]);
        let local_link = state.link("local").unwrap();
        assert_eq!(local_link.connection_epoch, 0);
        assert_eq!(local_link.terminal_epoch, 0);
        assert_eq!(local_link.detail, "");
        assert_eq!(state.link("qa").unwrap().connection_epoch, 1);
        assert_eq!(state.link("qa").unwrap().client_id, None);
    }

    #[test]
    fn a_sync_that_changes_nothing_is_a_no_op_and_a_surviving_link_keeps_its_state() {
        let mut state = two_hosts();
        host_links_reducer(
            &mut state,
            HostLinksAction::Client {
                profile_id: "qa".into(),
                client_id: Some("client-2".into()),
            },
        );
        let before = state.clone();
        sync_host_links(
            &mut state,
            &[host("local", local()), host("qa", remote("qa", "qa-host"))],
        );
        assert_eq!(state, before);

        sync_host_links(
            &mut state,
            &[
                host("qa", remote("qa", "qa-host")),
                host("staging", remote("staging", "staging-host")),
            ],
        );
        assert_eq!(state.order, ["qa", "staging"]);
        assert!(state.link("local").is_none());
        assert_eq!(state.link("qa"), before.link("qa"));
        assert_eq!(state.link("staging").unwrap().connection_epoch, 2);
    }

    #[test]
    fn is_a_fixed_point_after_one_sync() {
        let hosts = [host("local", local()), host("qa", remote("qa", "qa-host"))];
        let mut state = HostLinksState::default();
        sync_host_links(&mut state, &hosts);
        let once = state.clone();
        sync_host_links(&mut state, &hosts);
        assert_eq!(state, once);
        let corrected = [host("local", local()), host("qa", remote("qa", "qa-2"))];
        sync_host_links(&mut state, &corrected);
        assert_ne!(state, once);
        let moved = state.clone();
        sync_host_links(&mut state, &corrected);
        assert_eq!(state, moved);
    }

    #[test]
    fn moves_a_link_onto_a_corrected_address_without_minting_an_epoch() {
        let mut state = two_hosts();
        let local_before = state.link("local").unwrap().clone();
        sync_host_links(
            &mut state,
            &[host("local", local()), host("qa", remote("qa", "qa-2"))],
        );
        assert_eq!(state.link("qa").unwrap().connection, remote("qa", "qa-2"));
        assert_eq!(state.link("qa").unwrap().connection_epoch, 1);
        assert_eq!(state.link("local"), Some(&local_before));
    }

    #[test]
    fn never_hands_a_profile_an_epoch_it_has_had_before() {
        let mut state = two_hosts();
        let mut used = vec![state.link("qa").unwrap().connection_epoch];
        host_links_reducer(
            &mut state,
            HostLinksAction::Reconnect {
                profile_id: "qa".into(),
            },
        );
        assert!(!used.contains(&state.link("qa").unwrap().connection_epoch));
        used.push(state.link("qa").unwrap().connection_epoch);
        assert_eq!(state.link("local").unwrap().connection_epoch, 0);

        host_links_reducer(
            &mut state,
            HostLinksAction::Remove {
                profile_id: "qa".into(),
            },
        );
        host_links_reducer(
            &mut state,
            HostLinksAction::Add {
                profile_id: "qa".into(),
                connection: remote("qa", "qa-host"),
            },
        );
        assert!(!used.contains(&state.link("qa").unwrap().connection_epoch));
        assert_eq!(state.order, ["local", "qa"]);
    }

    #[test]
    fn reconnects_every_link_at_once_with_distinct_fresh_epochs() {
        let mut state = two_hosts();
        host_links_reducer(&mut state, HostLinksAction::ReconnectAll);
        let epochs: Vec<u64> = state
            .order
            .iter()
            .map(|id| state.link(id).unwrap().connection_epoch)
            .collect();
        assert_ne!(epochs[0], epochs[1]);
        assert!(epochs.iter().all(|epoch| *epoch > 1));
    }

    #[test]
    fn records_a_links_client_epoch_and_detail_and_ignores_an_unknown_host() {
        let mut state = two_hosts();
        for action in [
            HostLinksAction::Client {
                profile_id: "qa".into(),
                client_id: Some("client-2".into()),
            },
            HostLinksAction::TerminalEpoch {
                profile_id: "qa".into(),
                terminal_epoch: 7,
            },
            HostLinksAction::Detail {
                profile_id: "qa".into(),
                detail: "Connection refused".into(),
            },
            HostLinksAction::Client {
                profile_id: "ghost".into(),
                client_id: Some("client-9".into()),
            },
        ] {
            host_links_reducer(&mut state, action);
        }
        let qa = state.link("qa").unwrap();
        assert_eq!(qa.client_id.as_deref(), Some("client-2"));
        assert_eq!(qa.terminal_epoch, 7);
        assert_eq!(qa.detail, "Connection refused");
        assert!(state.link("ghost").is_none());
    }

    #[test]
    fn reduces_host_state_per_link_and_a_snapshot_lands_on_a_remembered_session() {
        let mut state = two_hosts();
        let local_before = state.link("local").unwrap().clone();
        host_links_reducer(
            &mut state,
            HostLinksAction::Host {
                profile_id: "qa".into(),
                action: HostAction::Connection {
                    phase: ConnectionPhase::Connected,
                },
            },
        );
        host_links_reducer(
            &mut state,
            snapshot_action("qa", world(&[("$0", "home"), ("$1", "work")]), 1),
        );
        assert_eq!(
            state.link("qa").unwrap().host_state.phase,
            ConnectionPhase::Connected
        );
        assert_eq!(
            state.link("qa").unwrap().active_session_id.as_deref(),
            Some("$0")
        );
        assert_eq!(state.link("local"), Some(&local_before));

        host_links_reducer(
            &mut state,
            HostLinksAction::ActiveSession {
                profile_id: "qa".into(),
                session_id: Some("$1".into()),
            },
        );
        // A rename keeps the session: the id is what the user is on.
        host_links_reducer(
            &mut state,
            snapshot_action("qa", world(&[("$0", "home"), ("$1", "renamed")]), 2),
        );
        assert_eq!(
            state.link("qa").unwrap().active_session_id.as_deref(),
            Some("$1")
        );
        // A recreated session with the remembered name is found by that name.
        host_links_reducer(
            &mut state,
            snapshot_action("qa", world(&[("$0", "home"), ("$5", "renamed")]), 3),
        );
        assert_eq!(
            state.link("qa").unwrap().active_session_id.as_deref(),
            Some("$5")
        );
        // Gone entirely, the first session is where the host lands.
        host_links_reducer(
            &mut state,
            snapshot_action("qa", world(&[("$0", "home")]), 4),
        );
        assert_eq!(
            state.link("qa").unwrap().active_session_id.as_deref(),
            Some("$0")
        );
    }

    fn windowed(active: &str) -> Arc<TmuxSnapshot> {
        let window = |id: &str, index| Window {
            id: id.into(),
            session_id: "$0".into(),
            index,
            name: id.into(),
            active: id == active,
            layout: String::new(),
            zoomed: false,
            pinned: false,
        };
        Arc::new(TmuxSnapshot {
            windows: vec![window("@2", 1), window("@1", 0)],
            ..(*world(&[("$0", "home"), ("$1", "work")])).clone()
        })
    }

    #[test]
    fn a_switch_painted_ahead_of_the_host_holds_until_a_snapshot_catches_up() {
        let mut state = two_hosts();
        host_links_reducer(&mut state, snapshot_action("qa", windowed("@1"), 1));
        assert_eq!(
            state.link("qa").unwrap().active_window_id.as_deref(),
            Some("@1")
        );
        let guard = |through| OptimisticWindow {
            session_id: "$0".into(),
            window_id: Some("@2".into()),
            through_generation: through,
        };
        host_links_reducer(
            &mut state,
            HostLinksAction::Optimistic {
                profile_id: "qa".into(),
                guard: Some(guard(None)),
            },
        );
        // Snapshots still naming the old window do not snap the shell back.
        host_links_reducer(&mut state, snapshot_action("qa", windowed("@1"), 2));
        assert_eq!(
            state.link("qa").unwrap().active_window_id.as_deref(),
            Some("@2")
        );
        host_links_reducer(
            &mut state,
            HostLinksAction::Optimistic {
                profile_id: "qa".into(),
                guard: Some(guard(Some(4))),
            },
        );
        host_links_reducer(&mut state, snapshot_action("qa", windowed("@1"), 3));
        assert!(state.link("qa").unwrap().optimistic_window.is_some());
        // Caught up: the host's own answer stands again.
        host_links_reducer(&mut state, snapshot_action("qa", windowed("@1"), 4));
        let link = state.link("qa").unwrap();
        assert_eq!(link.optimistic_window, None);
        assert_eq!(link.active_window_id.as_deref(), Some("@1"));
    }

    #[test]
    fn a_reconnect_drops_a_guard_its_switch_can_no_longer_release() {
        let mut state = two_hosts();
        host_links_reducer(&mut state, snapshot_action("qa", windowed("@1"), 1));
        host_links_reducer(
            &mut state,
            HostLinksAction::Optimistic {
                profile_id: "qa".into(),
                guard: Some(OptimisticWindow {
                    session_id: "$0".into(),
                    window_id: Some("@2".into()),
                    through_generation: None,
                }),
            },
        );
        host_links_reducer(
            &mut state,
            HostLinksAction::Reconnect {
                profile_id: "qa".into(),
            },
        );
        host_links_reducer(&mut state, snapshot_action("qa", windowed("@1"), 1));
        let link = state.link("qa").unwrap();
        assert_eq!(link.optimistic_window, None);
        assert_eq!(link.active_window_id.as_deref(), Some("@1"));
    }

    #[test]
    fn selecting_another_workspace_releases_the_guard() {
        let mut state = two_hosts();
        host_links_reducer(&mut state, snapshot_action("qa", windowed("@1"), 1));
        host_links_reducer(
            &mut state,
            HostLinksAction::Optimistic {
                profile_id: "qa".into(),
                guard: Some(OptimisticWindow {
                    session_id: "$0".into(),
                    window_id: Some("@2".into()),
                    through_generation: None,
                }),
            },
        );
        host_links_reducer(
            &mut state,
            HostLinksAction::ActiveSession {
                profile_id: "qa".into(),
                session_id: Some("$1".into()),
            },
        );
        let link = state.link("qa").unwrap();
        assert_eq!(link.optimistic_window, None);
        assert_eq!(link.active_window_id, None, "$1 has no windows");
    }

    #[test]
    fn resolves_the_window_in_order_of_claim() {
        let snapshot = windowed("@1");
        let windows = session_windows(&snapshot.windows, "$0");
        assert_eq!(
            windows.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(),
            ["@1", "@2"]
        );
        assert_eq!(
            resolve_active_window_id(&windows, Some("@2"), Some("@2")),
            Some("@2")
        );
        assert_eq!(
            resolve_active_window_id(&windows, Some("@2"), Some("@9")),
            Some("@1")
        );
        let none_active: Vec<&Window> = Vec::new();
        assert_eq!(
            resolve_active_window_id(&none_active, Some("@2"), None),
            None
        );
    }

    fn profile(id: &str, label: &str, connection: ConnectionSpec, shown: bool) -> HostProfile {
        HostProfile {
            id: id.into(),
            label: label.into(),
            connection,
            letter: None,
            shown,
        }
    }

    fn profiles() -> Vec<HostProfile> {
        vec![
            profile("local", "Local", local(), true),
            profile("qa", "qa", remote("qa", "qa-host"), true),
            profile("prod", "prod", remote("prod", "prod-host"), false),
        ]
    }

    #[test]
    fn lists_the_checked_profiles_in_profile_order_under_their_connection() {
        assert_eq!(
            shown_host_profiles(&profiles(), &local()),
            [host("local", local()), host("qa", remote("qa", "qa-host"))]
        );
    }

    #[test]
    fn always_includes_the_active_host_under_the_live_connection() {
        let editing = remote("prod", "prod-host-2");
        assert_eq!(
            shown_host_profiles(&profiles(), &editing),
            [
                host("local", local()),
                host("qa", remote("qa", "qa-host")),
                host("prod", editing.clone()),
            ]
        );
        let unsaved = remote("new", "new-host");
        assert_eq!(
            shown_host_profiles(&profiles(), &unsaved)[0],
            host("new", unsaved.clone())
        );
    }
}
