//! Bounded publication of the queued transition.
//!
//! Tauri channel handlers are supplied by the runtime and can execute
//! arbitrary platform work.  A wedged handler must not retain a transfer job,
//! destination reservation, or scheduler slot indefinitely.  The publisher
//! therefore owns at most one lightweight queued event while the admitting
//! caller retains (and can boundedly drop) all transfer resources.

use std::{
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};

use serde_json::Value;
use tauri::ipc::{Channel, InvokeResponseBody};

const PUBLICATION_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) struct QueuedPublication {
    queued: Box<dyn FnOnce() -> Result<(), String> + Send + 'static>,
    late_rollback: Box<dyn FnOnce() -> Result<(), String> + Send + 'static>,
}

impl QueuedPublication {
    pub(crate) fn json(
        channel: Channel<Value>,
        value: Value,
        late_rollback: Value,
        context: &'static str,
    ) -> Self {
        let rollback_channel = channel.clone();
        Self {
            queued: Box::new(move || {
                channel
                    .send(value)
                    .map_err(|error| format!("{context}: {error}"))
            }),
            late_rollback: Box::new(move || {
                rollback_channel
                    .send(late_rollback)
                    .map_err(|error| format!("{context} rollback: {error}"))
            }),
        }
    }

    pub(crate) fn raw(
        channel: Channel<InvokeResponseBody>,
        frame: Vec<u8>,
        late_rollback: Vec<u8>,
        context: &'static str,
    ) -> Self {
        let rollback_channel = channel.clone();
        Self {
            queued: Box::new(move || {
                channel
                    .send(InvokeResponseBody::Raw(frame))
                    .map_err(|error| format!("{context}: {error}"))
            }),
            late_rollback: Box::new(move || {
                rollback_channel
                    .send(InvokeResponseBody::Raw(late_rollback))
                    .map_err(|error| format!("{context} rollback: {error}"))
            }),
        }
    }

    #[cfg(test)]
    pub(super) fn callback(callback: impl FnOnce() -> Result<(), String> + Send + 'static) -> Self {
        Self::callback_with_rollback(callback, || Ok(()))
    }

    #[cfg(test)]
    fn callback_with_rollback(
        callback: impl FnOnce() -> Result<(), String> + Send + 'static,
        late_rollback: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Self {
        Self {
            queued: Box::new(callback),
            late_rollback: Box::new(late_rollback),
        }
    }
}

struct Request {
    publication: QueuedPublication,
    completion: mpsc::SyncSender<Result<(), String>>,
}

pub(super) struct PublicationActor {
    workers: Vec<Worker>,
}

struct Worker {
    available: Arc<AtomicBool>,
    sender: Option<mpsc::SyncSender<Request>>,
    handle: Option<JoinHandle<()>>,
}

impl PublicationActor {
    fn spawn() -> Result<Arc<Self>, String> {
        Self::spawn_with_failure(None)
    }

