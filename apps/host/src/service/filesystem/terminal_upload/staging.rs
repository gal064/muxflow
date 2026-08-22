use std::{
    ffi::{CString, OsStr, OsString},
    fs::{self, File, Metadata},
    os::unix::{
        ffi::OsStrExt,
        fs::MetadataExt,
        io::{AsRawFd, FromRawFd},
    },
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};

use super::super::path_policy::descriptor_path;

pub(super) struct StagingDirectory {
    file: File,
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl StagingDirectory {
    #[cfg(test)]
    pub(super) fn ensure_cache_path(path: &Path) -> anyhow::Result<()> {
        let parent = path.parent().context("cache path has no parent")?;
        let name = path.file_name().context("cache path has no basename")?;
        let parent = open_directory_path(parent, "cache parent parent")?;
        validate_owned_directory(&parent, "cache parent parent")?;
        let _ = open_or_create_child(&parent, name, false, "cache parent")?;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn open(path: &Path) -> anyhow::Result<Self> {
        let path_c =
            CString::new(path.as_os_str().as_bytes()).context("staging path contains NUL")?;
        // SAFETY: path_c remains live and the returned descriptor is freshly owned.
        let fd = unsafe {
            libc::open(
                path_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("private staging directory changed");
        }
        // SAFETY: fd is freshly owned after successful open.
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            bail!("private staging directory is not owned by this user");
        }
        Ok(Self {
            file,
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub(super) fn open_upload_cache(home: &Path) -> anyhow::Result<Self> {
        if !home.is_absolute() {
            bail!("HOME must be absolute");
        }
        let home_fd = open_directory_path(home, "HOME")?;
        validate_owned_directory(&home_fd, "HOME")?;
        let cache = open_or_create_child(&home_fd, OsStr::new(".cache"), false, "cache parent")?;
        let app = open_or_create_child(&cache, OsStr::new("muxflow"), true, "private app cache")?;
        let uploads =
            open_or_create_child(&app, OsStr::new("uploads"), true, "private upload staging")?;
        let metadata = uploads.metadata()?;
        Ok(Self {
            file: uploads,
            path: home.join(".cache/muxflow/uploads"),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn name_max(&self) -> anyhow::Result<usize> {
        // SAFETY: the descriptor is a live directory and _PC_NAME_MAX does not
        // mutate memory.
        let value = unsafe { libc::fpathconf(self.file.as_raw_fd(), libc::_PC_NAME_MAX) };
        if value <= 0 {
            bail!("could not determine destination NAME_MAX");
        }
        usize::try_from(value).context("destination NAME_MAX is invalid")
    }

    pub(super) fn current_namespace_matches(&self) -> bool {
        fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
        })
    }

    pub(super) fn create_private(&self, name: &OsStr) -> anyhow::Result<File> {
        let name = c_name(name)?;
        // SAFETY: directory/name remain live and the returned descriptor is freshly owned.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("could not create private staging file");
        }
        // SAFETY: fd is freshly owned after successful openat.
        let file = unsafe { File::from_raw_fd(fd) };
        set_private_file_mode(&file)?;
        Ok(file)
    }

    pub(super) fn open_readonly(&self, name: &OsStr) -> anyhow::Result<File> {
        let name = c_name(name)?;
        // SAFETY: directory/name remain live and the returned descriptor is freshly owned.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("staging entry changed or is unsafe");
        }
        // SAFETY: fd is freshly owned after successful openat.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub(super) fn open_readwrite(&self, name: &OsStr) -> anyhow::Result<File> {
        let name = c_name(name)?;
        // SAFETY: directory/name remain live and the returned descriptor is freshly owned.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("staging entry changed or is unsafe");
        }
        // SAFETY: fd is freshly owned after successful openat.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub(super) fn metadata(&self, name: &OsStr) -> anyhow::Result<Metadata> {
        Ok(fs::symlink_metadata(
            descriptor_path(self.file.as_raw_fd()).join(name),
        )?)
    }

    pub(super) fn entries(&self) -> anyhow::Result<Vec<(OsString, Metadata)>> {
        let mut result = Vec::new();
        for entry in fs::read_dir(descriptor_path(self.file.as_raw_fd()))? {
            let entry = entry?;
            result.push((entry.file_name(), fs::symlink_metadata(entry.path())?));
        }
        Ok(result)
    }

    pub(super) fn unlink(&self, name: &OsStr) -> anyhow::Result<()> {
        let name = c_name(name)?;
        // SAFETY: directory/name remain live; unlinkat never follows the leaf.
        let result = unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) };
        if result < 0 {
            return Err(std::io::Error::last_os_error())
                .context("could not remove owned staging file");
        }
        Ok(())
    }

    pub(super) fn quarantine_and_delete(
        &self,
        name: &OsStr,
        expected_device: u64,
        expected_inode: u64,
    ) -> anyhow::Result<bool> {
        let quarantine = OsString::from(format!(
            ".tmux-agent-quarantine-{}.partial",
            uuid::Uuid::new_v4()
        ));
        self.rename_noreplace(name, &quarantine)?;
        let file = self.open_readonly(&quarantine)?;
        let metadata = file.metadata()?;
        if metadata.dev() != expected_device || metadata.ino() != expected_inode {
            return Ok(false);
        }
        self.unlink(&quarantine)?;
        Ok(true)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn exchange(&self, left: &OsStr, right: &OsStr) -> anyhow::Result<()> {
        let left = c_name(left)?;
        let right = c_name(right)?;
        // SAFETY: both names are descriptor-relative and remain live.
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                self.file.as_raw_fd(),
                left.as_ptr(),
                self.file.as_raw_fd(),
                right.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        };
        #[cfg(target_os = "macos")]
        let result = unsafe {
            libc::renameatx_np(
                self.file.as_raw_fd(),
                left.as_ptr(),
                self.file.as_raw_fd(),
                right.as_ptr(),
                libc::RENAME_SWAP,
            ) as libc::c_long
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error()).context("atomic staging exchange failed");
        }
        Ok(())
    }

