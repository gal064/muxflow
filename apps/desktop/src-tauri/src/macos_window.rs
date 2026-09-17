//! macOS window behaviour that Tauri's window builder does not configure.

/// Opt every window into native full screen.
///
/// `tao` never adds `NSWindowCollectionBehaviorFullScreenPrimary`, so AppKit
/// treats the window as full-screen-incapable: `View > Toggle Full Screen` and
/// the green button both degrade to a zoom that fills the screen underneath the
/// menu bar, and `AXFullScreen` stays false. That is M10-E055. Setting the
/// behaviour once at startup restores the standard macOS behaviour — its own
/// Space, hidden menu bar, and the full-screen affordance on the green button.
#[cfg(target_os = "macos")]
pub fn enable_native_full_screen(window: &tauri::WebviewWindow) -> Result<(), String> {
    use objc2_app_kit::{NSWindow, NSWindowCollectionBehavior};

    let handle = window
        .ns_window()
        .map_err(|error| format!("resolve NSWindow for native full screen: {error}"))?;
    if handle.is_null() {
        return Err("NSWindow handle for native full screen was null".into());
    }
    // Safe: `setup` runs on the main thread, and Tauri owns the window for the
    // lifetime of the process, so the pointer stays valid for this borrow.
    let ns_window: &NSWindow = unsafe { &*handle.cast::<NSWindow>() };
    let behavior = ns_window.collectionBehavior();
    ns_window.setCollectionBehavior(behavior | NSWindowCollectionBehavior::FullScreenPrimary);
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn enable_native_full_screen(_window: &tauri::WebviewWindow) -> Result<(), String> {
    Ok(())
}
