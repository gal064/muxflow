use std::{
    sync::{Arc, Condvar, Mutex, atomic::Ordering, mpsc},
    time::Duration,
};

use tmux_agent_protocol::v1;
use tmux_control::{DESKTOP_INPUT_COALESCE_BYTES, MAX_INPUT_REQUEST_BYTES};

use super::{TerminalClient, input_epoch_is_current};

pub(super) const INPUT_MESSAGE_BUDGET: usize = 512;
pub(super) const INPUT_BYTE_BUDGET: usize = 4 * MAX_INPUT_REQUEST_BYTES;

#[derive(Default)]
pub(super) struct StopSignal {
    stopped: Mutex<bool>,
    wake: Condvar,
}

impl StopSignal {
    pub(super) fn is_stopped(&self) -> bool {
        *self.stopped.lock().unwrap()
    }

    pub(super) fn stop(&self) {
        *self.stopped.lock().unwrap() = true;
        self.wake.notify_all();
    }

    pub(super) fn if_running(&self, action: impl FnOnce()) -> bool {
        let stopped = self.stopped.lock().unwrap();
        if *stopped {
            return false;
        }
        action();
        true
    }

    pub(super) fn wait_timeout(&self, delay: Duration) -> bool {
        let stopped = self.stopped.lock().unwrap();
        let (stopped, _) = self
            .wake
            .wait_timeout_while(stopped, delay, |stopped| !*stopped)
            .unwrap();
        !*stopped
    }
}

#[derive(Default)]
pub(super) struct ClientInputQueue {
    pub(super) sender: Option<mpsc::SyncSender<ClientInputDispatch>>,
    pub(super) messages: usize,
    pub(super) bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TerminalSize {
    pub(super) columns: u16,
    pub(super) rows: u16,
}

#[derive(Clone, Copy)]
pub(super) struct VersionedTerminalSize {
    pub(super) version: u64,
    pub(super) size: TerminalSize,
}

#[derive(Default)]
pub(super) struct ResizeQueue {
    state: Mutex<ResizeState>,
    wake: Condvar,
}

#[derive(Default)]
struct ResizeState {
    desired: Option<VersionedTerminalSize>,
    next_version: u64,
    connection_epoch: u64,
    stopped: bool,
    waiters: Vec<(u64, mpsc::Sender<Result<(), String>>)>,
}

impl ResizeQueue {
    pub(super) fn replace(
        &self,
        size: TerminalSize,
    ) -> Result<mpsc::Receiver<Result<(), String>>, String> {
        let (sender, receiver) = mpsc::channel();
        let mut state = self.state.lock().unwrap();
        if state.stopped {
            return Err("terminal bridge is stopped; resize was not queued".into());
        }
        state.next_version = state.next_version.saturating_add(1);
        let version = state.next_version;
        state.desired = Some(VersionedTerminalSize { version, size });
        state.waiters.push((version, sender));
        drop(state);
        self.wake.notify_one();
        Ok(receiver)
    }

    pub(super) fn wait_for_attempt(
        &self,
        previous: Option<(u64, u64)>,
    ) -> Option<(VersionedTerminalSize, u64)> {
        let mut state = self.state.lock().unwrap();
        loop {
            if state.stopped {
                return None;
            }
            if let Some(desired) = state.desired {
                let key = (desired.version, state.connection_epoch);
                if Some(key) != previous {
                    return Some((desired, state.connection_epoch));
                }
            }
            state = self.wake.wait(state).unwrap();
        }
    }

    pub(super) fn complete(&self, version: u64, connection_epoch: u64, result: Result<(), String>) {
        let mut state = self.state.lock().unwrap();
        let result = if state.connection_epoch == connection_epoch {
            result
        } else {
            Err("terminal connection changed before resize acknowledgement".into())
        };
        let mut remaining = Vec::with_capacity(state.waiters.len());
        for (waiter_version, sender) in state.waiters.drain(..) {
            if waiter_version <= version {
                let _ = sender.send(result.clone());
            } else {
                remaining.push((waiter_version, sender));
            }
        }
        state.waiters = remaining;
    }

    pub(super) fn reconnected(&self) {
        let mut state = self.state.lock().unwrap();
        state.connection_epoch = state.connection_epoch.saturating_add(1);
        drop(state);
        self.wake.notify_one();
    }

