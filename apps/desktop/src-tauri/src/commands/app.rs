use desktop_core::{
    app_state::{AppStateStore, PersistedAppState},
    incidents::IncidentJournal,
    notifications::{
        AgentNotificationContent, AgentNotificationRouteContent, NativeNotifications,
        NotificationReceipt,
    },
    perf_log::bridge::BridgeFinalTotals,
    update_check::AvailableUpdate,
};
use phase0_core::SyntheticTopology;
use tauri::{AppHandle, State, WebviewWindow};

#[tauri::command]
pub fn load_app_state(store: State<'_, AppStateStore>) -> Result<PersistedAppState, String> {
    desktop_core::app_state::load_app_state(&store)
}

#[tauri::command]
pub async fn save_app_state(
    state: PersistedAppState,
    store: State<'_, AppStateStore>,
) -> Result<(), String> {
    desktop_core::app_state::save_app_state(state, &store).await
}

#[tauri::command]
pub fn reset_app_state(store: State<'_, AppStateStore>) -> Result<PersistedAppState, String> {
    desktop_core::app_state::reset_app_state(&store)
}

#[tauri::command]
pub fn record_incident(journal: State<'_, IncidentJournal>, line: String) -> Result<(), String> {
    desktop_core::incidents::record_incident(&journal, line)
}

#[tauri::command]
pub fn open_external_link(url: String, confirmed: bool) -> Result<(), String> {
    desktop_core::external_links::open_external_link(url, confirmed)
}

#[tauri::command]
pub async fn install_update() -> Result<(), String> {
    desktop_core::update_check::install_update().await
}

#[tauri::command]
pub fn acknowledge_bridge_events(
    measurement_id: String,
    cumulative_frame_count: u64,
    cumulative_byte_length: u64,
) -> Result<(), String> {
    desktop_core::perf_log::bridge::acknowledge_bridge_events(
        measurement_id,
        cumulative_frame_count,
        cumulative_byte_length,
    )
}

#[tauri::command]
pub fn bridge_final_totals(measurement_id: String) -> Result<BridgeFinalTotals, String> {
    desktop_core::perf_log::bridge::bridge_final_totals(measurement_id)
}

#[tauri::command]
pub fn finalize_bridge_measurement(measurement_id: String) -> Result<(), String> {
    desktop_core::perf_log::bridge::finalize_bridge_measurement(measurement_id)
}

#[tauri::command]
pub fn sample_native_measurements() -> Result<(), String> {
    desktop_core::perf_log::operations::sample_native_measurements()
}

#[tauri::command]
pub fn perf_log_enabled() -> bool {
    desktop_core::perf_log::sink::perf_log_enabled()
}

#[tauri::command]
pub async fn append_perf_log(lines: Vec<String>) -> Result<(), String> {
    desktop_core::perf_log::sink::append_perf_log(lines).await
}

/// Zooms the whole page, terminals included, the way a browser's zoom does:
/// every length scales together, so fixed-height rows never clip their text.
#[tauri::command]
pub fn set_ui_scale(window: WebviewWindow, scale: f64) -> Result<(), String> {
    window
        .set_zoom(desktop_core::app_state::valid_ui_scale(scale)?)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> Result<Option<AvailableUpdate>, String> {
    let running = app.package_info().version.clone();
    desktop_core::update_check::check_for_update((running.major, running.minor, running.patch))
        .await
}

#[tauri::command]
pub fn resolve_notification_route(
    route: AgentNotificationRouteContent,
    topology: SyntheticTopology,
) -> Result<serde_json::Value, String> {
    desktop_core::notifications::resolve_notification_route(route, topology)
}

#[tauri::command]
pub async fn emit_agent_notification(
    notification: AgentNotificationContent,
    notifications: State<'_, NativeNotifications>,
) -> Result<NotificationReceipt, String> {
    desktop_core::notifications::emit_agent_notification(notification, &notifications).await
}

#[tauri::command]
pub async fn notification_permission_status(
    notifications: State<'_, NativeNotifications>,
) -> Result<String, String> {
    desktop_core::notifications::notification_permission_status(&notifications).await
}

#[tauri::command]
pub async fn emit_test_notification(
    notifications: State<'_, NativeNotifications>,
) -> Result<NotificationReceipt, String> {
    desktop_core::notifications::emit_test_notification(&notifications).await
}
