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
    PathBuf::from(format!("/tmp/tmux-agent-ide-{}", unsafe {
        libc::geteuid()
    }))
}

pub fn default_socket_path() -> PathBuf {
    default_runtime_dir().join("host.sock")
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
    fn runtime_directory_is_private() {
        let root = std::env::temp_dir().join(format!("ade-path-test-{}", uuid::Uuid::new_v4()));
        prepare_runtime_dir(&root).unwrap();
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir(root).unwrap();
    }
}
