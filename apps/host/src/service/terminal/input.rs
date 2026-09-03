use std::{
    io::Write,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use tmux_control::{HOST_INPUT_COALESCE_BYTES, MAX_INPUT_REQUEST_BYTES};
use uuid::Uuid;

use super::capabilities::tmux_command_table;
use super::{queue_input, validate_tmux_id};

pub(super) type InputCompletion = (u64, Result<(), String>);

/// Builds a fresh tmux client command aimed at the server the input goes to.
/// Production passes `snapshot::tmux_command`; the real-tmux tests pass their
/// fixture's private socket so the forked batch path lands where the sidecar
/// is attached.
pub(super) type TmuxCommandFactory = Arc<dyn Fn() -> anyhow::Result<Command> + Send + Sync>;

/// Largest payload written in band through the already-open control client.
///
/// Below this, one `send-keys -H` command is both the whole request and its
/// single commit point, so the atomic-input contract is preserved with zero
/// tmux forks. Above it — real pastes and terminal uploads — the request keeps
/// the `load-buffer` + `paste-buffer` path, whose stdin transfer is the only
/// way to move an arbitrary blob without putting it through a command line.
/// It is deliberately the coalescing bound: the dispatcher merges same-pane
/// requests up to that size before choosing a path, so anything smaller would
/// hand a fast typist's merged burst straight back to the fork path.
pub(super) const INBAND_INPUT_MAX_BYTES: usize = HOST_INPUT_COALESCE_BYTES;
const INPUT_COMPLETION_TIMEOUT: Duration = Duration::from_secs(5);

// The fork path must remain reachable, or a real paste would have nowhere to go.
const _: () = assert!(MAX_INPUT_REQUEST_BYTES > INBAND_INPUT_MAX_BYTES);

/// How one input request reaches the pane.
///
/// `Keys` is the keystroke path: byte-exact, unbracketed, and free to be
/// coalesced with neighbouring keys for the same pane. `Paste` goes through
/// tmux's paste path, which wraps the bytes in bracketed-paste markers iff the
/// pane's application asked for them (`#{bracket_paste_flag}`); only tmux
/// knows that reliably. A paste is its own batch: the keys queued before or
/// after it — typically the CR that submits it — stay separate writes, so a
/// composer that tells a paste from a burst of typing sees exactly that.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum InputDelivery {
    Keys,
    Paste,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum InputPath {
    /// One `send-keys -H` on the open control client.
    InBand,
    /// A forked `load-buffer` + `paste-buffer` pair.
    Batch,
}

/// Keys small enough stay in band; a paste always takes the batch path,
/// because `paste-buffer` is the only tmux command that brackets.
fn input_path(len: usize, delivery: InputDelivery) -> InputPath {
    match delivery {
        InputDelivery::Keys if len <= INBAND_INPUT_MAX_BYTES => InputPath::InBand,
        InputDelivery::Keys | InputDelivery::Paste => InputPath::Batch,
    }
}

pub(super) enum InputDispatch {
    Bytes {
        input_id: u64,
        pane_id: String,
        data: Vec<u8>,
        delivery: InputDelivery,
    },
    Barrier(mpsc::SyncSender<Result<(), String>>),
    Stop,
}

pub(super) fn run_input_dispatch<W: Write>(
    receiver: mpsc::Receiver<InputDispatch>,
    control_stdin: Arc<Mutex<W>>,
    input_completion: mpsc::Receiver<InputCompletion>,
    failed: Arc<AtomicBool>,
    tmux: TmuxCommandFactory,
    report_failure: impl Fn(&str, &str),
) {
    run_input_dispatch_with(receiver, move |input_id, pane_id, data, delivery| {
        if failed.load(Ordering::Acquire) {
            return Err("persistent terminal input client stopped before dispatch".into());
        }
        match input_path(data.len(), delivery) {
            InputPath::InBand => {
                send_input_inband(&control_stdin, input_id, pane_id, data)?;
                wait_for_input_completion(&input_completion, input_id, INPUT_COMPLETION_TIMEOUT)
            }
            // Ordering against the in-band path is preserved because this
            // dispatch thread is the only writer of input: the previous
            // request's bytes were written and flushed to the control client's
            // socket before this call, so the tmux server has them in its
            // receive buffer before the `paste-buffer` client has even finished
            // connecting; and `paste-buffer` has exited before the next in-band
            // write starts.
            InputPath::Batch => send_input_batch(&*tmux, pane_id, data, delivery),
        }
        .inspect_err(|error| report_failure(pane_id, error))
    })
}

fn wait_for_input_completion(
    input_completion: &mpsc::Receiver<InputCompletion>,
    input_id: u64,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let (completed_id, result) =
            input_completion
                .recv_timeout(remaining)
                .map_err(|error| match error {
                    mpsc::RecvTimeoutError::Timeout => {
                        format!("timed out waiting for terminal input {input_id} completion")
                    }
                    mpsc::RecvTimeoutError::Disconnected => {
                        "terminal control stream closed before input completed".to_owned()
                    }
                })?;
        if completed_id == input_id {
            return result;
        }
        if completed_id > input_id {
            return Err(format!(
                "terminal input completion advanced from {input_id} to {completed_id}"
            ));
        }
        // Late completions for timed-out/aborted older requests are stale by
        // exact ID and cannot acknowledge the current input.
    }
}

