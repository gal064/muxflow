use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex, OnceLock,
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

struct EngineJob {
    id: String,
    binding: BulkBinding,
    cancellation: Arc<CancelState>,
    admitted: bool,
    started: Box<dyn FnOnce() + Send>,
    work: Box<dyn FnOnce() -> TransferResult + Send>,
    finished: Box<dyn FnOnce(TransferResult, CancelReason) + Send>,
}

#[derive(Default)]
struct EngineState {
    active: usize,
    active_bindings: HashMap<String, BulkBinding>,
    queue: VecDeque<EngineJob>,
    cancellations: HashMap<String, Arc<CancelState>>,
}

#[derive(Default)]
struct TransferEngine {
    state: Mutex<EngineState>,
}

static TRANSFER_ENGINE: OnceLock<Arc<TransferEngine>> = OnceLock::new();

fn transfer_engine() -> Arc<TransferEngine> {
    Arc::clone(TRANSFER_ENGINE.get_or_init(|| {
        let engine = Arc::new(TransferEngine::default());
        spawn_engine_binding_monitor(&engine);
        engine
    }))
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

#[cfg(test)]
pub(super) fn enqueue_transfer(
    id: String,
    binding: BulkBinding,
    cancellation: Arc<CancelState>,
    started: impl FnOnce() + Send + 'static,
    work: impl FnOnce() -> TransferResult + Send + 'static,
    finished: impl FnOnce(TransferResult, CancelReason) + Send + 'static,
) -> Result<(), String> {
    enqueue_transfer_with_queued(
        id,
        binding,
        cancellation,
        || Ok(()),
        started,
        work,
        finished,
    )
}

/// Atomically admits a transfer and publishes its queued transition.
///
/// `queued` runs only after the job and its cancellation state have been
/// inserted, and before dispatch can make the job running. Captured RAII guards
/// are therefore dropped normally on every admission error without leaving a
/// renderer event or scheduler entry behind.
pub(super) fn enqueue_transfer_with_queued(
    id: String,
    binding: BulkBinding,
    cancellation: Arc<CancelState>,
    queued: impl FnOnce() -> Result<(), String> + Send + 'static,
    started: impl FnOnce() + Send + 'static,
    work: impl FnOnce() -> TransferResult + Send + 'static,
    finished: impl FnOnce(TransferResult, CancelReason) + Send + 'static,
) -> Result<(), String> {
    if let Err(error) = binding.validate() {
        crate::perf_log::record_transfer_admission(crate::perf_log::TransferAdmission::Rejected);
        return Err(error);
    }
    let engine = transfer_engine();
    {
        let mut state = engine.state.lock().unwrap();
        #[cfg(test)]
        if INJECT_QUEUE_FULL.swap(false, Ordering::AcqRel) {
            crate::perf_log::record_transfer_admission(
                crate::perf_log::TransferAdmission::Rejected,
            );
            return Err("bulk transfer queue is full".into());
        }
        if state.queue.len() >= MAX_QUEUED_TRANSFERS {
            crate::perf_log::record_transfer_admission(
                crate::perf_log::TransferAdmission::Rejected,
            );
            return Err("bulk transfer queue is full".into());
        }
        if state.cancellations.contains_key(&id) || state.queue.iter().any(|job| job.id == id) {
            crate::perf_log::record_transfer_admission(
                crate::perf_log::TransferAdmission::Rejected,
            );
            return Err("bulk transfer ID is already queued or active".into());
        }
        state
            .cancellations
            .insert(id.clone(), Arc::clone(&cancellation));
        state.queue.push_back(EngineJob {
            id: id.clone(),
            binding: binding.clone(),
            cancellation: Arc::clone(&cancellation),
            admitted: false,
            started: Box::new(started),
            work: Box::new(work),
            finished: Box::new(finished),
        });
    }
    // Event delivery is deliberately outside the engine lock. The FIFO head's
    // admission barrier prevents every dispatcher from starting it (or later
    // work) until queued has returned, without letting a slow channel stall
    // cancellation and engine inspection.
    let queued_error = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(queued)) {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(panic) => Some(format!(
            "bulk transfer queued event panicked: {}",
            panic_message(panic)
        )),
    };
    if let Some(error) = queued_error {
        {
            let mut state = engine.state.lock().unwrap();
            let rejected = take_queued_job(&mut state.queue, |job| job.id == id);
            if rejected.is_some() {
                state.cancellations.remove(&id);
                cancellation.mark_finished();
            }
            crate::perf_log::record_transfer_state(state.active, state.queue.len());
        }
        crate::perf_log::record_transfer_admission(crate::perf_log::TransferAdmission::Rejected);
        // A later admission may have observed this pending FIFO head and
        // returned without starting. Removing the head must retrigger it.
        engine.dispatch();
        return Err(format!(
            "bulk transfer queued event could not be delivered: {error}"
        ));
    }
    {
        let mut state = engine.state.lock().unwrap();
        // Pending jobs cannot be removed by cancellation and their binding
        // monitor does not exist until after this commit. A missing entry is
        // therefore an internal scheduler invariant violation, not a
        // recoverable post-event rejection (which would create a UI ghost).
        let pending = state
            .queue
            .iter_mut()
            .find(|job| job.id == id)
            .expect("pending admission remains queued until commit");
        pending.admitted = true;
        crate::perf_log::record_transfer_admission(crate::perf_log::TransferAdmission::Accepted);
        crate::perf_log::record_transfer_state(state.active, state.queue.len());
    }
    engine.dispatch();
    Ok(())
}

