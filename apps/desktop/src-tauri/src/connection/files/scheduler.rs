#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, OnceLock},
};

mod admission;
mod lifecycle;
mod publication;
use admission::{Admission, AdmissionOutcome};
#[cfg(test)]
pub(super) use lifecycle::BulkChild;
pub(crate) use lifecycle::{BulkBinding, CancelState, DeadlineGuard};
pub(super) use lifecycle::{CancelDisposition, CancelReason, CancelResponse, TransferPhase};
pub(super) use publication::QueuedPublication;
use publication::{PublicationActor, publication_actor};

const ADMISSION_CANCELLATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

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
struct EngineJob {
    id: String,
    binding: BulkBinding,
    cancellation: Arc<CancelState>,
    started: Box<dyn FnOnce() + Send>,
    work: Box<dyn FnOnce() -> TransferResult + Send>,
    finished: Box<dyn FnOnce(TransferResult, CancelReason) + Send>,
}

struct PendingAdmission {
    binding: BulkBinding,
    state: Arc<Admission>,
}

#[derive(Default)]
struct EngineState {
    active: usize,
    pending_admissions: HashMap<String, PendingAdmission>,
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
    Arc::clone(TRANSFER_ENGINE.get_or_init(|| Arc::new(TransferEngine::default())))
}

#[cfg(test)]
pub(super) fn acceptance_engine_counts() -> (usize, usize) {
    let engine = transfer_engine();
    let state = engine.state.lock().unwrap();
    (
        state.active,
        state.queue.len() + state.pending_admissions.len(),
    )
}

#[cfg(test)]
pub(super) fn wait_for_admission_cancellation_latch(id: &str) {
    let engine = transfer_engine();
    let admission = Arc::clone(
        &engine
            .state
            .lock()
            .unwrap()
            .pending_admissions
            .get(id)
            .expect("test admission remains pending")
            .state,
    );
    admission.wait_until_cancelled(std::time::Duration::from_secs(3));
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
        QueuedPublication::callback(|| Ok(())),
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
    queued: QueuedPublication,
    started: impl FnOnce() + Send + 'static,
    work: impl FnOnce() -> TransferResult + Send + 'static,
    finished: impl FnOnce(TransferResult, CancelReason) + Send + 'static,
) -> Result<(), String> {
    let publisher = publication_actor().map_err(|error| {
        crate::perf_log::record_transfer_admission(crate::perf_log::TransferAdmission::Rejected);
        format!("bulk transfer queued publisher could not start: {error}")
    })?;
    enqueue_transfer_with_publisher(
        id,
        binding,
        cancellation,
        queued,
        started,
        work,
        finished,
        publisher,
    )
}

#[allow(clippy::too_many_arguments)]
fn enqueue_transfer_with_publisher(
    id: String,
    binding: BulkBinding,
    cancellation: Arc<CancelState>,
    queued: QueuedPublication,
    started: impl FnOnce() + Send + 'static,
    work: impl FnOnce() -> TransferResult + Send + 'static,
    finished: impl FnOnce(TransferResult, CancelReason) + Send + 'static,
    publisher: Arc<PublicationActor>,
) -> Result<(), String> {
    if let Err(error) = binding.validate() {
        crate::perf_log::record_transfer_admission(crate::perf_log::TransferAdmission::Rejected);
        return Err(error);
    }
    let engine = transfer_engine();
    let admission = Admission::new(Arc::clone(&cancellation));
    let mut job = Some(EngineJob {
        id: id.clone(),
        binding: binding.clone(),
        cancellation: Arc::clone(&cancellation),
        started: Box::new(started),
        work: Box::new(work),
        finished: Box::new(finished),
    });
    {
        let mut state = engine.state.lock().unwrap();
        #[cfg(test)]
        if INJECT_QUEUE_FULL.swap(false, Ordering::AcqRel) {
            crate::perf_log::record_transfer_admission(
                crate::perf_log::TransferAdmission::Rejected,
            );
            return Err("bulk transfer queue is full".into());
        }
        if state.queue.len() + state.pending_admissions.len() >= MAX_QUEUED_TRANSFERS {
            crate::perf_log::record_transfer_admission(
                crate::perf_log::TransferAdmission::Rejected,
            );
            return Err("bulk transfer queue is full".into());
        }
        if state.cancellations.contains_key(&id)
            || state.pending_admissions.contains_key(&id)
            || state.queue.iter().any(|job| job.id == id)
        {
            crate::perf_log::record_transfer_admission(
                crate::perf_log::TransferAdmission::Rejected,
            );
            return Err("bulk transfer ID is already queued or active".into());
        }
        state.pending_admissions.insert(
            id.clone(),
            PendingAdmission {
                binding,
                state: Arc::clone(&admission),
            },
        );
        crate::perf_log::record_transfer_state(
            state.active,
            state.queue.len() + state.pending_admissions.len(),
        );
    }
    // Event delivery is deliberately outside the engine and runnable-queue
    // locks. A slow renderer callback owns only this admission reservation;
    // already published transfers continue to use available worker lanes.
    let queued_error = publisher.publish(queued).err();
    if let Some(error) = queued_error {
        {
            let mut state = engine.state.lock().unwrap();
            state.pending_admissions.remove(&id);
            admission.finish_publication(false);
            cancellation.mark_finished();
            crate::perf_log::record_transfer_state(
                state.active,
                state.queue.len() + state.pending_admissions.len(),
            );
        }
        crate::perf_log::record_transfer_admission(crate::perf_log::TransferAdmission::Rejected);
        return Err(format!(
            "bulk transfer queued event could not be delivered: {error}"
        ));
    }
    let outcome = {
        let mut state = engine.state.lock().unwrap();
        state
            .pending_admissions
            .remove(&id)
            .expect("pending admission remains registered until publication");
        let outcome = admission.finish_publication(true);
        if outcome == AdmissionOutcome::Committed {
            state
                .cancellations
                .insert(id.clone(), Arc::clone(&cancellation));
            state
                .queue
                .push_back(job.take().expect("committed admission owns its job"));
        }
        crate::perf_log::record_transfer_admission(crate::perf_log::TransferAdmission::Accepted);
        crate::perf_log::record_transfer_state(
            state.active,
            state.queue.len() + state.pending_admissions.len(),
        );
        outcome
    };
    if outcome == AdmissionOutcome::Cancelled {
        let reason = cancellation.reason();
        terminalize_queued(
            job.take()
                .expect("cancelled published admission owns its job"),
            Err(failure_for_cancel(
                reason,
                TransferPhase::Queued,
                "bulk transfer cancelled while queued publication completed".into(),
            )),
            reason,
        );
    } else {
        engine.dispatch();
    }
    Ok(())
}

