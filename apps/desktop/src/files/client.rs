//! The file service and downloads as the Explorer reaches them: `desktop-core`
//! called directly, behind a trait so tests can hold every answer. Replaces the
//! Tauri client in `api.ts` and the abort plumbing in `transport/abortable.ts`.
//!
//! Cancellation is a dropped future: [`request`] carries a guard that tells the
//! host to stop a read nobody will wait for any more, so collapsing a folder or
//! replacing a root stops the bounded remote scan rather than paying for it.

use std::{rc::Rc, sync::Arc};

use desktop_core::{
    connection::{
        ProfileStore, TerminalClients,
        files::{
            DownloadManager, FileCommand, cancel_file_request,
            download_manager::{
                self, DownloadCollisionPolicy, cancel_download, suggest_download_destination,
            },
            download_opener, file_request,
        },
    },
    platform::{self, EventSink},
};
use futures::future::{BoxFuture, FutureExt};
use serde_json::Value;

use super::wire::{ActiveRoot, FileScope};

/// What a download asks for once its destination is chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadStart {
    pub scope: FileScope,
    pub root: ActiveRoot,
    pub source: String,
    pub destination: String,
    pub folder: bool,
    /// The save panel showed this exact name, so its own prompt was consent to
    /// replace it. Otherwise an existing file is refused, never overwritten.
    pub overwrite: bool,
}

pub trait FileTransport {
    /// One control-lane file request (`file_request`), answered with its JSON.
    fn request(
        &self,
        client_id: String,
        command: Value,
    ) -> BoxFuture<'static, Result<Value, String>>;
    /// Stops a cancellable read by its operation id. Fire and forget.
    fn cancel(&self, client_id: String, operation_id: String);
    fn start_download(
        &self,
        request: DownloadStart,
        events: Arc<dyn EventSink>,
    ) -> Result<String, String>;
    fn cancel_download(&self, transfer_id: String) -> Result<(), String>;
    /// A non-colliding path in the Downloads directory.
    fn suggest_destination(&self, file_name: String) -> Result<String, String>;
    fn open_download(&self, path: String) -> Result<(), String>;
    fn reveal_download(&self, path: String) -> Result<(), String>;
}

/// Tells the host to stop the read it guards unless the read finished first.
struct Pending {
    transport: Rc<dyn FileTransport>,
    client_id: String,
    operation_id: String,
    finished: bool,
}

impl Drop for Pending {
    fn drop(&mut self) {
        if !self.finished {
            self.transport
                .cancel(self.client_id.clone(), self.operation_id.clone());
        }
    }
}

/// One cancellable read: dropping the future before it answers cancels it on
/// the host. Only reads are cancelled this way; a mutation that could be
/// stopped midway would leave its outcome unknown.
pub async fn request(
    transport: Rc<dyn FileTransport>,
    client_id: String,
    operation_id: String,
    command: Value,
) -> Result<Value, String> {
    let answer = transport.request(client_id.clone(), command);
    let mut pending = Pending {
        transport,
        client_id,
        operation_id,
        finished: false,
    };
    let result = answer.await;
    pending.finished = true;
    result
}

pub fn operation_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The real service, through the bridge's clients.
pub struct NativeFiles {
    pub clients: Arc<TerminalClients>,
    pub profiles: Arc<ProfileStore>,
    pub downloads: DownloadManager,
}

impl FileTransport for NativeFiles {
    fn request(
        &self,
        client_id: String,
        command: Value,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let clients = self.clients.clone();
        let command = serde_json::from_value::<FileCommand>(command);
        // Started now, on the runtime, so a request nobody polls still goes out
        // (an unwatch is sent and forgotten).
        let handle = platform::spawn(async move {
            let command = command.map_err(|error| error.to_string())?;
            file_request(client_id, command, &clients).await
        });
        async move {
            handle
                .await
                .map_err(|error| format!("file request task failed: {error}"))?
        }
        .boxed()
    }

    fn cancel(&self, client_id: String, operation_id: String) {
        let clients = self.clients.clone();
        // A read that already finished has nothing to stop.
        drop(platform::spawn(async move {
            let _ = cancel_file_request(client_id, operation_id, &clients).await;
        }));
    }

    fn start_download(
        &self,
        request: DownloadStart,
        events: Arc<dyn EventSink>,
    ) -> Result<String, String> {
        let collision = if request.overwrite {
            DownloadCollisionPolicy::OverwriteConfirmed
        } else {
            DownloadCollisionPolicy::Fail
        };
        let scope = request.scope;
        download_manager::start_download(
            scope.client_id,
            scope.host_profile_id,
            scope.server_identity,
            scope.terminal_epoch.to_string(),
            request.root.path,
            request.root.token,
            request.source,
            request.destination,
            request.folder,
            collision,
            events,
            &self.profiles,
            &self.clients,
            &self.downloads,
        )
    }

    fn cancel_download(&self, transfer_id: String) -> Result<(), String> {
        cancel_download(transfer_id)
    }

    fn suggest_destination(&self, file_name: String) -> Result<String, String> {
        suggest_download_destination(file_name, &crate::paths::NativePaths, &self.downloads)
    }

    fn open_download(&self, path: String) -> Result<(), String> {
        download_opener::open_download(path, &self.downloads)
    }

    fn reveal_download(&self, path: String) -> Result<(), String> {
        download_opener::reveal_download(path, &self.downloads)
    }
}
