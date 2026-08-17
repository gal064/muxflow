//! Opt-in Phase 14 counters.
//!
//! Git subprocess counts are a property of the process, so they live in one
//! global. Watcher, subscriber and status-pipeline counts are properties of one
//! connection's Git service, so they live on that service: making them global
//! too would mean any two tests running in parallel could not assert an exact
//! number, and "exactly one watcher for 32 consumers" is precisely the fact
//! this package exists to prove.

use std::sync::Mutex;

#[cfg(test)]
use std::{ffi::OsStr, os::unix::ffi::OsStrExt, sync::OnceLock};

#[cfg(test)]
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GitProcessMeasurements {
    pub git_processes: u64,
    pub status_processes: u64,
    pub diff_processes: u64,
    pub mutation_processes: u64,
    pub active_processes: usize,
    pub active_processes_high_water: usize,
}

#[cfg(test)]
fn processes() -> &'static Mutex<GitProcessMeasurements> {
    static VALUE: OnceLock<Mutex<GitProcessMeasurements>> = OnceLock::new();
    VALUE.get_or_init(|| Mutex::new(GitProcessMeasurements::default()))
}

#[cfg(test)]
pub(super) struct GitProcessMeasurement;

#[cfg(test)]
pub(super) fn phase14_git_process_started(command: &OsStr) -> GitProcessMeasurement {
    let mut value = processes().lock().unwrap();
    value.git_processes += 1;
    match command.as_bytes() {
        b"status" => value.status_processes += 1,
        b"diff" => value.diff_processes += 1,
        b"add" | b"apply" | b"reset" | b"commit" | b"restore" => value.mutation_processes += 1,
        _ => {}
    }
    value.active_processes += 1;
    value.active_processes_high_water = value
        .active_processes_high_water
        .max(value.active_processes);
    GitProcessMeasurement
}

#[cfg(test)]
impl Drop for GitProcessMeasurement {
    fn drop(&mut self) {
        let mut value = processes().lock().unwrap();
        value.active_processes = value.active_processes.saturating_sub(1);
    }
}

#[cfg(test)]
pub(super) fn phase14_git_process_snapshot() -> GitProcessMeasurements {
    processes().lock().unwrap().clone()
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GitObservationCounts {
    /// Repository discoveries actually executed, as opposed to answered from
    /// the cached identity.
    pub discoveries: u64,
    pub native_watcher_creations: u64,
    pub native_watchers: usize,
    pub native_watchers_high_water: usize,
    /// Directories handed to `notify` after deduplication.
    pub watch_registrations: u64,
    pub subscribers: usize,
    pub subscribers_high_water: usize,
    /// Complete `git status` pipelines actually executed, as opposed to
    /// coalesced onto another consumer's in-flight pipeline.
    pub status_pipelines: u64,
}

/// One connection's observation counters, shared by its coordinators.
#[derive(Default)]
pub(super) struct GitObservation(Mutex<GitObservationCounts>);

impl GitObservation {
    pub(super) fn watcher_created(&self, registrations: u64) {
        let mut value = self.0.lock().unwrap();
        value.native_watcher_creations += 1;
        value.watch_registrations += registrations;
        value.native_watchers += 1;
        value.native_watchers_high_water =
            value.native_watchers_high_water.max(value.native_watchers);
    }

    pub(super) fn watcher_dropped(&self) {
        let mut value = self.0.lock().unwrap();
        value.native_watchers = value.native_watchers.saturating_sub(1);
    }

    pub(super) fn subscribers_changed(&self, delta: isize) {
        let mut value = self.0.lock().unwrap();
        value.subscribers = value.subscribers.saturating_add_signed(delta);
        value.subscribers_high_water = value.subscribers_high_water.max(value.subscribers);
    }

    pub(super) fn discovery(&self) {
        self.0.lock().unwrap().discoveries += 1;
    }

    pub(super) fn status_pipeline(&self) {
        self.0.lock().unwrap().status_pipelines += 1;
    }

    /// Only tests read these back; production only ever records into them.
    #[cfg(test)]
    pub(super) fn snapshot(&self) -> GitObservationCounts {
        self.0.lock().unwrap().clone()
    }
}
