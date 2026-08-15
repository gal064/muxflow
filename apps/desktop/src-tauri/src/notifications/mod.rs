/// The one notification a person can ask for, worded once for every backend.
pub(crate) const TEST_TITLE: &str = "tmux Agent IDE";
pub(crate) const TEST_BODY: &str = "Test notification — delivery works.";

/// The whole vocabulary `authorization_status` may answer with.
///
/// Every backend maps its own platform's states onto these, and the frontend
/// renders one sentence per word (`features/agents/notifications.ts`). Naming
/// them once here is what stops a backend inventing a sixth word that the UI
/// would render as an empty line.
pub(crate) const PERMISSION_STATUSES: [&str; 5] = [
    "authorized",
    "denied",
    "notDetermined",
    "provisional",
    "unsupported",
];

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod unsupported;

#[cfg(target_os = "linux")]
pub use linux::{NativeNotifications, NotificationReceipt};
#[cfg(target_os = "macos")]
pub use macos::{NativeNotifications, NotificationReceipt};
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub use unsupported::{NativeNotifications, NotificationReceipt};
