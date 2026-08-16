use std::{
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    sync::{Mutex, OnceLock},
};

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GitPhase14Measurements {
    pub native_watcher_creations: u64,
    pub native_watchers: usize,
    pub native_watchers_high_water: usize,
    pub subscribers: usize,
    pub subscribers_high_water: usize,
    pub git_processes: u64,
    pub status_processes: u64,
    pub diff_processes: u64,
    pub mutation_processes: u64,
    pub active_processes: usize,
    pub active_processes_high_water: usize,
}

fn values() -> &'static Mutex<GitPhase14Measurements> {
    static VALUE: OnceLock<Mutex<GitPhase14Measurements>> = OnceLock::new();
    VALUE.get_or_init(|| Mutex::new(GitPhase14Measurements::default()))
}

pub(super) fn phase14_git_watcher_created() {
    let mut value = values().lock().unwrap();
    value.native_watcher_creations += 1;
    value.native_watchers += 1;
    value.native_watchers_high_water = value.native_watchers_high_water.max(value.native_watchers);
}

pub(super) fn phase14_git_watcher_dropped() {
    let mut value = values().lock().unwrap();
    value.native_watchers = value.native_watchers.saturating_sub(1);
}

pub(super) fn phase14_git_subscribers(delta: isize) {
    let mut value = values().lock().unwrap();
    value.subscribers = value.subscribers.saturating_add_signed(delta);
    value.subscribers_high_water = value.subscribers_high_water.max(value.subscribers);
}

pub(super) struct GitProcessMeasurement;

pub(super) fn phase14_git_process_started(command: &OsStr) -> GitProcessMeasurement {
    let mut value = values().lock().unwrap();
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

impl Drop for GitProcessMeasurement {
    fn drop(&mut self) {
        let mut value = values().lock().unwrap();
        value.active_processes = value.active_processes.saturating_sub(1);
    }
}

pub(super) fn phase14_git_snapshot() -> GitPhase14Measurements {
    values().lock().unwrap().clone()
}
