use std::sync::Arc;

use crate::platform::EventSink;

#[cfg(target_os = "linux")]
pub fn start(resumed: Arc<dyn EventSink>) {
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
                    let _ = resumed.send_json(&());
                }
            }
        })
        .expect("spawn Linux power event listener");
}

#[cfg(target_os = "macos")]
pub fn start(resumed: Arc<dyn EventSink>) {
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2_app_kit::{
        NSWorkspace, NSWorkspaceDidWakeNotification, NSWorkspaceSessionDidBecomeActiveNotification,
    };
    use objc2_foundation::{NSNotificationCenter, NSObjectProtocol};

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
        let resumed = Arc::clone(&resumed);
        let callback = RcBlock::new(move |_| {
            let _ = resumed.send_json(&());
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
pub fn start(_: Arc<dyn EventSink>) {}
