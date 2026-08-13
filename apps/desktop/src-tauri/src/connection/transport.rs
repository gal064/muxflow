use std::{
    collections::HashMap,
    fs,
    io::Read,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{Child, ChildStderr, Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

use super::{ConnectionSpec, validate_ssh_target};

#[derive(Clone)]
struct SshMaster {
    target: String,
    config_path: Option<String>,
    socket: PathBuf,
    leases: usize,
}

pub(super) struct SshLease {
    socket: PathBuf,
}

impl Drop for SshLease {
    fn drop(&mut self) {
        release_control_master(&self.socket);
    }
}

static SSH_MASTERS: OnceLock<Mutex<HashMap<PathBuf, SshMaster>>> = OnceLock::new();

fn ssh_masters() -> &'static Mutex<HashMap<PathBuf, SshMaster>> {
    SSH_MASTERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn spawn_bridge(connection: &ConnectionSpec, _client_id: &str) -> Result<Child, String> {
    connection.validate()?;
    let mut command = match connection {
        ConnectionSpec::Local => {
            let mut command = Command::new(host_helper_path()?);
            command.args(["bridge", "--stdio"]);
            command
        }
        ConnectionSpec::Ssh {
            profile_id,
            target,
            config_path,
        } => {
            let socket = ssh_profile_control_socket(profile_id, target, config_path.as_deref())?;
            ensure_control_master(target, config_path.as_deref(), &socket)?;
            let mut command = ssh_base(config_path.as_deref());
            command
                .arg("-T")
                .arg("-S")
                .arg(socket)
                .arg(target)
                .arg("$HOME/.local/bin/tmux-ide-host bridge --stdio");
            command
        }
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Keep the child's stderr instead of discarding it. Without this, every
        // way a bridge or daemon can fail to start — an over-long AF_UNIX socket
        // path, a refused SSH key, a missing helper — reached the user as the
        // single generic string "host closed during handshake" (M10-E058).
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to start host bridge: {error}"))
}

/// Bound on retained bridge stderr. Large enough for a stack of `anyhow`
/// context lines or an OpenSSH refusal, small enough that a chatty or hostile
/// child cannot grow this buffer without limit.
const BRIDGE_STDERR_BYTES: usize = 2048;

/// Drains a bridge child's stderr on its own thread so the pipe can never fill
/// and block the child, retaining only the first [`BRIDGE_STDERR_BYTES`].
pub(super) struct BridgeStderr {
    buffer: Arc<Mutex<String>>,
    finished: Arc<Mutex<bool>>,
}

impl BridgeStderr {
    pub(super) fn capture(mut stderr: ChildStderr) -> Self {
        let buffer = Arc::new(Mutex::new(String::new()));
        let finished = Arc::new(Mutex::new(false));
        let writer = Arc::clone(&buffer);
        let done = Arc::clone(&finished);
        thread::spawn(move || {
            let mut raw = Vec::new();
            let mut chunk = [0_u8; 512];
            while let Ok(read) = stderr.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                if raw.len() < BRIDGE_STDERR_BYTES {
                    let room = BRIDGE_STDERR_BYTES - raw.len();
                    raw.extend_from_slice(&chunk[..read.min(room)]);
                }
            }
            *writer.lock().unwrap() = String::from_utf8_lossy(&raw).into_owned();
            *done.lock().unwrap() = true;
        });
        Self { buffer, finished }
    }

    /// Returns what the child wrote to stderr, waiting up to `grace` for it to
    /// flush. A failing child normally writes and exits immediately, so this
    /// wait is short; it exists only so a diagnostic is not lost to a race
    /// between the child's write and our own error path.
    pub(super) fn diagnostic(&self, grace: Duration) -> Option<String> {
        let deadline = Instant::now() + grace;
        loop {
            if *self.finished.lock().unwrap() {
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let text = self.buffer.lock().unwrap().trim().to_owned();
        (!text.is_empty()).then_some(text)
    }
}

/// Appends a bridge child's own stderr to a connection error, so a diagnosable
/// startup failure reaches the user as its real cause rather than as a generic
/// handshake message.
pub(super) fn with_bridge_diagnostic(error: String, stderr: Option<&BridgeStderr>) -> String {
    match stderr.and_then(|stderr| stderr.diagnostic(Duration::from_millis(300))) {
        Some(detail) => format!("{error}: {detail}"),
        None => error,
    }
}

pub(crate) fn spawn_bulk_bridge(connection: &ConnectionSpec) -> Result<Child, String> {
    connection.validate()?;
    let mut command = match connection {
        ConnectionSpec::Local => {
            let mut command = Command::new(host_helper_path()?);
            command.args(["bridge", "--stdio"]);
            command
        }
        ConnectionSpec::Ssh {
            target,
            config_path,
            ..
        } => {
            let mut command = ssh_base(config_path.as_deref());
            command
                .arg("-T")
                .args(["-o", "ControlMaster=no", "-o", "ControlPath=none"])
                .arg(target)
                .arg("$HOME/.local/bin/tmux-ide-host bridge --stdio");
            command
        }
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("failed to start independent bulk bridge: {error}"))
}

pub(super) fn acquire_control_master(
    connection: &ConnectionSpec,
) -> Result<Option<SshLease>, String> {
    let ConnectionSpec::Ssh {
        profile_id,
        target,
        config_path,
    } = connection
    else {
        return Ok(None);
    };
    let socket = ssh_profile_control_socket(profile_id, target, config_path.as_deref())?;
    ensure_control_master(target, config_path.as_deref(), &socket)?;
    let mut masters = ssh_masters().lock().unwrap();
    let master = masters
        .get_mut(&socket)
        .ok_or("SSH control master registry lost the acquired profile")?;
    master.leases += 1;
    Ok(Some(SshLease { socket }))
}

pub(super) fn host_helper_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("ADE_HOST_HELPER_PATH") {
        return Ok(path.into());
    }
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    if let Some(parent) = current.parent() {
        let sibling = parent.join("tmux-ide-host");
        if sibling.is_file() {
            return Ok(sibling);
        }
    }
    #[cfg(debug_assertions)]
    {
        for profile in ["debug", "release"] {
            let candidate = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../target")
                .join(profile)
                .join("tmux-ide-host");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(
        "tmux-ide-host helper is not installed beside the desktop; build or install the sidecar"
            .into(),
    )
}

// A Unix socket bind must fit `sockaddr_un::sun_path` including its terminator:
// 104 bytes on Darwin, 108 on Linux. OpenSSH binds a temporary sibling first,
// appending a dot and sixteen random characters before renaming it into place,
// so the published path needs that much extra headroom.
const CONTROL_SOCKET_PATH_BYTES: usize = if cfg!(target_os = "macos") { 104 } else { 108 };
const CONTROL_SOCKET_TEMPORARY_BYTES: usize = 17;

// Strict, because the bound counts the terminating NUL that must also fit.
fn control_socket_binds(socket: &Path) -> bool {
    socket.as_os_str().as_bytes().len() + CONTROL_SOCKET_TEMPORARY_BYTES < CONTROL_SOCKET_PATH_BYTES
}

// macOS gives every user a per-boot temporary directory roughly 49 bytes long,
// which leaves no room for the control socket. Fall back to the same short,
// uid-scoped root the helper already uses for its own runtime socket. The
// directory is still created 0700 and rejected unless this user owns it, so a
// pre-created path belonging to anyone else fails closed rather than downgrading.
const SHORT_CONTROL_ROOT: &str = "/tmp";

pub(super) fn ssh_profile_control_socket(
    profile_id: &str,
    target: &str,
    config_path: Option<&str>,
) -> Result<PathBuf, String> {
    let preferred = std::env::temp_dir();
    // A failure here is a real safety refusal, not a sizing problem, so it must
    // propagate instead of silently relocating the socket.
    let socket = ssh_profile_control_socket_in(&preferred, profile_id, target, config_path)?;
    if control_socket_binds(&socket) {
        return Ok(socket);
    }
    if preferred != Path::new(SHORT_CONTROL_ROOT) {
        let short = ssh_profile_control_socket_in(
            Path::new(SHORT_CONTROL_ROOT),
            profile_id,
            target,
            config_path,
        )?;
        if control_socket_binds(&short) {
            return Ok(short);
        }
    }
    Err(format!(
        "SSH control socket path does not fit this platform's {CONTROL_SOCKET_PATH_BYTES}-byte \
         limit: {}",
        socket.display()
    ))
}

fn ssh_profile_control_socket_in(
    temporary_root: &Path,
    profile_id: &str,
    target: &str,
    config_path: Option<&str>,
) -> Result<PathBuf, String> {
    validate_ssh_target(target)?;
    let base = temporary_root.join(format!("tmux-agent-ide-{}", unsafe { libc::geteuid() }));
    ensure_private_directory(&base)?;
    let directory = base.join("ssh");
    ensure_private_directory(&directory)?;
    let mut digest = Sha256::new();
    digest.update(profile_id.as_bytes());
    digest.update([0]);
    digest.update(target.as_bytes());
    digest.update([0]);
    if let Some(path) = config_path {
        digest.update(path.as_bytes());
    }
    let key = format!("{:x}", digest.finalize());
    Ok(directory.join(format!("profile-{}.sock", &key[..20])))
}

fn ensure_private_directory(directory: &Path) -> Result<(), String> {
    match fs::DirBuilder::new().mode(0o700).create(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.to_string()),
    }
    let metadata = fs::symlink_metadata(directory).map_err(|error| error.to_string())?;
    if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(
            "SSH control directory must be an owned, private, non-symlink directory".into(),
        );
    }
    if metadata.mode() & 0o077 != 0 {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
        let secured = fs::symlink_metadata(directory).map_err(|error| error.to_string())?;
        if !secured.file_type().is_dir()
            || secured.uid() != unsafe { libc::geteuid() }
            || secured.mode() & 0o077 != 0
        {
            return Err(
                "SSH control directory must be an owned, private, non-symlink directory".into(),
            );
        }
    }
    Ok(())
}

fn validate_control_socket(socket: &Path) -> Result<bool, String> {
    let metadata = match fs::symlink_metadata(socket) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err("refusing an unowned or unsafe SSH control-socket path".into());
    }
    Ok(true)
}

pub(super) fn ensure_control_master(
    target: &str,
    config_path: Option<&str>,
    socket: &Path,
) -> Result<(), String> {
    validate_control_socket(socket)?;
    let mut masters = ssh_masters().lock().unwrap();
    let check = ssh_base(config_path)
        .arg("-S")
        .arg(socket)
        .args(["-O", "check"])
        .arg(target)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if check.is_ok_and(|status| status.success()) {
        masters
            .entry(socket.to_owned())
            .or_insert_with(|| SshMaster {
                target: target.into(),
                config_path: config_path.map(ToOwned::to_owned),
                socket: socket.to_owned(),
                leases: 0,
            });
        return Ok(());
    }
    if validate_control_socket(socket)? {
        fs::remove_file(socket).map_err(|error| error.to_string())?;
    }
    let output = ssh_base(config_path)
        .args([
            "-M",
            "-N",
            "-f",
            "-o",
            "ControlMaster=yes",
            "-o",
            "ControlPersist=60",
        ])
        .arg("-S")
        .arg(socket)
        .arg(target)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        if !validate_control_socket(socket)? {
            return Err("OpenSSH succeeded without creating its private control socket".into());
        }
        masters.insert(
            socket.to_owned(),
            SshMaster {
                target: target.into(),
                config_path: config_path.map(ToOwned::to_owned),
                socket: socket.to_owned(),
                leases: 0,
            },
        );
        Ok(())
    } else {
        Err(format!(
            "OpenSSH control master failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn release_control_master(socket: &Path) {
    let master = {
        let mut masters = ssh_masters().lock().unwrap();
        let Some(master) = masters.get_mut(socket) else {
            return;
        };
        master.leases = master.leases.saturating_sub(1);
        if master.leases != 0 {
            return;
        }
        masters.remove(socket)
    };
    if let Some(master) = master {
        let _ = ssh_base(master.config_path.as_deref())
            .arg("-S")
            .arg(&master.socket)
            .args(["-O", "exit"])
            .arg(&master.target)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = fs::remove_file(master.socket);
    }
}

pub(crate) fn close_all_control_masters() {
    let masters: Vec<_> = ssh_masters()
        .lock()
        .unwrap()
        .drain()
        .map(|(_, value)| value)
        .collect();
    for master in masters {
        let _ = ssh_base(master.config_path.as_deref())
            .arg("-S")
            .arg(&master.socket)
            .args(["-O", "exit"])
            .arg(&master.target)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = fs::remove_file(master.socket);
    }
}

fn ssh_base(config_path: Option<&str>) -> Command {
    let mut command = Command::new("ssh");
    if let Some(path) = config_path {
        command.arg("-F").arg(path);
    }
    command.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ServerAliveInterval=1",
        "-o",
        "ServerAliveCountMax=2",
    ]);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_socket_identity_is_profile_scoped() {
        let first = ssh_profile_control_socket("profile-a", "same-host", None).unwrap();
        let second = ssh_profile_control_socket("profile-b", "same-host", None).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            first,
            ssh_profile_control_socket("profile-a", "same-host", None).unwrap()
        );
    }

    #[test]
    fn control_socket_fits_the_platform_bind_limit_from_the_real_temporary_root() {
        let socket = ssh_profile_control_socket("profile-a", "same-host", None).unwrap();
        assert!(
            control_socket_binds(&socket),
            "{} leaves no room for OpenSSH's temporary bind",
            socket.display()
        );
    }

    #[test]
    fn control_socket_relocates_when_the_temporary_root_is_too_long() {
        let temporary = tempfile::tempdir().unwrap();
        // Reproduce a macOS-length per-user temporary root, which alone pushes
        // the control socket past the 104-byte Darwin bind limit.
        let deep = temporary.path().join("a".repeat(80));
        fs::create_dir(&deep).unwrap();
        let direct = ssh_profile_control_socket_in(&deep, "profile-a", "same-host", None).unwrap();
        assert!(!control_socket_binds(&direct));
        let resolved = ssh_profile_control_socket("profile-a", "same-host", None).unwrap();
        assert!(control_socket_binds(&resolved));
    }

    #[test]
    fn control_directory_rejects_symlink_and_repairs_owned_legacy_mode() {
        let temporary = tempfile::tempdir().unwrap();
        let uid_root = temporary
            .path()
            .join(format!("tmux-agent-ide-{}", unsafe { libc::geteuid() }));
        let foreign = temporary.path().join("foreign");
        fs::create_dir(&foreign).unwrap();
        std::os::unix::fs::symlink(&foreign, &uid_root).unwrap();
        assert!(ssh_profile_control_socket_in(temporary.path(), "p", "host", None).is_err());
        fs::remove_file(&uid_root).unwrap();
        fs::create_dir(&uid_root).unwrap();
        fs::set_permissions(&uid_root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ssh_profile_control_socket_in(temporary.path(), "p", "host", None).is_ok());
        assert_eq!(
            fs::metadata(uid_root).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn control_socket_rejects_regular_files_and_symlinks() {
        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("mux.sock");
        fs::write(&socket, b"foreign").unwrap();
        assert!(validate_control_socket(&socket).is_err());
        fs::remove_file(&socket).unwrap();
        std::os::unix::fs::symlink("missing", &socket).unwrap();
        assert!(validate_control_socket(&socket).is_err());
    }
}
