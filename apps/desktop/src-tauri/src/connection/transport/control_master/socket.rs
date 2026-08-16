use super::*;

// A Unix socket bind must fit `sockaddr_un::sun_path` including its terminator:
// 104 bytes on Darwin, 108 on Linux. OpenSSH binds a temporary sibling first,
// appending a dot and sixteen random characters before renaming it into place,
// so the published path needs that much extra headroom.
pub(super) const CONTROL_SOCKET_PATH_BYTES: usize =
    if cfg!(target_os = "macos") { 104 } else { 108 };
pub(super) const CONTROL_SOCKET_TEMPORARY_BYTES: usize = 17;

// Strict, because the bound counts the terminating NUL that must also fit.
pub(super) fn control_socket_binds(socket: &Path) -> bool {
    socket.as_os_str().as_bytes().len() + CONTROL_SOCKET_TEMPORARY_BYTES < CONTROL_SOCKET_PATH_BYTES
}

// macOS gives every user a per-boot temporary directory roughly 49 bytes long,
// which leaves no room for the control socket. Fall back to the same short,
// uid-scoped root the helper already uses for its own runtime socket. The
// directory is still created 0700 and rejected unless this user owns it, so a
// pre-created path belonging to anyone else fails closed rather than downgrading.
const SHORT_CONTROL_ROOT: &str = "/tmp";

pub(in crate::connection) fn ssh_profile_control_socket(
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

pub(super) fn ssh_profile_control_socket_in(
    temporary_root: &Path,
    profile_id: &str,
    target: &str,
    config_path: Option<&str>,
) -> Result<PathBuf, String> {
    validate_ssh_target(target)?;
    let base = temporary_root.join(format!("tmux-agent-ide-{}", unsafe { libc::geteuid() }));
    ensure_private_directory(&base)?;
    let directory = base.join(process_socket_namespace());
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

fn process_socket_namespace() -> &'static str {
    static NAMESPACE: OnceLock<String> = OnceLock::new();
    NAMESPACE.get_or_init(|| {
        let nonce = Uuid::new_v4().simple().to_string();
        format!("ssh-{}-{}", std::process::id(), &nonce[..8])
    })
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

pub(super) fn validate_control_socket(socket: &Path) -> Result<bool, String> {
    validated_control_socket_identity(socket).map(|identity| identity.is_some())
}

pub(super) fn validated_control_socket_identity(
    socket: &Path,
) -> Result<Option<SocketIdentity>, String> {
    let metadata = match fs::symlink_metadata(socket) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err("refusing an unowned or unsafe SSH control-socket path".into());
    }
    Ok(Some(SocketIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }))
}
