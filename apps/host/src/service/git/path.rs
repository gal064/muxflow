use std::{
    ffi::{CString, OsStr, OsString},
    fs::{File, Metadata},
    io::Read,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileExt, MetadataExt},
        io::{AsRawFd, FromRawFd},
    },
    path::{Component, Path},
};

use anyhow::{Context, bail};

use super::validate_git_path;

/// A repository-root capability used for every direct worktree operation.
/// Parent components are opened one at a time with `O_NOFOLLOW`, so replacing
/// a pathname with an outside-root symlink cannot retarget a read or unlink.
pub(super) struct WorktreeRoot {
    directory: File,
}

pub(super) struct WorktreeEntry {
    parent: File,
    leaf: OsString,
}

impl WorktreeRoot {
    pub(super) fn capture(root: &str) -> anyhow::Result<Self> {
        let path = CString::new(root.as_bytes()).context("repository root contains NUL")?;
        let descriptor_backed = root.starts_with("/proc/self/fd/") || root.starts_with("/dev/fd/");
        // SAFETY: `path` is a live C string and open returns a new owned fd.
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY
                    | libc::O_DIRECTORY
                    | if descriptor_backed {
                        0
                    } else {
                        libc::O_NOFOLLOW
                    },
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("repository root is unavailable");
        }
        // Keep the capability available across exec so every Git child can
        // resolve its descriptor-backed `-C` path. The fd is read-only and is
        // closed with this operation-scoped capability.
        // SAFETY: fd is live and F_SETFD mutates only its close-on-exec flag.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, 0) } < 0 {
            let error = std::io::Error::last_os_error();
            // SAFETY: ownership has not yet moved into File.
            unsafe { libc::close(fd) };
            return Err(error).context("failed to retain repository capability for Git");
        }
        Ok(Self {
            // SAFETY: the successful `open` returned a uniquely owned fd.
            directory: unsafe { File::from_raw_fd(fd) },
        })
    }

    pub(super) fn stable_path(&self) -> String {
        descriptor_path(self.directory.as_raw_fd())
            .to_string_lossy()
            .into_owned()
    }

    pub(super) fn try_clone(&self) -> anyhow::Result<Self> {
        let directory = self.directory.try_clone()?;
        retain_across_exec(directory.as_raw_fd())?;
        Ok(Self { directory })
    }

    pub(super) fn validate_token(&self, logical_root: &str, expected: &str) -> anyhow::Result<()> {
        let metadata = self.directory.metadata()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(super::super::snapshot::server_identity().as_bytes());
        hasher.update(logical_root.as_bytes());
        hasher.update(&metadata.dev().to_le_bytes());
        hasher.update(&metadata.ino().to_le_bytes());
        if expected.is_empty() || expected != hasher.finalize().to_hex().as_str() {
            bail!("root snapshot token does not match the requested root");
        }
        Ok(())
    }

    pub(super) fn identity(&self) -> anyhow::Result<(u64, u64)> {
        let metadata = self.directory.metadata()?;
        Ok((metadata.dev(), metadata.ino()))
    }

    pub(super) fn entry(&self, path: &[u8]) -> anyhow::Result<WorktreeEntry> {
        validate_git_path(path)?;
        let value = Path::new(OsStr::from_bytes(path));
        let leaf = value
            .file_name()
            .context("repository root itself cannot be addressed")?
            .to_owned();
        let mut parent = self.directory.try_clone()?;
        retain_across_exec(parent.as_raw_fd())?;
        for component in value.parent().unwrap_or_else(|| Path::new("")).components() {
            let Component::Normal(name) = component else {
                bail!("Git path contains an unsafe component");
            };
            parent = open_directory(parent.as_raw_fd(), name.as_bytes())?;
        }
        Ok(WorktreeEntry { parent, leaf })
    }
}

