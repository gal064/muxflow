//! What the backend needs from the application embedding it, as traits, and
//! the tokio runtime it runs its own work on.

use std::{future::Future, path::PathBuf, sync::LazyLock};

use serde::Serialize;
use tokio::{
    runtime::{Handle, Runtime},
    task::JoinHandle,
};

/// Lives for the whole process, so its tasks outlast any one caller.
static RUNTIME: LazyLock<Runtime> =
    LazyLock::new(|| Runtime::new().expect("start the backend's tokio runtime"));

/// The backend's runtime, for an embedder that wants its async work on it too.
pub fn runtime_handle() -> Handle {
    RUNTIME.handle().clone()
}

/// Runs `future` on the backend's runtime. Callable from any thread, including
/// one with no tokio runtime, unlike `tokio::spawn`.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    RUNTIME.spawn(future)
}

/// Runs `work` on the backend's blocking pool. Callable from any thread,
/// including one with no tokio runtime, unlike `tokio::task::spawn_blocking`.
pub fn spawn_blocking<F, R>(work: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    RUNTIME.spawn_blocking(work)
}

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
/// pinned by the golden test in `apps/desktop/tests/paths.rs`.
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

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

    use super::*;

    const PATIENCE: Duration = Duration::from_secs(10);

    /// A thread with no ambient tokio runtime, such as GPUI's main thread.
    fn on_a_thread_without_a_runtime(work: impl FnOnce() + Send + 'static) {
        std::thread::spawn(move || {
            assert!(
                Handle::try_current().is_err(),
                "the test thread has a runtime"
            );
            work();
        })
        .join()
        .unwrap();
    }

    #[test]
    fn spawn_blocking_runs_from_a_thread_without_a_runtime() {
        let (sender, receiver) = mpsc::channel();
        on_a_thread_without_a_runtime(move || {
            spawn_blocking(move || sender.send(7).unwrap());
        });
        assert_eq!(receiver.recv_timeout(PATIENCE), Ok(7));
    }

    /// `git_content` awaits `tokio::task::spawn_blocking` inside `spawn`, and
    /// the commands use `tokio::time`, so a task needs the runtime's context
    /// and its time driver.
    #[test]
    fn a_spawned_task_has_the_runtime_context_and_timers() {
        let (sender, receiver) = mpsc::channel();
        on_a_thread_without_a_runtime(move || {
            spawn(async move {
                tokio::time::sleep(Duration::from_millis(1)).await;
                let value = tokio::task::spawn_blocking(|| 7).await.unwrap();
                sender.send(value).unwrap();
            });
        });
        assert_eq!(receiver.recv_timeout(PATIENCE), Ok(7));
    }
}
