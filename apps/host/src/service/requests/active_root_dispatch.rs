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
/// The capability returned is the pane's *own* active root, the one the caller
/// already holds, and never a new one derived from where the path happened to
/// land. Minting the resolved file's parent directory instead made one
/// Cmd-click on terminal output enough to mint a persistent root capability
/// anywhere on the host — `canonicalize` follows symlinks, so a file in the
/// pane's own directory could name `/etc/passwd` and hand back `/etc`. A path
/// that resolves outside the pane's authorized root is refused rather than
/// widening what the caller may read.
///
/// The generation check is monotonic, not an equality: a caller that is merely
/// behind a busy pane's topology is still asking about a route this host can
/// confirm, and the pane revalidation below is what actually decides staleness.
/// Only a caller from a *future* generation is nonsense.
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
        || file.expected_topology_generation > known_generation
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
    let cache_identity = identity.clone();
    let cache_pane_id = pane_id.clone();
    let resolve_cancellation = Arc::clone(cancellation);
    let (root, git_worktree, path) = tokio::task::spawn_blocking(move || {
        if resolve_cancellation.load(Ordering::Acquire) {
            bail!("terminal file path request was cancelled");
        }
        let (root, git_worktree) = resolve_cached(
            &cache_identity,
            &cache_pane_id,
            &expected_cwd,
            &resolve_cancellation,
        )?;
        let path = canonical_terminal_file(&root, &expected_cwd, &candidate)?;
        Ok((root, git_worktree, path))
    })
    .await
    .context("terminal file path task failed")??;
    let token = root_token(&root)?;

    let (fresh, fresh_identity, fresh_generation) = discover(generation, topology_lock).await?;
    let stable = !cancellation.load(Ordering::Acquire)
        && fresh_identity == identity
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
            git_worktree,
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

/// The canonical file a terminal path names, confined to `root`.
///
/// Confinement is checked *after* canonicalization, so a symlink is judged by
/// where it lands and not by how it is spelled. `root` is canonicalized here
/// too: comparing a canonical path against a root that still contains a symlink
/// would reject files that are genuinely inside it.
fn canonical_terminal_file(root: &str, cwd: &str, candidate: &str) -> anyhow::Result<String> {
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
    let canonical_root =
        std::fs::canonicalize(root).with_context(|| format!("{root} is unavailable"))?;
    if !canonical.starts_with(&canonical_root) {
        bail!(
            "{} is outside the pane's root {}",
            canonical.display(),
            canonical_root.display()
        );
    }
    Ok(canonical
        .to_str()
        .context("resolved file path is not valid UTF-8")?
        .to_owned())
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
    fn resolves_absolute_and_relative_regular_files_inside_the_pane_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let cwd = root.join("work");
        fs::create_dir_all(&cwd).unwrap();
        fs::write(cwd.join("local.txt"), "local").unwrap();
        fs::write(root.join("top.txt"), "top").unwrap();
        // The resolver answers in canonical form, and macOS reaches the
        // temporary directory through a /var -> /private/var symlink.
        let root = fs::canonicalize(&root).unwrap();
        let cwd = fs::canonicalize(&cwd).unwrap();

        let relative =
            canonical_terminal_file(root.to_str().unwrap(), cwd.to_str().unwrap(), "./local.txt")
                .unwrap();
        assert_eq!(relative, cwd.join("local.txt").to_str().unwrap());

        let absolute = canonical_terminal_file(
            root.to_str().unwrap(),
            cwd.to_str().unwrap(),
            root.join("top.txt").to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(absolute, root.join("top.txt").to_str().unwrap());
    }

    /// One Cmd-click on terminal output must not widen what the caller may read.
    ///
    /// The symlink case is the whole point: the path is spelled inside the
    /// pane's root and lands outside it, which is exactly what canonicalizing
    /// before comparing catches. The sibling case is the other half — a root
    /// compared as a string prefix would accept `…/project-notes` as part of
    /// `…/project`.
    #[test]
    fn refuses_paths_that_leave_the_pane_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let outside = temp.path().join("secrets");
        let sibling = temp.path().join("project-notes");
        for directory in [&root, &outside, &sibling] {
            fs::create_dir_all(directory).unwrap();
        }
        fs::write(outside.join("passwd"), "root:x:0:0").unwrap();
        fs::write(sibling.join("notes.txt"), "notes").unwrap();
        std::os::unix::fs::symlink(outside.join("passwd"), root.join("innocent.txt")).unwrap();
        let root = fs::canonicalize(&root).unwrap();
        let outside = fs::canonicalize(&outside).unwrap();
        let sibling = fs::canonicalize(&sibling).unwrap();

        for candidate in [
            "./innocent.txt".to_owned(),
            "../secrets/passwd".to_owned(),
            outside.join("passwd").to_str().unwrap().to_owned(),
            sibling.join("notes.txt").to_str().unwrap().to_owned(),
        ] {
            let error =
                canonical_terminal_file(root.to_str().unwrap(), root.to_str().unwrap(), &candidate)
                    .expect_err("a path outside the pane root was resolved");
            assert!(
                error.to_string().contains("outside the pane's root"),
                "{candidate} was refused for the wrong reason: {error}"
            );
        }
    }

    #[test]
    fn rejects_missing_paths_and_directories() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().to_str().unwrap();
        assert!(canonical_terminal_file(cwd, cwd, "./missing.txt").is_err());
        assert!(
            canonical_terminal_file(cwd, cwd, ".")
                .unwrap_err()
                .to_string()
                .contains("not a file")
        );
    }
}
