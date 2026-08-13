#[cfg(target_os = "linux")]
pub fn start(handle: tauri::AppHandle) {
    use tauri::Emitter;

    std::thread::Builder::new()
        .name("linux-power-events".into())
        .spawn(move || {
            let Ok(connection) = zbus::blocking::Connection::system() else {
                return;
            };
            let Ok(proxy) = zbus::blocking::Proxy::new(
                &connection,
                "org.freedesktop.login1",
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager",
            ) else {
                return;
            };
            let Ok(signals) = proxy.receive_signal("PrepareForSleep") else {
                return;
            };
            for signal in signals {
                if matches!(signal.body().deserialize::<bool>(), Ok(false)) {
                    let _ = handle.emit("desktop-resumed", ());
                }
            }
        })
        .expect("spawn Linux power event listener");
}

#[cfg(target_os = "macos")]
pub fn start(handle: tauri::AppHandle) {
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2_app_kit::{
        NSWorkspace, NSWorkspaceDidWakeNotification, NSWorkspaceSessionDidBecomeActiveNotification,
    };
    use objc2_foundation::{NSNotificationCenter, NSObjectProtocol};
    use tauri::Emitter;

    let center: Retained<NSNotificationCenter> =
        NSWorkspace::sharedWorkspace().notificationCenter();
    // SAFETY: these are immutable AppKit notification-name constants and the
    // process is linked against the minimum supported macOS AppKit.
    let names = unsafe {
        [
            NSWorkspaceDidWakeNotification,
            NSWorkspaceSessionDidBecomeActiveNotification,
        ]
    };
    for name in names {
        let handle = handle.clone();
        let callback = RcBlock::new(move |_| {
            let _ = handle.emit("desktop-resumed", ());
        });
        let observer: Retained<objc2::runtime::ProtocolObject<dyn NSObjectProtocol>> = unsafe {
            center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &callback)
        };
        // The notification center owns the copied block but removal requires
        // this token. Both are process-lifetime application observers.
        let _ = Retained::into_raw(observer);
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn start(_: tauri::AppHandle) {}
