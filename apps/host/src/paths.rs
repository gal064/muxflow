use std::{
    ffi::{CStr, OsStr, OsString},
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};

fn environment(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

pub fn default_runtime_dir() -> PathBuf {
    resolved_runtime_dir(environment, unsafe { libc::geteuid() })
}

fn resolved_runtime_dir(
    environment: impl Fn(&str) -> Option<OsString>,
    uid: libc::uid_t,
) -> PathBuf {
    if let Some(path) = environment("ADE_HOST_RUNTIME_DIR") {
        return PathBuf::from(path);
    }
    // Deliberately literal rather than `temp_dir()`: TMPDIR can differ between
    // an SSH command, a login shell and a tmux hook just like XDG_RUNTIME_DIR.
    // Only the socket and process-local diagnostics live here; the installed
    // executable and durable state do not.
    PathBuf::from(format!("/tmp/muxflow-{uid}"))
}

pub fn default_socket_path() -> PathBuf {
    default_runtime_dir().join("host.sock")
}

/// The temporary communication and durable state roots adopted by this daemon.
/// An explicit socket keeps both together for fixture isolation; production
/// separates the short-lived endpoint from state that survives restarts.
static ADOPTED_RUNTIME_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
static ADOPTED_STATE_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn adopt_socket_path(socket: &Path) -> anyhow::Result<()> {
    let runtime = socket
        .parent()
        .context("daemon socket has no parent directory")?;
    let state = state_dir_for_socket(socket)?;
    let _ = ADOPTED_RUNTIME_DIR.set(runtime.to_path_buf());
    let _ = ADOPTED_STATE_DIR.set(state);
    Ok(())
}

fn state_dir_for_socket(socket: &Path) -> anyhow::Result<PathBuf> {
    if socket == default_socket_path() {
        return default_state_dir();
    }
    // An explicit --socket and ADE_HOST_RUNTIME_DIR are isolation tools. Their
    // state stays beside their socket rather than touching production state.
    Ok(socket
        .parent()
        .context("daemon socket has no parent directory")?
        .to_path_buf())
}

pub fn runtime_dir() -> PathBuf {
    ADOPTED_RUNTIME_DIR
        .get()
        .cloned()
        .unwrap_or_else(default_runtime_dir)
}

pub fn default_state_dir() -> anyhow::Result<PathBuf> {
    if let Some(path) = environment("ADE_HOST_RUNTIME_DIR") {
        return Ok(PathBuf::from(path));
    }
    resolved_state_dir(
        environment,
        account_home_dir(unsafe { libc::geteuid() })?,
        cfg!(target_os = "macos"),
    )
}

fn resolved_state_dir(
    environment: impl Fn(&str) -> Option<OsString>,
    account_home: PathBuf,
    macos: bool,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = environment("ADE_HOST_RUNTIME_DIR") {
        return Ok(PathBuf::from(path));
    }
    Ok(if macos {
        account_home.join("Library/Application Support/dev.muxflow.desktop")
    } else {
        account_home.join(".local/state/muxflow")
    })
}

/// The effective account's home, independent of shell environment. OpenSSH,
/// GUI launchers and tmux hooks can inherit different (or no) `HOME`; the
/// account database is the machine-wide answer all of them share.
fn account_home_dir(uid: libc::uid_t) -> anyhow::Result<PathBuf> {
    let configured = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let mut size = usize::try_from(configured)
        .unwrap_or(16 * 1024)
        .clamp(1024, 1024 * 1024);
    loop {
        let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0_u8; size];
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                record.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE && size < 1024 * 1024 {
            size *= 2;
            continue;
        }
        if status != 0 {
            return Err(std::io::Error::from_raw_os_error(status))
                .context("resolve effective account home");
        }
        if result.is_null() {
            bail!("effective user {uid} has no account home");
        }
        let record = unsafe { record.assume_init() };
        if record.pw_dir.is_null() {
            bail!("effective user {uid} has no account home");
        }
        let bytes = unsafe { CStr::from_ptr(record.pw_dir) }.to_bytes();
        if bytes.is_empty() {
            bail!("effective user {uid} has an empty account home");
        }
        return Ok(PathBuf::from(OsStr::from_bytes(bytes)));
    }
}

