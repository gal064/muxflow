use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    process::Command,
    sync::Mutex,
};

use anyhow::{Context, bail};
use tmux_agent_protocol::v1;
use tmux_control::TmuxSnapshot;

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct SessionOrderState {
    #[serde(default)]
    servers: BTreeMap<String, Vec<String>>,
}

static SESSION_ORDER_LOCK: Mutex<()> = Mutex::new(());

pub(super) fn snapshot_from_identity(
    value: TmuxSnapshot,
    generation: u64,
    server_identity: String,
) -> v1::Snapshot {
    let _ = super::agents::AgentRuntime::global().reconcile_topology(&value, &server_identity);
    let agents = super::agents::AgentRuntime::global().snapshot_for(&server_identity);
    v1::Snapshot {
        server_identity,
        generation,
        sessions: value
            .sessions
            .into_iter()
            .map(|item| v1::Session {
                id: item.id,
                name: item.name,
                window_count: item.window_count,
                attached_clients: item.attached_clients,
                order: item.order,
            })
            .collect(),
        windows: value
            .windows
            .into_iter()
            .map(|item| v1::Window {
                id: item.id,
                session_id: item.session_id,
                index: item.index,
                name: item.name,
                active: item.active,
                layout: item.layout,
                zoomed: item.zoomed,
                layout_generation: generation,
            })
            .collect(),
        panes: value
            .panes
            .into_iter()
            .map(|item| v1::Pane {
                id: item.id,
                session_id: item.session_id,
                window_id: item.window_id,
                index: item.index,
                active: item.active,
                width: item.width.into(),
                height: item.height.into(),
                left: item.left.into(),
                top: item.top.into(),
                current_path: item.current_path,
                current_command: item.current_command,
            })
            .collect(),
        agents: Some(agents),
    }
}

/// Discovers the tmux server's identity and whole topology in one client fork.
///
/// The previous shape paid five forks — identity, sessions, windows, panes,
/// identity again — and compared the two identity probes to catch a server
/// restart mid-discovery. Batching removes both costs at once: every record
/// comes from a single tmux client, and a client is bound to one server for its
/// whole life, so there is no interval in which the records could straddle two
/// servers and nothing left for a second probe to detect.
pub(super) fn discover_consistent() -> anyhow::Result<(TmuxSnapshot, String)> {
    // Identity before the records, so a server that restarts *during* discovery
    // is caught rather than pairing the old topology with the new server. This
    // is a stat, not a fork: the five-fork before/after probe this replaces cost
    // milliseconds, and this costs a syscall.
    //
    // The comparison is on device and inode alone. The identity *string* also
    // carries the socket path, and the path derived from the environment here
    // ("/tmp/...") and the one tmux reports below ("/private/tmp/...") name the
    // same socket through different prefixes on macOS — comparing the strings
    // reports a server restart on every single discovery.
    let before = socket_identity_key(&tmux_socket_path(""));
    let output = tmux_command()
        .args(tmux_control::batched_discovery_args())
        .output()
        .context("run batched tmux discovery")?;
    let discovery = tmux_control::parse_batched_discovery(
        &output.stdout,
        &output.stderr,
        output.status.success(),
    )
    .map_err(|_| anyhow::anyhow!("tmux server is unavailable"))?;
    let socket = tmux_socket_path(&discovery.socket_path);
    let identity = socket_server_identity(&socket).unwrap_or_else(|_| "tmux:none".into());
    if identity == "tmux:none" {
        bail!("tmux server is unavailable");
    }
    let mut value = discovery.snapshot;
    overlay_session_order(&mut value, &identity)?;
    if before.is_some_and(|before| Some(before) != socket_identity_key(&socket)) {
        bail!("tmux server changed during snapshot discovery");
    }
    Ok((value, identity))
}

pub(super) fn discover_authoritative() -> anyhow::Result<(TmuxSnapshot, String)> {
    normalize_authoritative_discovery(discover_consistent(), server_identity)
}

fn normalize_authoritative_discovery(
    discovered: anyhow::Result<(TmuxSnapshot, String)>,
    current_identity: impl FnOnce() -> String,
) -> anyhow::Result<(TmuxSnapshot, String)> {
    match discovered {
        Ok(discovered) => Ok(discovered),
        Err(_) if current_identity() == "tmux:none" => {
            Ok((TmuxSnapshot::default(), "tmux:none".into()))
        }
        Err(error) => Err(error),
    }
}

