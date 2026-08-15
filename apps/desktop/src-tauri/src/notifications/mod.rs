/// The one notification a person can ask for, worded once for every backend.
pub(crate) const TEST_TITLE: &str = "tmux Agent IDE";
pub(crate) const TEST_BODY: &str = "Test notification — delivery works.";

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