pub fn state_dir() -> PathBuf {
    ADOPTED_STATE_DIR
        .get()
        .cloned()
        .unwrap_or_else(|| default_state_dir().expect("resolve effective account state directory"))
}

/// Where voice mode keeps the sidecar script, its log and the 697 MiB speech
/// model (docs/mobile/voice-mode-plan.md §4.1): `MUXFLOW_VOICE_CACHE_DIR`, else
/// `$XDG_CACHE_HOME/muxflow/voice`, else `~/.cache/muxflow/voice` (macOS
/// `~/Library/Caches/dev.muxflow.desktop/voice`). Nothing here is created until
/// the first voice request; `prepare_voice_cache_dir` makes it 0700.
pub fn voice_cache_dir() -> PathBuf {
    resolved_voice_cache_dir(environment)
}

fn resolved_voice_cache_dir(environment: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    if let Some(path) = environment("MUXFLOW_VOICE_CACHE_DIR") {
        return PathBuf::from(path);
    }
    if let Some(path) = environment("XDG_CACHE_HOME") {
        return PathBuf::from(path).join("muxflow/voice");
    }
    let home = environment("HOME").map(PathBuf::from).unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join("Library/Caches/dev.muxflow.desktop/voice")
    } else {
        home.join(".cache/muxflow/voice")
    }
}

pub fn prepare_voice_cache_dir(path: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// A Unix socket bind must fit `sockaddr_un::sun_path` including its
/// terminator: 104 bytes on Darwin, 108 on Linux. The default runtime
/// directory is short, but `ADE_HOST_RUNTIME_DIR` can point somewhere deep,
/// and the kernel's own refusal
/// (`path must be shorter than SUN_LEN`) never named the limit or the length.
/// This is the same bound the desktop already enforces for SSH control sockets
/// (M10-E040); M10-E058 is it going unchecked on the daemon's own socket.
const SOCKET_PATH_BYTES: usize = if cfg!(target_os = "macos") { 104 } else { 108 };

pub fn check_socket_path_length(path: &Path) -> anyhow::Result<()> {
    let length = path.as_os_str().as_encoded_bytes().len();
    if length >= SOCKET_PATH_BYTES {
        bail!(
            "daemon socket path is {length} bytes, but this platform allows at most \
             {} including the terminator: {}. Set ADE_HOST_RUNTIME_DIR to a shorter directory.",
            SOCKET_PATH_BYTES - 1,
            path.display()
        );
    }
    Ok(())
}

pub fn prepare_runtime_dir(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("runtime path is not a real directory: {}", path.display());
        }
    } else {
        fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    let mode = fs::metadata(path)?.permissions().mode() & 0o777;
    if mode != 0o700 {
        bail!("runtime directory must be mode 0700, got {mode:04o}");
    }
    Ok(())
}

