use anyhow::{Context as _, bail};
use std::path::{Path, PathBuf};

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
    let operation = v1::Operation::try_from(request.operation).ok();
    let Some(file) = request.file else {
        send_response(
            control_tx,
            request_id,
            response_error("invalid_file_request", "file request payload is required"),
        )
        .await;
        return;
    };
    if operation == Some(v1::Operation::ResolveTerminalFile) {
        let result = resolve_terminal_file(&file, &cancellation, generation, topology_lock).await;
        match result {
            Ok((active_root, path)) => {
                send_snapshot_response(
                    control_tx,
                    request_id,
                    file_response(&file.operation_id, |response| {
                        response.active_root = Some(active_root);
                        response.metadata = Some(v1::FileMetadata {
                            path,
                            kind: v1::FileKind::File.into(),
                            ..Default::default()
                        });
                    }),
                )
                .await;
            }
            Err(error) => {
                send_response(
                    control_tx,
                    request_id,
                    response_error("terminal_file_path_rejected", &error.to_string()),
                )
                .await;
            }
        }
        return;
    }
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

/// Resolves a terminal-output path on the host that owns the pane.
///
/// The parent directory becomes the editor capability. This intentionally does
/// not reuse the Explorer root: an absolute path printed by a command can be a
/// valid same-host file outside the pane's Git worktree.
async fn resolve_terminal_file(
    file: &v1::FileServiceRequest,
    cancellation: &Arc<AtomicBool>,
    generation: &Arc<AtomicU64>,
    topology_lock: &Arc<tokio::sync::Mutex<()>>,
) -> anyhow::Result<(v1::ActiveRoot, String)> {
    if file.path.is_empty()
        || !(file.path.starts_with('/') || file.path.contains('/'))
        || file.path.contains('\0')
    {
        bail!("terminal file path must be absolute or a relative path containing a separator");
    }
    if file.expected_session_id.is_empty()
        || file.expected_window_id.is_empty()
        || file.expected_cwd.is_empty()
    {
        bail!("terminal file path request omitted its expected pane route");
    }
    let (snapshot, identity, known_generation) = discover(generation, topology_lock).await?;
    if file.expected_server_identity.is_empty()
        || file.expected_server_identity != identity
        || file.expected_topology_generation != known_generation
    {
        bail!("stale terminal file path request");
    }
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane_matches_terminal_file_route(pane, file))
        .context("terminal pane route or working directory changed")?;
    let pane_id = pane.id.clone();
    let stable_session_id = pane.session_id.clone();
    let stable_window_id = pane.window_id.clone();
    let stable_cwd = pane.current_path.clone();
    let expected_cwd = stable_cwd.clone();
    let candidate = file.path.clone();
    let resolve_cancellation = Arc::clone(cancellation);
    let (path, root, token) = tokio::task::spawn_blocking(move || {
        if resolve_cancellation.load(Ordering::Acquire) {
            bail!("terminal file path request was cancelled");
        }
        canonical_terminal_file(&expected_cwd, &candidate)
    })
    .await
    .context("terminal file path task failed")??;

    let (fresh, fresh_identity, fresh_generation) = discover(generation, topology_lock).await?;
    let stable = !cancellation.load(Ordering::Acquire)
        && fresh_identity == identity
        && fresh_generation == known_generation
        && fresh.panes.iter().any(|candidate| {
            candidate.id == pane_id
                && candidate.session_id == stable_session_id
                && candidate.window_id == stable_window_id
                && candidate.current_path == stable_cwd
        });
    if !stable {
        bail!("stale terminal file path request: pane changed during resolution");
    }
    Ok((
        v1::ActiveRoot {
            pane_id,
            root,
            git_worktree: false,
            server_identity: identity,
            topology_generation: fresh_generation,
            root_generation: root_generation(&token),
            root_token: token,
        },
        path,
    ))
}

fn pane_matches_terminal_file_route(
    pane: &tmux_control::Pane,
    file: &v1::FileServiceRequest,
) -> bool {
    pane.id == file.pane_id
        && pane.session_id == file.expected_session_id
        && pane.window_id == file.expected_window_id
        && pane.current_path == file.expected_cwd
}

fn canonical_terminal_file(cwd: &str, candidate: &str) -> anyhow::Result<(String, String, String)> {
    let requested = Path::new(candidate);
    let joined: PathBuf = if requested.is_absolute() {
        requested.to_owned()
    } else {
        Path::new(cwd).join(requested)
    };
    let canonical = std::fs::canonicalize(&joined)
        .with_context(|| format!("{} does not exist", joined.display()))?;
    if !canonical.is_file() {
        bail!("{} is not a file", canonical.display());
    }
    let parent = canonical
        .parent()
        .context("resolved file has no parent directory")?;
    let path = canonical
        .to_str()
        .context("resolved file path is not valid UTF-8")?
        .to_owned();
    let root = parent
        .to_str()
        .context("resolved file parent is not valid UTF-8")?
        .to_owned();
    let token = root_token(&root)?;
    Ok((path, root, token))
}

#[cfg(test)]
mod terminal_file_tests {
    use super::{canonical_terminal_file, pane_matches_terminal_file_route};
    use std::fs;
    use tmux_agent_protocol::v1;
    use tmux_control::Pane;

    #[test]
    fn binds_resolution_to_the_exact_published_pane_route() {
        let pane = Pane {
            id: "%1".into(),
            session_id: "$1".into(),
            window_id: "@1".into(),
            index: 0,
            active: true,
            width: 80,
            height: 24,
            left: 0,
            top: 0,
            current_path: "/old".into(),
            current_command: "zsh".into(),
            pane_pid: 1,
            start_command: "zsh".into(),
        };
        let request = v1::FileServiceRequest {
            pane_id: "%1".into(),
            expected_session_id: "$1".into(),
            expected_window_id: "@1".into(),
            expected_cwd: "/old".into(),
            ..Default::default()
        };
        assert!(pane_matches_terminal_file_route(&pane, &request));
        for changed in [
            Pane {
                current_path: "/new".into(),
                ..pane.clone()
            },
            Pane {
                window_id: "@2".into(),
                ..pane.clone()
            },
            Pane {
                session_id: "$2".into(),
                ..pane.clone()
            },
        ] {
            assert!(!pane_matches_terminal_file_route(&changed, &request));
        }
    }

    #[test]
    fn resolves_absolute_and_relative_regular_files_from_the_pane_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("work");
        let sibling = temp.path().join("shared");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&sibling).unwrap();
        fs::write(cwd.join("local.txt"), "local").unwrap();
        fs::write(sibling.join("remote.txt"), "remote").unwrap();

        let (relative, relative_root, _) =
            canonical_terminal_file(cwd.to_str().unwrap(), "./local.txt").unwrap();
        assert_eq!(relative, cwd.join("local.txt").to_str().unwrap());
        assert_eq!(relative_root, cwd.to_str().unwrap());

        let (absolute, absolute_root, _) = canonical_terminal_file(
            cwd.to_str().unwrap(),
            sibling.join("remote.txt").to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(absolute, sibling.join("remote.txt").to_str().unwrap());
        assert_eq!(absolute_root, sibling.to_str().unwrap());
    }

    #[test]
    fn rejects_missing_paths_and_directories() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().to_str().unwrap();
        assert!(canonical_terminal_file(cwd, "./missing.txt").is_err());
        assert!(
            canonical_terminal_file(cwd, ".")
                .unwrap_err()
                .to_string()
                .contains("not a file")
        );
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