pub(super) fn reorder_session(
    server_identity: &str,
    snapshot: &mut TmuxSnapshot,
    session_id: &str,
    target_index: u32,
) -> anyhow::Result<()> {
    let _guard = SESSION_ORDER_LOCK.lock().unwrap();
    let path = crate::paths::runtime_dir().join("session-order.json");
    let mut state = load_session_order(&path)?;
    let mut order: Vec<_> = snapshot
        .sessions
        .iter()
        .map(|item| item.id.clone())
        .collect();
    reorder_ids(&mut order, session_id, target_index)?;
    state.servers.insert(server_identity.to_owned(), order);
    save_session_order(&path, &state)?;
    overlay_session_order_unlocked(snapshot, server_identity)
}

fn overlay_session_order(snapshot: &mut TmuxSnapshot, server_identity: &str) -> anyhow::Result<()> {
    let _guard = SESSION_ORDER_LOCK.lock().unwrap();
    overlay_session_order_unlocked(snapshot, server_identity)
}

fn overlay_session_order_unlocked(
    snapshot: &mut TmuxSnapshot,
    server_identity: &str,
) -> anyhow::Result<()> {
    let path = crate::paths::runtime_dir().join("session-order.json");
    let mut state = load_session_order(&path)?;
    let Some(saved) = state.servers.get_mut(server_identity) else {
        for (order, session) in snapshot.sessions.iter_mut().enumerate() {
            session.order = order.try_into().unwrap_or(u32::MAX);
        }
        return Ok(());
    };

    let live: HashSet<_> = snapshot
        .sessions
        .iter()
        .map(|item| item.id.clone())
        .collect();
    let before = saved.clone();
    saved.retain(|session_id| live.contains(session_id));
    for session in &snapshot.sessions {
        if !saved.contains(&session.id) {
            saved.push(session.id.clone());
        }
    }
    let order_by_id: BTreeMap<_, _> = saved
        .iter()
        .enumerate()
        .map(|(index, id)| (id.clone(), index))
        .collect();
    snapshot
        .sessions
        .sort_by_key(|session| order_by_id.get(&session.id).copied().unwrap_or(usize::MAX));
    for (order, session) in snapshot.sessions.iter_mut().enumerate() {
        session.order = order.try_into().unwrap_or(u32::MAX);
    }
    if *saved != before {
        save_session_order(&path, &state)?;
    }
    Ok(())
}

fn reorder_ids(order: &mut Vec<String>, session_id: &str, target_index: u32) -> anyhow::Result<()> {
    let Some(current) = order.iter().position(|value| value == session_id) else {
        bail!("session no longer exists");
    };
    let session_id = order.remove(current);
    let target = usize::try_from(target_index)
        .unwrap_or(usize::MAX)
        .min(order.len());
    order.insert(target, session_id);
    Ok(())
}

fn load_session_order(path: &std::path::Path) -> anyhow::Result<SessionOrderState> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("parse private session order state"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(SessionOrderState::default())
        }
        Err(error) => Err(error).context("read private session order state"),
    }
}

