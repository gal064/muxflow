use std::{
    ffi::{CString, OsString},
    fs::{self, File, Metadata},
    os::unix::{
        ffi::OsStrExt,
        io::{AsRawFd, FromRawFd},
    },
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, bail};

/// An inode-bound capability for one validated active root. The directory
/// descriptor remains live for the complete operation, so replacing the path
/// after validation cannot retarget any subsequent lookup.
pub(super) struct RootCapability {
    directory: File,
    logical_root: PathBuf,
    token: String,
}

impl RootCapability {
    pub(super) fn capture(root: &str) -> anyhow::Result<Self> {
        let logical_root = fs::canonicalize(root).context("active root is unavailable")?;
        let directory = open_directory(libc::AT_FDCWD, logical_root.as_os_str().as_bytes())?;
        let metadata = directory.metadata()?;
        let token = root_identity_token(&logical_root, &metadata);
        Ok(Self {
            directory,
            logical_root,
            token,
        })
    }

    pub(super) fn validate(root: &str, expected: &str) -> anyhow::Result<Self> {
        let capability = Self::capture(root)?;
        if expected.is_empty() || capability.token != expected {
            bail!("root snapshot token does not match the requested root");
        }
        Ok(capability)
    }

    pub(super) fn token(&self) -> &str {
        &self.token
    }

    pub(super) fn logical_root(&self) -> &Path {
        &self.logical_root
    }

    pub(super) fn stable_root(&self) -> PathBuf {
        descriptor_path(self.directory.as_raw_fd())
    }

    pub(super) fn open_root_directory(&self) -> anyhow::Result<File> {
        Ok(self.directory.try_clone()?)
    }

    pub(super) fn resolve_existing(&self, path: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
        let (logical, stable) = self.resolve(path)?;
        fs::symlink_metadata(&stable).context("requested path does not exist")?;
        Ok((logical, stable))
    }

    pub(super) fn resolve_new(&self, path: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
        self.resolve(path)
    }

    pub(super) fn anchor(&self, logical_target: &Path) -> anyhow::Result<AnchoredPath> {
        AnchoredPath::open_in(self, logical_target)
    }

    pub(super) fn regular_file_target(
        &self,
        logical: &Path,
        stable: &Path,
    ) -> anyhow::Result<(PathBuf, PathBuf)> {
        let link_metadata = fs::symlink_metadata(stable)?;
        let (logical_target, stable_target) = if link_metadata.file_type().is_symlink() {
            let followed = fs::canonicalize(stable).context("symlink target is unavailable")?;
            let current_root = fs::canonicalize(self.stable_root())?;
            let relative = followed
                .strip_prefix(&current_root)
                .context("path escapes the active root through a symlink")?;
            (
                self.logical_root.join(relative),
                self.stable_root().join(relative),
            )
        } else {
            (logical.to_owned(), stable.to_owned())
        };
        if !fs::metadata(&stable_target)?.is_file() {
            bail!("only regular files and safe in-root file symlinks can be opened or saved");
        }
        Ok((logical_target, stable_target))
    }

    fn resolve(&self, path: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
        let logical = super::request_path(&self.logical_root, path)?;
        let relative = logical
            .strip_prefix(&self.logical_root)
            .context("requested path is outside the active root")?
            .to_owned();
        Ok((logical, self.stable_root().join(relative)))
    }
}

pub(super) fn root_identity_token(root: &Path, metadata: &Metadata) -> String {
    use std::os::unix::fs::MetadataExt as _;

    let mut hasher = blake3::Hasher::new();
    hasher.update(super::super::snapshot::server_identity().as_bytes());
    hasher.update(root.as_os_str().as_encoded_bytes());
    hasher.update(&metadata.dev().to_le_bytes());
    hasher.update(&metadata.ino().to_le_bytes());
    hasher.finalize().to_hex().to_string()
}

pub(super) fn rename_noreplace(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let source_parent = source.parent().context("source has no parent")?;
    let destination_parent = destination.parent().context("destination has no parent")?;
    let source_name = source.file_name().context("source has no leaf")?.to_owned();
    let destination_name = destination
        .file_name()
        .context("destination has no leaf")?
        .to_owned();
    let source_parent = open_stable_descriptor_directory(source_parent)?;
    let destination_parent = open_stable_descriptor_directory(destination_parent)?;
    AnchoredPath {
        parent: source_parent,
        leaf: source_name,
    }
    .rename_to_noreplace(&AnchoredPath {
        parent: destination_parent,
        leaf: destination_name,
    })
}

