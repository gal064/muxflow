//! The seam between the connection state machine and `desktop-core`, so tests
//! can drive every flow without opening a bridge.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use std::sync::mpsc::Sender;

use desktop_core::{
    app_state::{AppStateStore, PersistedAppState, update_app_state},
    connection::{
        self, ConnectionSpec, ProfileStore, TerminalClients,
        agent::{AgentCommand, agent_request},
        automation::{AutomationCommand, automation_request},
        delivery_ack,
        files::upload_manager::{self, UploadManager},
        helper::{self, HelperInstallReport},
        tmux_action,
    },
    incidents::{IncidentJournal, record_incident},
    platform::{self, EventSink},
};
use futures::future::{BoxFuture, FutureExt};
use serde_json::Value;
use terminal_engine::{Command, EngineHandle, EngineOutput, HubLimits};
use uuid::Uuid;

use crate::terminal::integration::transfers::UploadRequest;

use super::{
    helper_upgrade::RemoteHelperProbe,
    resume_recovery::{network_online, suspend_aware_now_ms},
};

/// What the engine thread is reached through: the real thread, or in tests an
/// engine driven on the caller.
pub trait EngineChannel: Send + Sync {
    /// False once the engine has gone.
    fn send(&self, command: Command) -> bool;
}

impl EngineChannel for EngineHandle {
    fn send(&self, command: Command) -> bool {
        EngineHandle::send(self, command)
    }
}

/// The terminal path's half of the backend, split out so a terminal test needs
/// no connection flows.
pub trait TerminalBackend: Send + Sync {
    /// Starts one host's engine, reporting to `outputs`.
    fn spawn_engine(&self, outputs: Sender<EngineOutput>) -> Box<dyn EngineChannel>;
    fn request_seed(
        &self,
        client_id: String,
        pane_id: String,
    ) -> BoxFuture<'static, Result<(), String>>;
    fn request_history(
        &self,
        client_id: String,
        pane_id: String,
        lines: u32,
        skip_lines: u32,
    ) -> BoxFuture<'static, Result<(), String>>;
    fn set_pane_visibility(
        &self,
        client_id: String,
        pane_id: String,
        visible: bool,
        holds_snapshot: bool,
        terminal_epoch: u64,
        output_generation: u64,
    ) -> BoxFuture<'static, Result<(), String>>;
    fn resize_client(
        &self,
        client_id: String,
        columns: u16,
        rows: u16,
        session_id: String,
        window_id: String,
    ) -> BoxFuture<'static, Result<(), String>>;
    fn send_input(&self, client_id: String, pane_id: String, bytes: Vec<u8>) -> Result<(), String>;
    /// Text the host pastes through tmux, bracketed iff the program asked, in
    /// order with `send_input`'s keys.
    fn send_paste(&self, client_id: String, pane_id: String, text: Vec<u8>) -> Result<(), String>;
    fn record_incident(&self, kind: &str, detail: Value);
    /// The link's delivery state, as `terminal_link_stats` reports it.
    fn link_stats(&self, _client_id: &str) -> Option<Value> {
        None
    }
    /// Drains the input queue's latency window: `bucketCounts` and `maxMs`.
    fn input_latency_stats(&self, _client_id: &str) -> Option<Value> {
        None
    }
    /// Tells every host the terminal's colors (`TerminalColors`); also kept for hosts
    /// that connect later.
    fn set_terminal_colors(&self, foreground: String, background: String) -> Result<(), String>;
    /// One tmux action, in `desktop-core`'s wire shape, answered with its result.
    fn tmux_action(
        &self,
        client_id: String,
        action: Value,
    ) -> BoxFuture<'static, Result<Value, String>>;
    /// Starts a terminal upload, or its preflight, reporting to `events`.
    fn start_upload(
        &self,
        request: UploadRequest,
        events: Arc<dyn EventSink>,
    ) -> Result<String, String>;
    fn cancel_upload(&self, transfer_id: String) -> Result<Value, String>;
    /// One agent request, answered with the host's JSON (`agent_request`).
    fn agent_request(
        &self,
        _client_id: String,
        _command: AgentCommand,
    ) -> BoxFuture<'static, Result<Value, String>> {
        futures::future::ready(Err("agents are unavailable here".to_owned())).boxed()
    }
    /// One Automations panel request (`automation_request`).
    fn automation_request(
        &self,
        _client_id: String,
        _command: AutomationCommand,
    ) -> BoxFuture<'static, Result<Value, String>> {
        futures::future::ready(Err("automations are unavailable here".to_owned())).boxed()
    }
}