fn save_session_order(path: &std::path::Path, state: &SessionOrderState) -> anyhow::Result<()> {
    let parent = path.parent().context("session order path has no parent")?;
    crate::paths::prepare_runtime_dir(parent)?;
    let temporary = parent.join(format!(".session-order-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .context("create private session order state")?;
        file.write_all(&serde_json::to_vec(state)?)?;
        file.sync_all()?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        fs::rename(&temporary, path).context("atomically replace session order state")?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(super) fn server_identity() -> String {
    server_identity_from_command(tmux_command())
}

/// Resolves the tmux server inherited by the calling process. Unlike the
/// daemon's configured server, this intentionally honors `TMUX`; hook pane IDs
/// are meaningful only within that exact server.
pub(crate) fn inherited_server_identity() -> Option<String> {
    std::env::var_os("TMUX")?;
    let identity = server_identity_from_command(Command::new("tmux"));
    (identity != "tmux:none").then_some(identity)
}

fn server_identity_from_command(mut command: Command) -> String {
    let output = command
        .args(["display-message", "-p", "#{socket_path}"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let value = String::from_utf8_lossy(&output.stdout);
            let value = value.trim_end_matches(['\r', '\n']);
            // Older tmux releases expand socket_path to an empty string. A
            // successful query still proves the selected server is live, so
            // derive its socket from -L/TMUX and bind identity to the socket
            // device+inode. Unlike #{pid}, these values are not client-context
            // dependent and are identical in the daemon and inherited hook.
            let socket = tmux_socket_path(value);
            socket_server_identity(&socket).unwrap_or_else(|_| "tmux:none".into())
        }
        _ => "tmux:none".into(),
    }
}

fn tmux_socket_path(formatted: &str) -> std::path::PathBuf {
    if !formatted.is_empty() {
        return formatted.into();
    }
    let configured_name = std::env::var_os("ADE_TMUX_SOCKET_NAME");
    if configured_name.is_none()
        && let Some(from_environment) = std::env::var_os("TMUX")
            .and_then(|value| value.to_string_lossy().split(',').next().map(str::to_owned))
            .filter(|value| !value.is_empty())
    {
        return from_environment.into();
    }
    let base = std::env::var_os("TMUX_TMPDIR").unwrap_or_else(|| "/tmp".into());
    // SAFETY: geteuid has no preconditions and does not mutate process state.
    let uid = unsafe { libc::geteuid() };
    let name = configured_name.unwrap_or_else(|| "default".into());
    Path::new(&base).join(format!("tmux-{uid}")).join(name)
}

/// The socket's device and inode: what actually identifies a tmux server, with
/// none of the path aliasing that the printable identity carries.
fn socket_identity_key(socket: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

    let metadata = fs::metadata(socket).ok()?;
    metadata
        .file_type()
        .is_socket()
        .then(|| (metadata.dev(), metadata.ino()))
}

fn socket_server_identity(socket: &Path) -> anyhow::Result<String> {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

    // tmux socket paths may be symlinks (notably in containerized/runtime-dir
    // setups); identity is bound to the actual socket target, not the link.
    let metadata = fs::metadata(socket).context("inspect tmux socket identity")?;
    if !metadata.file_type().is_socket() {
        bail!("tmux socket path is not a Unix socket");
    }
    Ok(format!(
        "tmux:{}:{}:{}",
        socket.to_string_lossy(),
        metadata.dev(),
        metadata.ino()
    ))
}

pub(super) fn tmux_command() -> Command {
    let mut command = Command::new("tmux");
    if let Some(name) = std::env::var_os("ADE_TMUX_SOCKET_NAME") {
        command.env_remove("TMUX").arg("-L").arg(name);
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn session_reorder_shifts_without_losing_ids() {
        let mut order = vec!["$1".into(), "$2".into(), "$3".into()];
        reorder_ids(&mut order, "$3", 0).unwrap();
        assert_eq!(order, ["$3", "$1", "$2"]);
        reorder_ids(&mut order, "$3", 99).unwrap();
        assert_eq!(order, ["$1", "$2", "$3"]);
        assert!(reorder_ids(&mut order, "$99", 0).is_err());
    }

    #[test]
    fn authoritative_discovery_probes_identity_only_after_discovery_fails() {
        let discovered = (TmuxSnapshot::default(), "tmux:live".to_owned());
        let resolved = normalize_authoritative_discovery(Ok(discovered.clone()), || {
            panic!("successful discovery must not fork an identity probe")
        })
        .unwrap();
        assert_eq!(resolved, discovered);

        let resolved =
            normalize_authoritative_discovery(Err(anyhow::anyhow!("server unavailable")), || {
                "tmux:none".into()
            })
            .unwrap();
        assert_eq!(resolved, (TmuxSnapshot::default(), "tmux:none".into()));
    }

    #[test]
    fn server_identity_changes_when_the_same_path_gets_a_replacement_socket() {
        #[cfg(target_os = "macos")]
        let temporary_root = Path::new("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let temporary_root = std::env::temp_dir();
        let directory =
            temporary_root.join(format!("phase5-server-identity-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let socket = directory.join("tmux.sock");
        let first_listener = UnixListener::bind(&socket).unwrap();
        let first = socket_server_identity(&socket).unwrap();
        assert_eq!(first, socket_server_identity(&socket).unwrap());
        drop(first_listener);
        fs::remove_file(&socket).unwrap();
        let _replacement = UnixListener::bind(&socket).unwrap();
        let second = socket_server_identity(&socket).unwrap();
        assert_ne!(first, second);
        fs::remove_dir_all(&directory).unwrap();
    }
}