impl WorktreeEntry {
    pub(super) fn open_regular_for_git(&self) -> anyhow::Result<File> {
        let leaf = CString::new(self.leaf.as_bytes()).context("Git path contains NUL")?;
        // SAFETY: openat uses a live parent and O_NOFOLLOW pins only a regular leaf.
        let fd = unsafe {
            libc::openat(
                self.parent.as_raw_fd(),
                leaf.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("worktree file capability is unavailable");
        }
        if let Err(error) = retain_across_exec(fd) {
            // SAFETY: ownership has not moved into File.
            unsafe { libc::close(fd) };
            return Err(error);
        }
        // SAFETY: successful open returned a uniquely owned fd.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub(super) fn metadata(&self) -> anyhow::Result<Option<Metadata>> {
        let path = CString::new(self.leaf.as_bytes()).context("Git path contains NUL")?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the fd, C string, and output pointer remain live for fstatat.
        let result = unsafe {
            libc::fstatat(
                self.parent.as_raw_fd(),
                path.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(error.into());
        }
        // Rust does not expose a constructor from stat, so obtain Metadata
        // through the descriptor-bound proc path after the no-follow lookup.
        // The held parent fd makes this lookup immune to ancestor swaps.
        let stable = descriptor_path(self.parent.as_raw_fd()).join(&self.leaf);
        match std::fs::symlink_metadata(stable) {
            Ok(metadata) => Ok(Some(metadata)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn is_directory(&self) -> anyhow::Result<bool> {
        Ok(self
            .metadata()?
            .is_some_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink()))
    }

    pub(super) fn read(&self, maximum: Option<usize>) -> anyhow::Result<Option<Vec<u8>>> {
        let Some(metadata) = self.metadata()? else {
            return Ok(None);
        };
        if metadata.file_type().is_symlink() {
            return Ok(Some(self.read_link()?));
        }
        if metadata.is_dir() {
            return Ok(Some(Vec::new()));
        }
        if !metadata.is_file() {
            bail!("worktree entry is not a regular file or symlink");
        }
        if maximum.is_some_and(|limit| metadata.len() > limit as u64) {
            bail!("worktree entry exceeds the bounded content limit");
        }
        let leaf = CString::new(self.leaf.as_bytes()).context("Git path contains NUL")?;
        // SAFETY: openat uses a live parent fd and C string and returns an owned fd.
        let fd = unsafe {
            libc::openat(
                self.parent.as_raw_fd(),
                leaf.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(error.into());
        }
        // SAFETY: the successful `openat` returned a uniquely owned fd.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let mut content = Vec::new();
        if let Some(limit) = maximum {
            file.take(limit.saturating_add(1) as u64)
                .read_to_end(&mut content)?;
            if content.len() > limit {
                bail!("worktree entry exceeds the bounded content limit");
            }
        } else {
            file.read_to_end(&mut content)?;
        }
        Ok(Some(content))
    }

    pub(super) fn hash_identity(
        &self,
        hasher: &mut blake3::Hasher,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> anyhow::Result<()> {
        let Some(metadata) = self.metadata()? else {
            hasher.update(b"missing\0");
            return Ok(());
        };
        hasher.update(&metadata.mode().to_le_bytes());
        hasher.update(&metadata.len().to_le_bytes());
        hasher.update(&metadata.mtime().to_le_bytes());
        hasher.update(&metadata.mtime_nsec().to_le_bytes());
        hasher.update(&metadata.ctime().to_le_bytes());
        hasher.update(&metadata.ctime_nsec().to_le_bytes());
        if metadata.file_type().is_symlink() {
            hasher.update(&self.read_link()?);
            let after = self
                .metadata()?
                .context("worktree entry changed during status refresh")?;
            ensure_same_snapshot(&metadata, &after)?;
        } else if metadata.is_file() {
            let leaf = CString::new(self.leaf.as_bytes()).context("Git path contains NUL")?;
            // SAFETY: openat uses a live parent fd and returns an owned fd.
            let fd = unsafe {
                libc::openat(
                    self.parent.as_raw_fd(),
                    leaf.as_ptr(),
                    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error()).context("worktree entry changed");
            }
            // SAFETY: the successful `openat` returned a uniquely owned fd.
            let file = unsafe { File::from_raw_fd(fd) };
            let mut offsets = vec![0_u64];
            if metadata.len() > 64 * 1024 {
                offsets.push(metadata.len().saturating_div(2).saturating_sub(32 * 1024));
                offsets.push(metadata.len().saturating_sub(64 * 1024));
            }
            offsets.sort_unstable();
            offsets.dedup();
            for offset in offsets {
                if cancellation.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
                {
                    bail!("Git status refresh cancelled");
                }
                let mut buffer = [0_u8; 64 * 1024];
                let read = file.read_at(&mut buffer, offset)?;
                hasher.update(&offset.to_le_bytes());
                hasher.update(&buffer[..read]);
            }
            ensure_same_snapshot(&metadata, &file.metadata()?)?;
        }
        Ok(())
    }

    pub(super) fn sample_is_binary(&self) -> anyhow::Result<bool> {
        let Some(metadata) = self.metadata()? else {
            return Ok(false);
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Ok(false);
        }
        let leaf = CString::new(self.leaf.as_bytes()).context("Git path contains NUL")?;
        // SAFETY: openat uses a live parent fd and returns an owned fd.
        let fd = unsafe {
            libc::openat(
                self.parent.as_raw_fd(),
                leaf.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("worktree entry changed");
        }
        // SAFETY: the successful `openat` returned a uniquely owned fd.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let mut sample = [0_u8; 8192];
        let read = file.read(&mut sample)?;
        Ok(sample[..read].contains(&0))
    }

    pub(super) fn unlink_file(&self) -> anyhow::Result<()> {
        if self.is_directory()? {
            bail!("refusing to discard an untracked directory as one file");
        }
        let leaf = CString::new(self.leaf.as_bytes()).context("Git path contains NUL")?;
        // SAFETY: unlinkat never follows the leaf and both arguments remain live.
        if unsafe { libc::unlinkat(self.parent.as_raw_fd(), leaf.as_ptr(), 0) } < 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to discard worktree entry");
        }
        Ok(())
    }

    fn read_link(&self) -> anyhow::Result<Vec<u8>> {
        let leaf = CString::new(self.leaf.as_bytes()).context("Git path contains NUL")?;
        let mut size = 256_usize;
        loop {
            let mut bytes = vec![0_u8; size];
            // SAFETY: readlinkat writes at most `bytes.len()` bytes to this live buffer.
            let read = unsafe {
                libc::readlinkat(
                    self.parent.as_raw_fd(),
                    leaf.as_ptr(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if read < 0 {
                return Err(std::io::Error::last_os_error()).context("failed to read symlink");
            }
            let read = read as usize;
            if read < bytes.len() {
                bytes.truncate(read);
                return Ok(bytes);
            }
            size = size.checked_mul(2).context("symlink target is too large")?;
        }
    }
}

fn ensure_same_snapshot(before: &Metadata, after: &Metadata) -> anyhow::Result<()> {
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.mode() != after.mode()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        bail!("worktree entry changed during status refresh");
    }
    Ok(())
}

fn open_directory(parent: i32, name: &[u8]) -> anyhow::Result<File> {
    let name = CString::new(name).context("Git path contains NUL")?;
    // SAFETY: openat uses a live parent fd and C string and returns an owned fd.
    let fd = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .context("Git path parent is unavailable or unsafe");
    }
    // SAFETY: the successful `openat` returned a uniquely owned fd.
    if let Err(error) = retain_across_exec(fd) {
        // SAFETY: ownership has not moved into File.
        unsafe { libc::close(fd) };
        return Err(error);
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn retain_across_exec(fd: i32) -> anyhow::Result<()> {
    // SAFETY: fd is live and F_SETFD mutates only its close-on-exec flag.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, 0) } < 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to retain descriptor capability for Git");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn descriptor_path(fd: i32) -> std::path::PathBuf {
    format!("/proc/self/fd/{fd}").into()
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(super) fn descriptor_path(fd: i32) -> std::path::PathBuf {
    format!("/dev/fd/{fd}").into()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
compile_error!("descriptor-relative Git worktree access requires Linux or Apple platforms");
