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

#[cfg(not(target_os = "linux"))]
pub fn start(_: tauri::AppHandle) {}
