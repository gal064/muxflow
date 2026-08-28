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
/// A path inside the pane's root reuses that ordinary root capability. A path
/// outside it receives a read-only capability bound to that one canonical file:
/// its parent is carried only because file streaming is root-relative, and the
/// token cannot enumerate the directory or resolve a sibling. This is the
/// distinction that lets a deliberate Cmd-click open a Claude scratchpad under
/// `/tmp` without turning the click into a persistent `/tmp/.../scratchpad`
/// root capability.
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
    let (root, git_worktree, path, token) = tokio::task::spawn_blocking(move || {
        if resolve_cancellation.load(Ordering::Acquire) {
            bail!("terminal file path request was cancelled");
        }
        let (root, git_worktree) = resolve_cached(
            &cache_identity,
            &cache_pane_id,
            &expected_cwd,
            &resolve_cancellation,
        )?;
        let home = if candidate.starts_with("~/") {
            Some(PathBuf::from(
                std::env::var_os("HOME").context("HOME is unavailable")?,
            ))
        } else {
            None
        };
        let path = canonical_terminal_file(&expected_cwd, &candidate, home.as_deref())?;
        terminal_file_capability(root, git_worktree, path)
    })
    .await
    .context("terminal file path task failed")??;
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

fn terminal_file_capability(
    root: String,
    git_worktree: bool,
    path: String,
) -> anyhow::Result<(String, bool, String, String)> {
    let canonical_root =
        std::fs::canonicalize(&root).with_context(|| format!("{root} is unavailable"))?;
    if Path::new(&path).starts_with(&canonical_root) {
        let token = root_token(&root)?;
        Ok((root, git_worktree, path, token))
    } else {
        let (single_root, token) = single_file_root(Path::new(&path))?;
        Ok((single_root, false, path, token))
    }
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

/// The canonical regular file a terminal path names.
///
/// Capability selection happens after this returns: an in-root path keeps the
/// pane root, while an outside path gets a read-only capability for this exact
/// canonical leaf. Canonicalizing here makes a symlink's target — not its
/// user-controlled spelling — the identity the capability binds.
fn canonical_terminal_file(
    cwd: &str,
    candidate: &str,
    home: Option<&Path>,
) -> anyhow::Result<String> {
    let requested = if let Some(suffix) = candidate.strip_prefix("~/") {
        if suffix.is_empty() || suffix.starts_with('/') {
            bail!("terminal file path has a malformed current-user home prefix");
        }
        let home = home.context("HOME is unavailable")?;
        if !home.is_absolute() {
            bail!("HOME must be absolute");
        }
        home.join(suffix)
    } else {
        if candidate.starts_with('~') {
            bail!("terminal file paths support only the current-user ~/ prefix");
        }
        PathBuf::from(candidate)
    };
    let joined: PathBuf = if requested.is_absolute() {
        requested
    } else {
        Path::new(cwd).join(&requested)
    };
    let canonical = std::fs::canonicalize(&joined)
        .with_context(|| format!("{} does not exist", joined.display()))?;
    if !canonical.is_file() {
        bail!("{} is not a file", canonical.display());
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
    use super::{
        canonical_terminal_file, pane_matches_terminal_file_route, terminal_file_capability,
    };
    use std::{fs, path::Path};
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

        let relative = canonical_terminal_file(cwd.to_str().unwrap(), "./local.txt", None).unwrap();
        assert_eq!(relative, cwd.join("local.txt").to_str().unwrap());

        let absolute = canonical_terminal_file(
            cwd.to_str().unwrap(),
            root.join("top.txt").to_str().unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(absolute, root.join("top.txt").to_str().unwrap());
    }

    #[test]
    fn canonicalizes_paths_outside_the_pane_root_before_capability_selection() {
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

        for (candidate, expected) in [
            ("./innocent.txt".to_owned(), outside.join("passwd")),
            ("../secrets/passwd".to_owned(), outside.join("passwd")),
            (
                outside.join("passwd").to_str().unwrap().to_owned(),
                outside.join("passwd"),
            ),
            (
                sibling.join("notes.txt").to_str().unwrap().to_owned(),
                sibling.join("notes.txt"),
            ),
        ] {
            let resolved =
                canonical_terminal_file(root.to_str().unwrap(), &candidate, None).unwrap();
            assert_eq!(resolved, expected.to_str().unwrap());
        }
    }

    #[test]
    fn outside_paths_receive_one_file_capabilities_while_inside_paths_keep_the_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let outside = temp.path().join("scratchpad");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let inside_file = root.join("inside.md");
        let outside_file = outside.join("prompt.md");
        fs::write(&inside_file, "inside").unwrap();
        fs::write(&outside_file, "outside").unwrap();
        let root = fs::canonicalize(root).unwrap();
        let inside_file = fs::canonicalize(inside_file).unwrap();
        let outside_file = fs::canonicalize(outside_file).unwrap();

        let inside = terminal_file_capability(
            root.to_str().unwrap().to_owned(),
            true,
            inside_file.to_str().unwrap().to_owned(),
        )
        .unwrap();
        assert_eq!(inside.0, root.to_str().unwrap());
        assert!(inside.1);
        assert!(!inside.3.starts_with("file-v1:"));

        let outside = terminal_file_capability(
            root.to_str().unwrap().to_owned(),
            true,
            outside_file.to_str().unwrap().to_owned(),
        )
        .unwrap();
        assert_eq!(outside.0, outside_file.parent().unwrap().to_str().unwrap());
        assert!(!outside.1);
        assert_eq!(outside.2, outside_file.to_str().unwrap());
        assert!(outside.3.starts_with("file-v1:"));
    }

    #[test]
    fn rejects_missing_paths_and_directories() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().to_str().unwrap();
        assert!(canonical_terminal_file(cwd, "./missing.txt", None).is_err());
        assert!(
            canonical_terminal_file(cwd, ".", None)
                .unwrap_err()
                .to_string()
                .contains("not a file")
        );
    }

    #[test]
    fn resolves_current_user_home_paths_with_the_host_home() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let cwd = home.join("work");
        let report = home.join("dev/report.pdf");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(report.parent().unwrap()).unwrap();
        fs::write(&report, "report").unwrap();
        let home = fs::canonicalize(home).unwrap();
        let cwd = fs::canonicalize(cwd).unwrap();
        let report = fs::canonicalize(report).unwrap();

        let resolved =
            canonical_terminal_file(cwd.to_str().unwrap(), "~/dev/report.pdf", Some(&home))
                .unwrap();
        assert_eq!(resolved, report.to_str().unwrap());
    }

    #[test]
    fn home_paths_require_one_absolute_current_user_home() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_str().unwrap();
        for (candidate, home, expected) in [
            ("~/file", None, "HOME is unavailable"),
            (
                "~/file",
                Some(Path::new("relative/home")),
                "HOME must be absolute",
            ),
            ("~alice/file", None, "only the current-user ~/ prefix"),
            (
                "~//file",
                Some(temp.path()),
                "malformed current-user home prefix",
            ),
        ] {
            let error = canonical_terminal_file(root, candidate, home).unwrap_err();
            assert!(
                error.to_string().contains(expected),
                "{candidate} was refused for the wrong reason: {error}"
            );
        }
    }

    #[test]
    fn home_path_traversal_resolves_before_the_exact_file_capability_is_minted() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let root = home.join("project");
        fs::create_dir_all(&root).unwrap();
        fs::write(home.join("outside.txt"), "outside").unwrap();
        let home = fs::canonicalize(home).unwrap();
        let root = fs::canonicalize(root).unwrap();

        let resolved =
            canonical_terminal_file(root.to_str().unwrap(), "~/outside.txt", Some(&home)).unwrap();
        assert_eq!(resolved, home.join("outside.txt").to_str().unwrap());
    }
}
