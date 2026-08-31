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

/// What the user pinned on one tmux server.
///
/// Pins live beside the session order and for the same reason: tmux has no
/// concept of either, and the app must not invent tmux state to store one. The
/// two files are guarded by one lock because they are overlaid onto the same
/// snapshot in the same pass.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PinState {
    #[serde(default)]
    servers: BTreeMap<String, ServerPins>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerPins {
    #[serde(default)]
    sessions: Vec<PinnedSession>,
    #[serde(default)]
    windows: Vec<PinnedWindow>,
}

/// A pinned workspace, recorded in the order it was pinned.
///
/// Neither the vector's order nor `pinned_at` — milliseconds since the epoch —
/// decides anything today: the overlay reads the entries as a set, the wire
/// carries one boolean, and the app draws the pinned block in its own workspace
/// and window order. The timestamp is the record of when, kept so this file can
/// answer that question later without a second migration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PinnedSession {
    session_id: String,
    pinned_at: u64,
}

/// Carries its session so a window pin can be pruned against the exact
/// workspace it was written for: tmux can move a window to another session,
/// and a pin that followed it there was never asked for.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PinnedWindow {
    session_id: String,
    window_id: String,
    pinned_at: u64,
}

/// One lock for both private sidecars: they are written and overlaid together.
static SIDECAR_LOCK: Mutex<()> = Mutex::new(());

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
                pinned: item.pinned,
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
                pinned: item.pinned,
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
    let output = tmux_command()?
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
    overlay_pins(&mut value, &identity)?;
    if before.is_some_and(|before| Some(before) != socket_identity_key(&socket)) {
        bail!("tmux server changed during snapshot discovery");
    }
    Ok((value, identity))
}

pub(super) fn discover_authoritative() -> anyhow::Result<(TmuxSnapshot, String)> {
    tmux_control::tmux_executable().context("locate tmux executable")?;
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
    let _guard = SIDECAR_LOCK.lock().unwrap();
    let path = session_order_path();
    let mut state: SessionOrderState = load_sidecar(&path, SESSION_ORDER_LABEL)?;
    let mut order: Vec<_> = snapshot
        .sessions
        .iter()
        .map(|item| item.id.clone())
        .collect();
    reorder_ids(&mut order, session_id, target_index)?;
    state.servers.insert(server_identity.to_owned(), order);
    save_sidecar(&path, SESSION_ORDER_LABEL, &state)?;
    overlay_session_order_unlocked(snapshot, server_identity)
}

/// Pins or unpins one workspace, or one tab inside it, in the private sidecar.
///
/// Nothing is sent to tmux — a pin is presentation — but it is host state and
/// not app state, so every client of this server sees the same pinned block and
/// a reinstalled app inherits it. The target is checked against the snapshot
/// the action was validated on: a pin for a session that has just been closed
/// would be written for something that can never be drawn, and pruned again on
/// the next discovery anyway.
pub(super) fn set_pinned(
    server_identity: &str,
    snapshot: &mut TmuxSnapshot,
    session_id: &str,
    window_id: &str,
    pinned: bool,
) -> anyhow::Result<()> {
    let _guard = SIDECAR_LOCK.lock().unwrap();
    write_pin(
        &pins_path(),
        server_identity,
        snapshot,
        session_id,
        window_id,
        pinned,
    )?;
    overlay_pins_unlocked(snapshot, server_identity)
}

