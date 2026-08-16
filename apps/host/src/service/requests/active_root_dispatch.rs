use super::*;

pub(super) struct ActiveRootContext<'a> {
    pub(super) control_tx: &'a mpsc::Sender<SequencerControl>,
    pub(super) event_tx: &'a mpsc::Sender<SequencerControl>,
    pub(super) generation: &'a Arc<AtomicU64>,
    pub(super) pending: &'a Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    pub(super) topology_lock: &'a Arc<tokio::sync::Mutex<()>>,
    pub(super) files: &'a Arc<FileService>,
}

pub(super) async fn handle(
    request_id: u64,
    request: v1::Request,
    cancellation: Arc<AtomicBool>,
    context: ActiveRootContext<'_>,
) {
    let ActiveRootContext {
        control_tx,
        event_tx,
        generation,
        pending,
        topology_lock,
        files,
    } = context;
    match v1::Operation::ResolveActiveRoot {
        v1::Operation::ResolveActiveRoot => {
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
            let (first, known_generation) = {
                let (_guard, known_generation) =
                    lock_topology_generation(topology_lock, generation).await;
                (
                    tokio::task::spawn_blocking(discover_authoritative).await,
                    known_generation,
                )
            };
            let result = match first {
                Ok(Ok((snapshot, identity))) => {
                    if (!file.expected_server_identity.is_empty()
                        && file.expected_server_identity != identity)
                        || file.expected_topology_generation > known_generation
                    {
                        Err(anyhow::anyhow!("stale active-root request"))
                    } else if let Some(pane) = snapshot
                        .panes
                        .iter()
                        .find(|pane| pane.id == file.pane_id && pane.active)
                    {
                        let pane_id = pane.id.clone();
                        let cwd = pane.current_path.clone();
                        let expected_cwd = cwd.clone();
                        let cache_identity = identity.clone();
                        let cache_pane_id = pane_id.clone();
                        let root_cancellation = Arc::clone(&cancellation);
                        // Git may touch a slow filesystem. Never serialize topology
                        // reconciliation behind this probe.
                        let resolved = tokio::task::spawn_blocking(move || {
                            resolve_cached(
                                &cache_identity,
                                &cache_pane_id,
                                &cwd,
                                &root_cancellation,
                            )
                        })
                        .await;
                        match resolved {
                            // A backstop probe that confirms the caller's own
                            // root is the common case, and it must not cost a
                            // second authoritative discovery. The answer is the
                            // identity the caller already holds, so a pane that
                            // moved during resolution can only mean the *next*
                            // probe answers differently — never that this one
                            // published a root nobody asked about.
                            Ok(Ok((root, git_worktree)))
                                if !file.known_root_token.is_empty()
                                    && root_token(&root)
                                        .is_ok_and(|token| token == file.known_root_token) =>
                            {
                                Ok(v1::ActiveRoot {
                                    pane_id,
                                    root_generation: files
                                        .root_generation_for(&file.known_root_token),
                                    root_token: file.known_root_token.clone(),
                                    root,
                                    git_worktree,
                                    server_identity: identity,
                                    topology_generation: known_generation,
                                })
                            }
                            Ok(Ok((root, git_worktree))) => {
                                let (second, fresh_generation) = {
                                    let (_guard, fresh_generation) =
                                        lock_topology_generation(topology_lock, generation).await;
                                    (
                                        tokio::task::spawn_blocking(discover_authoritative).await,
                                        fresh_generation,
                                    )
                                };
                                match second {
                                    Ok(Ok((fresh, fresh_identity)))
                                        if !cancellation.load(Ordering::Acquire)
                                            && fresh_identity == identity
                                            && fresh.panes.iter().any(|candidate| {
                                                candidate.id == pane_id
                                                    && candidate.active
                                                    && candidate.current_path == expected_cwd
                                            }) =>
                                    {
                                        root_token(&root).map(|token| v1::ActiveRoot {
                                            pane_id,
                                            root,
                                            git_worktree,
                                            server_identity: identity,
                                            topology_generation: fresh_generation,
                                            root_generation: files.root_generation_for(&token),
                                            root_token: token,
                                        })
                                    }
                                    Ok(Ok(_)) => Err(anyhow::anyhow!(
                                        "stale active-root request: pane changed during resolution"
                                    )),
                                    Ok(Err(error)) => Err(error),
                                    Err(error) => Err(anyhow::anyhow!(
                                        "active-root verification task failed: {error}"
                                    )),
                                }
                            }
                            Ok(Err(error)) => Err(error),
                            Err(error) => Err(anyhow::anyhow!("active-root task failed: {error}")),
                        }
                    } else {
                        Err(anyhow::anyhow!("active pane no longer exists"))
                    }
                }
                Ok(Err(error)) => Err(error),
                Err(error) => Err(anyhow::anyhow!(
                    "active-root discovery task failed: {error}"
                )),
            };
            match result {
                Ok(active_root) => {
                    let unchanged = active_root.root_token == file.known_root_token;
                    // An unchanged root is news to nobody. Broadcasting it
                    // anyway made every backstop probe a connection-wide
                    // ActiveRoot payload, which is exactly the periodic traffic
                    // the backstop exists to avoid.
                    if !unchanged {
                        let _ = event_tx
                            .send(SequencerControl::OrderedEvent(v1::HostEvent {
                                kind: v1::EventKind::ActiveRoot.into(),
                                scope: active_root.pane_id.clone(),
                                file: Some(v1::FileServiceEvent {
                                    operation_id: file.operation_id.clone(),
                                    active_root: Some(active_root.clone()),
                                    root_token: active_root.root_token.clone(),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            }))
                            .await;
                    }
                    send_snapshot_response(
                        control_tx,
                        request_id,
                        file_response(&file.operation_id, |response| {
                            response.root_unchanged = unchanged;
                            response.active_root = Some(active_root);
                        }),
                    )
                    .await;
                }
                Err(error) => {
                    send_response(
                        control_tx,
                        request_id,
                        response_error("active_root_stale", &error.to_string()),
                    )
                    .await
                }
            }
        }
        _ => unreachable!(),
    }
}