    pub(super) fn stop(&self) {
        let mut state = self.state.lock().unwrap();
        state.stopped = true;
        for (_, sender) in state.waiters.drain(..) {
            let _ = sender.send(Err("terminal bridge stopped before resize landed".into()));
        }
        drop(state);
        self.wake.notify_all();
    }
}

pub(super) enum ClientInputDispatch {
    Bytes {
        pane_id: String,
        data: Vec<u8>,
        epoch: u64,
    },
    Barrier(mpsc::SyncSender<Result<(), String>>),
    Stop,
}

pub(super) fn run_client_input_dispatch(
    client: Arc<TerminalClient>,
    receiver: mpsc::Receiver<ClientInputDispatch>,
) {
    let mut deferred = None;
    let mut pending_error = None;
    loop {
        let message = match deferred.take() {
            Some(message) => message,
            None => match receiver.recv() {
                Ok(message) => message,
                Err(_) => break,
            },
        };
        match message {
            ClientInputDispatch::Bytes {
                pane_id,
                mut data,
                epoch,
            } => {
                let mut message_count = 1;
                while data.len() < DESKTOP_INPUT_COALESCE_BYTES {
                    match receiver.try_recv() {
                        Ok(ClientInputDispatch::Bytes {
                            pane_id: next_pane,
                            data: next_data,
                            epoch: next_epoch,
                        }) if next_pane == pane_id
                            && next_epoch == epoch
                            && data.len().saturating_add(next_data.len())
                                <= DESKTOP_INPUT_COALESCE_BYTES =>
                        {
                            data.extend_from_slice(&next_data);
                            message_count += 1;
                        }
                        Ok(message) => {
                            deferred = Some(message);
                            break;
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => break,
                    }
                }
                // Input accepted by an older connection must not poison the
                // replacement connection's ordered stream.
                if input_epoch_is_current(&client, epoch)
                    && client.ready.load(Ordering::Acquire)
                    && !client.read_only.load(Ordering::Acquire)
                {
                    let dispatched_bytes = data.len();
                    let result = client.dispatch_request(v1::Request {
                        operation: v1::Operation::TerminalInput.into(),
                        scope: pane_id,
                        data,
                        ..Default::default()
                    });
                    if let Err(error) = result
                        && pending_error.is_none()
                    {
                        pending_error = Some(error);
                    }
                    client.release_input_budget(message_count, dispatched_bytes);
                } else {
                    client.release_input_budget(message_count, data.len());
                }
            }
            ClientInputDispatch::Barrier(sender) => {
                let result = pending_error.take().map_or(Ok(()), Err);
                let _ = sender.send(result);
            }
            ClientInputDispatch::Stop => break,
        }
    }
}

pub(super) fn run_client_resize_dispatch(client: Arc<TerminalClient>) {
    let mut previous = None;
    while let Some((desired, connection_epoch)) = client.resize_queue.wait_for_attempt(previous) {
        previous = Some((desired.version, connection_epoch));
        let result = client.flush_input().and_then(|_| {
            if client.stop_signal.is_stopped() {
                return Err("terminal bridge stopped before resize landed".into());
            }
            client.request(v1::Request {
                operation: v1::Operation::ResizeTerminal.into(),
                columns: desired.size.columns.into(),
                rows: desired.size.rows.into(),
                ..Default::default()
            })?;
            Ok(())
        });
        client
            .resize_queue
            .complete(desired.version, connection_epoch, result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::mark_input_reconnected;
    use std::{thread, time::Instant};

    #[test]
    fn input_byte_budget_refuses_retryably_without_displacing_accepted_order() {
        let client = Arc::new(TerminalClient::new());
        let (sender, receiver) = mpsc::sync_channel(INPUT_MESSAGE_BUDGET);
        client.input_queue.lock().unwrap().sender = Some(sender);
        mark_input_reconnected(&client);
        client.ready.store(true, Ordering::Release);

        for marker in 0..4_u8 {
            assert_eq!(
                client.enqueue_input("%1".into(), vec![marker; MAX_INPUT_REQUEST_BYTES]),
                Ok(())
            );
        }
        let error = client
            .enqueue_input("%1".into(), vec![9])
            .expect_err("the byte budget must refuse excess synchronously");
        assert!(error.contains("retry without dropping bytes"), "{error}");
        for marker in 0..4_u8 {
            let ClientInputDispatch::Bytes { data, .. } = receiver.recv().unwrap() else {
                panic!("expected accepted input bytes");
            };
            assert_eq!(data[0], marker, "refusal must preserve accepted order");
        }
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn input_message_budget_refuses_the_513th_message_without_displacement() {
        let client = Arc::new(TerminalClient::new());
        let (sender, receiver) = mpsc::sync_channel(INPUT_MESSAGE_BUDGET);
        client.input_queue.lock().unwrap().sender = Some(sender);
        mark_input_reconnected(&client);
        client.ready.store(true, Ordering::Release);

        for marker in 0..INPUT_MESSAGE_BUDGET {
            assert_eq!(
                client.enqueue_input("%1".into(), marker.to_be_bytes().to_vec()),
                Ok(())
            );
        }
        let error = client
            .enqueue_input("%1".into(), b"refused".to_vec())
            .expect_err("the message budget must be explicit");
        assert!(error.contains("retry without dropping bytes"), "{error}");
        for marker in 0..INPUT_MESSAGE_BUDGET {
            let ClientInputDispatch::Bytes { data, .. } = receiver.recv().unwrap() else {
                panic!("expected accepted input bytes");
            };
            assert_eq!(data, marker.to_be_bytes());
        }
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn legacy_and_binary_input_share_the_atomic_per_message_limit() {
        let client = TerminalClient::new();
        client.ready.store(true, Ordering::Release);
        let oversized = vec![b'x'; MAX_INPUT_REQUEST_BYTES + 1];
        let error = client
            .enqueue_input("%1".into(), oversized)
            .expect_err("all callers must share the same atomic limit");
        assert!(error.contains("retry with a smaller batch"), "{error}");
    }

    #[test]
    fn resize_queue_replaces_intermediate_sizes_so_the_final_size_wins() {
        let queue = ResizeQueue::default();
        let first = queue
            .replace(TerminalSize {
                columns: 80,
                rows: 24,
            })
            .unwrap();
        let second = queue
            .replace(TerminalSize {
                columns: 120,
                rows: 40,
            })
            .unwrap();
        let final_receiver = queue
            .replace(TerminalSize {
                columns: 160,
                rows: 50,
            })
            .unwrap();
        let (desired, epoch) = queue.wait_for_attempt(None).unwrap();
        assert_eq!(
            desired.size,
            TerminalSize {
                columns: 160,
                rows: 50,
            }
        );
        queue.complete(desired.version, epoch, Ok(()));
        assert_eq!(first.recv().unwrap(), Ok(()));
        assert_eq!(second.recv().unwrap(), Ok(()));
        assert_eq!(final_receiver.recv().unwrap(), Ok(()));
    }

    #[test]
    fn failed_resize_is_retained_and_retried_after_reconnect() {
        let queue = ResizeQueue::default();
        let receiver = queue
            .replace(TerminalSize {
                columns: 132,
                rows: 43,
            })
            .unwrap();
        let (desired, epoch) = queue.wait_for_attempt(None).unwrap();
        let attempt = Some((desired.version, epoch));
        queue.complete(desired.version, epoch, Err("link lost".into()));
        assert_eq!(receiver.recv().unwrap(), Err("link lost".into()));
        queue.reconnected();
        let (retried, reconnect_epoch) = queue.wait_for_attempt(attempt).unwrap();
        assert_eq!(retried.version, desired.version);
        assert_eq!(retried.size, desired.size);
        assert_ne!(reconnect_epoch, epoch);
    }

    #[test]
    fn resize_ack_from_a_replaced_connection_is_not_reported_as_landed() {
        let queue = ResizeQueue::default();
        let receiver = queue
            .replace(TerminalSize {
                columns: 101,
                rows: 31,
            })
            .unwrap();
        let (desired, stale_epoch) = queue.wait_for_attempt(None).unwrap();
        queue.reconnected();
        queue.complete(desired.version, stale_epoch, Ok(()));
        let error = receiver.recv().unwrap().unwrap_err();
        assert!(error.contains("connection changed"), "{error}");
    }

    #[test]
    fn reconnect_backoff_is_cancelled_promptly_by_stop() {
        let client = Arc::new(TerminalClient::new());
        let waiter = Arc::clone(&client);
        let started = Instant::now();
        let thread = thread::spawn(move || waiter.wait_for_reconnect(Duration::from_secs(60)));
        client.stop_signal.stop();
        assert!(!thread.join().unwrap());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "stop must not wait for reconnect backoff"
        );
    }
}
