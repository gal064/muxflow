use anyhow::{Context as _, bail};

use super::*;

pub(super) struct ActiveRootContext<'a> {
    pub(super) control_tx: &'a mpsc::Sender<SequencerControl>,
    pub(super) event_tx: &'a mpsc::Sender<SequencerControl>,
    pub(super) generation: &'a Arc<AtomicU64>,
    pub(super) topology_lock: &'a Arc<tokio::sync::Mutex<()>>,
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
        topology_lock,
    } = context;
    let Some(file) = request.file else {
        send_response(
            control_tx,
            request_id,
            response_error("invalid_file_request", "file request payload is required"),
        )
        .await;
        return;
    };
    let result = resolve(&file, &cancellation, generation, topology_lock).await;
    match result {
        Ok(active_root) => {
            let unchanged = active_root.root_token == file.known_root_token;
            // An unchanged root is news to nobody. Broadcasting it anyway made
            // every backstop probe a connection-wide ActiveRoot payload, which
            // is exactly the periodic traffic the backstop exists to avoid.
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

/// Resolves the active root of one pane.
///
/// Two authoritative discoveries bracket the Git probe, because the probe can
/// touch a slow filesystem and the pane may move while it runs. The exception
/// is the case the backstop actually spends its life in — the answer is the
/// capability the caller already holds — where a second discovery could only
/// ever confirm what the caller told us.
async fn resolve(
    file: &v1::FileServiceRequest,
    cancellation: &Arc<AtomicBool>,
    generation: &Arc<AtomicU64>,
    topology_lock: &Arc<tokio::sync::Mutex<()>>,
) -> anyhow::Result<v1::ActiveRoot> {
    let (snapshot, identity, known_generation) = discover(generation, topology_lock).await?;
    if (!file.expected_server_identity.is_empty() && file.expected_server_identity != identity)
        || file.expected_topology_generation > known_generation
    {
        bail!("stale active-root request");
    }
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.id == file.pane_id && pane.active)
        .context("active pane no longer exists")?;
    let pane_id = pane.id.clone();
    let expected_cwd = pane.current_path.clone();
    let cwd = expected_cwd.clone();
    let cache_identity = identity.clone();
    let cache_pane_id = pane_id.clone();
    let root_cancellation = Arc::clone(cancellation);
    // Git may touch a slow filesystem. Never serialize topology reconciliation
    // behind this probe.
    let (root, git_worktree) = tokio::task::spawn_blocking(move || {
        resolve_cached(&cache_identity, &cache_pane_id, &cwd, &root_cancellation)
    })
    .await
    .context("active-root task failed")??;
    let token = root_token(&root)?;
    if !file.known_root_token.is_empty() && token == file.known_root_token {
        return Ok(v1::ActiveRoot {
            pane_id,
            root: root.clone(),
            git_worktree,
            server_identity: identity,
            topology_generation: known_generation,
            root_generation: root_generation(&token),
            root_token: token,
        });
    }
    let (fresh, fresh_identity, fresh_generation) = discover(generation, topology_lock).await?;
    let stable = !cancellation.load(Ordering::Acquire)
        && fresh_identity == identity
        && fresh.panes.iter().any(|candidate| {
            candidate.id == pane_id && candidate.active && candidate.current_path == expected_cwd
        });
    if !stable {
        bail!("stale active-root request: pane changed during resolution");
    }
    Ok(v1::ActiveRoot {
        pane_id,
        root,
        git_worktree,
        server_identity: identity,
        topology_generation: fresh_generation,
        root_generation: root_generation(&token),
        root_token: token,
    })
}

async fn discover(
    generation: &Arc<AtomicU64>,
    topology_lock: &Arc<tokio::sync::Mutex<()>>,
) -> anyhow::Result<(tmux_control::TmuxSnapshot, String, u64)> {
    let (discovered, known_generation) = {
        let (_guard, known_generation) = lock_topology_generation(topology_lock, generation).await;
        (
            tokio::task::spawn_blocking(discover_authoritative).await,
            known_generation,
        )
    };
    let (snapshot, identity) = discovered.context("active-root discovery task failed")??;
    Ok((snapshot, identity, known_generation))
}