pub trait ConnectionBackend: TerminalBackend {
    fn start_terminal(
        &self,
        session_id: String,
        connection: ConnectionSpec,
        attach: bool,
        sink: Arc<dyn EventSink>,
    ) -> Result<String, String>;
    fn stop_terminal(&self, client_id: String);
    fn acknowledge_delivery(
        &self,
        client_id: String,
        epoch: u64,
        frames: u64,
        bytes: u64,
    ) -> BoxFuture<'static, Result<(), String>>;
    fn probe_link(&self, client_id: String) -> BoxFuture<'static, Result<(), String>>;
    fn discard_ssh_masters(&self) -> BoxFuture<'static, Result<(), String>>;
    fn probe_helper(
        &self,
        connection: ConnectionSpec,
    ) -> BoxFuture<'static, Result<RemoteHelperProbe, String>>;
    fn install_helper(
        &self,
        connection: ConnectionSpec,
        allow_upgrade: bool,
    ) -> BoxFuture<'static, Result<HelperInstallReport, String>>;
    /// Writes `state`'s app tabs and workspace records, and nothing else: the
    /// shell's preferences, shortcuts and window geometry have writers of their
    /// own. A save asked for later always wins over one asked for earlier.
    fn save_app_state(&self, state: PersistedAppState) -> BoxFuture<'static, Result<(), String>>;
    /// `None` when the client has not read a host frame yet.
    fn late_requests(&self, client_id: &str) -> Option<u32>;
    /// Milliseconds on one clock that keeps counting through a suspend.
    fn now_ms(&self) -> u64;
    fn network_online(&self) -> bool;
}

pub struct NativeBackend {
    app_state: Arc<AppStateStore>,
    clients: Arc<TerminalClients>,
    /// Uploads open their own connection to the profile's host.
    profiles: Arc<ProfileStore>,
    journal: Arc<IncidentJournal>,
    launch: String,
    save_order: Arc<SaveOrder>,
}

/// The app-tab saves asked for, and the newest one written.
#[derive(Default)]
struct SaveOrder {
    issued: AtomicU64,
    applied: AtomicU64,
}

impl NativeBackend {
    pub fn new(
        app_state: Arc<AppStateStore>,
        profiles: Arc<ProfileStore>,
        journal: IncidentJournal,
    ) -> Self {
        Self {
            app_state,
            clients: Arc::default(),
            profiles,
            journal: Arc::new(journal),
            launch: Uuid::new_v4().simple().to_string()[..8].to_owned(),
            save_order: Arc::default(),
        }
    }
}

impl TerminalBackend for NativeBackend {
    fn spawn_engine(&self, outputs: Sender<EngineOutput>) -> Box<dyn EngineChannel> {
        Box::new(terminal_engine::spawn(HubLimits::default(), outputs))
    }