    fn spawn_with_failure(fail_at: Option<usize>) -> Result<Arc<Self>, String> {
        let mut workers = Vec::with_capacity(2);
        for index in 0..2 {
            let (sender, receiver) = mpsc::sync_channel::<Request>(1);
            let available = Arc::new(AtomicBool::new(true));
            let worker_available = Arc::clone(&available);
            if fail_at == Some(index) {
                return Err(format!(
                    "bulk transfer queued publisher worker {index} could not start: injected failure"
                ));
            }
            let handle = std::thread::Builder::new()
                .name(format!("transfer-event-publication-{index}"))
                .spawn(move || {
                    while let Ok(request) = receiver.recv() {
                        let QueuedPublication {
                            queued,
                            late_rollback,
                        } = request.publication;
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(queued))
                            .unwrap_or_else(|panic| {
                                Err(format!(
                                    "bulk transfer queued event panicked: {}",
                                    super::panic_message(panic)
                                ))
                            });
                        let published = result.is_ok();
                        let receiver_gone = request.completion.send(result).is_err();
                        let late = published && receiver_gone;
                        if late {
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                                late_rollback,
                            ));
                        }
                        worker_available.store(true, Ordering::Release);
                    }
                })
                .map_err(|error| {
                    format!(
                        "bulk transfer queued publisher worker {index} could not start: {error}"
                    )
                })?;
            workers.push(Worker {
                available,
                sender: Some(sender),
                handle: Some(handle),
            });
        }
        Ok(Arc::new(Self { workers }))
    }

    pub(super) fn publish(&self, publication: QueuedPublication) -> Result<(), String> {
        self.publish_with_timeout(publication, PUBLICATION_TIMEOUT)
    }

    fn publish_with_timeout(
        &self,
        publication: QueuedPublication,
        timeout: Duration,
    ) -> Result<(), String> {
        // A zero-capacity completion is the publication linearization point.
        // If the bounded wait times out, the worker cannot have successfully
        // handed off success to an abandoned buffered slot: its send fails and
        // one late rollback follows an already-observable queued event.
        let (completion, result) = mpsc::sync_channel(0);
        let request = Request {
            publication,
            completion,
        };
        let worker = self.workers.iter().find(|worker| {
            worker
                .available
                .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        });
        let Some(worker) = worker else {
            return Err("bulk transfer queued publisher is busy; retry the transfer".to_owned());
        };
        let Some(sender) = worker.sender.as_ref() else {
            worker.available.store(true, Ordering::Release);
            return Err("bulk transfer queued publisher stopped".to_owned());
        };
        if let Err(error) = sender.try_send(request) {
            worker.available.store(true, Ordering::Release);
            return Err(match error {
                mpsc::TrySendError::Full(_) => {
                    "bulk transfer queued publisher invariant failed".to_owned()
                }
                mpsc::TrySendError::Disconnected(_) => {
                    "bulk transfer queued publisher stopped".to_owned()
                }
            });
        }
        result.recv_timeout(timeout).map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => {
                "bulk transfer queued publication timed out; retry the transfer".to_owned()
            }
            mpsc::RecvTimeoutError::Disconnected => {
                "bulk transfer queued publisher stopped".to_owned()
            }
        })?
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Close the request channel before joining. This also makes a partial
        // two-worker initialization failure synchronously retire worker zero.
        self.sender.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(super) fn publication_actor() -> Result<Arc<PublicationActor>, String> {
    static ACTOR: OnceLock<Mutex<Option<Arc<PublicationActor>>>> = OnceLock::new();
    let mut actor = ACTOR.get_or_init(|| Mutex::new(None)).lock().unwrap();
    if let Some(actor) = actor.as_ref() {
        return Ok(Arc::clone(actor));
    }
    let spawned = PublicationActor::spawn()?;
    *actor = Some(Arc::clone(&spawned));
    Ok(spawned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wedged_publication_is_bounded_and_later_work_fails_fast() {
        let actor = PublicationActor::spawn().unwrap();
        let mut releases = Vec::new();
        let mut blocked = Vec::new();
        for _ in 0..2 {
            let (entered, entered_rx) = mpsc::channel();
            let (release, release_rx) = mpsc::channel::<()>();
            releases.push(release);
            let actor = Arc::clone(&actor);
            blocked.push(std::thread::spawn(move || {
                actor.publish_with_timeout(
                    QueuedPublication::callback(move || {
                        entered.send(()).unwrap();
                        let _ = release_rx.recv();
                        Ok(())
                    }),
                    Duration::from_millis(25),
                )
            }));
            entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        let started = std::time::Instant::now();
        assert!(
            actor
                .publish_with_timeout(QueuedPublication::callback(|| Ok(())), Duration::MAX)
                .unwrap_err()
                .contains("busy")
        );
        assert!(started.elapsed() < Duration::from_millis(100));
        for blocked in blocked {
            assert!(blocked.join().unwrap().unwrap_err().contains("timed out"));
        }
        drop(releases);
    }

    #[test]
    fn queued_event_visible_before_timeout_gets_one_late_terminal_rollback() {
        let actor = PublicationActor::spawn().unwrap();
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let (rolled_back, rolled_back_rx) = mpsc::channel();
        let result = actor.publish_with_timeout(
            QueuedPublication::callback_with_rollback(
                move || {
                    entered.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                },
                move || {
                    rolled_back.send(()).unwrap();
                    Ok(())
                },
            ),
            Duration::from_millis(25),
        );
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(result.unwrap_err().contains("timed out"));
        release.send(()).unwrap();
        rolled_back_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(rolled_back_rx.try_recv().is_err());
    }

    #[test]
    fn first_publication_worker_spawn_failure_is_an_error_not_a_panic() {
        let error = PublicationActor::spawn_with_failure(Some(0))
            .err()
            .expect("first worker failure must reject initialization");
        assert!(error.contains("worker 0"), "{error}");
    }

    #[test]
    fn second_publication_worker_spawn_failure_retires_the_partial_pool() {
        let error = PublicationActor::spawn_with_failure(Some(1))
            .err()
            .expect("second worker failure must reject initialization");
        assert!(error.contains("worker 1"), "{error}");
        // A fresh actor starts both workers, proving the failed attempt did not
        // publish or cache a partial pool.
        let actor = PublicationActor::spawn().unwrap();
        actor
            .publish_with_timeout(
                QueuedPublication::callback(|| Ok(())),
                Duration::from_secs(1),
            )
            .unwrap();
    }
}