fn write_pin(
    path: &std::path::Path,
    server_identity: &str,
    snapshot: &TmuxSnapshot,
    session_id: &str,
    window_id: &str,
    pinned: bool,
) -> anyhow::Result<()> {
    if !snapshot.sessions.iter().any(|item| item.id == session_id) {
        bail!("session no longer exists");
    }
    if !window_id.is_empty()
        && !snapshot
            .windows
            .iter()
            .any(|item| item.id == window_id && item.session_id == session_id)
    {
        bail!("window is not linked to the requested session");
    }
    let mut state: PinState = load_sidecar(path, PINS_LABEL)?;
    let pins = state.servers.entry(server_identity.to_owned()).or_default();
    let pinned_at = now_millis();
    if window_id.is_empty() {
        pins.sessions.retain(|item| item.session_id != session_id);
        if pinned {
            pins.sessions.push(PinnedSession {
                session_id: session_id.to_owned(),
                pinned_at,
            });
        }
    } else {
        // Keyed on the window alone: a window that moved to another session
        // must not end up pinned twice under two workspaces.
        pins.windows.retain(|item| item.window_id != window_id);
        if pinned {
            pins.windows.push(PinnedWindow {
                session_id: session_id.to_owned(),
                window_id: window_id.to_owned(),
                pinned_at,
            });
        }
    }
    // An unpin that empties a server leaves nothing worth a key. The overlay
    // collects the same emptiness after a prune; doing it here as well is what
    // keeps a server that was pinned and then unpinned from sitting in the file
    // forever, since unpinning is not a prune.
    if pins.sessions.is_empty() && pins.windows.is_empty() {
        state.servers.remove(server_identity);
    }
    save_sidecar(path, PINS_LABEL, &state)
}

fn overlay_pins(snapshot: &mut TmuxSnapshot, server_identity: &str) -> anyhow::Result<()> {
    let _guard = SIDECAR_LOCK.lock().unwrap();
    overlay_pins_unlocked(snapshot, server_identity)
}

/// Stamps `pinned` onto everything this server has pinned, and forgets the
/// pins whose session or window is gone.
///
/// Pruning belongs here rather than in a close action: a window can also
/// disappear because another tmux client killed it, and the sidecar must not
/// accumulate records for topology nobody can see. Discovery is batched, so a
/// snapshot that lists a session always lists its windows too — there is no
/// half-filled snapshot in which "no windows here" could mean "not yet known".
fn overlay_pins_unlocked(snapshot: &mut TmuxSnapshot, server_identity: &str) -> anyhow::Result<()> {
    apply_pins(&pins_path(), snapshot, server_identity)
}