fn run_input_dispatch_with(
    receiver: mpsc::Receiver<InputDispatch>,
    mut send_batch: impl FnMut(u64, &str, &[u8], InputDelivery) -> Result<(), String>,
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
            InputDispatch::Bytes {
                mut input_id,
                pane_id,
                mut data,
                delivery,
            } => {
                // The earliest per-batch point on this thread: the batch exists
                // from the moment its first message leaves the queue, and the
                // coalescing loop below only extends that same batch. Anything
                // earlier would be `receiver.recv()`, which is where an idle
                // dispatcher waits for work and would time the user's thinking.
                let dequeued = Instant::now();
                // Only keys merge with keys: a paste is one batch on its own,
                // in both directions, so the keystroke that follows it is
                // delivered as a keystroke and never inside the paste.
                while delivery == InputDelivery::Keys && data.len() < HOST_INPUT_COALESCE_BYTES {
                    match receiver.try_recv() {
                        Ok(InputDispatch::Bytes {
                            input_id: next_id,
                            pane_id: next_pane,
                            data: next_data,
                            delivery: InputDelivery::Keys,
                        }) if next_pane == pane_id
                            && data.len().saturating_add(next_data.len())
                                <= HOST_INPUT_COALESCE_BYTES =>
                        {
                            data.extend_from_slice(&next_data);
                            input_id = next_id;
                        }
                        Ok(message) => {
                            deferred = Some(message);
                            break;
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => break,
                    }
                }
                let result = send_batch(input_id, &pane_id, &data, delivery);
                // Only a committed batch has a leg to measure: a failed write
                // times a failure, not a latency, and the failure is already
                // reported through the barrier.
                if result.is_ok() {
                    crate::diagnostics::record_slow_input_leg(
                        dequeued.elapsed(),
                        data.len(),
                        &pane_id,
                    );
                }
                if pending_error.is_none() {
                    pending_error = result.err();
                }
            }
            InputDispatch::Barrier(sender) => {
                let result = pending_error.clone().map_or(Ok(()), Err);
                if sender.send(result).is_ok() {
                    pending_error = None;
                }
            }
            InputDispatch::Stop => break,
        }
    }
}