    pub(super) fn rename_noreplace(&self, source: &OsStr, target: &OsStr) -> anyhow::Result<()> {
        let source = c_name(source)?;
        let target = c_name(target)?;
        let result = renameat_noreplace(
            self.file.as_raw_fd(),
            &source,
            self.file.as_raw_fd(),
            &target,
        );
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if matches!(
                error.raw_os_error(),
                Some(libc::ENOSYS) | Some(libc::EINVAL)
            ) {
                bail!("atomic no-replace rename is unavailable; refusing unsafe fallback");
            }
            return Err(error).context("atomic no-replace staging rename failed");
        }
        Ok(())
    }

    pub(super) fn available_bytes(&self) -> anyhow::Result<u64> {
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: file is a live directory descriptor and stats is initialized on success.
        if unsafe { libc::fstatvfs(self.file.as_raw_fd(), stats.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("could not inspect staging free space");
        }
        let stats = unsafe { stats.assume_init() };
        let available = u128::from(stats.f_bavail).saturating_mul(u128::from(stats.f_frsize));
        Ok(u64::try_from(available).unwrap_or(u64::MAX))
    }

    pub(super) fn sync(&self) -> anyhow::Result<()> {
        Ok(self.file.sync_all()?)
    }
}

fn open_directory_path(path: &Path, label: &str) -> anyhow::Result<File> {
    let path = CString::new(path.as_os_str().as_bytes()).context("directory path contains NUL")?;
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("could not open {label}"));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn validate_owned_directory(file: &File, label: &str) -> anyhow::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        bail!("{label} is not an owned directory");
    }
    Ok(())
}

