use super::*;

pub(crate) async fn handle_request(
    request_id: u64,
    request: v1::Request,
    read_only: bool,
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
    let operation = v1::Operation::try_from(request.operation).unwrap_or_default();
    if matches!(
        operation,
        v1::Operation::FullSnapshot | v1::Operation::Subscribe | v1::Operation::Resync
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
    let is_mutation = matches!(
        operation,
        v1::Operation::AttachTerminal
            | v1::Operation::SelectTerminalSession
            | v1::Operation::TerminalInput
            | v1::Operation::ResizeTerminal
            | v1::Operation::TmuxAction
            | v1::Operation::SetTerminalVisibility
            | v1::Operation::RequestTerminalSeed
            | v1::Operation::WatchDirectory
            | v1::Operation::UnwatchDirectory
            | v1::Operation::FileMutation
            | v1::Operation::WriteFile
            | v1::Operation::StartDownload
            | v1::Operation::ReadDownloadChunk
            | v1::Operation::CancelDownload
            | v1::Operation::BeginFileWrite
            | v1::Operation::WriteFileChunk
            | v1::Operation::CommitFileWrite
            | v1::Operation::CancelFileWrite
            | v1::Operation::PrepareTerminalUpload
            | v1::Operation::WriteTerminalUploadChunk
            | v1::Operation::CommitTerminalUpload
            | v1::Operation::CancelTerminalUpload
            | v1::Operation::WatchGit
            | v1::Operation::UnwatchGit
            | v1::Operation::PrepareGitDiscard
            | v1::Operation::GitMutation
            | v1::Operation::GitCommit
            | v1::Operation::AgentAction
            | v1::Operation::AgentMarkSeen
            | v1::Operation::AgentHookIngest
            | v1::Operation::AgentHookManagement
    );
    if read_only && is_mutation {
        send_response(
            control_tx,
            request_id,
            response_error(
                "helper_incompatible",
                "host is read-only until the helper is upgraded",
            ),
        )
        .await;
        pending.lock().unwrap().remove(&request_id);
        return;
    }
    if !bulk_connection && file_ops::requires_bulk_connection(operation) {
        send_response(
            control_tx,
            request_id,
            response_error(
                "bulk_connection_required",
                "file bodies are allowed only on an independent bulk connection",
            ),
        )
        .await;
        pending.lock().unwrap().remove(&request_id);
        return;
    }

    if super::filesystem_dispatch::handles(operation) {
        super::filesystem_dispatch::handle(
            request_id,
            operation,
            request,
            Arc::clone(&cancellation),
            super::filesystem_dispatch::FileDispatchContext {
                control_tx,
                event_tx,
                pending,
                files,
                bulk_connection,
            },
        )
        .await;
        pending.lock().unwrap().remove(&request_id);
        return;
    }

    if super::git_dispatch::handles(operation) {
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
                closed: &closed,
            },
        )
        .await;
        pending.lock().unwrap().remove(&request_id);
        return;
    }

    if super::agent_dispatch::handles(operation) {
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

    if operation == v1::Operation::ResolveActiveRoot {
        super::active_root_dispatch::handle(
            request_id,
            request,
            Arc::clone(&cancellation),
            super::active_root_dispatch::ActiveRootContext {
                control_tx,
                event_tx,
                generation,
                pending,
                topology_lock,
                files,
            },
        )
        .await;
        pending.lock().unwrap().remove(&request_id);
        return;
    }

    if operation == v1::Operation::TmuxAction {
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
            },
        )
        .await;
        pending.lock().unwrap().remove(&request_id);
        return;
    }

    match operation {
        v1::Operation::FullSnapshot | v1::Operation::Resync => {
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
        v1::Operation::Subscribe => {
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
        v1::Operation::AttachTerminal => {
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
        v1::Operation::SelectTerminalSession => {
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
        v1::Operation::TerminalInput => {
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
            let result = terminal
                .lock()
                .unwrap()
                .send_input(&request.scope, &request.data);
            // A malformed scope has no pane to recover, and an unscoped
            // resnapshot event would escalate to a whole-connection reconnect.
            if let Err(error) = &result
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
        v1::Operation::ResizeTerminal => {
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
        v1::Operation::SetTerminalVisibility => {
            let result = terminal.lock().unwrap().set_visibility(
                &request.scope,
                VisibilityChange {
                    visible: request.visible,
                    serialized_snapshot: request.data,
                    checkpoint: tmux_control::VisibilityCheckpoint {
                        epoch: request.terminal_epoch,
                        generation: request.terminal_generation_cutoff,
                    },
                },
                event_tx,
                overflowed,
            );
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
        v1::Operation::RequestTerminalSeed => {
            let result = terminal.lock().unwrap().request_seed(&request.scope);
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
        v1::Operation::TestDelay if testing_enabled() => {
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
        v1::Operation::TestInjectGap if testing_enabled() => {
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
        v1::Operation::TestOverflow if testing_enabled() => {
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
        _ => {
            send_response(
                control_tx,
                request_id,
                response_error("unsupported_operation", "operation is not supported"),
            )
            .await;
        }
    }
    pending.lock().unwrap().remove(&request_id);
}