pub(super) fn cancel_transfer(id: &str) -> Result<CancelResponse, String> {
    let engine = transfer_engine();
    let queued = {
        let mut state = engine.state.lock().unwrap();
        let queued = take_queued_job(&mut state.queue, |job| job.id == id && job.admitted);
        if queued.is_some() {
            state.cancellations.remove(id);
            crate::perf_log::record_transfer_state(state.active, state.queue.len());
        }
        queued
    };
    if let Some(job) = queued {
        job.cancellation.cancel();
        terminalize_queued(
            job,
            Err(failure_for_cancel(
                CancelReason::User,
                TransferPhase::Queued,
                "bulk transfer cancelled while queued".into(),
            )),
            CancelReason::User,
        );
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
                if state.queue.front().is_some_and(|job| !job.admitted) {
                    return;
                }
                let Some(job) = state.queue.pop_front() else {
                    return;
                };
                if job.cancellation.is_cancelled() {
                    state.cancellations.remove(&job.id);
                    drop(state);
                    let reason = job.cancellation.reason();
                    terminalize_queued(
                        job,
                        Err(failure_for_cancel(
                            reason,
                            TransferPhase::Queued,
                            "bulk transfer cancelled before worker start".into(),
                        )),
                        reason,
                    );
                    continue;
                }
                if let Err(error) = job.binding.validate() {
                    state.cancellations.remove(&job.id);
                    drop(state);
                    terminalize_queued(
                        job,
                        Err(failure_for_cancel(
                            CancelReason::StaleBinding,
                            TransferPhase::Queued,
                            error,
                        )),
                        CancelReason::StaleBinding,
                    );
                    continue;
                }
                state.active += 1;
                state
                    .active_bindings
                    .insert(job.id.clone(), job.binding.clone());
                crate::perf_log::record_transfer_state(state.active, state.queue.len());
                job
            };
            let engine = Arc::clone(self);
            // `Builder::spawn` consumes its closure even when the OS refuses
            // the thread. Keep the job in a shared one-shot handoff so that the
            // rejecting path can still terminalize it and release its guards.
            let handoff = Arc::new(Mutex::new(Some(job)));
            let worker_handoff = Arc::clone(&handoff);
            let worker_name = {
                let handoff = handoff.lock().unwrap();
                format!(
                    "bulk-transfer-{}",
                    handoff.as_ref().expect("worker handoff contains job").id
                )
            };
            let spawn = spawn_worker(worker_name, move || {
                let job = worker_handoff
                    .lock()
                    .unwrap()
                    .take()
                    .expect("bulk worker owns its one-shot job");
                let EngineJob {
                    id,
                    binding,
                    cancellation,
                    admitted: _,
                    started,
                    work,
                    finished,
                } = job;
                let _active = ActiveJobGuard {
                    engine: Arc::clone(&engine),
                    id: id.clone(),
                    cancellation: Arc::clone(&cancellation),
                };
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        match binding.validate() {
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
                        }
                    }))
                    .unwrap_or_else(|panic| Err(failure_for_panic(&cancellation, panic)));
                let reason = cancellation.reason();
                cancellation.mark_finished();
                // Restore scheduler ownership before crossing the untrusted
                // renderer callback boundary. A slow or wedged receiver must
                // not hold a transfer lane or strand later queued work.
                drop(_active);
                engine.dispatch();
                let succeeded = result.is_ok();
                invoke_finished(finished, result, reason);
                crate::perf_log::record_transfer_completion(succeeded);
            });
            if let Err(error) = spawn {
                let job = handoff
                    .lock()
                    .unwrap()
                    .take()
                    .expect("failed spawn returns the unclaimed job");
                self.worker_spawn_failed(job, error);
            }
        }
    }

    fn worker_spawn_failed(&self, job: EngineJob, error: std::io::Error) {
        {
            let mut state = self.state.lock().unwrap();
            assert!(state.active > 0, "active transfer accounting underflow");
            state.active -= 1;
            state.active_bindings.remove(&job.id);
            state.cancellations.remove(&job.id);
            crate::perf_log::record_transfer_state(state.active, state.queue.len());
        }
        job.cancellation.mark_finished();
        invoke_finished(
            job.finished,
            Err(TransferFailure::new(
                TransferOutcome::NotPublished,
                TransferFailureKind::Transfer,
                CleanupStatus::NotNeeded,
                format!("could not start bulk transfer worker: {error}"),
                None,
            )),
            job.cancellation.reason(),
        );
        crate::perf_log::record_transfer_completion(false);
    }

    fn release_active(&self, id: &str) {
        let mut state = self.state.lock().unwrap();
        assert!(state.active > 0, "active transfer accounting underflow");
        state.active -= 1;
        state.active_bindings.remove(id);
        state.cancellations.remove(id);
        crate::perf_log::record_transfer_state(state.active, state.queue.len());
    }

    fn cancel_stale(self: &Arc<Self>, id: &str, message: String) {
        let queued = {
            let mut state = self.state.lock().unwrap();
            let queued = take_queued_job(&mut state.queue, |job| job.id == id && job.admitted);
            if queued.is_some() {
                state.cancellations.remove(id);
                crate::perf_log::record_transfer_state(state.active, state.queue.len());
            }
            queued
        };
        if let Some(job) = queued {
            job.cancellation.cancel_stale_binding();
            terminalize_queued(
                job,
                Err(failure_for_cancel(
                    CancelReason::StaleBinding,
                    TransferPhase::Queued,
                    message,
                )),
                CancelReason::StaleBinding,
            );
            self.dispatch();
        } else if let Some(cancellation) = self.state.lock().unwrap().cancellations.get(id).cloned()
        {
            cancellation.cancel_stale_binding();
        }
    }
}

