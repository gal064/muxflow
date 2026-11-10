use std::sync::Arc;

use serde_json::Value;
use tauri::{
    AppHandle, State,
    ipc::{Channel, InvokeBody, InvokeResponseBody, Request},
};

use crate::{
    connection::{
        ConnectionSpec, InputLatencyStats, ProfileStore, TerminalClients, TerminalLinkStats,
        agent::AgentCommand,
        automation::AutomationCommand,
        git::GitCommand,
        git_content::GitDiffContentCommand,
        helper::HelperInstallReport,
        ports::{DetectedPort, PortForward, PortForwards},
        profiles::HostProfile,
        tmux_action::{TmuxActionResultWire, TmuxActionWire},
    },
    platform::EventSink,
    tauri_adapters::{AppEvent, ChannelSink, PORT_FORWARDS_CHANGED},
};

#[tauri::command]
pub fn set_terminal_colors(
    foreground: String,
    background: String,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    crate::connection::set_terminal_colors(foreground, background, &clients)
}

#[tauri::command]
pub fn start_terminal(
    session_id: String,
    pane_ids: Vec<String>,
    connection: ConnectionSpec,
    attach: bool,
    measurement_id: String,
    on_event: Channel<InvokeResponseBody>,
    clients: State<'_, TerminalClients>,
) -> Result<String, String> {
    crate::connection::start_terminal(
        session_id,
        pane_ids,
        connection,
        attach,
        measurement_id,
        Arc::new(ChannelSink(on_event)),
        &clients,
    )
}

#[tauri::command]
pub fn stop_terminal(client_id: String, clients: State<'_, TerminalClients>) -> Result<(), String> {
    crate::connection::stop_terminal(client_id, &clients)
}

#[tauri::command]
pub fn prewarm_terminal_bulk(
    client_id: String,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    crate::connection::prewarm_terminal_bulk(client_id, &clients)
}

#[tauri::command]
pub fn send_terminal_input(
    client_id: String,
    pane_id: String,
    data: String,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    crate::connection::send_terminal_input(client_id, pane_id, data, &clients)
}

#[tauri::command]
pub async fn probe_terminal_link(
    client_id: String,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    crate::connection::probe_terminal_link(client_id, &clients).await
}

#[tauri::command]
pub async fn discard_ssh_masters() -> Result<(), String> {
    crate::connection::discard_ssh_masters().await
}

#[tauri::command]
pub async fn resize_terminal_client(
    client_id: String,
    columns: u16,
    rows: u16,
    session_id: String,
    window_id: String,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    crate::connection::resize_terminal_client(
        client_id, columns, rows, session_id, window_id, &clients,
    )
    .await
}

#[tauri::command]
pub async fn request_terminal_seed(
    client_id: String,
    pane_id: String,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    crate::connection::request_terminal_seed(client_id, pane_id, &clients).await
}

#[tauri::command]
pub async fn request_terminal_history(
    client_id: String,
    pane_id: String,
    lines: u32,
    skip_lines: u32,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    crate::connection::request_terminal_history(client_id, pane_id, lines, skip_lines, &clients)
        .await
}

#[tauri::command]
pub fn terminal_link_stats(
    client_id: String,
    clients: State<'_, TerminalClients>,
) -> Result<TerminalLinkStats, String> {
    crate::connection::terminal_link_stats(client_id, &clients)
}

#[tauri::command]
pub fn input_latency_stats(
    client_id: String,
    clients: State<'_, TerminalClients>,
) -> Option<InputLatencyStats> {
    crate::connection::input_latency_stats(client_id, &clients)
}

#[tauri::command]
pub fn list_host_profiles(store: State<'_, ProfileStore>) -> Result<serde_json::Value, String> {
    crate::connection::profiles::list_host_profiles(&store)
}

#[tauri::command]
pub fn reset_host_profiles(store: State<'_, ProfileStore>) -> Result<(), String> {
    crate::connection::profiles::reset_host_profiles(&store)
}

#[tauri::command]
pub fn save_host_profile(
    profile: HostProfile,
    store: State<'_, ProfileStore>,
) -> Result<(), String> {
    crate::connection::profiles::save_host_profile(profile, &store)
}

#[tauri::command]
pub fn set_last_profile_id(
    profile_id: String,
    store: State<'_, ProfileStore>,
) -> Result<(), String> {
    crate::connection::profiles::set_last_profile_id(profile_id, &store)
}

#[tauri::command]
pub fn delete_host_profile(
    profile_id: String,
    store: State<'_, ProfileStore>,
) -> Result<serde_json::Value, String> {
    crate::connection::profiles::delete_host_profile(profile_id, &store)
}