fn open_or_create_child(
    parent: &File,
    name: &OsStr,
    enforce_private_mode: bool,
    label: &str,
) -> anyhow::Result<File> {
    let name = c_name(name)?;
    let mut created = false;
    let mut fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error).with_context(|| format!("could not create {label}"));
            }
        } else {
            created = true;
        }
        fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
    }
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("could not open {label}"));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    validate_owned_directory(&file, label)?;
    if (created || enforce_private_mode) && unsafe { libc::fchmod(file.as_raw_fd(), 0o700) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("could not secure {label}"));
    }
    Ok(file)
}

pub(super) fn lock_exclusive(file: &File) -> anyhow::Result<()> {
    // SAFETY: file is a live descriptor; flock does not change ownership.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error()).context("could not lock owned staging file");
    }
    Ok(())
}

pub(super) fn lock_shared(file: &File) -> anyhow::Result<()> {
    // SAFETY: file is a live descriptor; flock does not change ownership.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error()).context("could not lock owned staging file");
    }
    Ok(())
}

pub(super) fn try_lock_exclusive(file: &File) -> anyhow::Result<bool> {
    // SAFETY: file is a live descriptor; flock does not change ownership.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(false)
    } else {
        Err(error).context("could not inspect staging-file activity")
    }
}

pub(super) fn set_private_file_mode(file: &File) -> anyhow::Result<()> {
    // SAFETY: file is a live descriptor and 0600 is the required final mode.
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
        return Err(std::io::Error::last_os_error()).context("could not set private staging mode");
    }
    Ok(())
}

fn c_name(name: &OsStr) -> anyhow::Result<CString> {
    CString::new(name.as_bytes()).context("staging basename contains NUL")
}

#[cfg(target_os = "linux")]
fn renameat_noreplace(
    source_fd: i32,
    source: &CString,
    target_fd: i32,
    target: &CString,
) -> libc::c_long {
    // SAFETY: descriptors and C strings remain live for the syscall.
    unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            source_fd,
            source.as_ptr(),
            target_fd,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn renameat_noreplace(
    source_fd: i32,
    source: &CString,
    target_fd: i32,
    target: &CString,
) -> libc::c_long {
    // SAFETY: descriptors and C strings remain live for renameatx_np.
    unsafe {
        libc::renameatx_np(
            source_fd,
            source.as_ptr(),
            target_fd,
            target.as_ptr(),
            libc::RENAME_EXCL,
        ) as libc::c_long
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
fn renameat_noreplace(
    _source_fd: i32,
    _source: &CString,
    _target_fd: i32,
    _target: &CString,
) -> libc::c_long {
    -1
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, symlink};

    #[test]
    fn quarantine_never_deletes_a_substituted_inode_or_symlink_target() {
        let root =
            std::env::temp_dir().join(format!("ade-host-quarantine-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let directory = StagingDirectory::open(&root).unwrap();
        let name = OsString::from("owned");
        let mut owned = directory.create_private(&name).unwrap();
        owned.write_all(b"owned").unwrap();
        let identity = owned.metadata().unwrap();
        fs::rename(root.join(&name), root.join("owned-moved")).unwrap();
        fs::write(root.join(&name), b"foreign").unwrap();

        assert!(
            !directory
                .quarantine_and_delete(&name, identity.dev(), identity.ino())
                .unwrap()
        );
        assert_eq!(fs::read(root.join("owned-moved")).unwrap(), b"owned");
        assert!(fs::read_dir(&root).unwrap().any(|entry| {
            let entry = entry.unwrap();
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".tmux-agent-quarantine-")
                && fs::read(entry.path()).unwrap() == b"foreign"
        }));

        let target = root.join("symlink-target");
        fs::write(&target, b"target").unwrap();
        let link = OsString::from("symlink-candidate");
        symlink(&target, root.join(&link)).unwrap();
        assert!(directory.quarantine_and_delete(&link, 1, 1).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"target");
        fs::remove_dir_all(root).unwrap();
    }
}