fn open_stable_descriptor_directory(path: &Path) -> anyhow::Result<File> {
    let path = CString::new(path.as_os_str().as_bytes()).context("path contains a NUL byte")?;
    // SAFETY: these paths are generated from live kernel descriptor capabilities.
    // Following that kernel-owned descriptor link duplicates the validated fd.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("stable parent is unavailable");
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// A leaf whose parent is held open with `O_NOFOLLOW`. Operations through the
/// proc-fd path remain attached to that directory even if an attacker swaps a
/// pathname component after validation.
pub(super) struct AnchoredPath {
    parent: File,
    leaf: OsString,
}

impl AnchoredPath {
    pub(super) fn open_in(root: &RootCapability, target: &Path) -> anyhow::Result<Self> {
        let relative = target
            .strip_prefix(root.logical_root())
            .context("mutation path is outside the active root")?;
        let leaf = relative
            .file_name()
            .context("the active root itself cannot be mutated")?
            .to_owned();
        let parent = relative.parent().unwrap_or_else(|| Path::new(""));
        let mut directory = root.directory.try_clone()?;
        for component in parent.components() {
            let Component::Normal(name) = component else {
                bail!("mutation path contains an unsafe component");
            };
            directory = open_directory(directory.as_raw_fd(), name.as_bytes())?;
        }
        Ok(Self {
            parent: directory,
            leaf,
        })
    }

    pub(super) fn path(&self) -> PathBuf {
        self.parent_proc_path().join(&self.leaf)
    }

    pub(super) fn open_file(&self) -> anyhow::Result<File> {
        open_leaf(self.parent.as_raw_fd(), &self.leaf, libc::O_RDONLY)
    }

    pub(super) fn open_directory(&self) -> anyhow::Result<File> {
        open_leaf(
            self.parent.as_raw_fd(),
            &self.leaf,
            libc::O_RDONLY | libc::O_DIRECTORY,
        )
    }

    pub(super) fn metadata_no_follow(&self) -> anyhow::Result<Metadata> {
        Ok(fs::symlink_metadata(self.path())?)
    }

    pub(super) fn unlink(&self, directory: bool) -> anyhow::Result<()> {
        let leaf = CString::new(self.leaf.as_bytes()).context("path contains a NUL byte")?;
        // SAFETY: parent and leaf are live and validated; unlinkat never follows the leaf.
        let result = unsafe {
            libc::unlinkat(
                self.parent.as_raw_fd(),
                leaf.as_ptr(),
                if directory { libc::AT_REMOVEDIR } else { 0 },
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }

    pub(super) fn rename_to_noreplace(&self, destination: &Self) -> anyhow::Result<()> {
        let source = CString::new(self.leaf.as_bytes()).context("path contains a NUL byte")?;
        let target =
            CString::new(destination.leaf.as_bytes()).context("path contains a NUL byte")?;
        let result = renameat_noreplace(
            self.parent.as_raw_fd(),
            &source,
            destination.parent.as_raw_fd(),
            &target,
        )?;
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if matches!(
                error.raw_os_error(),
                Some(libc::ENOSYS) | Some(libc::EINVAL)
            ) {
                bail!("atomic no-replace rename is unavailable; refusing unsafe fallback");
            }
            return Err(error.into());
        }
        Ok(())
    }

    fn parent_proc_path(&self) -> PathBuf {
        descriptor_path(self.parent.as_raw_fd())
    }
}

/// A path backed by a live descriptor. Linux exposes descriptors through
/// procfs; Apple platforms expose the equivalent vnode through devfs.
#[cfg(target_os = "linux")]
pub(super) fn descriptor_path(fd: i32) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{fd}"))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(super) fn descriptor_path(fd: i32) -> PathBuf {
    PathBuf::from(format!("/dev/fd/{fd}"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
compile_error!("descriptor-relative filesystem service requires Linux or an Apple platform");

#[cfg(target_os = "linux")]
fn renameat_noreplace(
    source_fd: i32,
    source: &CString,
    destination_fd: i32,
    destination: &CString,
) -> anyhow::Result<libc::c_long> {
    // SAFETY: both directory descriptors and both C strings remain live for
    // the syscall. RENAME_NOREPLACE is the atomic race-free contract.
    Ok(unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            source_fd,
            source.as_ptr(),
            destination_fd,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    })
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn renameat_noreplace(
    source_fd: i32,
    source: &CString,
    destination_fd: i32,
    destination: &CString,
) -> anyhow::Result<libc::c_long> {
    // SAFETY: renameatx_np has the same descriptor/C-string lifetime contract;
    // RENAME_EXCL is Apple's atomic no-replace operation.
    Ok(unsafe {
        libc::renameatx_np(
            source_fd,
            source.as_ptr(),
            destination_fd,
            destination.as_ptr(),
            libc::RENAME_EXCL,
        ) as libc::c_long
    })
}

fn open_leaf(parent_fd: i32, name: &OsString, flags: i32) -> anyhow::Result<File> {
    let name = CString::new(name.as_bytes()).context("path contains a NUL byte")?;
    // SAFETY: the directory descriptor remains owned by AnchoredPath and the
    // fresh descriptor is transferred to File. O_NOFOLLOW closes the leaf race.
    let fd = unsafe {
        libc::openat(
            parent_fd,
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("leaf changed or is unsafe");
    }
    // SAFETY: fd is freshly owned.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_directory(parent_fd: i32, path: &[u8]) -> anyhow::Result<File> {
    let path = CString::new(path).context("path contains a NUL byte")?;
    // SAFETY: `path` is NUL-terminated and the returned descriptor is uniquely
    // transferred into `File`. O_NOFOLLOW rejects a concurrent symlink swap.
    let fd = unsafe {
        libc::openat(
            parent_fd,
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .context("mutation parent changed or is not a no-follow directory");
    }
    // SAFETY: `fd` is a fresh owned descriptor from openat.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use uuid::Uuid;

    use super::*;

    #[test]
    fn anchored_parent_survives_concurrent_path_symlink_swap() {
        let root = std::env::temp_dir().join(format!("ade-anchor-{}", Uuid::new_v4()));
        let outside = std::env::temp_dir().join(format!("ade-anchor-out-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("parent")).unwrap();
        fs::create_dir(&outside).unwrap();
        let capability = RootCapability::capture(root.to_str().unwrap()).unwrap();
        let anchored = capability.anchor(&root.join("parent/created")).unwrap();
        fs::rename(root.join("parent"), root.join("original-parent")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("parent")).unwrap();
        fs::write(anchored.path(), "safe").unwrap();
        assert!(root.join("original-parent/created").exists());
        assert!(!outside.join("created").exists());
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn leaf_swap_is_never_followed_for_read_or_delete() {
        let root = std::env::temp_dir().join(format!("ade-leaf-{}", Uuid::new_v4()));
        let outside = std::env::temp_dir().join(format!("ade-leaf-out-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(root.join("leaf"), "inside").unwrap();
        fs::write(outside.join("secret"), "outside").unwrap();
        let capability = RootCapability::capture(root.to_str().unwrap()).unwrap();
        let anchored = capability.anchor(&root.join("leaf")).unwrap();
        fs::rename(root.join("leaf"), root.join("original")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), root.join("leaf")).unwrap();
        assert!(anchored.open_file().is_err());
        anchored.unlink(false).unwrap();
        assert_eq!(
            fs::read_to_string(outside.join("secret")).unwrap(),
            "outside"
        );
        assert_eq!(fs::read_to_string(root.join("original")).unwrap(), "inside");
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn no_replace_rename_never_clobbers_a_racing_destination() {
        let root = std::env::temp_dir().join(format!("ade-noreplace-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("source"), "ours").unwrap();
        fs::write(root.join("destination"), "racer").unwrap();
        let error = rename_noreplace(&root.join("source"), &root.join("destination")).unwrap_err();
        assert!(
            error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists)
        );
        assert_eq!(
            fs::read_to_string(root.join("destination")).unwrap(),
            "racer"
        );
        assert_eq!(fs::read_to_string(root.join("source")).unwrap(), "ours");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn captured_root_capability_stays_bound_to_the_original_inode() {
        let root = std::env::temp_dir().join(format!("ade-root-cap-{}", Uuid::new_v4()));
        let displaced = root.with_extension("displaced");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("value"), "original").unwrap();
        let capability = RootCapability::capture(root.to_str().unwrap()).unwrap();
        fs::rename(&root, &displaced).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("value"), "replacement").unwrap();
        let (_, stable) = capability.resolve_existing("value").unwrap();
        assert_eq!(fs::read_to_string(stable).unwrap(), "original");
        assert_eq!(
            fs::read_to_string(root.join("value")).unwrap(),
            "replacement"
        );
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(displaced).unwrap();
    }
}
