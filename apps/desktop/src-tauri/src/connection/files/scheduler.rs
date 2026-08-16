use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
    },
};

pub(super) fn take_queued_job<T>(
    queue: &mut VecDeque<T>,
    mut matches: impl FnMut(&T) -> bool,
) -> Option<T> {
    let index = queue.iter().position(&mut matches)?;
    queue.remove(index)
}

use super::MAX_QUEUED_TRANSFERS;
use super::transfer_event::{
    CleanupStatus, TransferFailure, TransferFailureKind, TransferOutcome, TransferResult,
};
use crate::connection::TerminalClient;

#[derive(Clone)]
pub(super) struct BulkBinding {
    pub(super) client: Arc<TerminalClient>,
    pub(super) expected_server_identity: String,
    pub(super) connection_epoch: u64,
}

impl BulkBinding {
    pub(super) fn capture(
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

    pub(super) fn validate(&self) -> Result<(), String> {
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

pub(super) struct CancelState {
    requested: AtomicBool,
    process_id: AtomicU32,
    reason: AtomicU8,
    phase: AtomicU8,
    finished: AtomicBool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CancelReason {
    None,
    User,
    StaleBinding,
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum TransferPhase {
    Queued,
    Running,
    Verifying,
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum CancelDisposition {
    CancelRequested,
    AwaitingAuthoritativeOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CancelResponse {
    pub(super) disposition: CancelDisposition,
    pub(super) phase: TransferPhase,
}

impl CancelState {
    pub(super) fn new() -> Self {
        Self {
            requested: AtomicBool::new(false),
            process_id: AtomicU32::new(0),
            reason: AtomicU8::new(0),
            phase: AtomicU8::new(0),
            finished: AtomicBool::new(false),
        }
    }
    pub(super) fn is_cancelled(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
    pub(super) fn cancel(&self) {
        // Running helpers receive the protocol cancellation request first. A
        // silent peer is still bounded by the inactivity deadline, which is
        // the only path allowed to kill the transport without claiming that
        // the remote partial was removed.
        self.cancel_for(CancelReason::User, false);
    }
    fn cancel_for(&self, reason: CancelReason, kill: bool) {
        let _ = self
            .reason
            .compare_exchange(0, reason as u8, Ordering::AcqRel, Ordering::Acquire);
        self.requested.store(true, Ordering::Release);
        // A user cancel that loses the pre-commit race cannot make an already
        // publishing operation ambiguous. Deadline and stale-scope failures
        // are different: they must terminate a silent transport so the worker
        // can reconcile or report an unknown outcome boundedly.
        if !kill || (self.phase() == TransferPhase::Verifying && reason == CancelReason::User) {
            return;
        }
        let process_id = self.process_id.swap(0, Ordering::AcqRel);
        if process_id != 0 {
            // SAFETY: process_id is the exact child returned by spawn_bulk_bridge.
            unsafe { libc::kill(process_id as i32, libc::SIGKILL) };
        }
    }
    pub(super) fn cancel_stale_binding(&self) {
        self.cancel_for(CancelReason::StaleBinding, true);
    }
    pub(super) fn reason(&self) -> CancelReason {
        match self.reason.load(Ordering::Acquire) {
            1 => CancelReason::User,
            2 => CancelReason::StaleBinding,
            3 => CancelReason::Timeout,
            _ => CancelReason::None,
        }
    }
    pub(super) fn phase(&self) -> TransferPhase {
        match self.phase.load(Ordering::Acquire) {
            1 => TransferPhase::Running,
            2 => TransferPhase::Verifying,
            3 => TransferPhase::Finished,
            _ => TransferPhase::Queued,
        }
    }

    fn mark_running(&self) {
        let _ = self
            .phase
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
    }
    pub(super) fn bind_process(&self, process_id: u32) -> Result<ProcessBinding<'_>, String> {
        if self.is_cancelled() {
            return Err("bulk transfer cancelled before its helper started".into());
        }
        self.process_id.store(process_id, Ordering::Release);
        if self.is_cancelled() {
            self.cancel();
            return Err("bulk transfer cancelled while its helper started".into());
        }
        Ok(ProcessBinding(self))
    }

    /// Binds the short-lived reconciliation helper after the atomic commit
    /// boundary. A user cancellation is intentionally unable to kill this
    /// helper because its response is needed to classify publication.
    pub(super) fn bind_authoritative_process(
        &self,
        process_id: u32,
    ) -> Result<ProcessBinding<'_>, String> {
        if self.phase() != TransferPhase::Verifying {
            return Err("authoritative reconciliation is only valid while verifying".into());
        }
        self.process_id.store(process_id, Ordering::Release);
        Ok(ProcessBinding(self))
    }

    /// Establishes the atomic commit point for a transfer. Cancellation that
    /// wins before this call still kills the helper; cancellation after it is
    /// recorded but cannot turn an already-publishing commit into an ambiguous
    /// connection loss.
    pub(super) fn prepare_finalize(&self) -> Result<(), String> {
        if self.is_cancelled() {
            return Err("bulk transfer cancelled before finalize".into());
        }
        self.phase.store(2, Ordering::Release);
        if self.is_cancelled() {
            return Err("bulk transfer cancelled while finalize started".into());
        }
        Ok(())
    }

    fn mark_finished(&self) {
        self.phase.store(3, Ordering::Release);
        self.process_id.store(0, Ordering::Release);
        self.finished.store(true, Ordering::Release);
    }

    pub(super) fn arm_inactivity_deadline(self: &Arc<Self>) -> DeadlineGuard {
        self.arm_deadline(std::time::Duration::from_secs(30))
    }

    fn arm_deadline(self: &Arc<Self>, timeout: std::time::Duration) -> DeadlineGuard {
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
}

struct DeadlineState {
    completed: AtomicBool,
    last_activity: Mutex<std::time::Instant>,
}
pub(super) struct DeadlineGuard(Arc<DeadlineState>);
impl DeadlineGuard {
    pub(super) fn touch(&self) {
        *self.0.last_activity.lock().unwrap() = std::time::Instant::now();
    }

    pub(super) fn complete(&self) {
        self.0.completed.store(true, Ordering::Release);
    }
}
impl Drop for DeadlineGuard {
    fn drop(&mut self) {
        self.complete();
    }
}

pub(super) struct ProcessBinding<'a>(&'a CancelState);
impl Drop for ProcessBinding<'_> {
    fn drop(&mut self) {
        self.0.process_id.store(0, Ordering::Release);
    }
}

/// A bridge child that dies with its owner. Test-only since the bulk lane
/// started pooling its connections (`bulk_pool`); the scheduler tests still
/// build raw peers this way.
#[cfg(test)]
pub(super) struct BulkChild(pub(super) std::process::Child);
pub(super) struct BulkPermit;
static BULK_ACTIVE: OnceLock<(Mutex<usize>, Condvar)> = OnceLock::new();

struct EngineJob {
    id: String,
    binding: BulkBinding,
    cancellation: Arc<CancelState>,
    started: Box<dyn FnOnce() + Send>,
    work: Box<dyn FnOnce() -> TransferResult + Send>,
    finished: Box<dyn FnOnce(TransferResult, CancelReason) + Send>,
}

#[derive(Default)]
struct EngineState {
    active: usize,
    queue: VecDeque<EngineJob>,
    cancellations: HashMap<String, Arc<CancelState>>,
}

#[derive(Default)]
struct TransferEngine {
    state: Mutex<EngineState>,
}

static TRANSFER_ENGINE: OnceLock<Arc<TransferEngine>> = OnceLock::new();

fn transfer_engine() -> Arc<TransferEngine> {
    Arc::clone(TRANSFER_ENGINE.get_or_init(|| Arc::new(TransferEngine::default())))
}

#[cfg(test)]
pub(super) fn acceptance_engine_counts() -> (usize, usize) {
    let engine = transfer_engine();
    let state = engine.state.lock().unwrap();
    (state.active, state.queue.len())
}

#[cfg(test)]
pub(super) fn engine_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(super) fn enqueue_transfer(
    id: String,
    binding: BulkBinding,
    cancellation: Arc<CancelState>,
    started: impl FnOnce() + Send + 'static,
    work: impl FnOnce() -> TransferResult + Send + 'static,
    finished: impl FnOnce(TransferResult, CancelReason) + Send + 'static,
) -> Result<(), String> {
    if let Err(error) = binding.validate() {
        crate::perf_log::record_transfer_admission(false);
        return Err(error);
    }
    let engine = transfer_engine();
    {
        let mut state = engine.state.lock().unwrap();
        if state.queue.len() >= MAX_QUEUED_TRANSFERS {
            crate::perf_log::record_transfer_admission(false);
            return Err("bulk transfer queue is full".into());
        }
        if state.cancellations.contains_key(&id) {
            crate::perf_log::record_transfer_admission(false);
            return Err("bulk transfer ID is already queued or active".into());
        }
        state
            .cancellations
            .insert(id.clone(), Arc::clone(&cancellation));
        state.queue.push_back(EngineJob {
            id: id.clone(),
            binding: binding.clone(),
            cancellation: Arc::clone(&cancellation),
            started: Box::new(started),
            work: Box::new(work),
            finished: Box::new(finished),
        });
        crate::perf_log::record_transfer_admission(true);
        crate::perf_log::record_transfer_state(state.active, state.queue.len());
    }
    spawn_binding_monitor(&engine, id, binding, cancellation);
    engine.dispatch();
    Ok(())
}

pub(super) fn cancel_transfer(id: &str) -> Result<CancelResponse, String> {
    let engine = transfer_engine();
    let queued = {
        let mut state = engine.state.lock().unwrap();
        let queued = take_queued_job(&mut state.queue, |job| job.id == id);
        if queued.is_some() {
            state.cancellations.remove(id);
            crate::perf_log::record_transfer_state(state.active, state.queue.len());
        }
        queued
    };
    if let Some(job) = queued {
        job.cancellation.cancel();
        job.cancellation.mark_finished();
        (job.finished)(
            Err(failure_for_cancel(
                CancelReason::User,
                TransferPhase::Queued,
                "bulk transfer cancelled while queued".into(),
            )),
            CancelReason::User,
        );
        crate::perf_log::record_transfer_outcome();
        engine.dispatch();
        return Ok(CancelResponse {
            disposition: CancelDisposition::CancelRequested,
            phase: TransferPhase::Queued,
        });
    }
    let cancellation = engine
        .state
        .lock()
        .unwrap()
        .cancellations
        .get(id)
        .cloned()
        .ok_or("bulk transfer is no longer queued or active")?;
    let phase = cancellation.phase();
    cancellation.cancel();
    let disposition = if phase == TransferPhase::Verifying {
        CancelDisposition::AwaitingAuthoritativeOutcome
    } else {
        CancelDisposition::CancelRequested
    };
    Ok(CancelResponse { disposition, phase })
}

impl TransferEngine {
    fn dispatch(self: &Arc<Self>) {
        loop {
            let job = {
                let mut state = self.state.lock().unwrap();
                if state.active >= 2 {
                    return;
                }
                let Some(job) = state.queue.pop_front() else {
                    return;
                };
                if job.cancellation.is_cancelled() {
                    state.cancellations.remove(&job.id);
                    job.cancellation.mark_finished();
                    drop(state);
                    let reason = job.cancellation.reason();
                    (job.finished)(
                        Err(failure_for_cancel(
                            reason,
                            TransferPhase::Queued,
                            "bulk transfer cancelled before worker start".into(),
                        )),
                        reason,
                    );
                    continue;
                }
                state.active += 1;
                crate::perf_log::record_transfer_state(state.active, state.queue.len());
                job
            };
            let engine = Arc::clone(self);
            std::thread::Builder::new()
                .name(format!("bulk-transfer-{}", job.id))
                .spawn(move || {
                    let EngineJob {
                        id,
                        binding,
                        cancellation,
                        started,
                        work,
                        finished,
                    } = job;
                    let result = match acquire_bulk_permit(&cancellation) {
                        Ok(_permit) => match binding.validate() {
                            Ok(()) => {
                                cancellation.mark_running();
                                started();
                                work()
                            }
                            Err(error) => Err(failure_for_cancel(
                                CancelReason::StaleBinding,
                                cancellation.phase(),
                                error,
                            )),
                        },
                        Err(error) => Err(failure_for_cancel(
                            cancellation.reason(),
                            cancellation.phase(),
                            error,
                        )),
                    };
                    let reason = cancellation.reason();
                    cancellation.mark_finished();
                    finished(result, reason);
                    crate::perf_log::record_transfer_outcome();
                    engine.complete(&id);
                })
                .expect("failed to start bounded bulk-transfer worker");
        }
    }

    fn complete(self: &Arc<Self>, id: &str) {
        {
            let mut state = self.state.lock().unwrap();
            state.active = state.active.saturating_sub(1);
            state.cancellations.remove(id);
            crate::perf_log::record_transfer_state(state.active, state.queue.len());
        }
        self.dispatch();
    }

    fn cancel_stale(self: &Arc<Self>, id: &str, message: String) {
        let queued = {
            let mut state = self.state.lock().unwrap();
            let queued = take_queued_job(&mut state.queue, |job| job.id == id);
            if queued.is_some() {
                state.cancellations.remove(id);
                crate::perf_log::record_transfer_state(state.active, state.queue.len());
            }
            queued
        };
        if let Some(job) = queued {
            job.cancellation.cancel_stale_binding();
            job.cancellation.mark_finished();
            (job.finished)(
                Err(failure_for_cancel(
                    CancelReason::StaleBinding,
                    TransferPhase::Queued,
                    message,
                )),
                CancelReason::StaleBinding,
            );
            crate::perf_log::record_transfer_outcome();
            self.dispatch();
        } else if let Some(cancellation) = self.state.lock().unwrap().cancellations.get(id).cloned()
        {
            cancellation.cancel_stale_binding();
        }
    }
}

fn failure_for_cancel(
    reason: CancelReason,
    phase: TransferPhase,
    error: String,
) -> TransferFailure {
    let outcome = if phase == TransferPhase::Verifying {
        TransferOutcome::Unknown
    } else {
        TransferOutcome::NotPublished
    };
    let failure_kind = match reason {
        CancelReason::StaleBinding => TransferFailureKind::StaleScope,
        CancelReason::Timeout => TransferFailureKind::Timeout,
        CancelReason::None | CancelReason::User => TransferFailureKind::Transfer,
    };
    let cleanup_status = if phase == TransferPhase::Queued {
        CleanupStatus::NotNeeded
    } else {
        CleanupStatus::ConnectionClosed
    };
    let cleanup_error = (cleanup_status != CleanupStatus::NotNeeded).then(|| error.clone());
    TransferFailure::new(outcome, failure_kind, cleanup_status, error, cleanup_error)
}

fn spawn_binding_monitor(
    engine: &Arc<TransferEngine>,
    id: String,
    binding: BulkBinding,
    cancellation: Arc<CancelState>,
) {
    let engine = Arc::downgrade(engine);
    std::thread::spawn(move || {
        while !cancellation.finished.load(Ordering::Acquire) {
            if let Err(error) = binding.validate() {
                if let Some(engine) = engine.upgrade() {
                    engine.cancel_stale(&id, error);
                }
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });
}

pub(super) fn acquire_bulk_permit(cancellation: &CancelState) -> Result<BulkPermit, String> {
    let (lock, changed) = BULK_ACTIVE.get_or_init(|| (Mutex::new(0), Condvar::new()));
    let mut active = lock.lock().unwrap();
    while *active >= 2 {
        if cancellation.is_cancelled() {
            return Err("bulk transfer cancelled while queued".into());
        }
        active = changed
            .wait_timeout(active, std::time::Duration::from_millis(100))
            .unwrap()
            .0;
    }
    *active += 1;
    Ok(BulkPermit)
}

impl Drop for BulkPermit {
    fn drop(&mut self) {
        let (lock, changed) = BULK_ACTIVE.get().expect("bulk permit registry exists");
        let mut active = lock.lock().unwrap();
        *active = active.saturating_sub(1);
        changed.notify_one();
    }
}

#[cfg(test)]
impl Drop for BulkChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
#[path = "scheduler/tests.rs"]
mod tests;
