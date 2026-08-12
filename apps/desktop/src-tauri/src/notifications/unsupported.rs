use phase0_core::NotificationRoute;
use serde::Serialize;
use tauri::AppHandle;

pub struct NativeNotifications;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationReceipt {
    pub id: u32,
    pub actionable: bool,
}

impl NativeNotifications {
    pub fn new(_app: AppHandle) -> Self {
        Self
    }

    pub fn notify(
        &self,
        _title: &str,
        _body: &str,
        _route: NotificationRoute,
        _request_action: bool,
    ) -> Result<NotificationReceipt, String> {
        Err("native notification activation is not implemented on this platform yet".into())
    }
}
