use std::{
    fs,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};

pub fn default_runtime_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("ADE_HOST_RUNTIME_DIR") {
        return PathBuf::from(path);
    }
    if let Some(path) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(path).join("tmux-agent-ide");
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join("Library/Caches/dev.dev.tmux-agent-ide/runtime");
    }
    PathBuf::from(format!("/tmp/tmux-agent-ide-{}", unsafe {
        libc::geteuid()
    }))
}

pub fn default_socket_path() -> PathBuf {
    default_runtime_dir().join("host.sock")
}

/// A Unix socket bind must fit `sockaddr_un::sun_path` including its
/// terminator: 104 bytes on Darwin, 108 on Linux. The default runtime
/// directory is short, but `ADE_HOST_RUNTIME_DIR` and `XDG_RUNTIME_DIR` can
/// both point somewhere deep, and the kernel's own refusal
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
    fn runtime_directory_is_private() {
        let root = std::env::temp_dir().join(format!("ade-path-test-{}", uuid::Uuid::new_v4()));
        prepare_runtime_dir(&root).unwrap();
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_default_runtime_uses_user_cache_instead_of_shared_tmp() {
        if std::env::var_os("ADE_HOST_RUNTIME_DIR").is_none()
            && std::env::var_os("XDG_RUNTIME_DIR").is_none()
            && let Some(home) = std::env::var_os("HOME")
        {
            assert_eq!(
                default_runtime_dir(),
                PathBuf::from(home).join("Library/Caches/dev.dev.tmux-agent-ide/runtime")
            );
        }
    }
}
