//! Transfer binding, cancellation, process ownership, and inactivity deadline.

use serde::Serialize;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
};

use crate::connection::TerminalClient;

#[derive(Clone)]
pub(crate) struct BulkBinding {
    pub(crate) client: Arc<TerminalClient>,
    pub(crate) expected_server_identity: String,
    pub(crate) connection_epoch: u64,
}

impl BulkBinding {
    pub(crate) fn capture(
        client: Arc<TerminalClient>,
        expected_server_identity: String,
        connection_epoch: u64,
    ) -> Result<Self, String> {
        let binding = Self {
            client,
            expected_server_identity,
            connection_epoch,
        };
        binding.validate()?;
        Ok(binding)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if !self.client.ready.load(Ordering::Acquire)
            || self.client.read_only.load(Ordering::Acquire)
        {
            return Err("bulk job is not bound to a writable live control connection".into());
        }
        if self.client.terminal_epoch.load(Ordering::Acquire) != self.connection_epoch {
            return Err("stale bulk job: control connection epoch was replaced".into());
        }
        if self.expected_server_identity.is_empty()
            || *self.client.server_identity.lock().unwrap() != self.expected_server_identity
        {
            return Err("stale bulk job: tmux server identity was replaced".into());
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct CancelState {
    requested: AtomicBool,
    transport_termination_requested: AtomicBool,
    process_id: AtomicU32,
    reason: AtomicU8,
    phase: AtomicU8,
    finished: AtomicBool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CancelReason {
    None,
    User,
    StaleBinding,
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum TransferPhase {
    Queued,
    Running,
    Verifying,
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CancelDisposition {
    CancelRequested,
    AwaitingAuthoritativeOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CancelResponse {
    pub(crate) disposition: CancelDisposition,
    pub(crate) phase: TransferPhase,
}

impl CancelState {
    pub(crate) fn new() -> Self {
        Self {
            requested: AtomicBool::new(false),
            transport_termination_requested: AtomicBool::new(false),
            process_id: AtomicU32::new(0),
            reason: AtomicU8::new(0),
            phase: AtomicU8::new(0),
            finished: AtomicBool::new(false),
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    pub(crate) fn cancel(&self) {
        self.cancel_for(CancelReason::User, false);
    }

    fn cancel_for(&self, reason: CancelReason, kill: bool) {
        let _ = self
            .reason
            .compare_exchange(0, reason as u8, Ordering::AcqRel, Ordering::Acquire);
        self.requested.store(true, Ordering::Release);
        if !kill || (self.phase() == TransferPhase::Verifying && reason == CancelReason::User) {
            return;
        }
        self.transport_termination_requested
            .store(true, Ordering::Release);
        let process_id = self.process_id.swap(0, Ordering::AcqRel);
        if process_id != 0 {
            // SAFETY: process_id is the exact child returned by spawn_bulk_bridge.
            unsafe { libc::kill(process_id as i32, libc::SIGKILL) };
        }
    }

    pub(crate) fn cancel_stale_binding(&self) {
        self.cancel_for(CancelReason::StaleBinding, true);
    }

    pub(crate) fn transport_termination_requested(&self) -> bool {
        self.transport_termination_requested.load(Ordering::Acquire)
    }

    pub(crate) fn reason(&self) -> CancelReason {
        match self.reason.load(Ordering::Acquire) {
            1 => CancelReason::User,
            2 => CancelReason::StaleBinding,
            3 => CancelReason::Timeout,
            _ => CancelReason::None,
        }
    }

    pub(crate) fn phase(&self) -> TransferPhase {
        match self.phase.load(Ordering::Acquire) {
            1 => TransferPhase::Running,
            2 => TransferPhase::Verifying,
            3 => TransferPhase::Finished,
            _ => TransferPhase::Queued,
        }
    }

    pub(crate) fn mark_running(&self) {
        let _ = self
            .phase
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
    }

    pub(crate) fn bind_process(&self, process_id: u32) -> Result<ProcessBinding<'_>, String> {
        self.process_id.store(process_id, Ordering::Release);
        if self.transport_termination_requested() {
            self.kill_if_bound(process_id);
            return Err("bulk transfer transport expired while its helper started".into());
        }
        if self.is_cancelled() {
            // Ordinary request cancellation has no authoritative outcome to
            // reconcile. A helper that was spawned before the request flag
            // became visible must not escape late process ownership.
            self.kill_if_bound(process_id);
            return Err("bulk transfer cancelled while its helper started".into());
        }
        Ok(ProcessBinding(self))
    }

    pub(crate) fn bind_authoritative_process(
        &self,
        process_id: u32,
    ) -> Result<ProcessBinding<'_>, String> {
        if self.phase() != TransferPhase::Verifying {
            return Err("authoritative reconciliation is only valid while verifying".into());
        }
        self.process_id.store(process_id, Ordering::Release);
        if self.transport_termination_requested() {
            self.kill_if_bound(process_id);
            return Err("authoritative reconciliation expired while its helper started".into());
        }
        Ok(ProcessBinding(self))
    }

    fn kill_if_bound(&self, process_id: u32) {
        if self
            .process_id
            .compare_exchange(process_id, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            // SAFETY: process_id is the exact child published by the caller.
            unsafe { libc::kill(process_id as i32, libc::SIGKILL) };
        }
    }

    pub(crate) fn prepare_finalize(&self) -> Result<(), String> {
        if self.is_cancelled() {
            return Err("bulk transfer cancelled before finalize".into());
        }
        self.phase.store(2, Ordering::Release);
        if self.is_cancelled() {
            return Err("bulk transfer cancelled while finalize started".into());
        }
        Ok(())
    }

    pub(crate) fn mark_finished(&self) {
        self.phase.store(3, Ordering::Release);
        self.process_id.store(0, Ordering::Release);
        self.finished.store(true, Ordering::Release);
    }

    pub(crate) fn arm_inactivity_deadline(self: &Arc<Self>) -> DeadlineGuard {
        self.arm_deadline(std::time::Duration::from_secs(30))
    }

    #[cfg(test)]
    pub(crate) fn arm_deadline(self: &Arc<Self>, timeout: std::time::Duration) -> DeadlineGuard {
        self.arm_deadline_inner(timeout)
    }

    #[cfg(not(test))]
    fn arm_deadline(self: &Arc<Self>, timeout: std::time::Duration) -> DeadlineGuard {
        self.arm_deadline_inner(timeout)
    }

    fn arm_deadline_inner(self: &Arc<Self>, timeout: std::time::Duration) -> DeadlineGuard {
        let state = Arc::new(DeadlineState {
            completed: AtomicBool::new(false),
            last_activity: Mutex::new(std::time::Instant::now()),
        });
        let worker_state = Arc::clone(&state);
        let cancellation = Arc::clone(self);
        std::thread::spawn(move || {
            let poll_interval = std::cmp::min(
                std::time::Duration::from_secs(1),
                std::cmp::max(
                    std::time::Duration::from_millis(10),
                    timeout.checked_div(4).unwrap_or(timeout),
                ),
            );
            loop {
                std::thread::sleep(poll_interval);
                if worker_state.completed.load(Ordering::Acquire) {
                    return;
                }
                if worker_state.last_activity.lock().unwrap().elapsed() >= timeout {
                    break;
                }
            }
            cancellation.cancel_for(CancelReason::Timeout, true);
        });
        DeadlineGuard(state)
    }

    #[cfg(test)]
    pub(crate) fn arm_test_deadline(
        self: &Arc<Self>,
        timeout: std::time::Duration,
    ) -> DeadlineGuard {
        self.arm_deadline(timeout)
    }
}

struct DeadlineState {
    completed: AtomicBool,
    last_activity: Mutex<std::time::Instant>,
}

pub(crate) struct DeadlineGuard(Arc<DeadlineState>);

impl DeadlineGuard {
    pub(crate) fn touch(&self) {
        *self.0.last_activity.lock().unwrap() = std::time::Instant::now();
    }

    pub(crate) fn complete(&self) {
        self.0.completed.store(true, Ordering::Release);
    }
}

impl Drop for DeadlineGuard {
    fn drop(&mut self) {
        self.complete();
    }
}

pub(crate) struct ProcessBinding<'a>(&'a CancelState);

impl Drop for ProcessBinding<'_> {
    fn drop(&mut self) {
        self.0.process_id.store(0, Ordering::Release);
    }
}

#[cfg(test)]
pub(crate) struct BulkChild(pub(crate) std::process::Child);

#[cfg(test)]
impl Drop for BulkChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