    fn request_seed(
        &self,
        client_id: String,
        pane_id: String,
    ) -> BoxFuture<'static, Result<(), String>> {
        let clients = self.clients.clone();
        spawned(
            async move { connection::request_terminal_seed(client_id, pane_id, &clients).await },
        )
    }

    fn request_history(
        &self,
        client_id: String,
        pane_id: String,
        lines: u32,
        skip_lines: u32,
    ) -> BoxFuture<'static, Result<(), String>> {
        let clients = self.clients.clone();
        spawned(async move {
            connection::request_terminal_history(client_id, pane_id, lines, skip_lines, &clients)
                .await
        })
    }

    fn set_pane_visibility(
        &self,
        client_id: String,
        pane_id: String,
        visible: bool,
        holds_snapshot: bool,
        terminal_epoch: u64,
        output_generation: u64,
    ) -> BoxFuture<'static, Result<(), String>> {
        let clients = self.clients.clone();
        spawned(async move {
            let body = visibility_frame(
                &client_id,
                &pane_id,
                visible,
                holds_snapshot,
                terminal_epoch,
                output_generation,
            );
            connection::set_terminal_visibility(&body, &clients).await
        })
    }

    fn resize_client(
        &self,
        client_id: String,
        columns: u16,
        rows: u16,
        session_id: String,
        window_id: String,
    ) -> BoxFuture<'static, Result<(), String>> {
        let clients = self.clients.clone();
        spawned(async move {
            connection::resize_terminal_client(
                client_id, columns, rows, session_id, window_id, &clients,
            )
            .await
        })
    }

    fn send_input(&self, client_id: String, pane_id: String, bytes: Vec<u8>) -> Result<(), String> {
        let body = input_frame(&client_id, &pane_id, &bytes);
        connection::send_terminal_input_bytes(&body, &self.clients)
    }

    fn send_paste(&self, client_id: String, pane_id: String, text: Vec<u8>) -> Result<(), String> {
        connection::send_terminal_paste(&client_id, &pane_id, text, &self.clients)
    }

    fn record_incident(&self, kind: &str, detail: Value) {
        let time = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let record = crate::diagnostics::incident_record(&self.launch, time, kind, detail);
        // Must stay harmless: a journal that cannot be written is never a
        // reason the app misbehaves.
        let _ = record_incident(&self.journal, record.to_string());
    }

    fn link_stats(&self, client_id: &str) -> Option<Value> {
        let stats = connection::terminal_link_stats(client_id.to_owned(), &self.clients).ok()?;
        serde_json::to_value(stats).ok()
    }

    fn input_latency_stats(&self, client_id: &str) -> Option<Value> {
        let stats = connection::input_latency_stats(client_id.to_owned(), &self.clients)?;
        serde_json::to_value(stats).ok()
    }

    fn set_terminal_colors(&self, foreground: String, background: String) -> Result<(), String> {
        connection::set_terminal_colors(foreground, background, &self.clients)
    }

    fn tmux_action(
        &self,
        client_id: String,
        action: Value,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let clients = self.clients.clone();
        spawned(async move {
            let action = serde_json::from_value(action).map_err(|error| error.to_string())?;
            let result = tmux_action::tmux_action(client_id, action, &clients).await?;
            serde_json::to_value(result).map_err(|error| error.to_string())
        })
    }

    fn start_upload(
        &self,
        request: UploadRequest,
        events: Arc<dyn EventSink>,
    ) -> Result<String, String> {
        let start = if request.preflight {
            upload_manager::start_terminal_upload_preflight
        } else {
            upload_manager::start_terminal_upload
        };
        let scope = request.scope;
        start(
            scope.client_id,
            scope.profile_id,
            scope.server_identity,
            scope.terminal_epoch.to_string(),
            request.source_path,
            request.destination_name,
            request.collision,
            request.large_upload_confirmed,
            request.image_png,
            events,
            &self.profiles,
            &self.clients,
            &UploadManager,
        )
    }

    fn cancel_upload(&self, transfer_id: String) -> Result<Value, String> {
        upload_manager::cancel_terminal_upload(transfer_id, &UploadManager)
    }

    fn agent_request(
        &self,
        client_id: String,
        command: AgentCommand,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let clients = self.clients.clone();
        spawned(async move { agent_request(client_id, command, &clients).await })
    }

    fn automation_request(
        &self,
        client_id: String,
        command: AutomationCommand,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let clients = self.clients.clone();
        spawned(async move { automation_request(client_id, command, &clients).await })
    }
}

impl ConnectionBackend for NativeBackend {
    fn start_terminal(
        &self,
        session_id: String,
        connection: ConnectionSpec,
        attach: bool,
        sink: Arc<dyn EventSink>,
    ) -> Result<String, String> {
        connection::start_terminal(
            session_id,
            Vec::new(),
            connection,
            attach,
            Uuid::new_v4().to_string(),
            sink,
            &self.clients,
        )
    }

