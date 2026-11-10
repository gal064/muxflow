//! The seam between the connection state machine and `desktop-core`, so tests
//! can drive every flow without opening a bridge.

use std::sync::Arc;

use desktop_core::{
    app_state::{AppStateStore, PersistedAppState, save_app_state},
    connection::{
        self, ConnectionSpec, TerminalClients, delivery_ack,
        helper::{self, HelperInstallReport},
    },
    incidents::{IncidentJournal, record_incident},
    platform::{self, EventSink},
};
use futures::future::{BoxFuture, FutureExt};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::{
    helper_upgrade::RemoteHelperProbe,
    resume_recovery::{network_online, suspend_aware_now_ms},
};

pub trait ConnectionBackend: Send + Sync {
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
    fn save_app_state(&self, state: PersistedAppState) -> BoxFuture<'static, Result<(), String>>;
    /// `None` when the client has not read a host frame yet.
    fn late_requests(&self, client_id: &str) -> Option<u32>;
    fn record_incident(&self, kind: &str, detail: Value);
    /// Milliseconds on one clock that keeps counting through a suspend.
    fn now_ms(&self) -> u64;
    fn network_online(&self) -> bool;
}

pub struct NativeBackend {
    app_state: Arc<AppStateStore>,
    clients: Arc<TerminalClients>,
    journal: Arc<IncidentJournal>,
    launch: String,
}

impl NativeBackend {
    pub fn new(app_state: Arc<AppStateStore>, journal: IncidentJournal) -> Self {
        Self {
            app_state,
            clients: Arc::default(),
            journal: Arc::new(journal),
            launch: Uuid::new_v4().simple().to_string()[..8].to_owned(),
        }
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
        spawned(async move { save_app_state(state, &store).await })
    }

    fn late_requests(&self, client_id: &str) -> Option<u32> {
        connection::terminal_link_stats(client_id.to_owned(), &self.clients)
            .ok()
            .map(|stats| stats.late_requests_total)
    }

    fn record_incident(&self, kind: &str, detail: Value) {
        let mut record = Map::new();
        record.insert(
            "t".into(),
            Value::String(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        );
        record.insert("launch".into(), Value::String(self.launch.clone()));
        record.insert("kind".into(), Value::String(kind.to_owned()));
        if let Value::Object(fields) = detail {
            record.extend(fields);
        }
        // Must stay harmless: a journal that cannot be written is never a
        // reason the app misbehaves.
        let _ = record_incident(&self.journal, Value::Object(record).to_string());
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