/// Writes one input request straight into the open tmux `-C` control client.
///
/// `send-keys -H` takes one hex literal per byte, so the payload is byte-exact
/// with no shell, no `vis(3)` sanitisation question, and no bracketed-paste or
/// implicit-Enter behaviour of its own — identical on the wire to what
/// `paste-buffer -d` without `-p` (which never brackets) delivers on the batch
/// path. Only `InputDelivery::Keys` comes here; a paste needs `paste-buffer`.
///
/// The request is preceded by a literal `__ADE_INPUT__` marker so an
/// asynchronous `%error` — a pane that vanished between reconciles is the real
/// case — can be attributed to the pane that was typed into. The marker is
/// deliberately *not* targeted at the pane: an untargeted `display-message -p`
/// cannot fail, so the correlation is established even when the request that
/// follows it is the thing that fails.
fn send_input_inband<W: Write>(
    stdin: &Arc<Mutex<W>>,
    input_id: u64,
    pane_id: &str,
    data: &[u8],
) -> Result<(), String> {
    validate_tmux_id(pane_id, '%').map_err(|error| error.to_string())?;
    let mut line = String::with_capacity(48 + pane_id.len() * 2 + data.len() * 3);
    queue_input_marker(&mut line, input_id, pane_id);
    line.push_str("send-keys -H -t ");
    line.push_str(pane_id);
    for byte in data {
        line.push(' ');
        // Two lowercase hex digits per byte; tmux parses these as literal keys.
        line.push(char::from_digit(u32::from(byte >> 4), 16).expect("nibble is hex"));
        line.push(char::from_digit(u32::from(byte & 0x0f), 16).expect("nibble is hex"));
    }
    line.push('\n');
    // INVARIANT: marker and command are written under one lock hold, exactly as
    // `queue_capture`'s marker and `capture-pane` are. Interleaving another
    // writer between a marker and its command would misattribute the following
    // command block and corrupt seed correlation.
    let mut writer = stdin
        .lock()
        .map_err(|_| "tmux control stdin is poisoned".to_owned())?;
    writer
        .write_all(line.as_bytes())
        .map_err(|error| format!("in-band tmux input write failed: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("in-band tmux input flush failed: {error}"))?;
    Ok(())
}

fn queue_input_marker(line: &mut String, input_id: u64, pane_id: &str) {
    line.push_str(queue_input(input_id, pane_id).as_str());
    line.push('\n');
}

/// Commits one request through a forked tmux client: `load-buffer` from
/// stdin, then one `paste-buffer -d` — the terminal commit point — that
/// delivers the buffer to the pane and deletes it. Keys arrive unbracketed;
/// a `Paste` adds `-p`, so tmux brackets the bytes iff the pane's application
/// asked for bracketed paste. Either way `paste-buffer`'s default LF→CR
/// replacement applies, as it did for every batch before there were pastes.
fn send_input_batch(
    tmux: &dyn Fn() -> anyhow::Result<Command>,
    pane_id: &str,
    data: &[u8],
    delivery: InputDelivery,
) -> Result<(), String> {
    validate_tmux_id(pane_id, '%').map_err(|error| error.to_string())?;
    let mut validate = tmux().map_err(|error| error.to_string())?;
    let output = validate
        .args(["display-message", "-p", "-t", pane_id, "#{pane_id}"])
        .output()
        .map_err(|error| format!("failed to validate terminal input pane: {error}"))?;
    if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim() != pane_id {
        return Err("terminal input pane is no longer authoritative".into());
    }

    // `send-keys -H` expands every byte into argv and must be split, which can
    // partially commit a request. Loading through stdin keeps the request out
    // of ARG_MAX; the single paste-buffer command is the terminal commit point.
    let buffer_name = format!("ade-input-{}", Uuid::new_v4().simple());
    let mut load = tmux().map_err(|error| error.to_string())?;
    load.args(["load-buffer", "-b", &buffer_name, "-"])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = load
        .spawn()
        .map_err(|error| format!("failed to start atomic tmux input: {error}"))?;
    let write_result = child
        .stdin
        .take()
        .ok_or_else(|| "atomic tmux input stdin unavailable".to_owned())?
        .write_all(data);
    let output = child
        .wait_with_output()
        .map_err(|error| format!("failed to wait for atomic tmux input: {error}"))?;
    if let Err(error) = write_result {
        cleanup_buffer(tmux, &buffer_name);
        return Err(format!(
            "atomic tmux input was not committed; accepted bytes: 0; loading failed: {error}"
        ));
    }
    if !output.status.success() {
        cleanup_buffer(tmux, &buffer_name);
        return Err(format!(
            "atomic tmux input was not committed; accepted bytes: 0: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut paste = tmux().map_err(|error| error.to_string())?;
    paste.args(["paste-buffer", "-d"]);
    // tmux 3.7 sanitizes control bytes with vis(3) unless -S is present.
    // Earlier supported releases do not expose -S and preserve them by
    // default. Probe the command surface once so keyboard controls such as
    // Ctrl-C and Ctrl-U remain bytes without dropping tmux 3.3 support.
    if paste_buffer_needs_unsanitized_flag()? {
        paste.arg("-S");
    }
    if delivery == InputDelivery::Paste {
        paste.arg("-p");
    }
    let output = paste
        .args(["-b", &buffer_name, "-t", pane_id])
        .output()
        .map_err(|error| {
            cleanup_buffer(tmux, &buffer_name);
            format!(
                "outcome unknown: tmux input paste commit could not be observed; do not retry: {error}"
            )
        })?;
    if !output.status.success() {
        cleanup_buffer(tmux, &buffer_name);
        return Err(format!(
            "atomic tmux input was not committed; accepted bytes: 0: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

fn paste_buffer_needs_unsanitized_flag() -> Result<bool, String> {
    tmux_command_table()
        .map(paste_buffer_supports_unsanitized_flag)
        .map_err(|error| format!("{error:#}"))
}

fn paste_buffer_supports_unsanitized_flag(output: &[u8]) -> bool {
    String::from_utf8_lossy(output).lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next() == Some("paste-buffer")
            && fields.any(|options| options.starts_with('[') && options.contains('S'))
    })
}

fn cleanup_buffer(tmux: &dyn Fn() -> anyhow::Result<Command>, buffer_name: &str) {
    if let Ok(mut command) = tmux() {
        let _ = command.args(["delete-buffer", "-b", buffer_name]).status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_37_unsanitized_paste_flag_is_detected_without_requiring_it_on_tmux_33() {
        assert!(paste_buffer_supports_unsanitized_flag(
            b"paste-buffer (pasteb) [-dprS] [-s separator] [-b buffer-name] [-t target-pane]\n"
        ));
        assert!(!paste_buffer_supports_unsanitized_flag(
            b"paste-buffer (pasteb) [-dpr] [-s separator] [-b buffer-name] [-t target-pane]\n"
        ));
        assert!(!paste_buffer_supports_unsanitized_flag(
            b"send-keys (send) [-FHlMRX] [key ...]\n"
        ));
    }

    #[test]
    fn writer_failure_is_retained_until_one_authoritative_barrier() {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(InputDispatch::Bytes {
                input_id: 1,
                pane_id: "%1".into(),
                data: b"a".to_vec(),
                delivery: InputDelivery::Keys,
            })
            .unwrap();
        sender
            .send(InputDispatch::Bytes {
                input_id: 2,
                pane_id: "%2".into(),
                data: b"b".to_vec(),
                delivery: InputDelivery::Keys,
            })
            .unwrap();
        let (first_barrier_tx, first_barrier_rx) = mpsc::sync_channel(1);
        sender
            .send(InputDispatch::Barrier(first_barrier_tx))
            .unwrap();
        let (second_barrier_tx, second_barrier_rx) = mpsc::sync_channel(1);
        sender
            .send(InputDispatch::Barrier(second_barrier_tx))
            .unwrap();
        sender.send(InputDispatch::Stop).unwrap();
        let mut calls = 0;
        run_input_dispatch_with(receiver, |_, _, _, _| {
            calls += 1;
            if calls == 1 {
                Err("injected writer failure".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(
            first_barrier_rx.recv().unwrap(),
            Err("injected writer failure".into())
        );
        assert_eq!(second_barrier_rx.recv().unwrap(), Ok(()));
    }

    #[test]
    fn an_abandoned_barrier_cannot_consume_the_authoritative_failure() {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(InputDispatch::Bytes {
                input_id: 1,
                pane_id: "%1".into(),
                data: b"a".to_vec(),
                delivery: InputDelivery::Keys,
            })
            .unwrap();
        let (abandoned_tx, abandoned_rx) = mpsc::sync_channel(1);
        drop(abandoned_rx);
        sender.send(InputDispatch::Barrier(abandoned_tx)).unwrap();
        let (live_tx, live_rx) = mpsc::sync_channel(1);
        sender.send(InputDispatch::Barrier(live_tx)).unwrap();
        sender.send(InputDispatch::Stop).unwrap();

        run_input_dispatch_with(receiver, |_, _, _, _| Err("late tmux failure".into()));

        assert_eq!(live_rx.recv().unwrap(), Err("late tmux failure".into()));
    }

    #[test]
    fn a_stale_completion_never_acknowledges_a_later_input() {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(InputDispatch::Bytes {
                input_id: 2,
                pane_id: "%1".into(),
                data: b"a".to_vec(),
                delivery: InputDelivery::Keys,
            })
            .unwrap();
        let (barrier_tx, barrier_rx) = mpsc::sync_channel(1);
        sender.send(InputDispatch::Barrier(barrier_tx)).unwrap();
        sender.send(InputDispatch::Stop).unwrap();
        let (completion_tx, completion_rx) = mpsc::channel();
        completion_tx.send((1, Ok(()))).unwrap();
        completion_tx
            .send((2, Err("the matching request failed".into())))
            .unwrap();
        let stdin = Arc::new(Mutex::new(Vec::<u8>::new()));

        run_input_dispatch(
            receiver,
            stdin,
            completion_rx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(super::super::super::snapshot::tmux_command),
            |_, _| {},
        );

        assert_eq!(
            barrier_rx.recv().unwrap(),
            Err("the matching request failed".into())
        );
    }

    #[test]
    fn completion_timeout_releases_the_dispatcher_and_late_id_is_stale() {
        let (completion_tx, completion_rx) = mpsc::channel();
        let error =
            wait_for_input_completion(&completion_rx, 1, Duration::from_millis(1)).unwrap_err();
        assert!(error.contains("timed out"));

        completion_tx.send((1, Ok(()))).unwrap();
        completion_tx.send((2, Ok(()))).unwrap();
        assert_eq!(
            wait_for_input_completion(&completion_rx, 2, Duration::from_secs(1)),
            Ok(())
        );
    }

    #[test]
    fn bounded_queue_backpressure_rejects_only_the_unqueued_caller() {
        let (sender, receiver) = mpsc::sync_channel(1);
        sender
            .try_send(InputDispatch::Bytes {
                input_id: 1,
                pane_id: "%1".into(),
                data: b"accepted".to_vec(),
                delivery: InputDelivery::Keys,
            })
            .unwrap();
        assert!(matches!(
            sender.try_send(InputDispatch::Bytes {
                input_id: 2,
                pane_id: "%1".into(),
                data: b"rejected".to_vec(),
                delivery: InputDelivery::Keys,
            }),
            Err(mpsc::TrySendError::Full(_))
        ));
        drop(sender);
        run_input_dispatch_with(receiver, |_, _, bytes, _| {
            assert_eq!(bytes, b"accepted");
            Ok(())
        });
    }

    #[test]
    fn stopped_client_rejects_queued_input_without_writing_or_forking() {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(InputDispatch::Bytes {
                input_id: 1,
                pane_id: "%1".into(),
                data: b"must not replay".to_vec(),
                delivery: InputDelivery::Keys,
            })
            .unwrap();
        let (barrier_tx, barrier_rx) = mpsc::sync_channel(1);
        sender.send(InputDispatch::Barrier(barrier_tx)).unwrap();
        sender.send(InputDispatch::Stop).unwrap();
        let stdin = Arc::new(Mutex::new(Vec::<u8>::new()));

        run_input_dispatch(
            receiver,
            Arc::clone(&stdin),
            mpsc::channel().1,
            Arc::new(AtomicBool::new(true)),
            Arc::new(super::super::super::snapshot::tmux_command),
            |_, _| {},
        );

        assert!(stdin.lock().unwrap().is_empty());
        assert_eq!(
            barrier_rx.recv().unwrap(),
            Err("persistent terminal input client stopped before dispatch".into())
        );
    }

    #[test]
    fn one_dispatch_fifo_orders_inputs_across_panes_before_the_fence() {
        let (sender, receiver) = mpsc::channel();
        for (input_id, pane_id, data) in [
            (1, "%1", b"a".as_slice()),
            (2, "%2", b"b".as_slice()),
            (3, "%1", b"c".as_slice()),
        ] {
            sender
                .send(InputDispatch::Bytes {
                    input_id,
                    pane_id: pane_id.into(),
                    data: data.to_vec(),
                    delivery: InputDelivery::Keys,
                })
                .unwrap();
        }
        let (barrier_tx, barrier_rx) = mpsc::sync_channel(1);
        sender.send(InputDispatch::Barrier(barrier_tx)).unwrap();
        sender.send(InputDispatch::Stop).unwrap();
        let mut observed = Vec::new();

        run_input_dispatch_with(receiver, |input_id, pane_id, data, _| {
            observed.push((input_id, pane_id.to_owned(), data.to_vec()));
            Ok(())
        });

        assert_eq!(
            observed,
            [
                (1, "%1".into(), b"a".to_vec()),
                (2, "%2".into(), b"b".to_vec()),
                (3, "%1".into(), b"c".to_vec()),
            ]
        );
        assert_eq!(barrier_rx.recv().unwrap(), Ok(()));
    }

    #[test]
    fn formerly_multi_batch_request_has_one_atomic_barrier_outcome() {
        let (sender, receiver) = mpsc::channel();
        let data = vec![b'x'; HOST_INPUT_COALESCE_BYTES * 3 + 17];
        sender
            .send(InputDispatch::Bytes {
                input_id: 1,
                pane_id: "%1".into(),
                data: data.clone(),
                delivery: InputDelivery::Keys,
            })
            .unwrap();
        let (barrier_tx, barrier_rx) = mpsc::sync_channel(1);
        sender.send(InputDispatch::Barrier(barrier_tx)).unwrap();
        sender.send(InputDispatch::Stop).unwrap();
        let mut commits = 0;
        run_input_dispatch_with(receiver, |_, pane_id, bytes, _| {
            commits += 1;
            assert_eq!(pane_id, "%1");
            assert_eq!(bytes, data);
            Err("injected commit-point failure; accepted bytes: 0".into())
        });
        assert_eq!(commits, 1);
        assert_eq!(
            barrier_rx.recv().unwrap(),
            Err("injected commit-point failure; accepted bytes: 0".into())
        );
    }

    #[test]
    fn a_paste_is_never_coalesced_with_the_keys_around_it() {
        let (sender, receiver) = mpsc::channel();
        for (input_id, data, delivery) in [
            (1, b"a".as_slice(), InputDelivery::Keys),
            (2, b"hello".as_slice(), InputDelivery::Paste),
            (3, b"\r".as_slice(), InputDelivery::Keys),
            (4, b"b".as_slice(), InputDelivery::Keys),
        ] {
            sender
                .send(InputDispatch::Bytes {
                    input_id,
                    pane_id: "%1".into(),
                    data: data.to_vec(),
                    delivery,
                })
                .unwrap();
        }
        sender.send(InputDispatch::Stop).unwrap();
        let mut observed = Vec::new();

        run_input_dispatch_with(receiver, |input_id, _, data, delivery| {
            observed.push((input_id, data.to_vec(), delivery));
            Ok(())
        });

        assert_eq!(
            observed,
            [
                (1, b"a".to_vec(), InputDelivery::Keys),
                (2, b"hello".to_vec(), InputDelivery::Paste),
                (4, b"\rb".to_vec(), InputDelivery::Keys),
            ]
        );
    }

    #[test]
    fn two_pastes_stay_two_batches() {
        let (sender, receiver) = mpsc::channel();
        for (input_id, data) in [(1, b"one".as_slice()), (2, b"two".as_slice())] {
            sender
                .send(InputDispatch::Bytes {
                    input_id,
                    pane_id: "%1".into(),
                    data: data.to_vec(),
                    delivery: InputDelivery::Paste,
                })
                .unwrap();
        }
        sender.send(InputDispatch::Stop).unwrap();
        let mut observed = Vec::new();

        run_input_dispatch_with(receiver, |input_id, _, data, delivery| {
            observed.push((input_id, data.to_vec(), delivery));
            Ok(())
        });

        assert_eq!(
            observed,
            [
                (1, b"one".to_vec(), InputDelivery::Paste),
                (2, b"two".to_vec(), InputDelivery::Paste),
            ]
        );
    }

    #[test]
    fn a_paste_always_takes_the_batch_path_and_small_keys_stay_in_band() {
        assert_eq!(input_path(1, InputDelivery::Paste), InputPath::Batch);
        assert_eq!(
            input_path(INBAND_INPUT_MAX_BYTES, InputDelivery::Keys),
            InputPath::InBand
        );
        assert_eq!(
            input_path(INBAND_INPUT_MAX_BYTES + 1, InputDelivery::Keys),
            InputPath::Batch
        );
        assert_eq!(INBAND_INPUT_MAX_BYTES, 4096);
    }

    #[test]
    fn in_band_input_is_byte_exact_hex_behind_its_own_correlation_marker() {
        let stdin = Arc::new(Mutex::new(Vec::<u8>::new()));
        send_input_inband(&stdin, 41, "%12", &[0x00, 0x03, 0x1b, 0xff, b'a']).unwrap();
        let written = String::from_utf8(stdin.lock().unwrap().clone()).unwrap();
        let mut lines = written.lines();
        // The pane sigil is absent on purpose: tmux runs a display message
        // through strftime, which swallows a literal `%12`.
        assert_eq!(
            lines.next().unwrap(),
            "display-message -p '__ADE_INPUT__:41:12'"
        );
        assert_eq!(lines.next().unwrap(), "send-keys -H -t %12 00 03 1b ff 61");
        assert!(lines.next().is_none());
        assert!(send_input_inband(&stdin, 42, "%12; kill-server", b"x").is_err());
    }

    /// The `__ADE_CAPTURE__` marker and its `capture-pane` must reach tmux with
    /// nothing between them, or the reader correlates the capture block with the
    /// wrong pane and the seed is corrupted. Input writes share the same stdin,
    /// so this proves the lock hold — not merely the write order — is what keeps
    /// them adjacent under concurrency.
    #[test]
    fn capture_marker_and_capture_command_are_never_split_by_concurrent_input() {
        use std::thread;

        let stdin = Arc::new(Mutex::new(Vec::<u8>::new()));
        let mut workers = Vec::new();
        for index in 0..8 {
            let capture_stdin = Arc::clone(&stdin);
            workers.push(thread::spawn(move || {
                for _ in 0..50 {
                    super::super::write_capture_request_resuming(
                        &capture_stdin,
                        &Mutex::new(std::collections::HashSet::new()),
                        "%1",
                        false,
                    )
                    .unwrap();
                    let _ = index;
                }
            }));
            let input_stdin = Arc::clone(&stdin);
            workers.push(thread::spawn(move || {
                for _ in 0..50 {
                    send_input_inband(&input_stdin, index * 50 + 1, "%2", b"z").unwrap();
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }

        let written = String::from_utf8(stdin.lock().unwrap().clone()).unwrap();
        let lines: Vec<_> = written.lines().collect();
        let mut capture_markers = 0;
        for (index, line) in lines.iter().enumerate() {
            if line.contains("__ADE_CAPTURE__") {
                capture_markers += 1;
                assert!(
                    lines
                        .get(index + 1)
                        .is_some_and(|next| next.starts_with("capture-pane ")),
                    "a write interleaved between the capture marker and its capture-pane"
                );
            }
            if line.contains("__ADE_INPUT__") {
                assert!(
                    lines
                        .get(index + 1)
                        .is_some_and(|next| next.starts_with("send-keys -H ")),
                    "a write interleaved between the input marker and its send-keys"
                );
            }
        }
        assert_eq!(capture_markers, 8 * 50);
    }
}