    fn stop_terminal(&self, client_id: String) {
        // A client already gone is the outcome this asked for.
        let _ = connection::stop_terminal(client_id, &self.clients);
    }

    fn acknowledge_delivery(
        &self,
        client_id: String,
        epoch: u64,
        frames: u64,
        bytes: u64,
    ) -> BoxFuture<'static, Result<(), String>> {
        let clients = self.clients.clone();
        spawned(async move {
            delivery_ack::acknowledge_terminal_delivery(client_id, epoch, frames, bytes, &clients)
                .await
        })
    }

    fn probe_link(&self, client_id: String) -> BoxFuture<'static, Result<(), String>> {
        let clients = self.clients.clone();
        spawned(async move { connection::probe_terminal_link(client_id, &clients).await })
    }

    fn discard_ssh_masters(&self) -> BoxFuture<'static, Result<(), String>> {
        spawned(connection::discard_ssh_masters())
    }

    fn probe_helper(
        &self,
        connection: ConnectionSpec,
    ) -> BoxFuture<'static, Result<RemoteHelperProbe, String>> {
        spawned(async move {
            let value = helper::probe_remote_helper(connection).await?;
            serde_json::from_value(value).map_err(|error| error.to_string())
        })
    }

    fn install_helper(
        &self,
        connection: ConnectionSpec,
        allow_upgrade: bool,
    ) -> BoxFuture<'static, Result<HelperInstallReport, String>> {
        spawned(async move { Ok(helper::install_remote_helper(connection, allow_upgrade).await) })
    }

    fn save_app_state(&self, state: PersistedAppState) -> BoxFuture<'static, Result<(), String>> {
        let store = self.app_state.clone();
        // Numbered when asked for, so the quit flush, which writes again while
        // an earlier save may still be waiting for the lock, cannot be undone
        // by that earlier one landing after it.
        let order = self.save_order.clone();
        let number = order.issued.fetch_add(1, Ordering::SeqCst) + 1;
        spawned(async move {
            update_app_state(&store, move |current| {
                if order.applied.fetch_max(number, Ordering::SeqCst) > number {
                    return;
                }
                current.app_tabs = state.app_tabs;
                current.workspace_ui = state.workspace_ui;
            })
            .await
        })
    }

    fn late_requests(&self, client_id: &str) -> Option<u32> {
        connection::terminal_link_stats(client_id.to_owned(), &self.clients)
            .ok()
            .map(|stats| stats.late_requests_total)
    }

    fn now_ms(&self) -> u64 {
        suspend_aware_now_ms()
    }

    fn network_online(&self) -> bool {
        network_online()
    }
}

/// Runs on the backend's runtime, where its tokio-based work belongs.
fn spawned<T: Send + 'static>(
    work: impl Future<Output = Result<T, String>> + Send + 'static,
) -> BoxFuture<'static, Result<T, String>> {
    async move {
        platform::spawn(work)
            .await
            .map_err(|error| format!("connection task failed: {error}"))?
    }
    .boxed()
}

fn length_prefixed(body: &mut Vec<u8>, value: &str) {
    body.extend_from_slice(&(value.len() as u16).to_be_bytes());
    body.extend_from_slice(value.as_bytes());
}

/// The raw-body shape `set_terminal_visibility` decodes.
fn visibility_frame(
    client_id: &str,
    pane_id: &str,
    visible: bool,
    holds_snapshot: bool,
    terminal_epoch: u64,
    output_generation: u64,
) -> Vec<u8> {
    let mut body = Vec::with_capacity(client_id.len() + pane_id.len() + 22);
    length_prefixed(&mut body, client_id);
    length_prefixed(&mut body, pane_id);
    body.push(u8::from(visible));
    body.push(u8::from(holds_snapshot));
    body.extend_from_slice(&terminal_epoch.to_be_bytes());
    body.extend_from_slice(&output_generation.to_be_bytes());
    body
}

/// The raw-body shape `send_terminal_input_bytes` decodes.
fn input_frame(client_id: &str, pane_id: &str, bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(client_id.len() + pane_id.len() + bytes.len() + 4);
    length_prefixed(&mut body, client_id);
    length_prefixed(&mut body, pane_id);
    body.extend_from_slice(bytes);
    body
}

