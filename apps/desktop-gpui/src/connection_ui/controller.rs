//! Every shown host's bridge, and the connection state the app reads. Port of
//! `app/useAppConnectionController.ts` and `app/useAppHostSettingsActions.ts`,
//! with the helper and app-tab recovery hooks wired in as `App.tsx` does.
//!
//! The React version recomputes through a render after every change. Here each
//! public entry point changes state with plain dispatches and then calls
//! [`ConnectionController::settle`] once.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
    time::Duration,
};

use desktop_core::{
    app_state::{AppStateStore, PersistedAppState, load_app_state},
    connection::{
        ConnectionSpec, ProfileStore,
        helper::{HelperInstallReport, HelperRollback},
        profiles::{
            HostProfile, delete_host_profile, list_host_profiles, reset_host_profiles,
            save_host_profile, set_last_profile_id,
        },
    },
    platform::EventBody,
};
use futures::{StreamExt, channel::mpsc};
use gpui_kit::{App, AppContext, Context, Entity, EventEmitter, Subscription, Task};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use terminal_engine::Command;
use tmux_control::Session;

use crate::terminal::{TerminalLink, link::LinkEvent};

use super::{
    app_recovery::{
        AppRecoveryAction, AppRecoveryPhase, AppRecoveryState, app_recovery_reducer,
        discard_server_app_state, reconcile_workspace_identity,
        recover_app_tabs_from_previous_server, recoverable_app_tab_count,
    },
    backend::ConnectionBackend,
    bridge_events::{AckBoundary, BridgeEvent, ChannelSink, DeliveryAcks, decode_frame},
    connection_reducer::{ConnectionPhase, HostAction, NormalizedHostState, SnapshotAction},
    helper_recovery::{HelperRecovery, ProbeRequest},
    helper_upgrade::{
        HelperOperation, HelperUpgradeAction, HelperUpgradeState, helper_upgrade_reducer,
    },
    host_links::{
        HostLink, HostLinksAction, HostLinksState, host_links_reducer, shown_host_profiles,
    },
    host_profiles::{
        LOCAL_PROFILE_ID, helper_connection_key, host_profile_id, profile_id_for_ssh_connection,
    },
    host_scope::{HostScopeToken, same_helper_install_connection, same_host_connection},
    link_quality::{LinkQualityChange, LinkQualityMonitor, describe_link_quality},
    resume_recovery::{
        CHECK_INTERVAL_MS, RESUME_PROBE_TIMEOUT_MS, ResumeDedupe, ResumeGapDetector,
        ResumeProbeOutcome, ResumeTransitionDetector, ResumeTrigger, probe_resumed_link,
    },
};

#[cfg(test)]
mod tests;

/// The native supervisor appends why this side tore a bridge down, for the
/// journal; the words a person reads stay the plain failure.
const LOCAL_TEARDOWN_MARKER: &str = " (torn down locally:";

pub fn user_facing_bridge_failure(detail: &str) -> &str {
    detail
        .find(LOCAL_TEARDOWN_MARKER)
        .map_or(detail, |marker| &detail[..marker])
}

/// The bridge supervisor says one of these for every failure of that stage,
/// and it is the only thing separating "no helper" from "cannot be reached".
static HANDSHAKE_FAILURE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)handshake|connection setup").expect("static pattern"));

const MAX_FRAME_BATCH: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionMode {
    Local,
    Ssh,
}

/// A saved-hosts file that could not be read and was set aside.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRecovery {
    pub error: String,
    pub preserved_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedProfiles {
    profiles: Vec<HostProfile>,
    last_profile_id: Option<String>,
    recovery: Option<ProfileRecovery>,
}

pub enum ConnectionEvent {
    /// The app's status channel: every write, including repeats.
    Status(String),
}

impl EventEmitter<ConnectionEvent> for ConnectionController {}

struct TopologyBurst {
    started_at: u64,
    name: String,
    count: u32,
}

/// One native bridge, from `start_terminal` to `stop_terminal`.
struct RunningBridge {
    key: (ConnectionSpec, u64),
    /// Tells this bridge from a later one on the same link.
    token: u64,
    client_id: Option<String>,
    terminal_epoch: u64,
    acks: DeliveryAcks,
    recovering_flow_stall: bool,
    topology_burst: Option<TopologyBurst>,
    _pump: Task<()>,
}

#[derive(Default)]
struct LinkRuntime {
    bridge: Option<RunningBridge>,
    phase: ConnectionPhase,
    degraded_since: Option<u64>,
    /// The host's terminal engine, created with its first bridge. It outlives
    /// any one bridge, as the panes it holds do.
    terminal: Option<Entity<TerminalLink>>,
    terminal_events: Option<Subscription>,
    /// The tmux server the engine's screens describe.
    server_identity: Option<String>,
}

pub struct ConnectionController {
    backend: Arc<dyn ConnectionBackend>,
    profile_store: Arc<ProfileStore>,
    app_state: Arc<AppStateStore>,
    links: HostLinksState,
    connection: ConnectionSpec,
    connection_mode: ConnectionMode,
    ssh_target: String,
    ssh_config_path: String,
    profiles: Vec<HostProfile>,
    selected_profile_id: String,
    profiles_hydrated: bool,
    profile_recovery: Option<ProfileRecovery>,
    helper: HelperUpgradeState,
    helper_recovery: HelperRecovery,
    link_quality: LinkQualityMonitor,
    link_quality_verdict: String,
    link_quality_profile: Option<String>,
    seen_late_requests: Option<(String, u32)>,
    runtimes: HashMap<String, LinkRuntime>,
    next_bridge_token: u64,
    app_recovery: Option<AppRecoveryState>,
    last_identities: HashMap<String, String>,
    observed_host: Option<(String, Option<String>, Vec<Session>)>,
    /// Written, and not yet durably in the store; the base of the next change.
    unsaved_app_state: Option<PersistedAppState>,
    app_state_saves: u64,
    app_state_save: Option<Task<()>>,
    status: String,
    resume_gap: ResumeGapDetector,
    resume_network: ResumeTransitionDetector,
    resume_dedupe: ResumeDedupe,
    window_active: bool,
    _ticker: Task<()>,
}

