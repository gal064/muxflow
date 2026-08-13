use std::{
    io::Write,
    process::Stdio,
    sync::{OnceLock, mpsc},
};

use tmux_control::HOST_INPUT_COALESCE_BYTES;
use uuid::Uuid;

use super::super::snapshot::tmux_command;
use super::validate_tmux_id;

pub(super) enum InputDispatch {
    Bytes {
        pane_id: String,
        data: Vec<u8>,
        completion: mpsc::SyncSender<Result<(), String>>,
    },
    Barrier(mpsc::SyncSender<Result<(), String>>),
    Stop,
}

pub(super) fn run_input_dispatch(receiver: mpsc::Receiver<InputDispatch>) {
    run_input_dispatch_with(receiver, send_input_batch)
}

fn run_input_dispatch_with(
    receiver: mpsc::Receiver<InputDispatch>,
    mut send_batch: impl FnMut(&str, &[u8]) -> Result<(), String>,
) {
    let mut deferred = None;
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
                pane_id,
                mut data,
                completion,
            } => {
                let mut completions = vec![completion];
                while data.len() < HOST_INPUT_COALESCE_BYTES {
                    match receiver.try_recv() {
                        Ok(InputDispatch::Bytes {
                            pane_id: next_pane,
                            data: next_data,
                            completion,
                        }) if next_pane == pane_id
                            && data.len().saturating_add(next_data.len())
                                <= HOST_INPUT_COALESCE_BYTES =>
                        {
                            data.extend_from_slice(&next_data);
                            completions.push(completion);
                        }
                        Ok(message) => {
                            deferred = Some(message);
                            break;
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => break,
                    }
                }
                let result = send_batch(&pane_id, &data);
                for completion in completions {
                    let _ = completion.send(result.clone());
                }
            }
            InputDispatch::Barrier(sender) => {
                let _ = sender.send(Ok(()));
            }
            InputDispatch::Stop => break,
        }
    }
}