fn terminalize_queued(job: EngineJob, result: TransferResult, reason: CancelReason) {
    job.cancellation.mark_finished();
    let succeeded = result.is_ok();
    invoke_finished(job.finished, result, reason);
    crate::perf_log::record_transfer_completion(succeeded);
}

fn invoke_finished(
    finished: Box<dyn FnOnce(TransferResult, CancelReason) + Send>,
    result: TransferResult,
    reason: CancelReason,
) {
    // A renderer/channel callback is outside the scheduler's trust boundary.
    // Its panic must never strand a lane or prevent the dispatcher advancing.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        finished(result, reason);
    }));
}

struct ActiveJobGuard {
    engine: Arc<TransferEngine>,
    id: String,
    cancellation: Arc<CancelState>,
}

impl Drop for ActiveJobGuard {
    fn drop(&mut self) {
        self.cancellation.mark_finished();
        self.engine.release_active(&self.id);
    }
}

fn failure_for_panic(
    cancellation: &CancelState,
    panic: Box<dyn std::any::Any + Send>,
) -> TransferFailure {
    let phase = cancellation.phase();
    let outcome = if phase == TransferPhase::Verifying {
        TransferOutcome::Unknown
    } else {
        TransferOutcome::NotPublished
    };
    TransferFailure::new(
        outcome,
        if outcome == TransferOutcome::Unknown {
            TransferFailureKind::OutcomeUnknown
        } else {
            TransferFailureKind::Transfer
        },
        CleanupStatus::ConnectionClosed,
        format!("bulk transfer worker panicked: {}", panic_message(panic)),
        Some("bulk worker stopped before cleanup could be confirmed".into()),
    )
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".into()
    }
}

fn spawn_worker(
    name: String,
    work: impl FnOnce() + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    #[cfg(test)]
    if INJECT_WORKER_SPAWN_FAILURE.swap(false, Ordering::AcqRel) {
        return Err(std::io::Error::other("injected worker spawn failure"));
    }
    std::thread::Builder::new().name(name).spawn(work)
}

#[cfg(test)]
static INJECT_QUEUE_FULL: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static INJECT_WORKER_SPAWN_FAILURE: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
fn inject_queue_full_once() {
    INJECT_QUEUE_FULL.store(true, Ordering::Release);
}

#[cfg(test)]
fn inject_worker_spawn_failure_once() {
    INJECT_WORKER_SPAWN_FAILURE.store(true, Ordering::Release);
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

fn spawn_engine_binding_monitor(engine: &Arc<TransferEngine>) {
    let engine = Arc::downgrade(engine);
    // One engine-owned watcher covers the at-most-two active bindings without
    // creating one polling thread per job. Queued bindings are validated at
    // their authoritative dequeue boundary instead of being polled.
    let _ = std::thread::Builder::new()
        .name("bulk-binding-monitor".into())
        .spawn(move || {
            loop {
                let Some(engine) = engine.upgrade() else {
                    return;
                };
                let stale = {
                    let state = engine.state.lock().unwrap();
                    state
                        .active_bindings
                        .iter()
                        .filter_map(|(id, binding)| {
                            binding.validate().err().map(|error| (id.clone(), error))
                        })
                        .collect::<Vec<_>>()
                };
                for (id, error) in stale {
                    engine.cancel_stale(&id, error);
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        });
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
