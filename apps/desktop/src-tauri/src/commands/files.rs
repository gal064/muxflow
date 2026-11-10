use serde_json::Value;
use std::sync::Arc;
use tauri::{
    AppHandle, State,
    ipc::{Channel, InvokeBody, InvokeResponseBody, Request},
};

use crate::connection::{
    ProfileStore, TerminalClients,
    files::{
        FileCommand, FileIoManager,
        download_manager::{DownloadCollisionPolicy, DownloadManager},
        upload_manager::{InspectedLocalTerminalPath, UploadCollisionPolicy, UploadManager},
    },
};
use crate::tauri_adapters::{ChannelSink, TauriApp};

#[tauri::command]
pub async fn file_request(
    client_id: String,
    command: FileCommand,
    clients: State<'_, TerminalClients>,
) -> Result<Value, String> {
    crate::connection::files::file_request(client_id, command, &clients).await
}

#[tauri::command]
pub async fn cancel_file_request(
    client_id: String,
    operation_id: String,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    crate::connection::files::cancel_file_request(client_id, operation_id, &clients).await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn start_download(
    client_id: String,
    profile_id: String,
    expected_server_identity: String,
    connection_epoch: String,
    root: String,
    root_token: String,
    source: String,
    destination: String,
    folder: bool,
    collision: DownloadCollisionPolicy,
    on_event: Channel<InvokeResponseBody>,
    profiles: State<'_, ProfileStore>,
    clients: State<'_, TerminalClients>,
    transfers: State<'_, DownloadManager>,
) -> Result<String, String> {
    crate::connection::files::download_manager::start_download(
        client_id,
        profile_id,
        expected_server_identity,
        connection_epoch,
        root,
        root_token,
        source,
        destination,
        folder,
        collision,
        Arc::new(ChannelSink(on_event)),
        &profiles,
        &clients,
        &transfers,
    )
}

#[tauri::command]
pub fn cancel_download(transfer_id: String) -> Result<(), String> {
    crate::connection::files::download_manager::cancel_download(transfer_id)
}

#[tauri::command]
pub fn open_download(path: String, transfers: State<'_, DownloadManager>) -> Result<(), String> {
    crate::connection::files::download_opener::open_download(path, &transfers)
}

#[tauri::command]
pub fn reveal_download(path: String, transfers: State<'_, DownloadManager>) -> Result<(), String> {
    crate::connection::files::download_opener::reveal_download(path, &transfers)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn start_file_read(
    client_id: String,
    profile_id: String,
    expected_server_identity: String,
    connection_epoch: String,
    root: String,
    root_token: String,
    path: String,
    on_event: Channel<InvokeResponseBody>,
    profiles: State<'_, ProfileStore>,
    clients: State<'_, TerminalClients>,
    file_io: State<'_, FileIoManager>,
) -> Result<String, String> {
    crate::connection::files::editor_manager::start_file_read(
        client_id,
        profile_id,
        expected_server_identity,
        connection_epoch,
        root,
        root_token,
        path,
        Arc::new(ChannelSink(on_event)),
        &profiles,
        &clients,
        &file_io,
    )
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn start_file_write(
    client_id: String,
    profile_id: String,
    expected_server_identity: String,
    connection_epoch: String,
    root: String,
    root_token: String,
    path: String,
    operation_id: String,
    file_generation: String,
    content: Vec<u8>,
    on_event: Channel<InvokeResponseBody>,
    profiles: State<'_, ProfileStore>,
    clients: State<'_, TerminalClients>,
    file_io: State<'_, FileIoManager>,
) -> Result<String, String> {
    crate::connection::files::editor_manager::start_file_write(
        client_id,
        profile_id,
        expected_server_identity,
        connection_epoch,
        root,
        root_token,
        path,
        operation_id,
        file_generation,
        content,
        Arc::new(ChannelSink(on_event)),
        &profiles,
        &clients,
        &file_io,
    )
}

#[tauri::command]
pub fn cancel_file_io(
    transfer_id: String,
    file_io: State<'_, FileIoManager>,
) -> Result<(), String> {
    crate::connection::files::editor_manager::cancel_file_io(transfer_id, &file_io)
}

#[tauri::command]
pub fn inspect_local_terminal_paths(
    paths: Vec<String>,
) -> Result<Vec<InspectedLocalTerminalPath>, String> {
    crate::connection::files::upload_manager::inspect_local_terminal_paths(paths)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn start_terminal_upload_preflight(
    client_id: String,
    profile_id: String,
    expected_server_identity: String,
    connection_epoch: String,
    source_path: String,
    destination_name: String,
    collision: UploadCollisionPolicy,
    large_upload_confirmed: bool,
    image_png: bool,
    on_event: Channel<InvokeResponseBody>,
    profiles: State<'_, ProfileStore>,
    clients: State<'_, TerminalClients>,
    uploads: State<'_, UploadManager>,
) -> Result<String, String> {
    crate::connection::files::upload_manager::start_terminal_upload_preflight(
        client_id,
        profile_id,
        expected_server_identity,
        connection_epoch,
        source_path,
        destination_name,
        collision,
        large_upload_confirmed,
        image_png,
        Arc::new(ChannelSink(on_event)),
        &profiles,
        &clients,
        &uploads,
    )
}

#[tauri::command]
pub fn cancel_terminal_upload_preflight(
    preflight_id: String,
    _uploads: State<'_, UploadManager>,
) -> Result<Value, String> {
    crate::connection::files::upload_manager::cancel_terminal_upload_preflight(
        preflight_id,
        &_uploads,
    )
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn start_terminal_upload(
    client_id: String,
    profile_id: String,
    expected_server_identity: String,
    connection_epoch: String,
    source_path: String,
    destination_name: String,
    collision: UploadCollisionPolicy,
    large_upload_confirmed: bool,
    image_png: bool,
    on_event: Channel<InvokeResponseBody>,
    profiles: State<'_, ProfileStore>,
    clients: State<'_, TerminalClients>,
    uploads: State<'_, UploadManager>,
) -> Result<String, String> {
    crate::connection::files::upload_manager::start_terminal_upload(
        client_id,
        profile_id,
        expected_server_identity,
        connection_epoch,
        source_path,
        destination_name,
        collision,
        large_upload_confirmed,
        image_png,
        Arc::new(ChannelSink(on_event)),
        &profiles,
        &clients,
        &uploads,
    )
}

#[tauri::command]
pub fn cancel_terminal_upload(
    transfer_id: String,
    uploads: State<'_, UploadManager>,
) -> Result<Value, String> {
    crate::connection::files::upload_manager::cancel_terminal_upload(transfer_id, &uploads)
}

#[tauri::command]
pub async fn read_native_terminal_clipboard() -> Result<Option<Value>, String> {
    crate::connection::files::native_clipboard::read_native_terminal_clipboard().await
}

#[tauri::command]
pub fn suggest_download_destination(
    file_name: String,
    app: AppHandle,
    transfers: State<'_, DownloadManager>,
) -> Result<String, String> {
    crate::connection::files::download_manager::suggest_download_destination(
        file_name,
        &TauriApp(app),
        &transfers,
    )
}

#[tauri::command]
pub fn stage_clipboard_png(request: Request<'_>) -> Result<Value, String> {
    let InvokeBody::Raw(png_bytes) = request.body() else {
        return Err("clipboard PNG IPC body must be raw binary".into());
    };
    crate::connection::files::upload_manager::stage_clipboard_png(png_bytes)
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub async fn write_native_terminal_clipboard(text: String) -> Result<(), String> {
    crate::connection::files::native_clipboard::write_native_terminal_clipboard(text).await
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn write_native_terminal_clipboard(app: AppHandle, text: String) -> Result<(), String> {
    use crate::connection::files::native_clipboard::write_macos_clipboard_on_main_thread;

    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = sender.send(write_macos_clipboard_on_main_thread(text));
    })
    .map_err(|error| format!("schedule native clipboard write on the main thread: {error}"))?;
    receiver
        .await
        .map_err(|_| "native clipboard main-thread write was cancelled".to_string())?
}