fn apply_pins(
    path: &std::path::Path,
    snapshot: &mut TmuxSnapshot,
    server_identity: &str,
) -> anyhow::Result<()> {
    let mut state: PinState = load_sidecar(path, PINS_LABEL)?;
    let Some(pins) = state.servers.get_mut(server_identity) else {
        for session in &mut snapshot.sessions {
            session.pinned = false;
        }
        for window in &mut snapshot.windows {
            window.pinned = false;
        }
        return Ok(());
    };
    let before = (pins.sessions.len(), pins.windows.len());
    pins.sessions
        .retain(|item| snapshot.sessions.iter().any(|s| s.id == item.session_id));
    pins.windows.retain(|item| {
        snapshot
            .windows
            .iter()
            .any(|w| w.id == item.window_id && w.session_id == item.session_id)
    });
    let pinned_sessions: HashSet<_> = pins
        .sessions
        .iter()
        .map(|item| item.session_id.clone())
        .collect();
    let pinned_windows: HashSet<_> = pins
        .windows
        .iter()
        .map(|item| item.window_id.clone())
        .collect();
    let pruned = before != (pins.sessions.len(), pins.windows.len());
    let empty = pins.sessions.is_empty() && pins.windows.is_empty();
    for session in &mut snapshot.sessions {
        session.pinned = pinned_sessions.contains(&session.id);
    }
    for window in &mut snapshot.windows {
        window.pinned = pinned_windows.contains(&window.id);
    }
    if pruned {
        if empty {
            state.servers.remove(server_identity);
        }
        save_sidecar(path, PINS_LABEL, &state)?;
    }
    Ok(())
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| u64::try_from(value.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn overlay_session_order(snapshot: &mut TmuxSnapshot, server_identity: &str) -> anyhow::Result<()> {
    let _guard = SIDECAR_LOCK.lock().unwrap();
    overlay_session_order_unlocked(snapshot, server_identity)
}

fn overlay_session_order_unlocked(
    snapshot: &mut TmuxSnapshot,
    server_identity: &str,
) -> anyhow::Result<()> {
    let path = session_order_path();
    let mut state: SessionOrderState = load_sidecar(&path, SESSION_ORDER_LABEL)?;
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
        save_sidecar(&path, SESSION_ORDER_LABEL, &state)?;
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

const SESSION_ORDER_LABEL: &str = "session order";
const PINS_LABEL: &str = "pins";

fn session_order_path() -> std::path::PathBuf {
    crate::paths::runtime_dir().join("session-order.json")
}

fn pins_path() -> std::path::PathBuf {
    crate::paths::runtime_dir().join("pins.json")
}

fn load_sidecar<T: Default + serde::de::DeserializeOwned>(
    path: &std::path::Path,
    label: &str,
) -> anyhow::Result<T> {
    match fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).with_context(|| format!("parse private {label} state"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error).with_context(|| format!("read private {label} state")),
    }
}

fn save_sidecar<T: serde::Serialize>(
    path: &std::path::Path,
    label: &str,
    state: &T,
) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{label} path has no parent"))?;
    crate::paths::prepare_runtime_dir(parent)?;
    let stem = path
        .file_stem()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sidecar".to_owned());
    let temporary = parent.join(format!(".{stem}-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .with_context(|| format!("create private {label} state"))?;
        file.write_all(&serde_json::to_vec(state)?)?;
        file.sync_all()?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        fs::rename(&temporary, path)
            .with_context(|| format!("atomically replace private {label} state"))?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(super) fn server_identity() -> String {
    tmux_command()
        .map(server_identity_from_command)
        .unwrap_or_else(|_| "tmux:none".into())
}

/// Resolves the tmux server inherited by the calling process. Unlike the
/// daemon's configured server, this intentionally honors `TMUX`; hook pane IDs
/// are meaningful only within that exact server.
pub(crate) fn inherited_server_identity() -> Option<String> {
    std::env::var_os("TMUX")?;
    let identity = server_identity_from_command(tmux_client().ok()?);
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

/// A tmux client that is forced to speak UTF-8.
///
/// The server replaces every non-ASCII byte it sends — agent status glyphs in
/// window names — with `_` for any client whose locale says it cannot render
/// UTF-8, and the clients we spawn inherit no `LANG`/`LC_*` at all (a
/// Dock-launched bundle, a non-interactive SSH exec session). `-u` is a global
/// flag, so it must stay ahead of the subcommand callers append.
fn tmux_client() -> anyhow::Result<Command> {
    let mut command = tmux_control::tmux_command().context("locate tmux executable")?;
    command.arg("-u");
    Ok(command)
}

pub(super) fn tmux_command() -> anyhow::Result<Command> {
    let mut command = tmux_client()?;
    if let Some(name) = std::env::var_os("ADE_TMUX_SOCKET_NAME") {
        command.env_remove("TMUX").arg("-L").arg(name);
    }
    Ok(command)
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

    fn pinned_fixture() -> TmuxSnapshot {
        let session = |id: &str, order: u32| tmux_control::Session {
            id: id.into(),
            name: id.into(),
            window_count: 1,
            attached_clients: 0,
            order,
            pinned: false,
        };
        let window = |id: &str, session_id: &str| tmux_control::Window {
            id: id.into(),
            session_id: session_id.into(),
            index: 0,
            name: id.into(),
            active: false,
            layout: String::new(),
            zoomed: false,
            pinned: false,
        };
        TmuxSnapshot {
            sessions: vec![session("$1", 0), session("$2", 1)],
            windows: vec![window("@1", "$1"), window("@2", "$1"), window("@3", "$2")],
            panes: Vec::new(),
        }
    }

    fn pins_fixture_path() -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!("muxflow-pins-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        directory.join("pins.json")
    }

    #[test]
    fn pins_overlay_marks_the_pinned_workspace_and_tab_and_nothing_else() {
        let path = pins_fixture_path();
        let mut snapshot = pinned_fixture();
        write_pin(&path, "tmux:one", &snapshot, "$2", "", true).unwrap();
        write_pin(&path, "tmux:one", &snapshot, "$1", "@2", true).unwrap();
        // Another server's pins are in the same file and must not leak.
        write_pin(&path, "tmux:two", &snapshot, "$1", "", true).unwrap();

        apply_pins(&path, &mut snapshot, "tmux:one").unwrap();
        assert_eq!(
            snapshot
                .sessions
                .iter()
                .map(|item| (item.id.as_str(), item.pinned))
                .collect::<Vec<_>>(),
            [("$1", false), ("$2", true)]
        );
        assert_eq!(
            snapshot
                .windows
                .iter()
                .map(|item| (item.id.as_str(), item.pinned))
                .collect::<Vec<_>>(),
            [("@1", false), ("@2", true), ("@3", false)]
        );

        // A server with no record of its own is a snapshot with nothing pinned,
        // not one that inherits the flags a previous overlay left behind.
        apply_pins(&path, &mut snapshot, "tmux:three").unwrap();
        assert!(snapshot.sessions.iter().all(|item| !item.pinned));
        assert!(snapshot.windows.iter().all(|item| !item.pinned));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_second_pin_of_the_same_thing_unpins_it() {
        let path = pins_fixture_path();
        let mut snapshot = pinned_fixture();
        write_pin(&path, "tmux:one", &snapshot, "$1", "", true).unwrap();
        write_pin(&path, "tmux:one", &snapshot, "$1", "@1", true).unwrap();
        apply_pins(&path, &mut snapshot, "tmux:one").unwrap();
        assert!(snapshot.sessions[0].pinned && snapshot.windows[0].pinned);

        write_pin(&path, "tmux:one", &snapshot, "$1", "", false).unwrap();
        write_pin(&path, "tmux:one", &snapshot, "$1", "@1", false).unwrap();
        apply_pins(&path, &mut snapshot, "tmux:one").unwrap();
        assert!(!snapshot.sessions[0].pinned && !snapshot.windows[0].pinned);
        // Nothing pinned here any more, so the server keeps no entry at all.
        let state: PinState = load_sidecar(&path, PINS_LABEL).unwrap();
        assert!(!state.servers.contains_key("tmux:one"));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn the_overlay_forgets_pins_whose_session_or_window_is_gone() {
        let path = pins_fixture_path();
        let full = pinned_fixture();
        write_pin(&path, "tmux:one", &full, "$2", "", true).unwrap();
        write_pin(&path, "tmux:one", &full, "$1", "@2", true).unwrap();

        // $2 closed, and @2 with it; $1 keeps only @1.
        let mut narrowed = TmuxSnapshot {
            sessions: full.sessions[..1].to_vec(),
            windows: full.windows[..1].to_vec(),
            panes: Vec::new(),
        };
        apply_pins(&path, &mut narrowed, "tmux:one").unwrap();
        assert!(!narrowed.sessions[0].pinned && !narrowed.windows[0].pinned);
        // Pruned in the file too, and the emptied server key with it, so a
        // session id tmux reuses cannot inherit a pin nobody made for it.
        let state: PinState = load_sidecar(&path, PINS_LABEL).unwrap();
        assert!(!state.servers.contains_key("tmux:one"));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_pin_is_refused_for_topology_the_snapshot_does_not_have() {
        let path = pins_fixture_path();
        let snapshot = pinned_fixture();
        assert!(
            write_pin(&path, "tmux:one", &snapshot, "$9", "", true)
                .unwrap_err()
                .to_string()
                .contains("session no longer exists")
        );
        // A window of another workspace is not this workspace's tab.
        assert!(
            write_pin(&path, "tmux:one", &snapshot, "$1", "@3", true)
                .unwrap_err()
                .to_string()
                .contains("not linked")
        );
        assert!(!path.exists(), "a refused pin must not write the sidecar");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn spawned_tmux_clients_ask_for_utf8_before_their_subcommand() {
        let mut command = tmux_command().unwrap();
        command.args(["list-windows", "-F", "#{window_name}"]);
        let args = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        // Without `-u` the server flattens the non-ASCII bytes of a window name
        // to `_` for our locale-less clients.
        let utf8 = args
            .iter()
            .position(|value| value == "-u")
            .unwrap_or_else(|| panic!("no -u in argv: {args:?}"));
        assert_eq!(utf8, 0, "unexpected argv: {args:?}");
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
