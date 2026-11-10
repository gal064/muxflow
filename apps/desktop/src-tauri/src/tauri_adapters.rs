//! Tauri's implementations of the backend's `platform` traits.

use std::path::PathBuf;

use tauri::{
    AppHandle, Emitter, Manager, Runtime, Wry,
    ipc::{Channel, InvokeResponseBody},
};

use desktop_core::{
    notifications::ActivationSink,
    platform::{AppPaths, EventBody, EventSink},
};

/// Tells the page the machine woke, so it can probe its links.
pub const DESKTOP_RESUMED: &str = "desktop-resumed";
/// Tells the page to re-read `ports_list`; the payload is the profile id.
pub const PORT_FORWARDS_CHANGED: &str = "port-forwards-changed";

/// One frontend `Channel`, as an [`EventSink`].
pub struct ChannelSink(pub Channel<InvokeResponseBody>);

impl EventSink for ChannelSink {
    fn send(&self, body: EventBody) -> Result<(), String> {
        let body = match body {
            EventBody::Json(json) => InvokeResponseBody::Json(json),
            EventBody::Raw(frame) => InvokeResponseBody::Raw(frame),
        };
        self.0.send(body).map_err(|error| error.to_string())
    }
}

/// One named application event, as an [`EventSink`].
pub struct AppEvent<R: Runtime = Wry> {
    app: AppHandle<R>,
    name: &'static str,
}

impl<R: Runtime> AppEvent<R> {
    pub fn new(app: AppHandle<R>, name: &'static str) -> Self {
        Self { app, name }
    }
}

impl<R: Runtime> EventSink for AppEvent<R> {
    fn send(&self, body: EventBody) -> Result<(), String> {
        match body {
            EventBody::Json(payload) => self
                .app
                .emit_str(self.name, payload)
                .map_err(|error| error.to_string()),
            EventBody::Raw(_) => Err(format!("application event {} is JSON only", self.name)),
        }
    }
}

/// The running application, as the backend's view of its paths and window.
pub struct TauriApp<R: Runtime = Wry>(pub AppHandle<R>);

impl<R: Runtime> AppPaths for TauriApp<R> {
    fn app_config_dir(&self) -> Result<PathBuf, String> {
        self.0
            .path()
            .app_config_dir()
            .map_err(|error| error.to_string())
    }

    fn app_data_dir(&self) -> Result<PathBuf, String> {
        self.0
            .path()
            .app_data_dir()
            .map_err(|error| error.to_string())
    }

    fn app_log_dir(&self) -> Result<PathBuf, String> {
        self.0
            .path()
            .app_log_dir()
            .map_err(|error| error.to_string())
    }

    fn download_dir(&self) -> Result<PathBuf, String> {
        self.0
            .path()
            .download_dir()
            .map_err(|error| error.to_string())
    }
}

impl<R: Runtime> ActivationSink for TauriApp<R> {
    fn focus_main_window(&self) {
        if let Some(window) = self.0.get_webview_window("main") {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
    }

    fn emit_activation(&self, payload: serde_json::Value) {
        let _ = self.0.emit("notification-activated", payload);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tauri::Listener;

    use super::*;

    fn delivered_body(body: EventBody) -> InvokeResponseBody {
        let delivered = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&delivered);
        let channel: Channel<InvokeResponseBody> = Channel::new(move |body| {
            *slot.lock().unwrap() = Some(body);
            Ok(())
        });
        ChannelSink(channel).send(body).unwrap();
        delivered.lock().unwrap().take().unwrap()
    }

    #[test]
    fn a_channel_delivers_each_body_kind_unchanged() {
        match delivered_body(EventBody::Raw(vec![1, 2, 3])) {
            InvokeResponseBody::Raw(frame) => assert_eq!(frame, [1, 2, 3]),
            InvokeResponseBody::Json(_) => panic!("raw frame became JSON"),
        }
        match delivered_body(EventBody::Json(r#"{"a":1}"#.into())) {
            InvokeResponseBody::Json(json) => assert_eq!(json, r#"{"a":1}"#),
            InvokeResponseBody::Raw(_) => panic!("JSON event became raw"),
        }
    }

    #[test]
    fn an_app_event_carries_the_json_it_was_given() {
        let app = tauri::test::mock_app();
        let received = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&received);
        app.listen(PORT_FORWARDS_CHANGED, move |event| {
            log.lock().unwrap().push(event.payload().to_owned());
        });
        let sink: Arc<dyn EventSink> =
            Arc::new(AppEvent::new(app.handle().clone(), PORT_FORWARDS_CHANGED));
        sink.send_json(&"profile-1").unwrap();
        sink.send_json(&()).unwrap();
        assert_eq!(*received.lock().unwrap(), [r#""profile-1""#, "null"]);
        assert!(sink.send_raw(vec![0]).is_err());
    }
}
