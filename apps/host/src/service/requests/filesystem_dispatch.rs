use super::*;

pub(super) fn handles(operation: v1::Operation) -> bool {
    matches!(
        operation,
        v1::Operation::ListDirectory
            | v1::Operation::WatchDirectory
            | v1::Operation::UnwatchDirectory
            | v1::Operation::ReadFile
            | v1::Operation::WriteFile
            | v1::Operation::FileMutation
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
            | v1::Operation::ReconcileTerminalUpload
    )
}

pub(super) struct FileDispatchContext<'a> {
    pub(super) control_tx: &'a mpsc::Sender<SequencerControl>,
    pub(super) event_tx: &'a mpsc::Sender<SequencerControl>,
    pub(super) pending: &'a Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    pub(super) files: &'a Arc<FileService>,
    pub(super) bulk_connection: bool,
}

/// Await blocking filesystem work while proving liveness on the bulk lane.
/// The desktop treats any well-formed frame as deadline activity, while the
/// request id remains reserved for the authoritative response.
async fn await_file_task<T: Send + 'static>(
    mut task: tokio::task::JoinHandle<T>,
    control_tx: &mpsc::Sender<SequencerControl>,
    operation_id: &str,
) -> Result<T, tokio::task::JoinError> {
    await_file_task_with_interval(&mut task, control_tx, operation_id, Duration::from_secs(2)).await
}

