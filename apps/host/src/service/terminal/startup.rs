//! Transactional ownership for persistent tmux client startup.
//!
//! A child is live before its pipes and worker threads are fully assembled.
//! This guard owns that provisional lifetime: every early return signals and
//! reaps the child, then joins every worker that did start. Ownership is
//! disarmed only when the complete client object is ready to take over.

use std::{
    process::Child,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

pub(super) struct ProcessStartup {
    child: Option<Arc<Mutex<Child>>>,
    stopped: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}

impl ProcessStartup {
    pub(super) fn new(child: Child, stopped: Arc<AtomicBool>) -> Self {
        #[cfg(test)]
        LAST_STARTUP_PID.with(|value| value.set(child.id()));
        Self {
            child: Some(Arc::new(Mutex::new(child))),
            stopped,
            workers: Vec::new(),
        }
    }

    pub(super) fn child(&self) -> Arc<Mutex<Child>> {
        Arc::clone(
            self.child
                .as_ref()
                .expect("startup child already committed"),
        )
    }

    pub(super) fn spawn(
        &mut self,
        stage: usize,
        builder: thread::Builder,
        work: impl FnOnce() + Send + 'static,
    ) -> std::io::Result<()> {
        #[cfg(not(test))]
        let _ = stage;
        #[cfg(test)]
        if FAIL_WORKER_STAGE.with(|value| value.get() == stage) {
            return Err(std::io::Error::other(format!(
                "injected persistent-client worker {stage} spawn failure"
            )));
        }
        self.workers.push(builder.spawn(work)?);
        Ok(())
    }

    pub(super) fn commit(mut self) -> (Arc<Mutex<Child>>, Vec<JoinHandle<()>>) {
        let child = self.child.take().expect("startup child already committed");
        let workers = std::mem::take(&mut self.workers);
        (child, workers)
    }
}

impl Drop for ProcessStartup {
    fn drop(&mut self) {
        if self.child.is_none() {
            // `commit` transferred every process/thread owner to the completed
            // client. Do not trip its shared failure/stop flag while dropping
            // the now-disarmed startup transaction.
            return;
        }
        self.stopped.store(true, Ordering::Release);
        if let Some(child) = self.child.take()
            && let Ok(mut child) = child.lock()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

pub(super) fn stop_process(stopped: &AtomicBool, child: &Arc<Mutex<Child>>) {
    stopped.store(true, Ordering::Release);
    if let Ok(mut child) = child.lock() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

pub(super) fn join_workers(workers: &mut Vec<JoinHandle<()>>) {
    for worker in workers.drain(..) {
        let _ = worker.join();
    }
}

#[cfg(test)]
thread_local! {
    static FAIL_WORKER_STAGE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static LAST_STARTUP_PID: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn with_worker_spawn_failure<T>(stage: usize, run: impl FnOnce() -> T) -> T {
    FAIL_WORKER_STAGE.with(|value| value.set(stage));
    let result = run();
    FAIL_WORKER_STAGE.with(|value| value.set(0));
    result
}

#[cfg(test)]
pub(super) fn last_startup_pid() -> u32 {
    LAST_STARTUP_PID.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(super) fn assert_last_startup_child_reaped() {
    let pid = last_startup_pid() as libc::pid_t;
    assert!(pid > 0);
    let mut status = 0;
    // The guard has already waited for this exact child. ECHILD proves it was
    // reaped rather than merely signalled and left as a zombie.
    assert_eq!(
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}