pub(super) fn cancel_transfer(id: &str) -> Result<CancelResponse, String> {
    let engine = transfer_engine();
    // A publishing admission is not yet externally real. Its independent
    // state supplies a bounded wait for commit/rollback without occupying the
    // runnable FIFO or acknowledging cancellation before queued is observable.
    let pending = engine
        .state
        .lock()
        .unwrap()
        .pending_admissions
        .get(id)
        .map(|pending| Arc::clone(&pending.state));
    if let Some(pending) = pending {
        match pending.cancel_and_wait(ADMISSION_CANCELLATION_TIMEOUT)? {
            AdmissionOutcome::Cancelled => {
                return Ok(CancelResponse {
                    disposition: CancelDisposition::CancelRequested,
                    phase: TransferPhase::Queued,
                });
            }
            AdmissionOutcome::Rejected => {
                return Err("bulk transfer queued publication was rejected".into());
            }
            AdmissionOutcome::Committed => return cancel_transfer(id),
            AdmissionOutcome::Publishing => unreachable!("bounded wait returns a final outcome"),
        }
    }
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

/// Actively invalidates every transfer owned by a replaced control client.
///
/// Connection lifecycle transitions call this before replacing epoch or
/// identity. That makes staleness event-driven; worker-boundary validation is
/// retained as defense in depth, not as a 50 ms polling state machine.
pub(crate) fn invalidate_bulk_scope(scope: uuid::Uuid, message: &str) {
    let engine = transfer_engine();
    let (pending, queued, active) = {
        let mut state = engine.state.lock().unwrap();
        let pending = state
            .pending_admissions
            .values()
            .filter(|pending| pending.binding.client.bulk_scope == scope)
            .map(|pending| Arc::clone(&pending.state))
            .collect::<Vec<_>>();
        let mut queued = Vec::new();
        while let Some(index) = state
            .queue
            .iter()
            .position(|job| job.binding.client.bulk_scope == scope)
        {
            let job = state
                .queue
                .remove(index)
                .expect("located queued transfer remains present");
            state.cancellations.remove(&job.id);
            queued.push(job);
        }
        let active = state
            .active_bindings
            .iter()
            .filter(|(_, binding)| binding.client.bulk_scope == scope)
            .filter_map(|(id, _)| state.cancellations.get(id).cloned())
            .collect::<Vec<_>>();
        crate::perf_log::record_transfer_state(
            state.active,
            state.queue.len() + state.pending_admissions.len(),
        );
        (pending, queued, active)
    };
    for admission in pending {
        admission.cancel_stale();
    }
    for cancellation in active {
        cancellation.cancel_stale_binding();
    }
    for job in queued {
        job.cancellation.cancel_stale_binding();
        terminalize_queued(
            job,
            Err(failure_for_cancel(
                CancelReason::StaleBinding,
                TransferPhase::Queued,
                message.to_owned(),
            )),
            CancelReason::StaleBinding,
        );
    }
    engine.dispatch();
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

#[cfg(test)]
#[path = "scheduler/tests.rs"]
mod tests;
