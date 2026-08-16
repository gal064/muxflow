use super::*;

pub(super) struct TmuxActionContext<'a> {
    pub(super) control_tx: &'a mpsc::Sender<SequencerControl>,
    pub(super) event_tx: &'a mpsc::Sender<SequencerControl>,
    pub(super) generation: &'a Arc<AtomicU64>,
    pub(super) overflowed: &'a Arc<AtomicBool>,
    pub(super) pending: &'a Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    pub(super) terminal: &'a Arc<Mutex<TerminalClients>>,
    pub(super) topology_lock: &'a Arc<tokio::sync::Mutex<()>>,
    pub(super) topology_baseline: &'a Arc<Mutex<Option<(tmux_control::TmuxSnapshot, String)>>>,
    pub(super) topology_signal: &'a super::super::topology::TopologySignal,
}

pub(super) async fn handle(request_id: u64, request: v1::Request, context: TmuxActionContext<'_>) {
    let TmuxActionContext {
        control_tx,
        event_tx,
        generation,
        overflowed,
        pending,
        terminal,
        topology_lock,
        topology_baseline,
        topology_signal,
    } = context;
    match v1::Operation::TmuxAction {
        v1::Operation::TmuxAction => {
            let Some(action) = request.tmux_action else {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_tmux_action", "tmux action payload is required"),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            };
            let (_topology_guard, known_generation) =
                lock_topology_generation(topology_lock, generation).await;
            let cached_baseline = topology_baseline.lock().unwrap().clone();
            // Resolving the baseline has to know the action kind: creating the
            // first session is allowed to run with no tmux server, and every
            // other action is not (M10-E060).
            let action_kind = v1::TmuxActionKind::try_from(action.kind).unwrap_or_default();
            // Accepted terminal input must land before the one fresh topology
            // precheck used by the action. Doing this after discovery made the
            // action rediscover inside `execute`, paying a second remote tmux
            // fork merely to cover changes during the barrier itself.
            let fresh_task = match begin_after_input_barrier(
                || terminal.lock().unwrap().flush_input(),
                || {
                    tokio::task::spawn_blocking(move || {
                        tmux_actions::discover_for_action(action_kind)
                    })
                },
            ) {
                Ok(task) => task,
                Err(error) => {
                    send_response(
                        control_tx,
                        request_id,
                        response_error("terminal_input_flush_failed", &error.to_string()),
                    )
                    .await;
                    pending.lock().unwrap().remove(&request_id);
                    return;
                }
            };
            let fresh = fresh_task.await;
            let (fresh_snapshot, fresh_identity) = match fresh {
                Ok(Ok(value)) => value,
                Ok(Err(error)) => {
                    send_response(
                        control_tx,
                        request_id,
                        response_error("tmux_action_rejected", &error.to_string()),
                    )
                    .await;
                    pending.lock().unwrap().remove(&request_id);
                    return;
                }
                Err(error) => {
                    send_response(
                        control_tx,
                        request_id,
                        response_error("tmux_action_task_failed", &error.to_string()),
                    )
                    .await;
                    pending.lock().unwrap().remove(&request_id);
                    return;
                }
            };
            let same_identity = cached_baseline
                .as_ref()
                .is_some_and(|(_, identity)| identity.as_str() == fresh_identity);
            let same_topology = cached_baseline
                .as_ref()
                .is_some_and(|(snapshot, _)| same_action_topology(snapshot, &fresh_snapshot));
            if !same_identity || !same_topology {
                let next_generation = generation.fetch_add(1, Ordering::AcqRel) + 1;
                *topology_baseline.lock().unwrap() =
                    Some((fresh_snapshot.clone(), fresh_identity.clone()));
                reconcile_terminal_clients(terminal, &fresh_snapshot, event_tx, overflowed);
                let snapshot =
                    snapshot_from_identity(fresh_snapshot, next_generation, fresh_identity);
                let _ = event_tx
                    .send(SequencerControl::OrderedEvent(v1::HostEvent {
                        kind: v1::EventKind::TopologySnapshot.into(),
                        scope: "topology".into(),
                        snapshot: Some(snapshot),
                        detail: "external topology mutation observed before action".into(),
                        ..Default::default()
                    }))
                    .await;
                send_response(
                    control_tx,
                    request_id,
                    response_error(
                        "stale_topology",
                        "stale topology: external tmux structural mutation was reconciled before action",
                    ),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            }
            let barrier_sender = event_tx.clone();
            let result = tokio::task::spawn_blocking(move || {
                tmux_actions::execute(
                    action,
                    known_generation,
                    fresh_snapshot,
                    fresh_identity,
                    || topology_epoch_barrier(&barrier_sender),
                )
            })
            .await;
            match result {
                Ok(Ok(mut outcome)) => {
                    let selection_required = match select_and_refresh_action_outcome(
                        action_kind,
                        terminal,
                        &mut outcome,
                    )
                    .await
                    {
                        Ok(required) => required,
                        Err(error) => {
                            send_response(
                                control_tx,
                                request_id,
                                response_error("outcome_unknown", &error),
                            )
                            .await;
                            return;
                        }
                    };
                    let next_generation = generation.fetch_add(1, Ordering::AcqRel) + 1;
                    outcome.result.topology_generation = next_generation;
                    *topology_baseline.lock().unwrap() =
                        Some((outcome.snapshot.clone(), outcome.server_identity.clone()));
                    if let Some(epoch) = outcome.covered_dirty_epoch {
                        topology_signal.acknowledge_through(epoch);
                    }
                    reconcile_terminal_clients(terminal, &outcome.snapshot, event_tx, overflowed);
                    let snapshot = snapshot_from_identity(
                        outcome.snapshot,
                        next_generation,
                        outcome.server_identity,
                    );
                    let mut success_response = Some(v1::Response {
                        ok: true,
                        tmux_action_result: Some(outcome.result),
                        ..Default::default()
                    });
                    if selection_required {
                        // This acknowledges that selection and geometry
                        // reconciliation already landed. Put the ack directly
                        // before its topology event on the one ordered
                        // sequencer so the desktop can suppress its generic
                        // visibility restatement before applying the snapshot.
                        send_response(
                            control_tx,
                            request_id,
                            success_response.take().expect("success response exists"),
                        )
                        .await;
                    }
                    let _ = event_tx
                        .send(SequencerControl::OrderedEvent(v1::HostEvent {
                            kind: v1::EventKind::TopologySnapshot.into(),
                            scope: "topology".into(),
                            snapshot: Some(snapshot),
                            ..Default::default()
                        }))
                        .await;
                    if let Some(response) = success_response {
                        send_response(control_tx, request_id, response).await;
                    }
                }
                Ok(Err(error)) => {
                    let message = error.to_string();
                    let code = if message.starts_with("stale topology") {
                        "stale_topology"
                    } else if message.starts_with("outcome unknown") {
                        "outcome_unknown"
                    } else if message.contains("requires confirmation") {
                        "confirmation_required"
                    } else if message.contains("no authoritative session reorder") {
                        "session_reorder_unsupported"
                    } else {
                        "tmux_action_rejected"
                    };
                    send_response(control_tx, request_id, response_error(code, &message)).await;
                }
                Err(error) => {
                    send_response(
                        control_tx,
                        request_id,
                        response_error("tmux_action_task_failed", &error.to_string()),
                    )
                    .await;
                }
            }
        }
        _ => unreachable!(),
    }
}

/// Completes the control-client side of actions whose contract includes
/// selection, then replaces their pre-selection topology with the geometry
/// that will be published and acknowledged.
async fn select_and_refresh_action_outcome(
    action_kind: v1::TmuxActionKind,
    terminal: &Arc<Mutex<TerminalClients>>,
    outcome: &mut tmux_actions::ActionOutcome,
) -> Result<bool, String> {
    let required = matches!(
        action_kind,
        v1::TmuxActionKind::SelectSession
            | v1::TmuxActionKind::CreateSession
            | v1::TmuxActionKind::CreateWindow
    );
    if !required {
        return Ok(false);
    }
    terminal
        .lock()
        .unwrap()
        .select_session(&outcome.result.session_id)
        .map_err(|error| {
            format!(
                "outcome unknown: action remained authoritative but client session selection failed: {error}"
            )
        })?;

    // Selecting the app's control client can resize panes. The caller still
    // holds the topology lock, so this one discovery is the atomic action +
    // selection result rather than a later competing reconciliation.
    let (snapshot, identity) = tokio::task::spawn_blocking(tmux_actions::discover_before_action)
        .await
        .map_err(|error| {
            format!("outcome unknown: client selection reconciliation task failed: {error}")
        })?
        .map_err(|error| {
            format!(
                "outcome unknown: client selection landed but its topology could not be reconciled: {error}"
            )
        })?;
    outcome.snapshot = snapshot;
    outcome.server_identity = identity;
    Ok(true)
}

fn topology_epoch_barrier(sender: &mpsc::Sender<SequencerControl>) -> Option<u64> {
    let (completion, completed) = std::sync::mpsc::sync_channel(1);
    sender
        .try_send(SequencerControl::TopologyEpochBarrier(completion))
        .ok()?;
    completed.recv_timeout(Duration::from_secs(2)).ok()
}

fn begin_after_input_barrier<T>(
    flush: impl FnOnce() -> anyhow::Result<()>,
    begin: impl FnOnce() -> T,
) -> anyhow::Result<T> {
    flush()?;
    Ok(begin())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_precheck_begins_once_and_only_after_input_flush() {
        let order = std::cell::RefCell::new(Vec::new());
        let result = begin_after_input_barrier(
            || {
                order.borrow_mut().push("flush");
                Ok(())
            },
            || {
                order.borrow_mut().push("discover");
                42
            },
        )
        .unwrap();
        assert_eq!(result, 42);
        assert_eq!(*order.borrow(), ["flush", "discover"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn topology_epoch_barrier_captures_prior_dirty_but_not_later_dirty() {
        let signal = super::super::super::topology::TopologySignal::default();
        let writer_signal = signal.clone();
        let (sender, mut receiver) = mpsc::channel(4);
        let writer = tokio::spawn(async move {
            while let Some(message) = receiver.recv().await {
                match message {
                    SequencerControl::TopologyEpochBarrier(completion) => {
                        let _ = completion.send(writer_signal.current_epoch());
                    }
                    message => writer_signal.observe_event(&message),
                }
            }
        });
        sender
            .send(SequencerControl::OrderedEvent(v1::HostEvent {
                kind: v1::EventKind::TopologyDirty.into(),
                ..Default::default()
            }))
            .await
            .unwrap();
        let barrier_sender = sender.clone();
        let covered = tokio::task::spawn_blocking(move || topology_epoch_barrier(&barrier_sender))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(covered, 1);
        signal.mark_dirty();
        assert_eq!(signal.current_epoch(), 2);
        drop(sender);
        writer.await.unwrap();
    }
}