#[cfg(test)]
pub(crate) mod inline {
    //! The engine, driven on the caller's thread: frames apply before `send`
    //! returns, and outputs wait in the channel for the link's next drain.

    use std::sync::{Mutex, mpsc::Sender};
    use std::time::Instant;

    use terminal_engine::{Command, Engine, EngineOutput, HubLimits};

    use super::EngineChannel;

    pub(crate) struct InlineEngine {
        pub(crate) engine: Mutex<Engine>,
        pub(crate) outputs: Sender<EngineOutput>,
    }

    impl InlineEngine {
        pub(crate) fn new(outputs: Sender<EngineOutput>) -> Self {
            Self {
                engine: Mutex::new(Engine::new(HubLimits::default())),
                outputs,
            }
        }
    }

    impl EngineChannel for InlineEngine {
        fn send(&self, command: Command) -> bool {
            for output in self.engine.lock().unwrap().handle(command, Instant::now()) {
                let _ = self.outputs.send(output);
            }
            true
        }
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;

    #[test]
    fn a_visibility_frame_has_the_shape_the_core_decodes() {
        let body = visibility_frame("cli", "%7", true, true, 5, 9);
        assert_eq!(
            body,
            [
                0, 3, b'c', b'l', b'i', 0, 2, b'%', b'7', 1, 1, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0,
                0, 0, 0, 9,
            ]
        );
    }

    #[test]
    fn an_input_frame_ends_with_the_bytes() {
        let body = input_frame("c", "%1", b"ab");
        assert_eq!(body, [0, 1, b'c', 0, 2, b'%', b'1', b'a', b'b']);
    }
}

#[cfg(test)]
mod save_tests {
    use desktop_core::app_state::{WorkspaceUiRecord, load_app_state};

    use super::*;

    fn named(name: &str) -> PersistedAppState {
        PersistedAppState {
            workspace_ui: vec![WorkspaceUiRecord {
                host_profile_id: "local".into(),
                server_identity: "tmux".into(),
                session_id: "$1".into(),
                session_name: name.into(),
                selected_app_tab_id: None,
                docked_app_tab_id: None,
                dock_side: None,
            }],
            ..PersistedAppState::default()
        }
    }

    #[test]
    fn an_earlier_save_landing_late_does_not_undo_a_later_one() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(AppStateStore::load(dir.path().join("app-state.json")));
        let backend = NativeBackend::new(
            store.clone(),
            Arc::new(ProfileStore::load(dir.path().join("profiles.json")).unwrap()),
            IncidentJournal::new(dir.path().join("logs")),
        );
        let earlier = backend.save_app_state(named("earlier"));
        let later = backend.save_app_state(named("later"));
        futures::executor::block_on(later).unwrap();
        futures::executor::block_on(earlier).unwrap();
        let saved = load_app_state(&store).unwrap();
        assert_eq!(saved.workspace_ui[0].session_name, "later");
    }

    #[test]
    fn a_tab_save_keeps_a_setting_saved_after_its_snapshot_was_taken() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app-state.json");
        let store = Arc::new(AppStateStore::load(path.clone()));
        let backend = NativeBackend::new(
            store.clone(),
            Arc::new(ProfileStore::load(dir.path().join("profiles.json")).unwrap()),
            IncidentJournal::new(dir.path().join("logs")),
        );
        // The controller's base, read before Settings saved.
        let stale = load_app_state(&store).unwrap();
        futures::executor::block_on(update_app_state(&store, |state| {
            state.shell.compact_workspaces = Some(false);
        }))
        .unwrap();
        futures::executor::block_on(backend.save_app_state(PersistedAppState {
            workspace_ui: named("tabs").workspace_ui,
            ..stale
        }))
        .unwrap();
        let on_disk = load_app_state(&AppStateStore::load(path)).unwrap();
        assert_eq!(on_disk.shell.compact_workspaces, Some(false));
        assert_eq!(on_disk.workspace_ui[0].session_name, "tabs");
    }
}