async fn await_file_task_with_interval<T: Send + 'static>(
    task: &mut tokio::task::JoinHandle<T>,
    control_tx: &mpsc::Sender<SequencerControl>,
    operation_id: &str,
    interval: Duration,
) -> Result<T, tokio::task::JoinError> {
    let mut heartbeat = tokio::time::interval(interval);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat.tick().await;
    loop {
        tokio::select! {
            result = &mut *task => return result,
            _ = heartbeat.tick() => {
                let _ = control_tx.try_send(SequencerControl::OrderedEvent(v1::HostEvent {
                    kind: v1::EventKind::TransferProgress.into(),
                    scope: operation_id.to_owned(),
                    detail: "filesystem operation active".into(),
                    file: Some(v1::FileServiceEvent {
                        operation_id: operation_id.to_owned(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }));
            }
        }
    }
}

fn upload_commit_failure_response(
    error: crate::service::filesystem::UploadCommitFailure,
) -> v1::Response {
    let message = error.to_string();
    let code = match (error.outcome, error.cleanup_failed) {
        (tmux_agent_protocol::PublicationOutcome::Published, _) => {
            "upload_commit_published_cleanup_failed"
        }
        (tmux_agent_protocol::PublicationOutcome::Unknown, _) => "upload_commit_outcome_unknown",
        (tmux_agent_protocol::PublicationOutcome::NotPublished, true) => {
            "upload_commit_not_published_cleanup_failed"
        }
        (tmux_agent_protocol::PublicationOutcome::NotPublished, false) => {
            "upload_commit_not_published"
        }
    };
    let mut response = response_error(code, &message);
    response.publication_outcome = match error.outcome {
        tmux_agent_protocol::PublicationOutcome::NotPublished => {
            v1::PublicationOutcome::NotPublished.into()
        }
        tmux_agent_protocol::PublicationOutcome::Published => {
            v1::PublicationOutcome::Published.into()
        }
        tmux_agent_protocol::PublicationOutcome::Unknown => v1::PublicationOutcome::Unknown.into(),
    };
    response.cleanup_failed = error.cleanup_failed;
    response
}

pub(super) async fn handle(
    request_id: u64,
    operation: v1::Operation,
    request: v1::Request,
    cancellation: Arc<AtomicBool>,
    context: FileDispatchContext<'_>,
) {
    let FileDispatchContext {
        control_tx,
        event_tx,
        pending,
        files,
        bulk_connection,
    } = context;
    match operation {
        v1::Operation::ListDirectory | v1::Operation::WatchDirectory => {
            let Some(file) = request.file else {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_file_request", "file request payload is required"),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            };
            let service = Arc::clone(files);
            if let Err(error) = validate_root_token(&file.root, &file.root_token) {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_root_token", &error.to_string()),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            }
            let watch = operation == v1::Operation::WatchDirectory;
            let root = file.root.clone();
            let root_token = file.root_token.clone();
            let path = file.path.clone();
            let watch_id = file.watch_id.clone();
            let page_token = file.page_token.clone();
            let page_size = file.page_size;
            let result = tokio::task::spawn_blocking(move || {
                if watch {
                    service.watch_directory_authorized(&root, &root_token, &path, &watch_id)
                } else {
                    service.list_directory_page_authorized(
                        &root,
                        &root_token,
                        &path,
                        &watch_id,
                        &page_token,
                        page_size,
                    )
                }
            })
            .await;
            let response = match result {
                Ok(Ok(snapshot)) => {
                    file_response(&file.operation_id, |value| value.directory = Some(snapshot))
                }
                Ok(Err(error)) => response_error("directory_rejected", &error.to_string()),
                Err(error) => response_error("directory_task_failed", &error.to_string()),
            };
            send_response(control_tx, request_id, response).await;
        }
        v1::Operation::UnwatchDirectory => {
            let Some(file) = request.file else {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_file_request", "file request payload is required"),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            };
            let response = files.unwatch_directory(&file.watch_id).map_or_else(
                |error| response_error("unwatch_rejected", &error.to_string()),
                |_| file_response(&file.operation_id, |_| {}),
            );
            send_response(control_tx, request_id, response).await;
        }
        v1::Operation::ReadFile => {
            let Some(file) = request.file else {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_file_request", "file request payload is required"),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            };
            let service = Arc::clone(files);
            if let Err(error) = validate_root_token(&file.root, &file.root_token) {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_root_token", &error.to_string()),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            }
            let root = file.root.clone();
            let path = file.path.clone();
            let root_token = file.root_token.clone();
            let result = tokio::task::spawn_blocking(move || {
                service.read_file_authorized(&root, &root_token, &path)
            })
            .await;
            let response = match result {
                Ok(Ok(content)) => {
                    file_response(&file.operation_id, |value| value.content = Some(content))
                }
                Ok(Err(error)) => response_error("file_read_rejected", &error.to_string()),
                Err(error) => response_error("file_read_task_failed", &error.to_string()),
            };
            send_response(control_tx, request_id, response).await;
        }
        v1::Operation::WriteFile => {
            send_response(
                control_tx,
                request_id,
                response_error(
                    "chunked_write_required",
                    "editor writes must use begin/chunk/commit on a bulk connection",
                ),
            )
            .await;
        }
        v1::Operation::FileMutation => {
            let Some(file) = request.file else {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_file_request", "file request payload is required"),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            };
            let service = Arc::clone(files);
            if let Err(error) = validate_root_token(&file.root, &file.root_token) {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_root_token", &error.to_string()),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            }
            let work = file.clone();
            let mutation_cancellation = Arc::clone(&cancellation);
            let result = tokio::task::spawn_blocking(move || {
                service.mutate_cancellable(&work, &mutation_cancellation)
            })
            .await;
            let response = match result {
                Ok(Ok(metadata)) => {
                    let deleted = v1::FileMutationKind::try_from(file.mutation).unwrap_or_default()
                        == v1::FileMutationKind::Delete;
                    broadcast_control_event(v1::HostEvent {
                        kind: v1::EventKind::FileChanged.into(),
                        scope: metadata.path.clone(),
                        file: Some(v1::FileServiceEvent {
                            operation_id: file.operation_id.clone(),
                            metadata: Some(metadata.clone()),
                            deleted,
                            root_token: file.root_token.clone(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    });
                    file_response(&file.operation_id, |value| {
                        value.metadata = Some(metadata);
                        value.deleted = deleted;
                    })
                }
                Ok(Err(error)) => {
                    let message = error.to_string();
                    let code = if message.contains("cancelled") {
                        "cancelled"
                    } else if message.contains("confirmation_required") {
                        "confirmation_required"
                    } else {
                        "file_mutation_rejected"
                    };
                    response_error(code, &message)
                }
                Err(error) => response_error("file_mutation_task_failed", &error.to_string()),
            };
            send_response(control_tx, request_id, response).await;
        }
        v1::Operation::StartDownload => {
            let Some(file) = request.file else {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_file_request", "file request payload is required"),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            };
            let service = Arc::clone(files);
            if let Err(error) = validate_root_token(&file.root, &file.root_token) {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_root_token", &error.to_string()),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            }
            let work = file.clone();
            let result = await_file_task(
                tokio::task::spawn_blocking(move || {
                    service.start_download_authorized(
                        &work.root,
                        &work.root_token,
                        &work.path,
                        work.folder,
                        &work.transfer_id,
                        work.file_generation,
                    )
                }),
                control_tx,
                &file.operation_id,
            )
            .await;
            let response = match result {
                Ok(Ok(download)) => {
                    file_response(&file.operation_id, |value| value.download = Some(download))
                }
                Ok(Err(error)) => response_error("download_preflight_rejected", &error.to_string()),
                Err(error) => response_error("download_preflight_task_failed", &error.to_string()),
            };
            send_response(control_tx, request_id, response).await;
        }
        v1::Operation::ReadDownloadChunk => {
            let Some(file) = request.file else {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_file_request", "file request payload is required"),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            };
            let service = Arc::clone(files);
            let transfer_id = file.transfer_id.clone();
            let result = await_file_task(
                tokio::task::spawn_blocking(move || {
                    service.read_download_chunk(&transfer_id, file.offset, file.chunk_bytes)
                }),
                control_tx,
                &file.operation_id,
            )
            .await;
            let response = match result {
                Ok(Ok(chunk)) => file_response(&file.operation_id, |value| {
                    value.transfer_chunk = Some(chunk)
                }),
                Ok(Err(error)) => response_error("download_read_rejected", &error.to_string()),
                Err(error) => response_error("download_read_task_failed", &error.to_string()),
            };
            send_response(control_tx, request_id, response).await;
        }
        v1::Operation::CancelDownload => {
            let Some(file) = request.file else {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_file_request", "file request payload is required"),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            };
            let service = Arc::clone(files);
            let transfer_id = file.transfer_id.clone();
            let result =
                tokio::task::spawn_blocking(move || service.cancel_download(&transfer_id)).await;
            let response = match result {
                Ok(Ok(())) => file_response(&file.operation_id, |_| {}),
                Ok(Err(error)) => response_error("download_cancel_rejected", &error.to_string()),
                Err(error) => response_error("download_cancel_task_failed", &error.to_string()),
            };
            send_response(control_tx, request_id, response).await;
        }
        v1::Operation::BeginFileWrite => {
            let Some(file) = request.file else {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_file_request", "file request payload is required"),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            };
            let response = if let Err(error) = validate_root_token(&file.root, &file.root_token) {
                response_error("invalid_root_token", &error.to_string())
            } else {
                let service = Arc::clone(files);
                let work = file.clone();
                match tokio::task::spawn_blocking(move || {
                    service.begin_file_write_authorized(
                        &work.root,
                        &work.root_token,
                        &work.path,
                        &work.transfer_id,
                        &work.operation_id,
                        work.total_bytes,
                        work.file_generation,
                    )
                })
                .await
                {
                    Ok(Ok(())) => file_response(&file.operation_id, |_| {}),
                    Ok(Err(error)) => {
                        response_error("file_write_preflight_rejected", &error.to_string())
                    }
                    Err(error) => {
                        response_error("file_write_preflight_task_failed", &error.to_string())
                    }
                }
            };
            send_response(control_tx, request_id, response).await;
        }
        v1::Operation::WriteFileChunk => {
            let Some(file) = request.file else {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_file_request", "file request payload is required"),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            };
            let service = Arc::clone(files);
            let work = file.clone();
            let result = tokio::task::spawn_blocking(move || {
                service.write_file_chunk(&work.transfer_id, work.offset, &work.content)
            })
            .await;
            let response = match result {
                Ok(Ok(next)) => file_response(&file.operation_id, |value| {
                    value.transfer_chunk = Some(v1::TransferChunk {
                        transfer_id: file.transfer_id.clone(),
                        offset: next,
                        total_bytes: file.total_bytes,
                        total_known: true,
                        ..Default::default()
                    })
                }),
                Ok(Err(error)) => response_error("file_write_chunk_rejected", &error.to_string()),
                Err(error) => response_error("file_write_chunk_task_failed", &error.to_string()),
            };
            send_response(control_tx, request_id, response).await;
        }
        v1::Operation::CommitFileWrite => {
            let Some(file) = request.file else {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_file_request", "file request payload is required"),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            };
            let service = Arc::clone(files);
            let work = file.clone();
            let result = tokio::task::spawn_blocking(move || {
                service.commit_file_write_authorized(&work.transfer_id, &work.blake3)
            })
            .await;
            let response = match result {
                Ok(Ok((metadata, root_token))) => {
                    let event = v1::HostEvent {
                        kind: v1::EventKind::FileChanged.into(),
                        scope: metadata.path.clone(),
                        file: Some(v1::FileServiceEvent {
                            operation_id: file.operation_id.clone(),
                            metadata: Some(metadata.clone()),
                            state: "committed".into(),
                            root_token,
                            ..Default::default()
                        }),
                        ..Default::default()
                    };
                    if bulk_connection {
                        broadcast_control_event(event);
                    } else {
                        let _ = event_tx.send(SequencerControl::OrderedEvent(event)).await;
                    }
                    file_response(&file.operation_id, |value| value.metadata = Some(metadata))
                }
                Ok(Err(error)) => response_error("file_write_commit_rejected", &error.to_string()),
                Err(error) => response_error("file_write_commit_task_failed", &error.to_string()),
            };
            send_response(control_tx, request_id, response).await;
        }
        v1::Operation::CancelFileWrite => {
            let Some(file) = request.file else {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_file_request", "file request payload is required"),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            };
            let service = Arc::clone(files);
            let transfer_id = file.transfer_id.clone();
            let result =
                tokio::task::spawn_blocking(move || service.cancel_file_write(&transfer_id)).await;
            let response = match result {
                Ok(Ok(())) => file_response(&file.operation_id, |_| {}),
                Ok(Err(error)) => response_error("file_write_cancel_rejected", &error.to_string()),
                Err(error) => response_error("file_write_cancel_task_failed", &error.to_string()),
            };
            send_response(control_tx, request_id, response).await;
        }
        v1::Operation::PrepareTerminalUpload => {
            let Some(file) = request.file else {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_file_request", "file request payload is required"),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            };
            let collision = v1::CollisionPolicy::try_from(file.collision_policy)
                .unwrap_or(v1::CollisionPolicy::Unspecified);
            let service = Arc::clone(files);
            let work = file.clone();
            let result = await_file_task(
                tokio::task::spawn_blocking(move || {
                    service.prepare_terminal_upload(
                        &work.transfer_id,
                        &work.destination,
                        work.total_bytes,
                        collision,
                        work.large_upload_confirmed,
                        work.image_png,
                    )
                }),
                control_tx,
                &file.operation_id,
            )
            .await;
            let response = match result {
                Ok(Ok(upload)) => {
                    file_response(&file.operation_id, |value| value.upload = Some(upload))
                }
                Ok(Err(error)) => response_error("upload_preflight_rejected", &error.to_string()),
                Err(error) => response_error("upload_preflight_task_failed", &error.to_string()),
            };
            send_response(control_tx, request_id, response).await;
        }
        v1::Operation::WriteTerminalUploadChunk => {
            let Some(file) = request.file else {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_file_request", "file request payload is required"),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            };
            let service = Arc::clone(files);
            let work = file.clone();
            let result = await_file_task(
                tokio::task::spawn_blocking(move || {
                    service.write_terminal_upload_chunk(
                        &work.transfer_id,
                        work.offset,
                        &work.content,
                    )
                }),
                control_tx,
                &file.operation_id,
            )
            .await;
            let response = match result {
                Ok(Ok(next)) => file_response(&file.operation_id, |value| {
                    value.transfer_chunk = Some(v1::TransferChunk {
                        transfer_id: file.transfer_id.clone(),
                        offset: next,
                        total_bytes: file.total_bytes,
                        total_known: true,
                        ..Default::default()
                    });
                }),
                Ok(Err(error)) => response_error("upload_chunk_rejected", &error.to_string()),
                Err(error) => response_error("upload_chunk_task_failed", &error.to_string()),
            };
            send_response(control_tx, request_id, response).await;
        }
        v1::Operation::CommitTerminalUpload => {
            let Some(file) = request.file else {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_file_request", "file request payload is required"),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            };
            let service = Arc::clone(files);
            let work = file.clone();
            let result = await_file_task(
                tokio::task::spawn_blocking(move || {
                    service.commit_terminal_upload(&work.transfer_id, &work.blake3)
                }),
                control_tx,
                &file.operation_id,
            )
            .await;
            let response = match result {
                Ok(Err(error)) => upload_commit_failure_response(error),
                Ok(Ok(upload)) => {
                    file_response(&file.operation_id, |value| value.upload = Some(upload))
                }
                Err(error) => response_error("upload_commit_task_failed", &error.to_string()),
            };
            send_response(control_tx, request_id, response).await;
        }
        v1::Operation::CancelTerminalUpload => {
            let Some(file) = request.file else {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_file_request", "file request payload is required"),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            };
            let service = Arc::clone(files);
            let transfer_id = file.transfer_id.clone();
            let result =
                tokio::task::spawn_blocking(move || service.cancel_terminal_upload(&transfer_id))
                    .await;
            let response = match result {
                Ok(Ok(cleanup_error)) => file_response(&file.operation_id, |value| {
                    value.upload = Some(v1::UploadDescriptor {
                        transfer_id: file.transfer_id.clone(),
                        cleanup_status: if cleanup_error.is_empty() {
                            v1::CleanupStatus::Removed.into()
                        } else {
                            v1::CleanupStatus::Failed.into()
                        },
                        cleanup_error,
                        ..Default::default()
                    });
                }),
                Ok(Err(error)) => response_error("upload_cancel_rejected", &error.to_string()),
                Err(error) => response_error("upload_cancel_task_failed", &error.to_string()),
            };
            send_response(control_tx, request_id, response).await;
        }
        v1::Operation::ReconcileTerminalUpload => {
            let Some(file) = request.file else {
                send_response(
                    control_tx,
                    request_id,
                    response_error("invalid_file_request", "file request payload is required"),
                )
                .await;
                pending.lock().unwrap().remove(&request_id);
                return;
            };
            let service = Arc::clone(files);
            let transfer_id = file.transfer_id.clone();
            let result = await_file_task(
                tokio::task::spawn_blocking(move || {
                    service.reconcile_terminal_upload(&transfer_id)
                }),
                control_tx,
                &file.operation_id,
            )
            .await;
            let response = match result {
                Ok(Ok(upload)) => {
                    file_response(&file.operation_id, |value| value.upload = Some(upload))
                }
                Ok(Err(error)) => response_error("upload_outcome_unavailable", &error.to_string()),
                Err(error) => response_error("upload_outcome_task_failed", &error.to_string()),
            };
            send_response(control_tx, request_id, response).await;
        }
        _ => unreachable!("non-file operation routed to filesystem dispatcher"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Condvar, Mutex};

    #[tokio::test(flavor = "current_thread")]
    async fn two_blocked_file_commits_do_not_starve_async_control_lane() {
        let gate = Arc::new((Mutex::new((0_u8, false)), Condvar::new()));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let gate = Arc::clone(&gate);
            workers.push(tokio::task::spawn_blocking(move || {
                let (lock, changed) = &*gate;
                let mut state = lock.lock().unwrap();
                state.0 += 1;
                changed.notify_all();
                while !state.1 {
                    state = changed.wait(state).unwrap();
                }
            }));
        }
        let started = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if gate.0.lock().unwrap().0 == 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(started.is_ok());
        let control_probe = tokio::time::timeout(std::time::Duration::from_millis(100), async {
            tokio::task::yield_now().await
        })
        .await;
        assert!(control_probe.is_ok());
        {
            let mut state = gate.0.lock().unwrap();
            state.1 = true;
            gate.1.notify_all();
        }
        for worker in workers {
            worker.await.unwrap();
        }
    }

    #[tokio::test]
    async fn live_blocking_filesystem_work_emits_deadline_heartbeats() {
        let (tx, mut rx) = mpsc::channel(4);
        let waiter = tokio::spawn(async move {
            let mut worker = tokio::task::spawn_blocking(|| {
                std::thread::sleep(Duration::from_millis(80));
                7_u8
            });
            await_file_task_with_interval(
                &mut worker,
                &tx,
                "heartbeat-test",
                Duration::from_millis(10),
            )
            .await
        });
        let event = tokio::time::timeout(Duration::from_millis(60), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let SequencerControl::OrderedEvent(event) = event else {
            panic!("heartbeat was not an ordered event");
        };
        assert_eq!(event.kind, v1::EventKind::TransferProgress as i32);
        assert_eq!(event.scope, "heartbeat-test");
        assert_eq!(waiter.await.unwrap().unwrap(), 7);
    }

    #[test]
    fn upload_commit_response_preserves_outcome_independently_from_failure_cause() {
        for (outcome, expected) in [
            (
                tmux_agent_protocol::PublicationOutcome::NotPublished,
                v1::PublicationOutcome::NotPublished,
            ),
            (
                tmux_agent_protocol::PublicationOutcome::Published,
                v1::PublicationOutcome::Published,
            ),
            (
                tmux_agent_protocol::PublicationOutcome::Unknown,
                v1::PublicationOutcome::Unknown,
            ),
        ] {
            let response =
                upload_commit_failure_response(crate::service::filesystem::UploadCommitFailure {
                    outcome,
                    cleanup_failed: true,
                    message: "same failure cause".into(),
                });
            assert_eq!(response.publication_outcome, expected as i32);
            assert!(response.cleanup_failed);
            assert_eq!(response.display_message, "same failure cause");
        }
    }
}
