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
