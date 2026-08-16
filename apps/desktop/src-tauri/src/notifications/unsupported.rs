use phase0_core::NotificationRoute;
use serde::Serialize;
use tauri::AppHandle;

#[derive(Clone)]
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
        _present_in_foreground: bool,
    ) -> Result<NotificationReceipt, String> {
        Err(UNIMPLEMENTED.into())
    }

    pub fn send_test_notification(&self) -> Result<NotificationReceipt, String> {
        Err(UNIMPLEMENTED.into())
    }

    /// The same word the other two backends use for "there is nothing here to
    /// grant" — the UI has one branch for it rather than a platform check.
    pub fn authorization_status(&self) -> Result<String, String> {
        Ok("unsupported".into())
    }
}

const UNIMPLEMENTED: &str =
    "native notification activation is not implemented on this platform yet";