#[tauri::command]
pub async fn probe_remote_helper(connection: ConnectionSpec) -> Result<serde_json::Value, String> {
    crate::connection::helper::probe_remote_helper(connection).await
}

#[tauri::command]
pub async fn install_remote_helper(
    connection: ConnectionSpec,
    allow_upgrade: bool,
) -> HelperInstallReport {
    crate::connection::helper::install_remote_helper(connection, allow_upgrade).await
}

#[tauri::command]
pub async fn acknowledge_terminal_delivery(
    client_id: String,
    connection_epoch: u64,
    cumulative_frame_count: u64,
    cumulative_byte_length: u64,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    crate::connection::delivery_ack::acknowledge_terminal_delivery(
        client_id,
        connection_epoch,
        cumulative_frame_count,
        cumulative_byte_length,
        &clients,
    )
    .await
}

#[tauri::command]
pub async fn tmux_action(
    client_id: String,
    action: TmuxActionWire,
    clients: State<'_, TerminalClients>,
) -> Result<TmuxActionResultWire, String> {
    crate::connection::tmux_action::tmux_action(client_id, action, &clients).await
}

#[tauri::command]
pub async fn agent_request(
    client_id: String,
    command: AgentCommand,
    clients: State<'_, TerminalClients>,
) -> Result<Value, String> {
    crate::connection::agent::agent_request(client_id, command, &clients).await
}

#[tauri::command]
pub async fn automation_request(
    client_id: String,
    command: AutomationCommand,
    clients: State<'_, TerminalClients>,
) -> Result<Value, String> {
    crate::connection::automation::automation_request(client_id, command, &clients).await
}

#[tauri::command]
pub async fn git_request(
    client_id: String,
    command: GitCommand,
    clients: State<'_, TerminalClients>,
) -> Result<Value, String> {
    crate::connection::git::git_request(client_id, command, &clients).await
}

#[tauri::command]
pub fn cancel_git_request(
    client_id: String,
    operation_id: String,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    crate::connection::git::cancel_git_request(client_id, operation_id, &clients)
}

#[tauri::command]
pub async fn read_git_diff_content(
    command: GitDiffContentCommand,
    on_event: Channel<InvokeResponseBody>,
    profiles: State<'_, ProfileStore>,
    clients: State<'_, TerminalClients>,
) -> Result<String, String> {
    crate::connection::git_content::read_git_diff_content(
        command,
        Arc::new(ChannelSink(on_event)),
        &profiles,
        &clients,
    )
    .await
}

#[tauri::command]
pub fn cancel_git_diff_content(
    client_id: String,
    read_id: String,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    crate::connection::git_content::cancel_git_diff_content(client_id, read_id, &clients)
}

#[tauri::command]
pub fn ports_list(forwards: State<'_, PortForwards>) -> Vec<PortForward> {
    crate::connection::ports::ports_list(&forwards)
}

#[tauri::command]
pub async fn ports_detect(connection: ConnectionSpec) -> Result<Vec<DetectedPort>, String> {
    crate::connection::ports::ports_detect(connection).await
}

#[tauri::command]
pub fn send_terminal_input_bytes(
    request: Request<'_>,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    let InvokeBody::Raw(body) = request.body() else {
        return Err("terminal input IPC body must be raw binary".into());
    };
    crate::connection::send_terminal_input_bytes(body, &clients)
}

#[tauri::command]
pub async fn set_terminal_visibility(
    request: Request<'_>,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    let InvokeBody::Raw(body) = request.body() else {
        return Err("terminal visibility IPC body must be raw binary".into());
    };
    crate::connection::set_terminal_visibility(body, &clients).await
}

#[tauri::command]
pub fn ports_forward(
    app: AppHandle,
    forwards: State<'_, PortForwards>,
    connection: ConnectionSpec,
    remote_port: u16,
    local_port: u16,
) -> Result<(), String> {
    crate::connection::ports::ports_forward(
        port_changes(app),
        &forwards,
        connection,
        remote_port,
        local_port,
    )
}

#[tauri::command]
pub fn ports_stop(
    app: AppHandle,
    forwards: State<'_, PortForwards>,
    profile_id: String,
    remote_port: u16,
) {
    crate::connection::ports::ports_stop(port_changes(app), &forwards, profile_id, remote_port)
}

#[tauri::command]
pub fn ports_stop_host(app: AppHandle, forwards: State<'_, PortForwards>, profile_id: String) {
    crate::connection::ports::ports_stop_host(port_changes(app), &forwards, profile_id)
}

fn port_changes(app: AppHandle) -> Arc<dyn EventSink> {
    Arc::new(AppEvent::new(app, PORT_FORWARDS_CHANGED))
}
