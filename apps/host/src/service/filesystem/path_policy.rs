use std::{
    ffi::{CString, OsStr, OsString},
    fs::{self, File, Metadata, Permissions},
    os::unix::{
        ffi::OsStrExt,
        fs::PermissionsExt,
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
    single_file: Option<SingleFileCapability>,
}

struct SingleFileCapability {
    root_token: String,
    path_digest: String,
}

pub(super) const SINGLE_FILE_TOKEN_PREFIX: &str = "file-v2:";

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
            single_file: None,
        })
    }

    /// Mints an editable capability for one existing regular file.
    ///
    /// The public root remains the file's parent because the file streaming
    /// protocol is root-relative, but the token binds that root to the exact
    /// canonical leaf. A caller holding it cannot turn one terminal click into
    /// a directory listing or access to a sibling file.
    pub(super) fn capture_single_file(path: &Path) -> anyhow::Result<Self> {
        let canonical = fs::canonicalize(path).context("terminal file is unavailable")?;
        let metadata = fs::metadata(&canonical)?;
        if !metadata.is_file() {
            bail!("terminal path is not a regular file");
        }
        let parent = canonical
            .parent()
            .context("terminal file has no parent directory")?;
        let mut capability = Self::capture(
            parent
                .to_str()
                .context("terminal file parent is not valid UTF-8")?,
        )?;
        let root_token = capability.token.clone();
        let path_digest = single_file_path_digest(&root_token, &canonical);
        // Never serialize `root_token`: it is the ordinary parent-directory
        // capability, so revealing it inside the narrow token would let a
        // client peel it out and use it for listing, sibling reads and writes.
        //
        // The path-bound token intentionally survives the editor's atomic
        // replace. Its version and digest domain are distinct from the removed
        // inode-bound read token, so old authority cannot be repurposed.
        capability.token = format!("{SINGLE_FILE_TOKEN_PREFIX}{path_digest}");
        capability.single_file = Some(SingleFileCapability {
            root_token,
            path_digest,
        });
        Ok(capability)
    }

    pub(super) fn validate(root: &str, expected: &str) -> anyhow::Result<Self> {
        let capability = Self::capture(root)?;
        if expected.is_empty() || capability.token != expected {
            bail!("root snapshot token does not match the requested root");
        }
        Ok(capability)
    }

    /// Accepts ordinary root capabilities plus the narrow terminal-link
    /// capability used for reads and exact-file saves.
    pub(super) fn validate_read(root: &str, expected: &str) -> anyhow::Result<Self> {
        Self::validate_single_file(root, expected)
    }

    /// Accepts ordinary roots plus path-bound single-file capabilities.
    pub(super) fn validate_write(root: &str, expected: &str) -> anyhow::Result<Self> {
        Self::validate_single_file(root, expected)
    }

    fn validate_single_file(root: &str, expected: &str) -> anyhow::Result<Self> {
        let Some(encoded) = expected.strip_prefix(SINGLE_FILE_TOKEN_PREFIX) else {
            return Self::validate(root, expected);
        };
        if encoded.len() != 64 || encoded.contains(':') {
            bail!("single-file token is malformed");
        }
        let mut capability = Self::capture(root)?;
        let root_token = capability.token.clone();
        capability.token = expected.to_owned();
        capability.single_file = Some(SingleFileCapability {
            root_token,
            path_digest: encoded.to_owned(),
        });
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

    pub(super) fn directory_entries(&self) -> anyhow::Result<Vec<OsString>> {
        directory_entry_names(&self.directory)
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
        if self.is_single_file() && link_metadata.file_type().is_symlink() {
            bail!("single-file capability does not follow a replaced symlink leaf");
        }
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

    pub(super) fn is_single_file(&self) -> bool {
        self.single_file.is_some()
    }

    /// Re-checks a narrow capability against the descriptor actually opened.
    /// Pathname validation alone is insufficient because another process can
    /// replace the leaf between `stat` and `openat`.
    pub(super) fn authorize_opened_file(
        &self,
        _logical: &Path,
        metadata: &Metadata,
    ) -> anyhow::Result<()> {
        if self.single_file.is_some() && !metadata.is_file() {
            bail!("single-file capability does not authorize the opened file");
        }
        Ok(())
    }

    fn resolve(&self, path: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
        let logical = super::request_path(&self.logical_root, path)?;
        let relative = logical
            .strip_prefix(&self.logical_root)
            .context("requested path is outside the active root")?
            .to_owned();
        let stable = self.stable_root().join(relative);
        if let Some(single_file) = &self.single_file {
            let path_digest = single_file_path_digest(&single_file.root_token, &logical);
            if path_digest != single_file.path_digest {
                bail!("single-file capability does not authorize the requested path");
            }
            let metadata =
                fs::symlink_metadata(&stable).context("single-file target is unavailable")?;
            if !metadata.is_file() {
                bail!("single-file capability does not authorize the requested path");
            }
        }
        Ok((logical, stable))
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

fn single_file_path_digest(root_token: &str, path: &Path) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(root_token.as_bytes());
    hasher.update(&[0]);
    hasher.update(b"editable-single-file-v2");
    hasher.update(&[0]);
    hasher.update(path.as_os_str().as_encoded_bytes());
    hasher.finalize().to_hex().to_string()
}

/// A leaf whose parent is held open with `O_NOFOLLOW`. Operations through the
/// proc-fd path remain attached to that directory even if an attacker swaps a
/// pathname component after validation.
pub(super) struct AnchoredPath {
    parent: File,
    leaf: OsString,
}

pub(super) struct AnchoredMetadata {
    stat: libc::stat,
}

impl AnchoredMetadata {
    pub(super) fn is_dir(&self) -> bool {
        self.kind() == libc::S_IFDIR
    }

    pub(super) fn is_file(&self) -> bool {
        self.kind() == libc::S_IFREG
    }

    pub(super) fn is_symlink(&self) -> bool {
        self.kind() == libc::S_IFLNK
    }

    // Already the target type on Linux, a narrower and sometimes signed one
    // elsewhere. See `device`.
    #[allow(clippy::unnecessary_cast)]
    pub(super) fn device(&self) -> u64 {
        // Widened, never validated. These three are identity components, not
        // quantities: what has to survive is the bits. Platforms disagree
        // about the sign and width of every one of them — `dev_t` is a signed
        // 32-bit value on macOS and unsigned 64-bit on Linux — so a conversion
        // that refused a negative device number panicked on the listing hot
        // path for a value that platform considers perfectly ordinary.
        self.stat.st_dev as u64
    }

    // Already the target type on Linux, a narrower and sometimes signed one
    // elsewhere. See `device`.
    #[allow(clippy::unnecessary_cast)]
    pub(super) fn inode(&self) -> u64 {
        self.stat.st_ino as u64
    }

    pub(super) fn len(&self) -> u64 {
        self.stat.st_size.max(0) as u64
    }

    // Already the target type on Linux, a narrower and sometimes signed one
    // elsewhere. See `device`.
    #[allow(clippy::unnecessary_cast)]
    pub(super) fn mode(&self) -> u32 {
        self.stat.st_mode as u32
    }

    pub(super) fn permissions(&self) -> Permissions {
        Permissions::from_mode(self.mode() & 0o7777)
    }

    pub(super) fn modified_unix_millis(&self) -> i64 {
        let (seconds, nanos) = stat_modified(&self.stat);
        seconds
            .saturating_mul(1_000)
            .saturating_add(nanos / 1_000_000)
    }

    pub(super) fn generation(&self) -> u64 {
        let (seconds, nanos) = stat_modified(&self.stat);
        let mut value = self.device().rotate_left(7) ^ self.inode();
        value ^= self.len().rotate_left(19);
        value ^= (seconds as u64).rotate_left(31);
        value ^= (nanos as u64).rotate_left(43);
        value
    }

    fn kind(&self) -> libc::mode_t {
        self.stat.st_mode & libc::S_IFMT
    }
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

    pub(super) fn leaf(&self) -> &OsStr {
        &self.leaf
    }

    pub(super) fn in_directory(directory: &File, leaf: OsString) -> anyhow::Result<Self> {
        Ok(Self {
            parent: directory.try_clone()?,
            leaf,
        })
    }

    pub(super) fn sibling(&self, leaf: OsString) -> anyhow::Result<Self> {
        Ok(Self {
            parent: self.parent.try_clone()?,
            leaf,
        })
    }

    pub(super) fn same_parent(&self, other: &Self) -> anyhow::Result<bool> {
        let left = self.parent.metadata()?;
        let right = other.parent.metadata()?;
        use std::os::unix::fs::MetadataExt as _;
        Ok((left.dev(), left.ino()) == (right.dev(), right.ino()))
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

    pub(super) fn metadata_no_follow(&self) -> anyhow::Result<AnchoredMetadata> {
        metadata_at(
            self.parent.as_raw_fd(),
            &self.leaf,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    }

    pub(super) fn exists(&self) -> anyhow::Result<bool> {
        match self.metadata_no_follow() {
            Ok(_) => Ok(true),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn create_file(&self, mode: u32) -> anyhow::Result<File> {
        let name = CString::new(self.leaf.as_bytes()).context("path contains a NUL byte")?;
        // SAFETY: parent/name remain live; O_EXCL makes creation race-free.
        let fd = unsafe {
            libc::openat(
                self.parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
                mode as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub(super) fn create_directory(&self, mode: u32) -> anyhow::Result<()> {
        let name = CString::new(self.leaf.as_bytes()).context("path contains a NUL byte")?;
        // SAFETY: parent/name remain live and mkdirat does not follow the leaf.
        if unsafe { libc::mkdirat(self.parent.as_raw_fd(), name.as_ptr(), mode as libc::mode_t) }
            < 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }

    pub(super) fn read_link(&self) -> anyhow::Result<PathBuf> {
        let name = CString::new(self.leaf.as_bytes()).context("path contains a NUL byte")?;
        let mut buffer = vec![0_u8; libc::PATH_MAX as usize];
        // SAFETY: parent/name and the output buffer remain live for readlinkat.
        let read = unsafe {
            libc::readlinkat(
                self.parent.as_raw_fd(),
                name.as_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if read < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        buffer.truncate(read as usize);
        use std::os::unix::ffi::OsStringExt as _;
        Ok(PathBuf::from(OsString::from_vec(buffer)))
    }

    pub(super) fn create_symlink(&self, target: &Path) -> anyhow::Result<()> {
        let target = CString::new(target.as_os_str().as_bytes())
            .context("link target contains a NUL byte")?;
        let name = CString::new(self.leaf.as_bytes()).context("path contains a NUL byte")?;
        // SAFETY: both C strings and the parent descriptor remain live.
        if unsafe { libc::symlinkat(target.as_ptr(), self.parent.as_raw_fd(), name.as_ptr()) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }

    pub(super) fn directory_entries(&self) -> anyhow::Result<Vec<OsString>> {
        directory_entry_names(&self.open_directory()?)
    }

    pub(super) fn child(&self, name: OsString) -> anyhow::Result<Self> {
        Ok(Self {
            parent: self.open_directory()?,
            leaf: name,
        })
    }

    pub(super) fn sync_parent(&self) -> anyhow::Result<()> {
        self.parent.sync_all()?;
        Ok(())
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

    pub(super) fn rename_to_replace(&self, destination: &Self) -> anyhow::Result<()> {
        let source = CString::new(self.leaf.as_bytes()).context("path contains a NUL byte")?;
        let target =
            CString::new(destination.leaf.as_bytes()).context("path contains a NUL byte")?;
        // SAFETY: both directory descriptors and C strings remain live.
        let result = unsafe {
            libc::renameat(
                self.parent.as_raw_fd(),
                source.as_ptr(),
                destination.parent.as_raw_fd(),
                target.as_ptr(),
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}

fn metadata_at(parent_fd: i32, name: &OsStr, flags: i32) -> anyhow::Result<AnchoredMetadata> {
    let name = CString::new(name.as_bytes()).context("path contains a NUL byte")?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: parent/name remain live and stat points to writable storage.
    if unsafe { libc::fstatat(parent_fd, name.as_ptr(), stat.as_mut_ptr(), flags) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(AnchoredMetadata {
        stat: unsafe { stat.assume_init() },
    })
}

fn stat_modified(stat: &libc::stat) -> (i64, i64) {
    (stat.st_mtime, stat.st_mtime_nsec)
}

fn directory_entry_names(directory: &File) -> anyhow::Result<Vec<OsString>> {
    // `fdopendir` owns the descriptor it is given, so this needs one of its
    // own — and it must be an *independent* one. A `dup` shares the file
    // offset with the descriptor it copied, so the second enumeration of a
    // long-lived capability began where the first one stopped: at the end.
    // Every authoritative rescan of an already-listed directory therefore
    // reported it as empty, and the desktop installs that as its contents.
    // `openat(fd, ".")` re-opens the same directory the descriptor already
    // names — still relative to it, so no path is re-traversed and no symlink
    // can be interposed — with its own offset.
    let duplicate = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if duplicate < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(duplicate) };
        return Err(error.into());
    }
    let mut entries = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        use std::os::unix::ffi::OsStringExt as _;
        entries.push(OsString::from_vec(name.to_bytes().to_vec()));
    }
    if unsafe { libc::closedir(stream) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(entries)
}

/// A path backed by a live descriptor. Linux exposes descriptors through
/// procfs. Darwin's `/dev/fd` entries cannot be traversed as directories, so
/// ask the kernel for the descriptor's current vnode path instead. The live
/// descriptor remains the authority used by the openat/renameat operations.
#[cfg(target_os = "linux")]
pub(super) fn descriptor_path(fd: i32) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{fd}"))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(super) fn descriptor_path(fd: i32) -> PathBuf {
    let mut buffer = [0_u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes at most MAXPATHLEN bytes into this live buffer,
    // and callers only provide owned, open descriptors.
    let result = unsafe { libc::fcntl(fd, libc::F_GETPATH, buffer.as_mut_ptr()) };
    if result == 0 {
        let length = buffer
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(buffer.len());
        use std::os::unix::ffi::OsStringExt as _;
        return PathBuf::from(OsString::from_vec(buffer[..length].to_vec()));
    }
    // Preserve a fail-closed path: subsequent operations report the kernel
    // error instead of falling back to the user-controlled logical root.
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
        let anchored = capability
            .anchor(&capability.logical_root().join("parent/created"))
            .unwrap();
        fs::rename(root.join("parent"), root.join("original-parent")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("parent")).unwrap();
        use std::io::Write as _;
        anchored
            .create_file(0o600)
            .unwrap()
            .write_all(b"safe")
            .unwrap();
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
        let anchored = capability
            .anchor(&capability.logical_root().join("leaf"))
            .unwrap();
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
        let capability = RootCapability::capture(root.to_str().unwrap()).unwrap();
        let source = capability
            .anchor(&capability.logical_root().join("source"))
            .unwrap();
        let destination = capability
            .anchor(&capability.logical_root().join("destination"))
            .unwrap();
        let error = source.rename_to_noreplace(&destination).unwrap_err();
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

    #[test]
    fn single_file_capability_authorizes_only_the_exact_existing_leaf() {
        let temp = tempfile::tempdir().unwrap();
        // Capture canonicalizes, so the paths asked about must be canonical
        // too: macOS puts the tempdir under /var, a symlink to /private/var.
        let dir = fs::canonicalize(temp.path()).unwrap();
        let allowed = dir.join("allowed.md");
        let sibling = dir.join("sibling.md");
        fs::write(&allowed, "allowed").unwrap();
        fs::write(&sibling, "sibling").unwrap();

        let minted = RootCapability::capture_single_file(&allowed).unwrap();
        let root = minted.logical_root().to_str().unwrap().to_owned();
        let token = minted.token().to_owned();
        assert!(token.starts_with(SINGLE_FILE_TOKEN_PREFIX));
        let exposed_digest = token.strip_prefix(SINGLE_FILE_TOKEN_PREFIX).unwrap();
        assert_eq!(exposed_digest.len(), 64);
        assert!(!exposed_digest.contains(':'));

        // Directory and general mutation operations use ordinary validation
        // and cannot reinterpret the narrow token as a root capability.
        assert!(RootCapability::validate(&root, &token).is_err());
        assert!(RootCapability::validate(&root, exposed_digest).is_err());

        let reader = RootCapability::validate_read(&root, &token).unwrap();
        let writer = RootCapability::validate_write(&root, &token).unwrap();
        assert!(reader.resolve_existing(allowed.to_str().unwrap()).is_ok());
        assert!(writer.resolve_existing(allowed.to_str().unwrap()).is_ok());
        let existing_sibling = reader
            .resolve_existing(sibling.to_str().unwrap())
            .unwrap_err()
            .to_string();
        let missing_sibling = reader
            .resolve_existing(dir.join("missing.md").to_str().unwrap())
            .unwrap_err()
            .to_string();
        assert_eq!(existing_sibling, missing_sibling);
        assert!(reader.resolve_existing(&root).is_err());
        assert!(writer.resolve_existing(sibling.to_str().unwrap()).is_err());
    }

    #[test]
    fn writable_single_file_capability_does_not_follow_a_replaced_symlink_leaf() {
        let temp = tempfile::tempdir().unwrap();
        let allowed = temp.path().join("allowed.md");
        let sibling = temp.path().join("sibling.md");
        fs::write(&allowed, "allowed").unwrap();
        fs::write(&sibling, "sibling").unwrap();
        let minted = RootCapability::capture_single_file(&allowed).unwrap();
        let root = minted.logical_root().to_str().unwrap().to_owned();
        let token = minted.token().to_owned();

        fs::remove_file(&allowed).unwrap();
        std::os::unix::fs::symlink(&sibling, &allowed).unwrap();

        let reader = RootCapability::validate_read(&root, &token).unwrap();
        let writer = RootCapability::validate_write(&root, &token).unwrap();
        assert!(reader.resolve_existing(allowed.to_str().unwrap()).is_err());
        assert!(writer.resolve_existing(allowed.to_str().unwrap()).is_err());
    }
}
