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
            // Switch-timing instrumentation; delete with `timing.log`.
            let started = std::time::Instant::now();
            let (_topology_guard, mut known_generation) =
                lock_topology_generation(topology_lock, generation).await;
            let cached_baseline = topology_baseline.lock().unwrap().clone();
            // Resolving the baseline has to know the action kind: creating the
            // first session is allowed to run with no tmux server, and every
            // other action is not (M10-E060).
            let action_kind = v1::TmuxActionKind::try_from(action.kind).unwrap_or_default();
            // Switch-timing instrumentation; delete with `timing.log`. Held
            // apart from `action` because `execute` takes it by value.
            let timing_kind = format!("{action_kind:?}");
            let timing_session_id = action.session_id.clone();
            let timing_window_id = action.window_id.clone();
            // Set only when the precheck found the topology moved, so every
            // other line keeps the shape it had.
            let timing_topology_diff = Mutex::new(None::<&'static str>);
            let log_timing = |flush_discover: Duration,
                              execute: Duration,
                              barrier: Option<(Duration, bool)>,
                              queue_depth: usize,
                              outcome: &str| {
                crate::diagnostics::write_tmux_action_timing_log(
                    request_id,
                    &timing_kind,
                    &timing_session_id,
                    &timing_window_id,
                    flush_discover,
                    execute,
                    barrier,
                    started.elapsed(),
                    queue_depth,
                    outcome,
                    *timing_topology_diff.lock().unwrap(),
                );
            };
            let discover_started = std::time::Instant::now();
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
                    let depth = send_timed_response(
                        control_tx,
                        request_id,
                        response_error("terminal_input_flush_failed", &error.to_string()),
                    )
                    .await;
                    log_timing(
                        discover_started.elapsed(),
                        Duration::ZERO,
                        None,
                        depth,
                        "terminal_input_flush_failed",
                    );
                    pending.lock().unwrap().remove(&request_id);
                    return;
                }
            };
            let fresh = fresh_task.await;
            let flush_discover = discover_started.elapsed();
            let (fresh_snapshot, fresh_identity) = match fresh {
                Ok(Ok(value)) => value,
                Ok(Err(error)) => {
                    let depth = send_timed_response(
                        control_tx,
                        request_id,
                        response_error("tmux_action_rejected", &error.to_string()),
                    )
                    .await;
                    log_timing(
                        flush_discover,
                        Duration::ZERO,
                        None,
                        depth,
                        "tmux_action_rejected",
                    );
                    pending.lock().unwrap().remove(&request_id);
                    return;
                }
                Err(error) => {
                    let depth = send_timed_response(
                        control_tx,
                        request_id,
                        response_error("tmux_action_task_failed", &error.to_string()),
                    )
                    .await;
                    log_timing(
                        flush_discover,
                        Duration::ZERO,
                        None,
                        depth,
                        "tmux_action_task_failed",
                    );
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
                *timing_topology_diff.lock().unwrap() = Some(action_topology_diff(
                    cached_baseline.as_ref(),
                    &fresh_snapshot,
                    &fresh_identity,
                ));
                known_generation = publish_refreshed_baseline(
                    &fresh_snapshot,
                    &fresh_identity,
                    generation,
                    topology_baseline,
                    terminal,
                    event_tx,
                    overflowed,
                )
                .await;
                // A selection carries on against the topology just published.
                // Refusing it was the loop this fix exists to break: on a slow
                // link the resize each switch performs bumps the generation,
                // and the switch that would have fixed the screen was refused
                // for arriving before its own snapshot did. `execute` still
                // validates the target against this fresh snapshot, so a
                // selection of something that is gone fails as it always did.
                //
                // A *different server* is not that: nothing the caller named
                // exists on it, so it keeps refusing every kind, exactly as the
                // identity guard inside `execute` does.
                if !same_identity || !tmux_actions::selection_only(action_kind) {
                    let depth = send_timed_response(
                        control_tx,
                        request_id,
                        response_error(
                            "stale_topology",
                            "stale topology: external tmux structural mutation was reconciled before action",
                        ),
                    )
                    .await;
                    log_timing(
                        flush_discover,
                        Duration::ZERO,
                        None,
                        depth,
                        "stale_topology",
                    );
                    pending.lock().unwrap().remove(&request_id);
                    return;
                }
            }
            let barrier_sender = event_tx.clone();
            // Switch-timing instrumentation; delete with `timing.log`. The
            // barrier runs on the blocking pool inside `execute`, so its wait
            // is reported back rather than measured here.
            let barrier_report = Arc::new(Mutex::new(None::<(Duration, bool)>));
            let barrier_timing = Arc::clone(&barrier_report);
            let selection_terminal = Arc::clone(terminal);
            let selection_events = event_tx.clone();
            let selection_overflowed = Arc::clone(overflowed);
            let execute_started = std::time::Instant::now();
            let result = tokio::task::spawn_blocking(move || {
                tmux_actions::execute(
                    action,
                    known_generation,
                    fresh_snapshot,
                    fresh_identity,
                    move |kind, result| {
                        prepare_action_session_selection(
                            kind,
                            result,
                            &selection_terminal,
                            &selection_events,
                            &selection_overflowed,
                        )?;
                        let barrier_started = std::time::Instant::now();
                        let covered = topology_epoch_barrier(&barrier_sender);
                        *barrier_timing.lock().unwrap() =
                            Some((barrier_started.elapsed(), covered.is_none()));
                        Ok(covered)
                    },
                )
            })
            .await;
            let execute_elapsed = execute_started.elapsed();
            let barrier = *barrier_report.lock().unwrap();
            match result {
                Ok(Ok(mut outcome)) => {
                    let selection_required = action_selects_session(action_kind);
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
                        let depth = send_timed_response(
                            control_tx,
                            request_id,
                            success_response.take().expect("success response exists"),
                        )
                        .await;
                        log_timing(flush_discover, execute_elapsed, barrier, depth, "ok");
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
                        let depth = send_timed_response(control_tx, request_id, response).await;
                        log_timing(flush_discover, execute_elapsed, barrier, depth, "ok");
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
                    let depth =
                        send_timed_response(control_tx, request_id, response_error(code, &message))
                            .await;
                    log_timing(flush_discover, execute_elapsed, barrier, depth, code);
                }
                Err(error) => {
                    let depth = send_timed_response(
                        control_tx,
                        request_id,
                        response_error("tmux_action_task_failed", &error.to_string()),
                    )
                    .await;
                    log_timing(
                        flush_discover,
                        execute_elapsed,
                        barrier,
                        depth,
                        "tmux_action_task_failed",
                    );
                }
            }
        }
        _ => unreachable!(),
    }
}

