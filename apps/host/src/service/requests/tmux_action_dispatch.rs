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
            let fresh =
                tokio::task::spawn_blocking(move || tmux_actions::discover_for_action(action_kind))
                    .await;
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
            let flush_result = { terminal.lock().unwrap().flush_input() };
            if let Err(error) = flush_result {
                send_response(
                    control_tx,
                    request_id,
                    response_error("terminal_input_flush_failed", &error.to_string()),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            }
            let result = tokio::task::spawn_blocking(move || {
                tmux_actions::execute(action, known_generation, fresh_snapshot, fresh_identity)
            })
            .await;
            match result {
                Ok(Ok(mut outcome)) => {
                    let next_generation = generation.fetch_add(1, Ordering::AcqRel) + 1;
                    outcome.result.topology_generation = next_generation;
                    *topology_baseline.lock().unwrap() =
                        Some((outcome.snapshot.clone(), outcome.server_identity.clone()));
                    reconcile_terminal_clients(terminal, &outcome.snapshot, event_tx, overflowed);
                    let snapshot = snapshot_from_identity(
                        outcome.snapshot,
                        next_generation,
                        outcome.server_identity,
                    );
                    let _ = event_tx
                        .send(SequencerControl::OrderedEvent(v1::HostEvent {
                            kind: v1::EventKind::TopologySnapshot.into(),
                            scope: "topology".into(),
                            snapshot: Some(snapshot),
                            ..Default::default()
                        }))
                        .await;
                    let selection_error = (action_kind == v1::TmuxActionKind::SelectSession)
                        .then(|| {
                            terminal
                                .lock()
                                .unwrap()
                                .select_session(&outcome.result.session_id)
                        })
                        .and_then(Result::err);
                    if let Some(error) = selection_error {
                        send_response(
                            control_tx,
                            request_id,
                            response_error(
                                "outcome_unknown",
                                &format!(
                                    "outcome unknown: selected session remained authoritative but client selection failed: {error}"
                                ),
                            ),
                        )
                        .await;
                    } else {
                        send_response(
                            control_tx,
                            request_id,
                            v1::Response {
                                ok: true,
                                tmux_action_result: Some(outcome.result),
                                ..Default::default()
                            },
                        )
                        .await;
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