pub fn ensure_private_socket(path: &Path) -> anyhow::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() {
        bail!("daemon endpoint is not a Unix socket: {}", path.display());
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o600 {
        bail!("daemon socket must be mode 0600, got {mode:04o}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_within_the_platform_bound_is_accepted() {
        let path = PathBuf::from(format!(
            "/tmp/{}/host.sock",
            "a".repeat(SOCKET_PATH_BYTES - "/tmp//host.sock".len() - 1)
        ));
        assert_eq!(
            path.as_os_str().as_encoded_bytes().len(),
            SOCKET_PATH_BYTES - 1
        );
        check_socket_path_length(&path).unwrap();
    }

    #[test]
    fn socket_path_at_or_over_the_bound_is_refused_with_the_limit_and_length() {
        let path = PathBuf::from(format!(
            "/tmp/{}/host.sock",
            "a".repeat(SOCKET_PATH_BYTES - "/tmp//host.sock".len())
        ));
        let error = check_socket_path_length(&path).unwrap_err().to_string();
        assert!(error.contains(&SOCKET_PATH_BYTES.to_string()), "{error}");
        assert!(error.contains("ADE_HOST_RUNTIME_DIR"), "{error}");
    }

    #[test]
    fn voice_cache_dir_prefers_the_override_then_xdg_then_home() {
        let environment = |name: &str| match name {
            "MUXFLOW_VOICE_CACHE_DIR" => Some(OsString::from("/pinned/voice")),
            "XDG_CACHE_HOME" => Some(OsString::from("/xdg")),
            "HOME" => Some(OsString::from("/home/someone")),
            _ => None,
        };
        assert_eq!(
            resolved_voice_cache_dir(environment),
            PathBuf::from("/pinned/voice")
        );
        let xdg = |name: &str| match name {
            "XDG_CACHE_HOME" => Some(OsString::from("/xdg")),
            "HOME" => Some(OsString::from("/home/someone")),
            _ => None,
        };
        assert_eq!(
            resolved_voice_cache_dir(xdg),
            PathBuf::from("/xdg/muxflow/voice")
        );
        let home_only = |name: &str| match name {
            "HOME" => Some(OsString::from("/home/someone")),
            _ => None,
        };
        let expected = if cfg!(target_os = "macos") {
            "/home/someone/Library/Caches/dev.muxflow.desktop/voice"
        } else {
            "/home/someone/.cache/muxflow/voice"
        };
        assert_eq!(resolved_voice_cache_dir(home_only), PathBuf::from(expected));
    }

    #[test]
    fn runtime_directory_is_private() {
        let root = std::env::temp_dir().join(format!("ade-path-test-{}", uuid::Uuid::new_v4()));
        prepare_runtime_dir(&root).unwrap();
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn production_runtime_is_literal_tmp_regardless_of_shell_environment() {
        let with_xdg = |name: &str| match name {
            "XDG_RUNTIME_DIR" => Some(OsString::from("/run/user/4242")),
            "TMPDIR" => Some(OsString::from("/private/session/tmp")),
            _ => None,
        };
        assert_eq!(
            resolved_runtime_dir(with_xdg, 4242),
            PathBuf::from("/tmp/muxflow-4242")
        );
        assert_eq!(
            resolved_runtime_dir(|_| None, 4242),
            PathBuf::from("/tmp/muxflow-4242")
        );
    }

    #[test]
    fn runtime_override_is_the_one_development_root() {
        let pinned = |name: &str| match name {
            "ADE_HOST_RUNTIME_DIR" => Some(OsString::from("/fixture/runtime")),
            "HOME" => Some(OsString::from("/home/someone")),
            "XDG_RUNTIME_DIR" => Some(OsString::from("/run/user/7")),
            _ => None,
        };
        assert_eq!(
            resolved_runtime_dir(pinned, 7),
            PathBuf::from("/fixture/runtime")
        );
        assert_eq!(
            resolved_state_dir(pinned, PathBuf::from("/account/home"), false).unwrap(),
            PathBuf::from("/fixture/runtime")
        );
    }

    #[test]
    fn a_custom_socket_in_the_production_runtime_still_isolates_state() {
        let runtime = default_runtime_dir();
        let socket = runtime.join("development.sock");
        assert_ne!(socket, default_socket_path());
        assert_eq!(state_dir_for_socket(&socket).unwrap(), runtime);
    }

    #[test]
    fn durable_state_uses_platform_account_home_conventions() {
        let environment = |name: &str| match name {
            "HOME" => Some(OsString::from("/misleading/inherited-home")),
            _ => None,
        };
        assert_eq!(
            resolved_state_dir(environment, PathBuf::from("/home/someone"), false).unwrap(),
            PathBuf::from("/home/someone/.local/state/muxflow")
        );
        assert_eq!(
            resolved_state_dir(environment, PathBuf::from("/home/someone"), true).unwrap(),
            PathBuf::from("/home/someone/Library/Application Support/dev.muxflow.desktop")
        );
    }
}