impl ConnectionController {
    pub fn new(
        backend: Arc<dyn ConnectionBackend>,
        profile_store: Arc<ProfileStore>,
        app_state: Arc<AppStateStore>,
        cx: &mut Context<Self>,
    ) -> Self {
        let now = backend.now_ms();
        let online = backend.network_online();
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(CHECK_INTERVAL_MS))
                    .await;
                let tick = this.update(cx, |this, cx| {
                    let foreground = cx.active_window().is_some();
                    this.tick(foreground, cx);
                });
                if tick.is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            backend,
            profile_store,
            app_state,
            links: HostLinksState::default(),
            connection: ConnectionSpec::Local,
            connection_mode: ConnectionMode::Local,
            ssh_target: String::new(),
            ssh_config_path: String::new(),
            profiles: Vec::new(),
            selected_profile_id: String::new(),
            profiles_hydrated: false,
            profile_recovery: None,
            helper: HelperUpgradeState::Idle,
            helper_recovery: HelperRecovery::default(),
            link_quality: LinkQualityMonitor::default(),
            link_quality_verdict: String::new(),
            link_quality_profile: None,
            seen_late_requests: None,
            runtimes: HashMap::new(),
            next_bridge_token: 0,
            app_recovery: None,
            last_identities: HashMap::new(),
            observed_host: None,
            unsaved_app_state: None,
            app_state_saves: 0,
            app_state_save: None,
            status: String::new(),
            resume_gap: ResumeGapDetector::new(now),
            resume_network: ResumeTransitionDetector::new(!online),
            resume_dedupe: ResumeDedupe::default(),
            window_active: true,
            _ticker: ticker,
        };
        this.hydrate_profiles(cx);
        this
    }

    fn hydrate_profiles(&mut self, cx: &mut Context<Self>) {
        let saved = list_host_profiles(&self.profile_store).and_then(|value| {
            serde_json::from_value::<PersistedProfiles>(value).map_err(|error| error.to_string())
        });
        match saved {
            Ok(saved) => {
                self.profiles = saved.profiles;
                self.profile_recovery = saved.recovery;
                let selected = self
                    .profiles
                    .iter()
                    .find(|profile| Some(&profile.id) == saved.last_profile_id.as_ref())
                    .cloned();
                if let Some(selected) = selected {
                    self.show_in_form(&selected);
                    self.connection = selected.connection;
                    self.selected_profile_id = selected.id;
                }
            }
            Err(error) => self.set_status(error, cx),
        }
        self.profiles_hydrated = true;
        self.settle(cx);
    }

    // Read by the UI.

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn connection(&self) -> &ConnectionSpec {
        &self.connection
    }

    pub fn connection_mode(&self) -> ConnectionMode {
        self.connection_mode
    }

    pub fn ssh_target(&self) -> &str {
        &self.ssh_target
    }

    pub fn ssh_config_path(&self) -> &str {
        &self.ssh_config_path
    }

    pub fn profiles(&self) -> &[HostProfile] {
        &self.profiles
    }

    pub fn selected_profile_id(&self) -> &str {
        &self.selected_profile_id
    }

    pub fn profile_recovery(&self) -> Option<&ProfileRecovery> {
        self.profile_recovery.as_ref()
    }

    pub fn helper(&self) -> &HelperUpgradeState {
        &self.helper
    }

    pub fn app_recovery(&self) -> Option<&AppRecoveryState> {
        self.app_recovery.as_ref()
    }

    pub fn links(&self) -> impl Iterator<Item = &HostLink> {
        self.links.order.iter().filter_map(|id| self.links.link(id))
    }

    pub fn link(&self, profile_id: &str) -> Option<&HostLink> {
        self.links.link(profile_id)
    }

    pub fn active_profile_id(&self) -> String {
        host_profile_id(&self.connection)
    }

    pub fn active_link(&self) -> Option<&HostLink> {
        self.links.link(&self.active_profile_id())
    }

    /// The terminal link of a host, once it has had a bridge.
    pub fn terminal_link(&self, profile_id: &str) -> Option<Entity<TerminalLink>> {
        self.runtimes.get(profile_id)?.terminal.clone()
    }

    /// A message for the status bar from outside this module.
    pub fn report_status(&mut self, message: String, cx: &mut Context<Self>) {
        self.set_status(message, cx);
    }

    pub fn active_host_state(&self) -> Option<&NormalizedHostState> {
        self.active_link().map(|link| &link.host_state)
    }

    pub fn active_host_label(&self) -> String {
        let id = self.active_profile_id();
        self.profiles
            .iter()
            .find(|profile| profile.id == id)
            .map(|profile| profile.label.clone())
            .or_else(|| match &self.connection {
                ConnectionSpec::Local => None,
                ConnectionSpec::Ssh { target, .. } => Some(target.clone()),
            })
            .unwrap_or_else(|| "Local".to_owned())
    }

    /// The saved host the form is editing, when it is one Delete may remove.
    pub fn deletable_profile(&self) -> Option<&HostProfile> {
        if self.profiles.len() <= 1 {
            return None;
        }
        self.profiles
            .iter()
            .find(|profile| profile.id == self.selected_profile_id)
    }

    // Setting the connection form.

    pub fn set_ssh_target(&mut self, value: String, cx: &mut Context<Self>) {
        self.ssh_target = value;
        cx.notify();
    }

    pub fn set_ssh_config_path(&mut self, value: String, cx: &mut Context<Self>) {
        self.ssh_config_path = value;
        cx.notify();
    }

    /// A saved host does not change transport in place.
    pub fn set_connection_mode(&mut self, mode: ConnectionMode, cx: &mut Context<Self>) {
        self.selected_profile_id.clear();
        self.connection_mode = mode;
        cx.notify();
    }

    /// An empty form with no saved host behind it.
    pub fn add_host(&mut self, cx: &mut Context<Self>) {
        self.selected_profile_id.clear();
        self.ssh_target.clear();
        self.ssh_config_path.clear();
        self.connection_mode = ConnectionMode::Ssh;
        cx.notify();
    }

    pub fn select_profile(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        self.selected_profile_id = profile_id.to_owned();
        if let Some(profile) = self
            .profiles
            .iter()
            .find(|item| item.id == profile_id)
            .cloned()
        {
            self.show_in_form(&profile);
        }
        cx.notify();
    }

    fn show_in_form(&mut self, profile: &HostProfile) {
        match &profile.connection {
            ConnectionSpec::Local => self.connection_mode = ConnectionMode::Local,
            ConnectionSpec::Ssh {
                target,
                config_path,
                ..
            } => {
                self.connection_mode = ConnectionMode::Ssh;
                self.ssh_target = target.clone();
                self.ssh_config_path = config_path.clone().unwrap_or_default();
            }
        }
    }

    // Connecting.

    /// Connects to the host the form describes and makes it the one on screen,
    /// on a fresh bridge. The connection moves first: the reset, the cleared
    /// selection and the new epoch all land on the host being connected to.
    pub fn connect(&mut self, cx: &mut Context<Self>) {
        let edited = self.is_editing_saved_ssh_host();
        let (profile, status) = match self.connection_mode {
            ConnectionMode::Local => (self.local_profile(), "Discovering local tmux…".to_owned()),
            ConnectionMode::Ssh => {
                let target = self.ssh_target.trim().to_owned();
                if target.is_empty() {
                    self.set_status("Enter an SSH host or config alias.".to_owned(), cx);
                    return;
                }
                let status = format!("Connecting to {target}…");
                (self.ssh_profile(target), status)
            }
        };
        self.connection = profile.connection.clone();
        self.reset_active_host();
        self.bump_active_epoch();
        self.selected_profile_id = profile.id.clone();
        self.remember_profile(profile.clone(), edited);
        self.save_active_profile(profile, cx);
        self.set_status(status, cx);
        self.settle(cx);
    }

    fn is_editing_saved_ssh_host(&self) -> bool {
        self.profiles.iter().any(|profile| {
            profile.id == self.selected_profile_id
                && matches!(profile.connection, ConnectionSpec::Ssh { .. })
        })
    }

    /// An edit keeps its place: a machine that jumped to the bottom every time
    /// its address was corrected would read as a different one.
    fn remember_profile(&mut self, profile: HostProfile, edited: bool) {
        if profile.id == LOCAL_PROFILE_ID || edited {
            match self.profiles.iter_mut().find(|item| item.id == profile.id) {
                Some(existing) => *existing = profile,
                None => self.profiles.insert(0, profile),
            }
        } else {
            self.profiles.retain(|item| item.id != profile.id);
            self.profiles.push(profile);
        }
    }

    fn local_profile(&self) -> HostProfile {
        let letter = self
            .profiles
            .iter()
            .find(|item| item.id == LOCAL_PROFILE_ID)
            .and_then(|item| item.letter.clone());
        HostProfile {
            id: LOCAL_PROFILE_ID.to_owned(),
            label: "Local".to_owned(),
            connection: ConnectionSpec::Local,
            letter,
            shown: true,
        }
    }

    /// The saved host being edited keeps its id, so correcting an address moves
    /// that host rather than leaving a second entry beside it.
    fn ssh_profile(&self, target: String) -> HostProfile {
        let config_path = self.ssh_config_path.trim().to_owned();
        let edited = self.profiles.iter().find(|profile| {
            profile.id == self.selected_profile_id
                && matches!(profile.connection, ConnectionSpec::Ssh { .. })
        });
        let id = edited.map_or_else(
            || profile_id_for_ssh_connection(&self.profiles, &target, &config_path),
            |profile| profile.id.clone(),
        );
        HostProfile {
            id: id.clone(),
            label: target.clone(),
            connection: ConnectionSpec::Ssh {
                profile_id: id,
                target,
                config_path: (!config_path.is_empty()).then_some(config_path),
            },
            letter: edited.and_then(|profile| profile.letter.clone()),
            shown: true,
        }
    }

    /// As it drops into `reset_active_host`, the selection goes too.
    fn reset_active_host(&mut self) {
        let profile_id = self.active_profile_id();
        self.dispatch(HostLinksAction::Host {
            profile_id: profile_id.clone(),
            action: HostAction::Reset,
        });
        self.dispatch(HostLinksAction::ActiveSession {
            profile_id: profile_id.clone(),
            session_id: None,
        });
        self.dispatch(HostLinksAction::ActiveWindow {
            profile_id,
            window_id: None,
        });
    }

    fn bump_active_epoch(&mut self) {
        self.dispatch(HostLinksAction::Reconnect {
            profile_id: self.active_profile_id(),
        });
    }

    /// Saves the host and makes it the one a restart reconnects to.
    fn save_active_profile(&mut self, profile: HostProfile, cx: &mut Context<Self>) {
        let id = profile.id.clone();
        let result = save_host_profile(profile, &self.profile_store)
            .and_then(|()| set_last_profile_id(id, &self.profile_store));
        if let Err(error) = result {
            self.set_status(error, cx);
        }
    }

    /// Restarts the active host's bridge.
    pub fn reconnect(&mut self, cx: &mut Context<Self>) {
        self.bump_active_epoch();
        self.settle(cx);
    }

    pub fn reconnect_host(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        self.dispatch(HostLinksAction::Reconnect {
            profile_id: profile_id.to_owned(),
        });
        self.settle(cx);
    }

    /// Makes another shown host the one on screen. The pointer moves; the
    /// bridges do not.
    pub fn activate_host(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        let target = self
            .links
            .link(profile_id)
            .map(|link| link.connection.clone())
            .or_else(|| {
                self.profiles
                    .iter()
                    .find(|item| item.id == profile_id)
                    .map(|item| item.connection.clone())
            });
        let Some(target) = target else {
            return;
        };
        if let Err(error) = set_last_profile_id(profile_id.to_owned(), &self.profile_store) {
            self.set_status(error, cx);
        }
        if profile_id == self.active_profile_id() {
            return;
        }
        self.connection = target;
        // The form follows the host on screen, so Connect cannot go back.
        if let Some(profile) = self
            .profiles
            .iter()
            .find(|item| item.id == profile_id)
            .cloned()
        {
            self.selected_profile_id = profile.id.clone();
            self.show_in_form(&profile);
        }
        self.settle(cx);
    }

    pub fn save_profile_fields(
        &mut self,
        profile_id: &str,
        letter: Option<String>,
        shown: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.profiles.iter().position(|item| item.id == profile_id) else {
            return;
        };
        let previous = self.profiles[index].clone();
        let mut next = previous.clone();
        next.letter = letter.filter(|value| !value.is_empty());
        // The host on screen is always shown, whoever asks.
        next.shown = shown || profile_id == self.active_profile_id();
        self.profiles[index] = next.clone();
        // Optimistic, because the link set follows `profiles` at once; a store
        // that refuses puts the saved shape back.
        if let Err(error) = save_host_profile(next, &self.profile_store) {
            self.profiles[index] = previous;
            self.set_status(error, cx);
        }
        self.settle(cx);
    }

    pub fn delete_profile(&mut self, profile: &HostProfile, cx: &mut Context<Self>) {
        let saved =
            delete_host_profile(profile.id.clone(), &self.profile_store).and_then(|value| {
                serde_json::from_value::<PersistedProfiles>(value)
                    .map_err(|error| error.to_string())
            });
        match saved {
            Ok(saved) => {
                self.profiles = saved.profiles;
                self.selected_profile_id.clear();
            }
            Err(error) => self.set_status(
                format!("Could not delete the saved host {}: {error}", profile.label),
                cx,
            ),
        }
        self.settle(cx);
    }

    pub fn confirm_profile_reset(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = reset_host_profiles(&self.profile_store) {
            self.set_status(
                format!("Could not reset recovered host profiles: {error}"),
                cx,
            );
            return;
        }
        self.profiles = vec![HostProfile {
            id: LOCAL_PROFILE_ID.to_owned(),
            label: "Local".to_owned(),
            connection: ConnectionSpec::Local,
            letter: None,
            shown: false,
        }];
        self.profile_recovery = None;
        self.reset_active_host();
        self.connection = ConnectionSpec::Local;
        self.connection_mode = ConnectionMode::Local;
        self.bump_active_epoch();
        self.set_status(
            "Recovered host profiles were reset to Local; the invalid original remains preserved."
                .to_owned(),
            cx,
        );
        self.settle(cx);
    }

    // The remote helper.

    /// The Settings button: informational, it never opens the upgrade prompt.
    pub fn probe_helper(&mut self, cx: &mut Context<Self>) {
        let request = self
            .helper_recovery
            .probe_manually(&self.connection.clone(), &mut self.helper);
        self.run_helper_probe(request, cx);
        cx.notify();
    }

    pub fn request_helper_install(&mut self, cx: &mut Context<Self>) {
        helper_upgrade_reducer(&mut self.helper, HelperUpgradeAction::RequestUpgrade);
        cx.notify();
    }

    pub fn cancel_helper_install(&mut self, cx: &mut Context<Self>) {
        helper_upgrade_reducer(&mut self.helper, HelperUpgradeAction::CancelUpgrade);
        cx.notify();
    }

    pub fn confirm_helper_install(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.connection, ConnectionSpec::Ssh { .. }) {
            return;
        }
        let HelperUpgradeState::Confirming {
            connection_key,
            probe,
            operation,
        } = &self.helper
        else {
            return;
        };
        if *connection_key != helper_connection_key(&self.connection) {
            return;
        }
        let connection_key = connection_key.clone();
        let operation = *operation;
        let allow_upgrade = probe.installed;
        let scope = self.active_scope();
        helper_upgrade_reducer(&mut self.helper, HelperUpgradeAction::Upgrade);
        let install = self
            .backend
            .install_helper(self.connection.clone(), allow_upgrade);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = install.await;
            let _ = this.update(cx, |this, cx| {
                this.finish_helper_install(scope, connection_key, operation, result, cx);
            });
        })
        .detach();
    }

    fn finish_helper_install(
        &mut self,
        scope: HostScopeToken,
        connection_key: String,
        operation: HelperOperation,
        result: Result<HelperInstallReport, String>,
        cx: &mut Context<Self>,
    ) {
        if !same_helper_install_connection(&scope, &self.active_scope()) {
            return;
        }
        let (installed_word, failed_word) = match operation {
            HelperOperation::Install => ("installed", "install"),
            HelperOperation::Upgrade => ("upgraded", "upgrade"),
        };
        match result {
            Ok(report) if report.ok => {
                helper_upgrade_reducer(
                    &mut self.helper,
                    HelperUpgradeAction::UpgradeSucceeded {
                        connection_key,
                        message: report.message,
                    },
                );
                self.dispatch(HostLinksAction::Detail {
                    profile_id: self.active_profile_id(),
                    detail: format!(
                        "Remote helper {installed_word}; reconnecting for a fresh authoritative snapshot."
                    ),
                });
                self.bump_active_epoch();
            }
            Ok(report) => {
                self.set_status(
                    format!("Helper {failed_word} failed: {}", report.message),
                    cx,
                );
                helper_upgrade_reducer(
                    &mut self.helper,
                    HelperUpgradeAction::UpgradeFailed {
                        connection_key,
                        message: report.message,
                        rollback: report.rollback,
                    },
                );
            }
            Err(error) => {
                self.set_status(format!("Helper {failed_word} failed: {error}"), cx);
                helper_upgrade_reducer(
                    &mut self.helper,
                    HelperUpgradeAction::UpgradeFailed {
                        connection_key,
                        message: error,
                        rollback: HelperRollback::NotNeeded,
                    },
                );
            }
        }
        self.settle(cx);
    }

    fn run_helper_probe(&mut self, request: Option<ProbeRequest>, cx: &mut Context<Self>) {
        let Some(request) = request else {
            return;
        };
        let probe = self.backend.probe_helper(request.connection);
        cx.spawn(async move |this, cx| {
            let result = probe.await;
            let _ = this.update(cx, |this, cx| {
                this.helper_recovery
                    .settle(request.generation, result, &mut this.helper);
                cx.notify();
            });
        })
        .detach();
    }

    // App tabs left by a replaced server.

    pub fn restore_app_tabs(&mut self, cx: &mut Context<Self>) {
        let Some(offer) = self.current_offer(cx) else {
            return;
        };
        let sessions = self.active_sessions();
        self.update_app_state(
            |state| {
                recover_app_tabs_from_previous_server(
                    state,
                    &offer.scope.host_profile_id,
                    &offer.previous_server_identity,
                    offer.scope.server_identity.as_deref().unwrap_or_default(),
                    &sessions,
                );
                true
            },
            cx,
        );
        app_recovery_reducer(&mut self.app_recovery, AppRecoveryAction::Clear);
        cx.notify();
    }

    pub fn request_app_tab_discard(&mut self, cx: &mut Context<Self>) {
        app_recovery_reducer(&mut self.app_recovery, AppRecoveryAction::ConfirmDiscard);
        cx.notify();
    }

    pub fn cancel_app_tab_discard(&mut self, cx: &mut Context<Self>) {
        app_recovery_reducer(&mut self.app_recovery, AppRecoveryAction::CancelDiscard);
        cx.notify();
    }

    pub fn confirm_app_tab_discard(&mut self, cx: &mut Context<Self>) {
        if !self.app_recovery_modal_open() {
            return;
        }
        let Some(offer) = self.current_offer(cx) else {
            return;
        };
        self.update_app_state(
            |state| {
                discard_server_app_state(
                    state,
                    &offer.scope.host_profile_id,
                    &offer.previous_server_identity,
                );
                true
            },
            cx,
        );
        app_recovery_reducer(&mut self.app_recovery, AppRecoveryAction::Clear);
        cx.notify();
    }

    /// The offer, unless the connection it was made for is gone.
    fn current_offer(&mut self, cx: &mut Context<Self>) -> Option<AppRecoveryState> {
        let offer = self.app_recovery.clone()?;
        if same_host_connection(&offer.scope, &self.active_scope()) {
            return Some(offer);
        }
        app_recovery_reducer(&mut self.app_recovery, AppRecoveryAction::Clear);
        cx.notify();
        None
    }

    fn active_sessions(&self) -> Vec<Session> {
        self.active_host_state()
            .map(|state| state.snapshot.sessions.clone())
            .unwrap_or_default()
    }

    fn current_app_state(&self) -> Result<PersistedAppState, String> {
        match &self.unsaved_app_state {
            Some(state) => Ok(state.clone()),
            None => load_app_state(&self.app_state),
        }
    }

    /// Saves are chained so an older one can never land after a newer one.
    fn update_app_state(
        &mut self,
        change: impl FnOnce(&mut PersistedAppState) -> bool,
        cx: &mut Context<Self>,
    ) {
        // A frozen store is reported by the shell that owns it.
        let Ok(mut state) = self.current_app_state() else {
            return;
        };
        if !change(&mut state) {
            return;
        }
        self.unsaved_app_state = Some(state.clone());
        self.app_state_saves += 1;
        let save_number = self.app_state_saves;
        let save = self.backend.save_app_state(state);
        let previous = self.app_state_save.take();
        self.app_state_save = Some(cx.spawn(async move |this, cx| {
            if let Some(previous) = previous {
                previous.await;
            }
            let result = save.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) if this.app_state_saves == save_number => {
                        this.unsaved_app_state = None;
                    }
                    Ok(()) => {}
                    // Kept as the base of the next change, which saves it again.
                    Err(error) => this.set_status(
                        format!(
                            "Could not save application tabs; changes will be retried: {error}"
                        ),
                        cx,
                    ),
                }
            });
        }));
    }

    // Resume from sleep.

    /// The native wake notification, from `desktop-core`'s power events.
    pub fn desktop_resumed(&mut self, cx: &mut Context<Self>) {
        self.request_resume(ResumeTrigger::Native, cx);
    }

    fn tick(&mut self, foreground: bool, cx: &mut Context<Self>) {
        let now = self.backend.now_ms();
        let foreground_changed = foreground != self.window_active;
        self.window_active = foreground;
        // A window that is not in front is ordinary desktop use, not a suspend.
        if !foreground || foreground_changed {
            self.resume_gap.reset(now);
        } else if self.resume_gap.observe(now) {
            self.request_resume(ResumeTrigger::TimerGap, cx);
        }
        if self.resume_network.network(self.backend.network_online()) {
            self.request_resume(ResumeTrigger::Online, cx);
        }
        self.poll_late_requests(now, cx);
        if !self.link_quality_verdict.is_empty() {
            let change = self.link_quality.poll(now);
            self.apply_link_quality(change, cx);
            cx.notify();
        }
    }

    fn request_resume(&mut self, trigger: ResumeTrigger, cx: &mut Context<Self>) {
        if !self.resume_dedupe.admit(self.backend.now_ms()) || !self.profiles_hydrated {
            return;
        }
        // Only a link that does not answer costs a rebuild, of every host:
        // they all slept through the same suspend.
        let profile_id = self.active_profile_id();
        let connected = self
            .active_host_state()
            .is_some_and(|state| state.phase == ConnectionPhase::Connected);
        let probe = self
            .runtimes
            .get(&profile_id)
            .and_then(|runtime| runtime.bridge.as_ref())
            .and_then(|bridge| {
                Some((
                    bridge.client_id.clone()?,
                    bridge.token,
                    bridge.terminal_epoch,
                ))
            })
            .filter(|_| connected);
        let Some((client_id, token, epoch)) = probe else {
            self.rebuild_after_resume(trigger, "skipped", cx);
            return;
        };
        let answer = self.backend.probe_link(client_id);
        let timeout = cx
            .background_executor()
            .timer(Duration::from_millis(RESUME_PROBE_TIMEOUT_MS));
        cx.spawn(async move |this, cx| {
            let outcome = probe_resumed_link(answer, timeout).await;
            let _ = this.update(cx, |this, cx| {
                // The bridge that was probed is the one the outcome speaks
                // for. A replaced link is already the rebuild this would ask
                // for; a host switch meanwhile is neither.
                let same_bridge = this
                    .runtimes
                    .get(&profile_id)
                    .and_then(|runtime| runtime.bridge.as_ref())
                    .is_some_and(|bridge| bridge.token == token && bridge.terminal_epoch == epoch);
                if !same_bridge {
                    return;
                }
                match outcome {
                    ResumeProbeOutcome::Alive => {
                        this.incident("resume.linkAlive", json!({ "trigger": trigger.name() }));
                    }
                    ResumeProbeOutcome::Dead => {
                        this.rebuild_after_resume(trigger, outcome.name(), cx);
                    }
                }
            });
        })
        .detach();
    }

    fn rebuild_after_resume(
        &mut self,
        trigger: ResumeTrigger,
        probe: &str,
        cx: &mut Context<Self>,
    ) {
        self.incident(
            "reconnect.desktopResume",
            json!({ "trigger": trigger.name(), "probe": probe }),
        );
        for profile_id in self.links.order.clone() {
            self.dispatch(HostLinksAction::Detail {
                profile_id,
                detail: "System resumed; reconnecting for an authoritative state refresh."
                    .to_owned(),
            });
        }
        cx.notify();
        // The SSH master outlives the suspend and still answers locally, but
        // its TCP connection did not. Fresh masters reconnect at once.
        let discard = self.backend.discard_ssh_masters();
        cx.spawn(async move |this, cx| {
            let _ = discard.await;
            let _ = this.update(cx, |this, cx| {
                this.dispatch(HostLinksAction::ReconnectAll);
                this.settle(cx);
            });
        })
        .detach();
    }

    // Link quality.

    fn link_quality_host(&self) -> Option<String> {
        match &self.connection {
            ConnectionSpec::Local => None,
            ConnectionSpec::Ssh { target, .. } => Some(target.clone()),
        }
    }

    /// Returns whether a verdict was just spoken.
    fn apply_link_quality(
        &mut self,
        change: Option<LinkQualityChange>,
        cx: &mut Context<Self>,
    ) -> bool {
        match change {
            Some(LinkQualityChange::Degraded {
                state,
                losses,
                lag_events,
                late_requests,
            }) => {
                self.incident(
                    "link.quality",
                    json!({
                        "state": state.name(),
                        "losses": losses,
                        "lagEvents": lag_events,
                        "lateRequests": late_requests,
                    }),
                );
                let verdict =
                    describe_link_quality(state, &self.link_quality_host().unwrap_or_default());
                self.link_quality_verdict = verdict.clone();
                self.set_status(verdict, cx);
                true
            }
            Some(LinkQualityChange::Cleared { after_ms }) => {
                self.incident(
                    "link.quality",
                    json!({ "state": "ok", "afterMs": after_ms }),
                );
                self.link_quality_verdict.clear();
                false
            }
            None => false,
        }
    }

    fn poll_late_requests(&mut self, now: u64, cx: &mut Context<Self>) {
        let client_id = self
            .link_quality_host()
            .and_then(|_| self.active_link())
            .filter(|link| link.host_state.phase == ConnectionPhase::Connected)
            .and_then(|link| link.client_id.clone());
        let Some(client_id) = client_id else {
            self.seen_late_requests = None;
            return;
        };
        let Some(total) = self.backend.late_requests(&client_id) else {
            return;
        };
        if self
            .seen_late_requests
            .as_ref()
            .is_some_and(|(seen_client, seen)| *seen_client == client_id && total > *seen)
        {
            let change = self.link_quality.note_late_request(now);
            self.apply_link_quality(change, cx);
            cx.notify();
        }
        self.seen_late_requests = Some((client_id, total));
    }

    // Reconciling.

    fn dispatch(&mut self, action: HostLinksAction) {
        host_links_reducer(&mut self.links, action);
    }

    fn set_status(&mut self, message: String, cx: &mut Context<Self>) {
        self.status = message.clone();
        cx.emit(ConnectionEvent::Status(message));
    }

    fn announce(&mut self, profile_id: &str, message: String, cx: &mut Context<Self>) {
        if self.is_active(profile_id) {
            self.set_status(message, cx);
        }
    }

    fn incident(&self, kind: &str, detail: Value) {
        self.backend.record_incident(kind, detail);
    }

    fn is_active(&self, profile_id: &str) -> bool {
        profile_id == self.active_profile_id()
    }

    fn active_scope(&self) -> HostScopeToken {
        match self.active_link() {
            Some(link) => link.scope(),
            None => HostScopeToken {
                host_profile_id: self.active_profile_id(),
                connection_key: helper_connection_key(&self.connection),
                connection_epoch: 0,
                server_identity: None,
                generation: 0,
            },
        }
    }

    /// Everything that follows from a state change, once per entry point.
    fn settle(&mut self, cx: &mut Context<Self>) {
        if self.profiles_hydrated {
            let hosts = shown_host_profiles(&self.profiles, &self.connection);
            self.dispatch(HostLinksAction::Sync(hosts));
        }
        self.reconcile_bridges(cx);
        self.sync_terminal_clients(cx);
        self.observe_transitions(cx);
        if let Some(link) = self.active_link() {
            let epoch = link.connection_epoch;
            let connection = self.connection.clone();
            self.helper_recovery
                .set_scope(&connection, epoch, &mut self.helper);
        }
        // A different machine's link is a different link; nothing carries over.
        let active = self.active_profile_id();
        if self.link_quality_profile.as_ref() != Some(&active) {
            self.link_quality_profile = Some(active);
            self.link_quality.reset();
            self.link_quality_verdict.clear();
        }
        self.observe_app_recovery(cx);
        cx.notify();
    }

    fn reconcile_bridges(&mut self, cx: &mut Context<Self>) {
        let active = self.active_profile_id();
        let stale: Vec<String> = self
            .runtimes
            .iter()
            .filter(|(profile_id, runtime)| {
                let current = self
                    .links
                    .link(profile_id)
                    .map(|link| (link.connection.clone(), link.connection_epoch));
                current.is_none()
                    || runtime
                        .bridge
                        .as_ref()
                        .is_some_and(|bridge| Some(&bridge.key) != current.as_ref())
            })
            .map(|(profile_id, _)| profile_id.clone())
            .collect();
        for profile_id in stale {
            let bridge = self
                .runtimes
                .get_mut(&profile_id)
                .and_then(|runtime| runtime.bridge.take());
            if let Some(bridge) = bridge {
                self.stop_bridge(&profile_id, bridge);
            }
            if self.links.link(&profile_id).is_none() && profile_id != active {
                self.runtimes.remove(&profile_id);
            }
        }
        for profile_id in self.links.order.clone() {
            if self
                .runtimes
                .entry(profile_id.clone())
                .or_default()
                .bridge
                .is_none()
            {
                self.start_bridge(&profile_id, cx);
            }
        }
    }

    fn stop_bridge(&mut self, profile_id: &str, bridge: RunningBridge) {
        self.dispatch(HostLinksAction::Host {
            profile_id: profile_id.to_owned(),
            action: HostAction::Connection {
                phase: ConnectionPhase::Disconnected,
            },
        });
        self.dispatch(HostLinksAction::Client {
            profile_id: profile_id.to_owned(),
            client_id: None,
        });
        self.dispatch(HostLinksAction::TerminalEpoch {
            profile_id: profile_id.to_owned(),
            terminal_epoch: 0,
        });
        if let Some(client_id) = bridge.client_id {
            self.backend.stop_terminal(client_id);
        }
    }

    /// A bridge starting or stopping changes the client every terminal link
    /// names in its requests.
    fn sync_terminal_clients(&mut self, cx: &mut Context<Self>) {
        let terminals: Vec<(String, Entity<TerminalLink>)> = self
            .runtimes
            .iter()
            .filter_map(|(id, runtime)| Some((id.clone(), runtime.terminal.clone()?)))
            .collect();
        for (profile_id, terminal) in terminals {
            let link = self.links.link(&profile_id);
            let client_id = link.and_then(|link| link.client_id.clone());
            let epoch = link.map_or(0, |link| link.connection_epoch);
            if terminal.read(cx).client_id() != client_id.as_deref() {
                terminal.update(cx, |terminal, _| terminal.set_client(client_id, epoch));
            }
        }
    }

    /// One host's terminal engine and what it owes this controller.
    fn ensure_terminal_link(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        let runtime = self.runtimes.entry(profile_id.to_owned()).or_default();
        if runtime.terminal.is_some() {
            return;
        }
        let backend: Arc<dyn crate::connection_ui::backend::TerminalBackend> = self.backend.clone();
        let id = profile_id.to_owned();
        let terminal = cx.new(|cx| TerminalLink::new(id, backend, cx));
        let owner = profile_id.to_owned();
        let events = cx.subscribe(&terminal, move |this, _, event, cx| {
            this.on_terminal_event(&owner, event, cx);
        });
        let runtime = self.runtimes.entry(profile_id.to_owned()).or_default();
        runtime.terminal = Some(terminal);
        runtime.terminal_events = Some(events);
    }

    fn on_terminal_event(&mut self, profile_id: &str, event: &LinkEvent, cx: &mut Context<Self>) {
        match event {
            LinkEvent::FramesConsumed(frames) => {
                let Some(runtime) = self.runtimes.get_mut(profile_id) else {
                    return;
                };
                let Some(bridge) = runtime.bridge.as_mut() else {
                    return;
                };
                for (byte_length, epoch) in frames {
                    bridge.acks.record_applied(*byte_length, *epoch);
                }
                let token = bridge.token;
                self.send_acknowledgement(profile_id, token, cx);
            }
            LinkEvent::FlowStalled { pane_id, epoch } => {
                self.flow_stalled(profile_id, pane_id, *epoch, cx);
            }
            LinkEvent::Status(message) => self.announce(profile_id, message.clone(), cx),
            LinkEvent::ClipboardWrite(text) => self.application_copied(profile_id, text, cx),
        }
    }

    /// A program on a host copied, by OSC 52 or into a tmux buffer. A host
    /// shown once stays attached and can still do so; only the host on screen
    /// may write the user's clipboard.
    fn application_copied(&mut self, profile_id: &str, text: &str, cx: &mut Context<Self>) {
        if self.active_profile_id() == profile_id {
            crate::terminal::integration::clipboard::write_from_application(text.to_owned(), cx);
        }
    }

    fn flow_stalled(
        &mut self,
        profile_id: &str,
        pane_id: &str,
        epoch: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        let Some(bridge) = self
            .runtimes
            .get(profile_id)
            .and_then(|runtime| runtime.bridge.as_ref())
        else {
            return;
        };
        // A stall the old bridge left queued is not this bridge's to answer.
        if epoch != Some(bridge.terminal_epoch) {
            return;
        }
        let token = bridge.token;
        // The host used up its in-place resumes. One bridge may report several
        // panes; they collapse into one reconnect.
        let first = self
            .bridge_mut(profile_id, token)
            .is_some_and(|bridge| !std::mem::replace(&mut bridge.recovering_flow_stall, true));
        if first {
            self.incident(
                "reconnect.flowStall",
                json!({ "hostProfileId": profile_id, "paneId": pane_id }),
            );
            self.dispatch(HostLinksAction::Detail {
                profile_id: profile_id.to_owned(),
                detail: "A terminal output stream stalled; reconnecting it now.".to_owned(),
            });
            self.dispatch(HostLinksAction::Reconnect {
                profile_id: profile_id.to_owned(),
            });
            self.settle(cx);
        }
    }

    fn terminal_command(&self, profile_id: &str, command: Command, cx: &App) {
        if let Some(terminal) = self
            .runtimes
            .get(profile_id)
            .and_then(|r| r.terminal.as_ref())
        {
            terminal.read(cx).send(command);
        }
    }

    /// One host's bridge: every event it reports, handled the same way for
    /// every host, with the profile id deciding which link the answer belongs
    /// to and whether it is the one on screen.
    fn start_bridge(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        let Some(link) = self.links.link(profile_id).cloned() else {
            return;
        };
        self.ensure_terminal_link(profile_id, cx);
        let (sender, mut frames) = mpsc::unbounded();
        // Only the host on screen attaches a terminal. A host shown beside it
        // relays topology and agents until it is activated.
        let attach = self.is_active(profile_id);
        let session_id = attach
            .then(|| link.active_session_id.clone())
            .flatten()
            .unwrap_or_default();
        let started = self.backend.start_terminal(
            session_id,
            link.connection.clone(),
            attach,
            Arc::new(ChannelSink(sender)),
        );
        let token = self.next_bridge_token;
        self.next_bridge_token += 1;
        let owner = profile_id.to_owned();
        let pump = cx.spawn(async move |this, cx| {
            // A native window of 2 MiB and 4096 records bounds this channel:
            // nothing more is sent until these frames are acknowledged.
            while let Some(first) = frames.next().await {
                let mut batch = vec![first];
                while batch.len() < MAX_FRAME_BATCH {
                    match frames.try_recv() {
                        Ok(next) => batch.push(next),
                        Err(_) => break,
                    }
                }
                let delivered = this.update(cx, |this, cx| {
                    this.on_frames(&owner, token, batch, cx);
                });
                if delivered.is_err() {
                    break;
                }
            }
        });
        let bridge = RunningBridge {
            key: (link.connection.clone(), link.connection_epoch),
            token,
            client_id: None,
            terminal_epoch: 0,
            acks: DeliveryAcks::default(),
            recovering_flow_stall: false,
            topology_burst: None,
            _pump: pump,
        };
        self.runtimes
            .entry(profile_id.to_owned())
            .or_default()
            .bridge = Some(bridge);
        match started {
            Ok(client_id) => {
                if let Some(bridge) = self.bridge_mut(profile_id, token) {
                    bridge.client_id = Some(client_id.clone());
                }
                self.dispatch(HostLinksAction::Client {
                    profile_id: profile_id.to_owned(),
                    client_id: Some(client_id),
                });
            }
            Err(error) => {
                self.dispatch(HostLinksAction::Detail {
                    profile_id: profile_id.to_owned(),
                    detail: error.clone(),
                });
                self.announce(profile_id, error, cx);
            }
        }
    }

    fn bridge_mut(&mut self, profile_id: &str, token: u64) -> Option<&mut RunningBridge> {
        self.runtimes
            .get_mut(profile_id)?
            .bridge
            .as_mut()
            .filter(|bridge| bridge.token == token)
    }

    /// One batch from a bridge's channel, then one settle for all of it. Every
    /// frame goes on to the terminal engine, whatever came of decoding it here:
    /// the engine is what spends its credit, once applied.
    fn on_frames(
        &mut self,
        profile_id: &str,
        token: u64,
        frames: Vec<EventBody>,
        cx: &mut Context<Self>,
    ) {
        if self.bridge_mut(profile_id, token).is_none() {
            return;
        }
        let mut changed = false;
        for body in frames {
            let EventBody::Raw(frame) = body else {
                continue;
            };
            let decoded = decode_frame(&frame);
            match decoded {
                Ok(event) => {
                    changed |= event != BridgeEvent::Ignored;
                    if let (BridgeEvent::GenerationEpoch(epoch), Some(bridge)) =
                        (&event, self.bridge_mut(profile_id, token))
                    {
                        bridge.acks.begin_epoch(*epoch);
                    }
                    self.handle_event(profile_id, token, event, cx);
                }
                Err(message) => self.incident("link.decodeFailure", json!({ "message": message })),
            }
            // Credit is spent when the engine has applied the frame, so the
            // host cannot outrun a parser that is behind.
            if let Some(terminal) = self
                .runtimes
                .get(profile_id)
                .and_then(|runtime| runtime.terminal.as_ref())
            {
                terminal.read(cx).forward_frame(frame);
            }
        }
        // Terminal output passes through here and is no state of this module's.
        if changed {
            self.settle(cx);
        }
    }

    fn send_acknowledgement(&mut self, profile_id: &str, token: u64, cx: &mut Context<Self>) {
        let Some(bridge) = self.bridge_mut(profile_id, token) else {
            return;
        };
        let Some(client_id) = bridge.client_id.clone() else {
            return;
        };
        let Some(boundary) = bridge.acks.take_boundary() else {
            return;
        };
        let request = self.backend.acknowledge_delivery(
            client_id,
            boundary.epoch,
            boundary.frames,
            boundary.bytes,
        );
        let owner = profile_id.to_owned();
        cx.spawn(async move |this, cx| {
            let delivered = request.await.is_ok();
            let _ = this.update(cx, |this, cx| {
                this.settle_acknowledgement(&owner, token, boundary, delivered, cx);
            });
        })
        .detach();
    }

    fn settle_acknowledgement(
        &mut self,
        profile_id: &str,
        token: u64,
        boundary: AckBoundary,
        delivered: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(bridge) = self.bridge_mut(profile_id, token) else {
            return;
        };
        if bridge.acks.settle(boundary, delivered) {
            self.send_acknowledgement(profile_id, token, cx);
        }
    }

    fn handle_event(
        &mut self,
        profile_id: &str,
        token: u64,
        event: BridgeEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            BridgeEvent::GenerationEpoch(epoch) => {
                if let Some(bridge) = self.bridge_mut(profile_id, token) {
                    bridge.terminal_epoch = epoch;
                }
                self.dispatch(HostLinksAction::TerminalEpoch {
                    profile_id: profile_id.to_owned(),
                    terminal_epoch: epoch,
                });
            }
            BridgeEvent::TopologyDirty { name } => {
                let now = self.backend.now_ms();
                if let Some(bridge) = self.bridge_mut(profile_id, token) {
                    // First dirty of a burst wins: the snapshot that answers a
                    // burst answers all of it.
                    let burst = bridge.topology_burst.get_or_insert(TopologyBurst {
                        started_at: now,
                        name,
                        count: 0,
                    });
                    burst.count += 1;
                }
                self.announce(profile_id, "Topology changed; reconciling…".to_owned(), cx);
            }
            BridgeEvent::FlowPaused { pane_id } => {
                // Journal only: the host resumes the pane itself.
                self.incident("flow.paused", json!({ "paneId": pane_id }));
            }
            // Spoken by the engine, which knows whether this frame was admitted.
            BridgeEvent::FlowStalled { .. } => {}
            BridgeEvent::ClipboardWrite { text } => self.application_copied(profile_id, &text, cx),
            BridgeEvent::Error { message } => {
                self.bridge_down(profile_id, "error", message.clone(), Some(&message), cx);
            }
            BridgeEvent::Exit { reason } => {
                self.bridge_down(profile_id, "exit", format!("Detached: {reason}"), None, cx);
            }
            BridgeEvent::ConnectionState { phase, detail } => {
                self.connection_state(profile_id, phase, detail, cx);
            }
            BridgeEvent::Snapshot(event) => {
                self.note_server_identity(profile_id, &event.server_identity, cx);
                let now = self.backend.now_ms();
                let burst = self
                    .bridge_mut(profile_id, token)
                    .and_then(|bridge| bridge.topology_burst.take());
                if let Some(burst) = burst {
                    self.incident(
                        "topo.snapshot",
                        json!({
                            "hostProfileId": profile_id,
                            "msSinceDirty": now.saturating_sub(burst.started_at),
                            "answering": burst.name,
                            "dirtyCount": burst.count,
                        }),
                    );
                }
                self.dispatch(HostLinksAction::Host {
                    profile_id: profile_id.to_owned(),
                    action: HostAction::Snapshot(SnapshotAction {
                        snapshot: event.snapshot,
                        sequence: event.sequence,
                        server_identity: event.server_identity,
                        generation: Some(event.generation),
                    }),
                });
                // Either way it closes the reconciling status above.
                self.announce(profile_id, "Live".to_owned(), cx);
            }
            BridgeEvent::Ignored => {}
        }
    }

    /// A different tmux server: no screen the engine kept describes it.
    fn note_server_identity(&mut self, profile_id: &str, identity: &str, cx: &App) {
        let Some(runtime) = self.runtimes.get_mut(profile_id) else {
            return;
        };
        let previous = runtime.server_identity.replace(identity.to_owned());
        if previous.is_some_and(|previous| previous != identity) {
            self.terminal_command(profile_id, Command::ClearTerminalState, cx);
        }
    }

    fn bridge_down(
        &mut self,
        profile_id: &str,
        kind: &str,
        detail: String,
        error: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        self.incident(
            "link.bridgeDown",
            json!({ "hostProfileId": profile_id, "event": kind, "detail": detail }),
        );
        let shown = user_facing_bridge_failure(&detail).to_owned();
        let was_connected = self
            .links
            .link(profile_id)
            .is_some_and(|link| link.host_state.phase == ConnectionPhase::Connected);
        let active = self.is_active(profile_id);
        // An error while the link is up is the link dropping under the app:
        // exactly one per outage. A verdict spoken here is the notice for this
        // drop; the raw failure would only be a second line saying less.
        let verdict_spoken =
            if error.is_some() && was_connected && active && self.link_quality_host().is_some() {
                let change = self.link_quality.note_link_lost(self.backend.now_ms());
                self.apply_link_quality(change, cx)
            } else {
                false
            };
        let line = if active && !self.link_quality_verdict.is_empty() {
            self.link_quality_verdict.clone()
        } else {
            shown.clone()
        };
        self.dispatch(HostLinksAction::Detail {
            profile_id: profile_id.to_owned(),
            detail: line,
        });
        // Once reconnecting, the strip already shows this exact cause.
        if was_connected && !verdict_spoken {
            self.announce(profile_id, shown, cx);
        }
        // Helper recovery is the active host's. A host beside it keeps its
        // failure in its own detail until it is activated.
        if active
            && matches!(self.connection, ConnectionSpec::Ssh { .. })
            && error.is_some_and(|message| HANDSHAKE_FAILURE.is_match(message))
        {
            let connection = self.connection.clone();
            let request = self
                .helper_recovery
                .handshake_failed(&connection, &mut self.helper);
            self.run_helper_probe(request, cx);
        }
    }

    fn connection_state(
        &mut self,
        profile_id: &str,
        phase: ConnectionPhase,
        detail: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if phase == ConnectionPhase::Resyncing {
            self.incident(
                "link.resyncStarted",
                json!({
                    "hostProfileId": profile_id,
                    "reason": detail.as_deref().unwrap_or("resync reason unavailable"),
                }),
            );
        }
        if self.is_active(profile_id) {
            // Before publishing to the shell, so a live helper probe can
            // arbitrate against other host questions on the same settle.
            let connection = self.connection.clone();
            let request =
                self.helper_recovery
                    .connection_state_changed(&connection, phase, &mut self.helper);
            self.run_helper_probe(request, cx);
        }
        self.dispatch(HostLinksAction::Host {
            profile_id: profile_id.to_owned(),
            action: HostAction::Connection { phase },
        });
        // Per event: a batch can hold `resyncing` and `connected` together.
        self.observe_transitions(cx);
        if phase == ConnectionPhase::Connected {
            self.dispatch(HostLinksAction::Detail {
                profile_id: profile_id.to_owned(),
                detail: String::new(),
            });
        }
        let message = if phase == ConnectionPhase::Connected {
            "Live".to_owned()
        } else {
            format!("Connection {}…", phase.wire_name())
        };
        self.announce(profile_id, message, cx);
    }

    /// What a host's phase change means: the journal's record of what the user
    /// saw, per link.
    fn observe_transitions(&mut self, cx: &App) {
        let now = self.backend.now_ms();
        for profile_id in self.links.order.clone() {
            let Some(link) = self.links.link(&profile_id) else {
                continue;
            };
            let phase = link.host_state.phase;
            let runtime = self.runtimes.entry(profile_id.clone()).or_default();
            let before = std::mem::replace(&mut runtime.phase, phase);
            let degraded = matches!(
                phase,
                ConnectionPhase::Disconnected
                    | ConnectionPhase::Reconnecting
                    | ConnectionPhase::Resyncing
            );
            let mut restored = None;
            let mut newly_degraded = false;
            if degraded && runtime.degraded_since.is_none() {
                runtime.degraded_since = Some(now);
                newly_degraded = true;
            } else if !degraded && let Some(since) = runtime.degraded_since.take() {
                restored = Some(now.saturating_sub(since));
            }
            if before == ConnectionPhase::Resyncing && phase == ConnectionPhase::Connected {
                self.incident("link.resynced", json!({ "hostProfileId": profile_id }));
                self.terminal_command(
                    &profile_id,
                    Command::ReseedMounted("post-resync reseed".to_owned()),
                    cx,
                );
            }
            if newly_degraded {
                self.incident(
                    "link.degraded",
                    json!({ "hostProfileId": profile_id, "phase": phase.wire_name() }),
                );
            }
            if let Some(after_ms) = restored {
                self.incident(
                    "link.restored",
                    json!({ "hostProfileId": profile_id, "afterMs": after_ms }),
                );
            }
        }
    }

    /// Port of `useAppRecoveryController`: replacement-server reconciliation.
    fn observe_app_recovery(&mut self, cx: &mut Context<Self>) {
        let Some(link) = self.active_link() else {
            return;
        };
        let profile_id = link.profile_id.clone();
        let identity = link.host_state.server_identity.clone();
        let sessions = link.host_state.snapshot.sessions.clone();
        let scope = link.scope();
        let observed = (profile_id.clone(), identity.clone(), sessions.clone());
        if self.observed_host.as_ref() != Some(&observed) {
            self.observed_host = Some(observed);
            // Each saved host owns its own replacement history.
            let previous = self.last_identities.get(&profile_id).cloned();
            if let (Some(previous), Some(current)) = (previous, identity.as_ref())
                && previous != *current
                && let Ok(state) = self.current_app_state()
            {
                let count = recoverable_app_tab_count(&state, &profile_id, &previous, &sessions);
                if count > 0 {
                    app_recovery_reducer(
                        &mut self.app_recovery,
                        AppRecoveryAction::Offer {
                            count,
                            previous_server_identity: previous,
                            scope: scope.clone(),
                        },
                    );
                }
            }
            self.update_app_state(
                |state| {
                    reconcile_workspace_identity(state, &profile_id, identity.as_deref(), &sessions)
                },
                cx,
            );
            // A reconnect clears the live host state before its new hello;
            // that never erases the last authoritative identity.
            if let Some(identity) = identity {
                self.last_identities.insert(profile_id, identity);
            }
        }
        app_recovery_reducer(
            &mut self.app_recovery,
            AppRecoveryAction::ReconcileScope(scope),
        );
    }

    /// True while an app-tab discard is waiting on the user.
    pub fn app_recovery_modal_open(&self) -> bool {
        self.app_recovery
            .as_ref()
            .is_some_and(|state| state.phase == AppRecoveryPhase::ConfirmingDiscard)
    }
}