fn send_input_batch(pane_id: &str, data: &[u8]) -> Result<(), String> {
    validate_tmux_id(pane_id, '%').map_err(|error| error.to_string())?;
    let mut validate = tmux_command();
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
    let mut load = tmux_command();
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
        cleanup_buffer(&buffer_name);
        return Err(format!(
            "atomic tmux input was not committed; accepted bytes: 0; loading failed: {error}"
        ));
    }
    if !output.status.success() {
        cleanup_buffer(&buffer_name);
        return Err(format!(
            "atomic tmux input was not committed; accepted bytes: 0: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut paste = tmux_command();
    paste.args(["paste-buffer", "-d"]);
    // tmux 3.7 sanitizes control bytes with vis(3) unless -S is present.
    // Earlier supported releases do not expose -S and preserve them by
    // default. Probe the command surface once so keyboard controls such as
    // Ctrl-C and Ctrl-U remain bytes without dropping tmux 3.3 support.
    if paste_buffer_needs_unsanitized_flag()? {
        paste.arg("-S");
    }
    let output = paste
        .args(["-b", &buffer_name, "-t", pane_id])
        .output()
        .map_err(|error| {
            cleanup_buffer(&buffer_name);
            format!(
                "outcome unknown: tmux input paste commit could not be observed; do not retry: {error}"
            )
        })?;
    if !output.status.success() {
        cleanup_buffer(&buffer_name);
        return Err(format!(
            "atomic tmux input was not committed; accepted bytes: 0: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

fn paste_buffer_needs_unsanitized_flag() -> Result<bool, String> {
    static SUPPORTS_FLAG: OnceLock<bool> = OnceLock::new();
    cache_successful_probe(&SUPPORTS_FLAG, || {
        let output = tmux_command()
            .arg("list-commands")
            .output()
            .map_err(|error| format!("failed to inspect tmux paste semantics: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "failed to inspect tmux paste semantics: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(paste_buffer_supports_unsanitized_flag(&output.stdout))
    })
}

fn cache_successful_probe(
    cache: &OnceLock<bool>,
    probe: impl FnOnce() -> Result<bool, String>,
) -> Result<bool, String> {
    if let Some(value) = cache.get() {
        return Ok(*value);
    }
    let value = probe()?;
    let _ = cache.set(value);
    Ok(*cache.get().unwrap_or(&value))
}

fn paste_buffer_supports_unsanitized_flag(output: &[u8]) -> bool {
    String::from_utf8_lossy(output).lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next() == Some("paste-buffer")
            && fields.any(|options| options.starts_with('[') && options.contains('S'))
    })
}

fn cleanup_buffer(buffer_name: &str) {
    let _ = tmux_command()
        .args(["delete-buffer", "-b", buffer_name])
        .status();
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
    fn transient_probe_failure_is_retried_and_only_success_is_cached() {
        let cache = OnceLock::new();
        assert_eq!(
            cache_successful_probe(&cache, || Err("tmux temporarily unavailable".into())),
            Err("tmux temporarily unavailable".into())
        );
        assert_eq!(cache_successful_probe(&cache, || Ok(true)), Ok(true));
        assert_eq!(cache_successful_probe(&cache, || Ok(false)), Ok(true));
    }

    #[test]
    fn writer_failure_is_correlated_without_poisoning_the_next_request() {
        let (sender, receiver) = mpsc::channel();
        let (first_tx, first_rx) = mpsc::sync_channel(1);
        let (second_tx, second_rx) = mpsc::sync_channel(1);
        sender
            .send(InputDispatch::Bytes {
                pane_id: "%1".into(),
                data: b"a".to_vec(),
                completion: first_tx,
            })
            .unwrap();
        sender
            .send(InputDispatch::Bytes {
                pane_id: "%2".into(),
                data: b"b".to_vec(),
                completion: second_tx,
            })
            .unwrap();
        sender.send(InputDispatch::Stop).unwrap();
        let mut calls = 0;
        run_input_dispatch_with(receiver, |_, _| {
            calls += 1;
            if calls == 1 {
                Err("injected writer failure".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(
            first_rx.recv().unwrap(),
            Err("injected writer failure".into())
        );
        assert_eq!(second_rx.recv().unwrap(), Ok(()));
    }

    #[test]
    fn bounded_queue_backpressure_rejects_only_the_unqueued_caller() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let (accepted_tx, accepted_rx) = mpsc::sync_channel(1);
        sender
            .try_send(InputDispatch::Bytes {
                pane_id: "%1".into(),
                data: b"accepted".to_vec(),
                completion: accepted_tx,
            })
            .unwrap();
        let (rejected_tx, _rejected_rx) = mpsc::sync_channel(1);
        assert!(matches!(
            sender.try_send(InputDispatch::Bytes {
                pane_id: "%1".into(),
                data: b"rejected".to_vec(),
                completion: rejected_tx,
            }),
            Err(mpsc::TrySendError::Full(_))
        ));
        drop(sender);
        run_input_dispatch_with(receiver, |_, bytes| {
            assert_eq!(bytes, b"accepted");
            Ok(())
        });
        assert_eq!(accepted_rx.recv().unwrap(), Ok(()));
    }

    #[test]
    fn formerly_multi_batch_request_has_one_atomic_commit_outcome() {
        let (sender, receiver) = mpsc::channel();
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        let data = vec![b'x'; HOST_INPUT_COALESCE_BYTES * 3 + 17];
        sender
            .send(InputDispatch::Bytes {
                pane_id: "%1".into(),
                data: data.clone(),
                completion: completion_tx,
            })
            .unwrap();
        sender.send(InputDispatch::Stop).unwrap();
        let mut commits = 0;
        run_input_dispatch_with(receiver, |pane_id, bytes| {
            commits += 1;
            assert_eq!(pane_id, "%1");
            assert_eq!(bytes, data);
            Err("injected commit-point failure; accepted bytes: 0".into())
        });
        assert_eq!(commits, 1);
        assert_eq!(
            completion_rx.recv().unwrap(),
            Err("injected commit-point failure; accepted bytes: 0".into())
        );
    }
}
