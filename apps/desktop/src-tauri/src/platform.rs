//! What the backend needs from the application embedding it, as traits.
//! `tauri_adapters` implements them and `commands` hands them in.

use std::path::PathBuf;

use serde::Serialize;

/// The Tauri runtime stays the backend's runtime until `desktop-core` owns one.
pub use tauri::async_runtime::{spawn, spawn_blocking};

pub enum EventBody {
    Json(String),
    Raw(Vec<u8>),
}

/// One ordered stream of events to the UI: a terminal, a transfer, or one named
/// application event. Callable from any thread, and it may run arbitrary UI
/// work, so a caller holding a lock or a transfer slot must bound the call.
pub trait EventSink: Send + Sync {
    fn send(&self, body: EventBody) -> Result<(), String>;
}

impl dyn EventSink {
    pub fn send_json(&self, value: &impl Serialize) -> Result<(), String> {
        let json = serde_json::to_string(value).map_err(|error| error.to_string())?;
        self.send(EventBody::Json(json))
    }

    pub fn send_raw(&self, frame: Vec<u8>) -> Result<(), String> {
        self.send(EventBody::Raw(frame))
    }
}

/// The per-user directories the backend stores into. The three `app_*` ones are
/// pinned by the golden test in `golden_paths.rs`.
pub trait AppPaths {
    fn app_config_dir(&self) -> Result<PathBuf, String>;
    fn app_data_dir(&self) -> Result<PathBuf, String>;
    fn app_log_dir(&self) -> Result<PathBuf, String>;
    fn download_dir(&self) -> Result<PathBuf, String>;
}

/// Tests' stand-in for a UI listener.
#[cfg(test)]
pub(crate) fn test_sink(
    handler: impl Fn(EventBody) -> Result<(), String> + Send + Sync + 'static,
) -> std::sync::Arc<dyn EventSink> {
    struct FnSink<F>(F);

    impl<F: Fn(EventBody) -> Result<(), String> + Send + Sync> EventSink for FnSink<F> {
        fn send(&self, body: EventBody) -> Result<(), String> {
            (self.0)(body)
        }
    }

    std::sync::Arc::new(FnSink(handler))
}