/// Adopts a topology that moved under the app as the new baseline: bumps the
/// generation, reconciles the control clients, and puts the authoritative
/// snapshot on the ordered sequencer. Returns the generation it published,
/// which is the one any action that continues from here runs against.
#[allow(clippy::too_many_arguments)]
async fn publish_refreshed_baseline(
    fresh_snapshot: &tmux_control::TmuxSnapshot,
    fresh_identity: &str,
    generation: &Arc<AtomicU64>,
    topology_baseline: &Arc<Mutex<Option<(tmux_control::TmuxSnapshot, String)>>>,
    terminal: &Arc<Mutex<TerminalClients>>,
    event_tx: &mpsc::Sender<SequencerControl>,
    overflowed: &Arc<AtomicBool>,
) -> u64 {
    let next_generation = generation.fetch_add(1, Ordering::AcqRel) + 1;
    *topology_baseline.lock().unwrap() = Some((fresh_snapshot.clone(), fresh_identity.to_owned()));
    reconcile_terminal_clients(terminal, fresh_snapshot, event_tx, overflowed);
    let snapshot = snapshot_from_identity(
        fresh_snapshot.clone(),
        next_generation,
        fresh_identity.to_owned(),
    );
    let _ = event_tx
        .send(SequencerControl::OrderedEvent(v1::HostEvent {
            kind: v1::EventKind::TopologySnapshot.into(),
            scope: "topology".into(),
            snapshot: Some(snapshot),
            detail: "external topology mutation observed before action".into(),
            ..Default::default()
        }))
        .await;
    next_generation
}

/// Switch-timing instrumentation; delete with `timing.log`.
///
/// Notes the enqueue instant for the writer task to join against, and returns
/// the sequencer queue depth this response was put behind. Ordinary
/// `send_response` in every other dispatcher stays untimed.
async fn send_timed_response(
    control_tx: &mpsc::Sender<SequencerControl>,
    request_id: u64,
    response: v1::Response,
) -> usize {
    let depth = control_tx
        .max_capacity()
        .saturating_sub(control_tx.capacity());
    crate::diagnostics::note_response_enqueued(request_id);
    send_response(control_tx, request_id, response).await;
    depth
}

fn action_selects_session(action_kind: v1::TmuxActionKind) -> bool {
    matches!(
        action_kind,
        v1::TmuxActionKind::SelectSession
            | v1::TmuxActionKind::CreateSession
            | v1::TmuxActionKind::CreateWindow
    )
}

