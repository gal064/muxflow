use super::*;

pub(crate) async fn handle_request(
    request_id: u64,
    request: v1::Request,
    policy: super::operation_policy::OperationPolicy,
    cancellation: Arc<AtomicBool>,
    context: RequestContext,
) {
    let RequestContext {
        control_tx,
        event_tx,
        generation,
        overflowed,
        subscribed,
        pending,
        terminal,
        topology_lock,
        topology_baseline,
        topology_signal,
        files,
        git,
        bulk_connection,
        connection_epoch,
        perf_connection_epoch,
        connection_id,
        closed,
    } = context;
    let control_tx = &control_tx;
    let event_tx = &event_tx;
    let generation = &generation;
    let overflowed = &overflowed;
    let subscribed = &subscribed;
    let pending = &pending;
    let terminal = &terminal;
    let topology_lock = &topology_lock;
    let topology_baseline = &topology_baseline;
    let topology_signal = &topology_signal;
    let files = &files;
    let git = &git;
    if closed.load(Ordering::Acquire) || cancellation.load(Ordering::Acquire) {
        send_response(
            control_tx,
            request_id,
            response_error("cancelled", "request was cancelled before execution"),
        )
        .await;
        pending.lock().unwrap().remove(&request_id);
        return;
    }
    use super::operation_policy::{Handler, OperationValue};

    let operation = policy.known_operation();
    if matches!(
        operation,
        Some(v1::Operation::FullSnapshot | v1::Operation::Subscribe | v1::Operation::Resync)
    ) && !matches!(request.scope.as_str(), "" | "full" | "topology")
    {
        send_response(
            control_tx,
            request_id,
            response_error(
                "invalid_snapshot_scope",
                "snapshot scope must be full or topology",
            ),
        )
        .await;
        pending.lock().unwrap().remove(&request_id);
        return;
    }
    match (policy.handler, operation) {
        (Handler::Filesystem, Some(operation)) => {
            super::filesystem_dispatch::handle(
                request_id,
                operation,
                request,
                Arc::clone(&cancellation),
                super::filesystem_dispatch::FileDispatchContext {
                    control_tx,
                    event_tx,
                    files,
                    bulk_connection,
                },
            )
            .await;
            pending.lock().unwrap().remove(&request_id);
            return;
        }

        (Handler::Git, Some(operation)) => {
            super::git_dispatch::handle(
                request_id,
                operation,
                request,
                Arc::clone(&cancellation),
                super::git_dispatch::GitDispatchContext {
                    control_tx,
                    event_tx,
                    git,
                    connection_epoch,
                },
            )
            .await;
            pending.lock().unwrap().remove(&request_id);
            return;
        }

        (Handler::Agent, Some(operation)) => {
            super::agent_dispatch::handle(
                request_id,
                operation,
                request,
                control_tx,
                generation,
                topology_baseline,
                &cancellation,
            )
            .await;
            pending.lock().unwrap().remove(&request_id);
            return;
        }

        (Handler::Voice, Some(operation)) => {
            let active_server_identity = topology_baseline
                .lock()
                .unwrap()
                .as_ref()
                .map(|(_, identity)| identity.clone())
                .unwrap_or_default();
            super::voice_dispatch::handle(
                request_id,
                operation,
                request,
                control_tx,
                connection_id,
                &active_server_identity,
                &cancellation,
                &closed,
            )
            .await;
            pending.lock().unwrap().remove(&request_id);
            return;
        }

        (
            Handler::ActiveRoot,
            Some(v1::Operation::ResolveActiveRoot | v1::Operation::ResolveTerminalFile),
        ) => {
            super::active_root_dispatch::handle(
                request_id,
                request,
                Arc::clone(&cancellation),
                super::active_root_dispatch::ActiveRootContext {
                    control_tx,
                    event_tx,
                    generation,
                    topology_lock,
                },
            )
            .await;
            pending.lock().unwrap().remove(&request_id);
            return;
        }

        (Handler::TmuxAction, Some(v1::Operation::TmuxAction)) => {
            super::tmux_action_dispatch::handle(
                request_id,
                request,
                super::tmux_action_dispatch::TmuxActionContext {
                    control_tx,
                    event_tx,
                    generation,
                    overflowed,
                    pending,
                    terminal,
                    topology_lock,
                    topology_baseline,
                    topology_signal,
                    connection_epoch: perf_connection_epoch,
                },
            )
            .await;
            pending.lock().unwrap().remove(&request_id);
            return;
        }

        (Handler::Snapshot, Some(v1::Operation::FullSnapshot | v1::Operation::Resync)) => {
            spawn_snapshot_request(SnapshotRequest {
                request_id,
                subscribe_after: false,
                sender: control_tx.clone(),
                generation: Arc::clone(generation),
                cancellation,
                pending: Arc::clone(pending),
                subscribed: Arc::clone(subscribed),
                topology_lock: Arc::clone(topology_lock),
                topology_baseline: Arc::clone(topology_baseline),
                terminal: Arc::clone(terminal),
                event_sender: event_tx.clone(),
                overflowed: Arc::clone(overflowed),
            });
            return;
        }
        (Handler::Snapshot, Some(v1::Operation::Subscribe)) => {
            spawn_snapshot_request(SnapshotRequest {
                request_id,
                subscribe_after: true,
                sender: control_tx.clone(),
                generation: Arc::clone(generation),
                cancellation,
                pending: Arc::clone(pending),
                subscribed: Arc::clone(subscribed),
                topology_lock: Arc::clone(topology_lock),
                topology_baseline: Arc::clone(topology_baseline),
                terminal: Arc::clone(terminal),
                event_sender: event_tx.clone(),
                overflowed: Arc::clone(overflowed),
            });
            return;
        }
        (Handler::Terminal, Some(v1::Operation::AttachTerminal)) => {
            let mut result = terminal.lock().unwrap().attach(
                &request.session_id,
                &request.pane_ids,
                true,
                event_tx.clone(),
                Arc::clone(overflowed),
            );
            if result.is_ok() {
                result = reconcile_internal_tmux_change(
                    topology_lock,
                    topology_baseline,
                    generation,
                    terminal,
                    event_tx,
                    overflowed,
                )
                .await;
            }
            send_response(
                control_tx,
                request_id,
                result.map_or_else(
                    |error| response_error("terminal_attach_failed", &error.to_string()),
                    |_| response_ok(),
                ),
            )
            .await;
        }
        (Handler::Terminal, Some(v1::Operation::SelectTerminalSession)) => {
            // Handled exactly like `ResizeTerminal` below, because on the path
            // that matters it *is* one: a client becoming visible for the first
            // time is given a size, and that is the same `refresh-client -C`
            // write. So it takes the same input barrier — a resize is ordered
            // behind every queued keystroke, or the panes reflow underneath
            // bytes the user typed before the switch — and the same
            // reconciliation, or the topology baseline still describes the
            // geometry from before the reflow and the next tmux action is
            // refused as stale.
            let mut result = {
                let mut terminal = terminal.lock().unwrap();
                terminal
                    .flush_input()
                    .and_then(|()| terminal.select_session(&request.session_id))
            };
            if result.is_ok() {
                result = reconcile_internal_tmux_change(
                    topology_lock,
                    topology_baseline,
                    generation,
                    terminal,
                    event_tx,
                    overflowed,
                )
                .await;
            }
            send_response(
                control_tx,
                request_id,
                result.map_or_else(
                    |error| response_error("terminal_selection_failed", &error.to_string()),
                    |_| response_ok(),
                ),
            )
            .await;
        }
        (Handler::Terminal, Some(v1::Operation::TerminalInput)) => {
            // Enqueue and answer; do not wait for tmux to accept the bytes.
            //
            // Enqueueing is ordered and cannot block, so keystroke order is
            // still exactly the order they arrived in. Waiting here, by
            // contrast, held the connection's read loop for the whole round
            // trip: only one keystroke could be in flight at a time and nothing
            // else on the connection — a snapshot, a tmux action, a resize —
            // could be served while it was. The commit point callers actually
            // depend on is the input barrier that every tmux action and resize
            // already takes before it runs.
            let delivery = if request.terminal_input_paste {
                super::super::terminal::InputDelivery::Paste
            } else {
                super::super::terminal::InputDelivery::Keys
            };
            let timing = crate::diagnostics::HostInputTiming::begin(
                request_id,
                perf_connection_epoch,
                &request.scope,
                request.data.len(),
            );
            let guarded = request.terminal_input_voice;
            let submission = matches!(request.data.as_slice(), b"\r" | b"\n");
            let (input_server_identity, target_pane) = if guarded || submission {
                topology_baseline
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|(snapshot, identity)| {
                        (
                            identity.clone(),
                            snapshot
                                .panes
                                .iter()
                                .find(|pane| pane.id == request.scope)
                                .cloned(),
                        )
                    })
                    .unwrap_or_default()
            } else {
                Default::default()
            };
            let result = if guarded
                && request.terminal_input_expected_server_identity != input_server_identity
            {
                Err(anyhow::anyhow!(
                    "voice session belongs to a different tmux server"
                ))
            } else if guarded {
                // A current process-tree observation is the only reliable way
                // to distinguish the agent from its surviving shell. Keep its
                // filesystem/process queries off the async service thread.
                let process_present = if let Some(pane) = target_pane {
                    tokio::task::spawn_blocking(move || {
                        super::super::agents::pane_has_supported_process(&pane)
                    })
                    .await
                    .unwrap_or(false)
                } else {
                    false
                };
                super::super::agents::AgentRuntime::global().with_valid_input_pane(
                    &input_server_identity,
                    &request.scope,
                    process_present,
                    submission,
                    || {
                        let mut terminal = terminal.lock().unwrap();
                        terminal.send_input(&request.scope, &request.data, delivery, timing)?;
                        // Generic terminal typing is acknowledged after queue
                        // admission. Guarded voice input fences the queue while
                        // the agent record is locked, so retirement cannot be
                        // confirmed between validation and the tmux commit.
                        terminal.flush_input()
                    },
                )
            } else if submission {
                super::super::agents::AgentRuntime::global().with_input_diagnostic(
                    &input_server_identity,
                    &request.scope,
                    || {
                        terminal.lock().unwrap().send_input(
                            &request.scope,
                            &request.data,
                            delivery,
                            timing,
                        )
                    },
                )
            } else {
                terminal
                    .lock()
                    .unwrap()
                    .send_input(&request.scope, &request.data, delivery, timing)
            };
            // A malformed scope has no pane to recover, and an unscoped
            // resnapshot event would escalate to a whole-connection reconnect.
            if !guarded
                && let Err(error) = &result
                && super::super::terminal::validate_tmux_id(&request.scope, '%').is_ok()
            {
                // The desktop no longer awaits this response on the keystroke
                // path, so the event stream is the only channel that still
                // reaches the user. Scope the recovery to the pane: bytes the
                // user typed did not reach it, so its screen no longer reflects
                // what they think they sent.
                emit_event(
                    event_tx,
                    overflowed,
                    v1::HostEvent {
                        kind: v1::EventKind::TerminalResnapshotRequired.into(),
                        scope: request.scope.clone(),
                        detail: format!("terminal input was not delivered: {error}"),
                        ..Default::default()
                    },
                );
            }
            send_response(
                control_tx,
                request_id,
                result.map_or_else(
                    |error| response_error("terminal_input_rejected", &error.to_string()),
                    |_| response_ok(),
                ),
            )
            .await;
        }
        (Handler::Terminal, Some(v1::Operation::ResizeTerminal)) => {
            let mut result = {
                let mut terminal = terminal.lock().unwrap();
                terminal
                    .flush_input()
                    .and_then(|()| terminal.resize(request.columns, request.rows))
            };
            if result.is_ok() {
                result = reconcile_internal_tmux_change(
                    topology_lock,
                    topology_baseline,
                    generation,
                    terminal,
                    event_tx,
                    overflowed,
                )
                .await;
            }
            send_response(
                control_tx,
                request_id,
                result.map_or_else(
                    |error| response_error("terminal_resize_rejected", &error.to_string()),
                    |_| response_ok(),
                ),
            )
            .await;
        }
        (Handler::Terminal, Some(v1::Operation::YieldTerminalSizing)) => {
            let mut result = {
                let mut terminal = terminal.lock().unwrap();
                terminal.yield_sizing()
            };
            if result.is_ok() {
                result = reconcile_internal_tmux_change(
                    topology_lock,
                    topology_baseline,
                    generation,
                    terminal,
                    event_tx,
                    overflowed,
                )
                .await;
            }
            send_response(
                control_tx,
                request_id,
                result.map_or_else(
                    |error| response_error("terminal_sizing_yield_rejected", &error.to_string()),
                    |_| response_ok(),
                ),
            )
            .await;
        }
        (Handler::Terminal, Some(v1::Operation::SetTerminalVisibility)) => {
            // Reserve sequencer capacity before entering the blocking section.
            // Waiting here holds no terminal/resource lock, and the connection
            // frame reader remains free to consume delivery acknowledgements.
            let event_permit = match event_tx.clone().reserve_owned().await {
                Ok(permit) => permit,
                Err(_) => {
                    send_response(
                        control_tx,
                        request_id,
                        response_error(
                            "terminal_visibility_rejected",
                            "terminal event sequencer is closed",
                        ),
                    )
                    .await;
                    pending.lock().unwrap().remove(&request_id);
                    return;
                }
            };
            let terminal = Arc::clone(terminal);
            let event_tx = event_tx.clone();
            let overflowed = Arc::clone(overflowed);
            let result = tokio::task::spawn_blocking(move || {
                terminal.lock().unwrap().set_visibility(
                    &request.scope,
                    VisibilityChange {
                        visible: request.visible,
                        renderer_holds_snapshot: request.terminal_renderer_holds_snapshot,
                        checkpoint: tmux_control::VisibilityCheckpoint {
                            epoch: request.terminal_epoch,
                            generation: request.terminal_generation_cutoff,
                        },
                    },
                    event_permit,
                    &event_tx,
                    &overflowed,
                )
            })
            .await
            .unwrap_or_else(|error| {
                Err(anyhow::anyhow!(
                    "terminal visibility worker failed: {error}"
                ))
            });
            send_response(
                control_tx,
                request_id,
                result.map_or_else(
                    |error| response_error("terminal_visibility_rejected", &error.to_string()),
                    |_| response_ok(),
                ),
            )
            .await;
        }
        (Handler::Terminal, Some(v1::Operation::RequestTerminalSeed)) => {
            // The desktop asks for a seed only for a pane it is rendering, so
            // this is also the authoritative statement that the pane is
            // visible; `request_seed_for_render` makes the host's resource
            // agree before the capture it queues can be suppressed for
            // disagreeing.
            let result = terminal.lock().unwrap().request_seed_for_render(
                &request.scope,
                event_tx,
                overflowed,
            );
            send_response(
                control_tx,
                request_id,
                result.map_or_else(
                    |error| response_error("terminal_seed_rejected", &error.to_string()),
                    |_| response_ok(),
                ),
            )
            .await;
        }
        (Handler::Terminal, Some(v1::Operation::RequestTerminalHistory)) => {
            // Nothing about the pane's state changes here, so — unlike a seed
            // request — this makes no claim of visibility and settles no debt.
            // It writes one capture and the answer arrives as its own event.
            let result = terminal.lock().unwrap().request_history(
                &request.scope,
                request.terminal_history_lines,
                request.terminal_history_skip_lines,
            );
            send_response(
                control_tx,
                request_id,
                result.map_or_else(
                    |error| response_error("terminal_history_rejected", &error.to_string()),
                    |_| response_ok(),
                ),
            )
            .await;
        }
        (Handler::Test, Some(v1::Operation::TestDelay)) if testing_enabled() => {
            let sender = control_tx.clone();
            let pending = Arc::clone(pending);
            tokio::spawn(async move {
                let mut remaining = request.delay_millis.min(30_000);
                while remaining > 0 && !cancellation.load(Ordering::Acquire) {
                    let step = remaining.min(10);
                    sleep(Duration::from_millis(step)).await;
                    remaining -= step;
                }
                let response = if cancellation.load(Ordering::Acquire) {
                    response_error("cancelled", "request was cancelled")
                } else {
                    response_ok()
                };
                send_response(&sender, request_id, response).await;
                pending.lock().unwrap().remove(&request_id);
            });
            return;
        }
        (Handler::Test, Some(v1::Operation::TestInjectGap)) if testing_enabled() => {
            let _ = control_tx
                .send(SequencerControl::InjectGap(v1::HostEvent {
                    kind: v1::EventKind::TopologyDirty.into(),
                    scope: "topology".into(),
                    detail: "injected sequence gap".into(),
                    ..Default::default()
                }))
                .await;
            send_response(control_tx, request_id, response_ok()).await;
        }
        (Handler::Test, Some(v1::Operation::TestOverflow)) if testing_enabled() => {
            for index in 0..(EVENT_QUEUE * 8) {
                emit_event(
                    event_tx,
                    overflowed,
                    v1::HostEvent {
                        kind: v1::EventKind::TopologyDirty.into(),
                        scope: "topology".into(),
                        detail: format!("overflow-{index}"),
                        ..Default::default()
                    },
                );
            }
            send_response(control_tx, request_id, response_ok()).await;
        }
        (Handler::Unsupported | Handler::Test, _) => {
            let message = match policy.operation {
                OperationValue::Unknown(raw) => {
                    format!("operation number {raw} is not recognized by this host")
                }
                OperationValue::Known(_) => "operation is not supported".to_owned(),
            };
            send_response(
                control_tx,
                request_id,
                response_error("unsupported_operation", &message),
            )
            .await;
        }
        (Handler::Daemon, _) => {
            unreachable!("daemon shutdown is handled before request registration")
        }
        _ => unreachable!("operation policy handler does not match its generated operation"),
    }
    pending.lock().unwrap().remove(&request_id);
}