/// Completes selection before the action's one authoritative postcheck.
/// Create-session already returned its exact pane identity, so its control
/// client can be attached without a discovery solely for attachment.
fn prepare_action_session_selection(
    action_kind: v1::TmuxActionKind,
    result: &v1::TmuxActionResult,
    terminal: &Arc<Mutex<TerminalClients>>,
    event_tx: &mpsc::Sender<SequencerControl>,
    overflowed: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    if !action_selects_session(action_kind) {
        return Ok(());
    }
    let mut terminal = terminal.lock().unwrap();
    if action_kind == v1::TmuxActionKind::CreateSession {
        if result.session_id.is_empty() || result.pane_id.is_empty() {
            anyhow::bail!("outcome unknown: create-session returned no attachable pane identity");
        }
        terminal
            .attach(
                &result.session_id,
                std::slice::from_ref(&result.pane_id),
                true,
                event_tx.clone(),
                Arc::clone(overflowed),
            )
            .map_err(|error| {
                anyhow::anyhow!(
                    "outcome unknown: created session control client could not attach/select: {error}"
                )
            })?;
    } else {
        terminal.select_session(&result.session_id).map_err(|error| {
            anyhow::anyhow!(
                "outcome unknown: action remained authoritative but client session selection failed: {error}"
            )
        })?;
    }
    Ok(())
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
    use crate::service::OutputCredit;

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

    #[test]
    fn fresh_server_create_session_reaches_discovery_without_a_sidecar() {
        let terminal = Arc::new(Mutex::new(TerminalClients::new(
            Arc::new(OutputCredit::negotiated(false)),
            crate::service::topology_output_trigger::TopologyOutputTrigger::default(),
        )));
        let action = v1::TmuxActionKind::CreateSession;
        let reached =
            begin_after_input_barrier(|| terminal.lock().unwrap().flush_input(), || action)
                .unwrap();
        assert_eq!(reached, v1::TmuxActionKind::CreateSession);
    }

    #[test]
    fn only_session_owning_actions_select_before_the_postcheck() {
        assert!(action_selects_session(v1::TmuxActionKind::SelectSession));
        assert!(action_selects_session(v1::TmuxActionKind::CreateSession));
        assert!(action_selects_session(v1::TmuxActionKind::CreateWindow));
        assert!(!action_selects_session(v1::TmuxActionKind::SelectWindow));
    }

    /// The precheck's two halves are separable on purpose: a topology that moved
    /// is always adopted and always announced, and only the refusal that used to
    /// follow it is now limited to the kinds a stale generation says something
    /// about. A selection continues into `execute` against the snapshot this
    /// just published, which is also the generation it is measured against.
    #[tokio::test]
    async fn a_refreshed_baseline_is_announced_then_only_non_selections_are_refused() {
        let (event_tx, mut events) = mpsc::channel(4);
        let generation = Arc::new(AtomicU64::new(7));
        let baseline = Arc::new(Mutex::new(Some((
            tmux_control::TmuxSnapshot::default(),
            "tmux:live".to_owned(),
        ))));
        let terminal = Arc::new(Mutex::new(TerminalClients::new(
            Arc::new(OutputCredit::negotiated(false)),
            crate::service::topology_output_trigger::TopologyOutputTrigger::default(),
        )));
        let overflowed = Arc::new(AtomicBool::new(false));
        // Sessions without panes: the reconciliation has nothing to attach, so
        // the test never forks a tmux control client.
        let fresh = tmux_control::TmuxSnapshot {
            sessions: vec![tmux_control::Session {
                id: "$1".into(),
                name: "moved".into(),
                window_count: 1,
                attached_clients: 0,
                order: 0,
                pinned: false,
            }],
            ..Default::default()
        };

        let published = publish_refreshed_baseline(
            &fresh,
            "tmux:live",
            &generation,
            &baseline,
            &terminal,
            &event_tx,
            &overflowed,
        )
        .await;

        assert_eq!(published, 8);
        assert_eq!(generation.load(Ordering::Acquire), 8);
        assert_eq!(
            *baseline.lock().unwrap(),
            Some((fresh.clone(), "tmux:live".to_owned()))
        );
        let Some(SequencerControl::OrderedEvent(event)) = events.recv().await else {
            panic!("the refreshed topology was not announced");
        };
        assert_eq!(event.kind, i32::from(v1::EventKind::TopologySnapshot));
        assert_eq!(
            event.detail,
            "external topology mutation observed before action"
        );
        assert_eq!(event.snapshot.as_ref().unwrap().generation, 8);

        assert!(tmux_actions::selection_only(
            v1::TmuxActionKind::SelectSession
        ));
        assert!(!tmux_actions::selection_only(
            v1::TmuxActionKind::CloseSession
        ));
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
