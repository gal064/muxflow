use std::{
    ffi::{CStr, CString, OsStr, OsString},
    fs::File,
    os::unix::{
        fs::MetadataExt,
        io::{AsRawFd, FromRawFd},
    },
    path::{Path, PathBuf},
};

use super::identity_component;

pub(super) struct LocalOwnedDirectory {
    file: File,
    path: PathBuf,
    device: u64,
    inode: u64,
}

pub(super) struct EntryMetadata {
    pub(super) regular: bool,
    pub(super) symlink: bool,
    pub(super) uid: u32,
    pub(super) device: u64,
    pub(super) inode: u64,
}

impl LocalOwnedDirectory {
    #[cfg(test)]
    pub(super) fn open(path: &Path) -> Result<Self, String> {
        let path_c = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| "staging path contains NUL")?;
        let fd = unsafe {
            libc::open(
                path_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(format!(
                "clipboard staging directory changed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        Ok(Self {
            file,
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub(super) fn open_clipboard_cache(home: &Path) -> Result<Self, String> {
        if !home.is_absolute() {
            return Err("HOME must be absolute".into());
        }
        let home_fd = open_directory_path(home, "HOME")?;
        validate_owned_directory(&home_fd, "HOME")?;
        #[cfg(target_os = "macos")]
        let cache = {
            let library =
                open_or_create_child(&home_fd, OsStr::new("Library"), false, "Library parent")?;
            open_or_create_child(&library, OsStr::new("Caches"), false, "cache parent")?
        };
        #[cfg(not(target_os = "macos"))]
        let cache = open_or_create_child(&home_fd, OsStr::new(".cache"), false, "cache parent")?;
        #[cfg(target_os = "macos")]
        let app_name = OsStr::new("dev.muxflow.desktop");
        #[cfg(not(target_os = "macos"))]
        let app_name = OsStr::new("muxflow");
        let app = open_or_create_child(&cache, app_name, true, "private app cache")?;
        let clipboard = open_or_create_child(
            &app,
            OsStr::new("clipboard"),
            true,
            "private clipboard staging",
        )?;
        let metadata = clipboard.metadata().map_err(|error| error.to_string())?;
        Ok(Self {
            file: clipboard,
            path: clipboard_cache_path(home),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub(super) fn current_namespace_matches(&self) -> bool {
        std::fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
        })
    }

    pub(super) fn create_private(&self, name: &OsStr) -> Result<File, String> {
        let file = self.open_at(name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o600)?;
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(file)
    }

    pub(super) fn open_readonly(&self, name: &OsStr) -> Result<File, String> {
        self.open_at(name, libc::O_RDONLY, 0)
    }

    fn open_at(&self, name: &OsStr, flags: i32, mode: libc::mode_t) -> Result<File, String> {
        let name =
            CString::new(name.as_encoded_bytes()).map_err(|_| "staging name contains NUL")?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                libc::c_uint::from(mode),
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub(super) fn entries(&self) -> Result<Vec<(OsString, EntryMetadata)>, String> {
        let mut result = Vec::new();
        for name in directory_entry_names(&self.file)? {
            let name_c = CString::new(name.as_encoded_bytes()).map_err(|_| "entry contains NUL")?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
            if unsafe {
                libc::fstatat(
                    self.file.as_raw_fd(),
                    name_c.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                continue;
            }
            let stat = unsafe { stat.assume_init() };
            result.push((
                name,
                EntryMetadata {
                    regular: stat.st_mode & libc::S_IFMT == libc::S_IFREG,
                    symlink: stat.st_mode & libc::S_IFMT == libc::S_IFLNK,
                    uid: stat.st_uid,
                    device: identity_component(stat.st_dev, "staging device").unwrap_or(u64::MAX),
                    inode: stat.st_ino,
                },
            ));
        }
        Ok(result)
    }

    pub(super) fn unlink(&self, name: &OsStr) -> Result<(), String> {
        let name =
            CString::new(name.as_encoded_bytes()).map_err(|_| "staging name contains NUL")?;
        if unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }

    pub(super) fn quarantine_and_delete(
        &self,
        name: &OsStr,
        expected_device: u64,
        expected_inode: u64,
    ) -> Result<bool, String> {
        let quarantine = OsString::from(format!(
            ".tmux-agent-quarantine-{}.partial",
            uuid::Uuid::new_v4()
        ));
        rename_noreplace(self.file.as_raw_fd(), name, &quarantine)?;
        let quarantined = self.open_readonly(&quarantine)?;
        let metadata = quarantined.metadata().map_err(|error| error.to_string())?;
        if metadata.dev() != expected_device || metadata.ino() != expected_inode {
            // Preserve a substituted/foreign inode under the private quarantine
            // name rather than deleting it based on a stale path check.
            return Ok(false);
        }
        self.unlink(&quarantine)?;
        Ok(true)
    }
}

pub(super) fn clipboard_cache_path(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    return home.join("Library/Caches/dev.muxflow.desktop/clipboard");
    #[cfg(not(target_os = "macos"))]
    home.join(".cache/muxflow/clipboard")
}

fn open_directory_path(path: &Path, label: &str) -> Result<File, String> {
    let path = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| format!("{label} path contains NUL"))?;
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!(
            "could not open {label}: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn validate_owned_directory(file: &File, label: &str) -> Result<(), String> {
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(format!("{label} is not an owned directory"));
    }
    Ok(())
}

fn open_or_create_child(
    parent: &File,
    name: &OsStr,
    enforce_private_mode: bool,
    label: &str,
) -> Result<File, String> {
    let name = CString::new(name.as_encoded_bytes()).map_err(|_| "staging name contains NUL")?;
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
                return Err(format!("could not create {label}: {error}"));
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
        return Err(format!(
            "could not open {label}: {}",
            std::io::Error::last_os_error()
        ));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    validate_owned_directory(&file, label)?;
    if (created || enforce_private_mode) && unsafe { libc::fchmod(file.as_raw_fd(), 0o700) } != 0 {
        return Err(format!(
            "could not secure {label}: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(file)
}

#[cfg(target_os = "linux")]
fn rename_noreplace(directory_fd: i32, source: &OsStr, target: &OsStr) -> Result<(), String> {
    let source =
        CString::new(source.as_encoded_bytes()).map_err(|_| "staging name contains NUL")?;
    let target =
        CString::new(target.as_encoded_bytes()).map_err(|_| "staging name contains NUL")?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            directory_fd,
            source.as_ptr(),
            directory_fd,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result < 0 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn rename_noreplace(directory_fd: i32, source: &OsStr, target: &OsStr) -> Result<(), String> {
    let source =
        CString::new(source.as_encoded_bytes()).map_err(|_| "staging name contains NUL")?;
    let target =
        CString::new(target.as_encoded_bytes()).map_err(|_| "staging name contains NUL")?;
    let result = unsafe {
        libc::renameatx_np(
            directory_fd,
            source.as_ptr(),
            directory_fd,
            target.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result < 0 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn rename_noreplace(_directory_fd: i32, _source: &OsStr, _target: &OsStr) -> Result<(), String> {
    Err("identity-preserving quarantine is unavailable on this platform".into())
}

pub(super) fn lock_file(file: &File, operation: i32) -> Result<(), String> {
    if unsafe { libc::flock(file.as_raw_fd(), operation | libc::LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

pub(super) fn try_lock_file_exclusive(file: &File) -> Result<bool, String> {
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(false)
    } else {
        Err(error.to_string())
    }
}

pub(super) fn directory_entry_names(directory: &File) -> Result<Vec<OsString>, String> {
    // Open a new file description instead of duplicating `directory`: dup/fcntl
    // descriptors share the directory offset, which makes concurrent or repeated
    // enumeration stateful on Darwin and Linux.
    let enumeration_fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if enumeration_fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let stream = unsafe { libc::fdopendir(enumeration_fd) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error().to_string();
        unsafe { libc::close(enumeration_fd) };
        return Err(error);
    }
    let mut names = Vec::new();
    loop {
        unsafe { *errno_location() = 0 };
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let errno = unsafe { *errno_location() };
            if errno != 0 {
                let error = std::io::Error::from_raw_os_error(errno).to_string();
                unsafe { libc::closedir(stream) };
                return Err(error);
            }
            break;
        }
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if bytes != b"." && bytes != b".." {
            use std::os::unix::ffi::OsStringExt;
            names.push(OsString::from_vec(bytes.to_vec()));
        }
    }
    if unsafe { libc::closedir(stream) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(names)
}

#[cfg(target_os = "linux")]
unsafe fn errno_location() -> *mut libc::c_int {
    unsafe { libc::__errno_location() }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
unsafe fn errno_location() -> *mut libc::c_int {
    unsafe { libc::__error() }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn clipboard_quarantine_preserves_path_substitution() {
        let root =
            std::env::temp_dir().join(format!("ade-clipboard-quarantine-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let directory = LocalOwnedDirectory::open(&root).unwrap();
        let name = OsString::from(format!("{}.png", uuid::Uuid::new_v4()));
        let mut owned = directory.create_private(&name).unwrap();
        owned.write_all(b"owned").unwrap();
        let identity = owned.metadata().unwrap();
        std::fs::rename(root.join(&name), root.join("owned-moved")).unwrap();
        std::fs::write(root.join(&name), b"foreign").unwrap();

        assert!(
            !directory
                .quarantine_and_delete(&name, identity.dev(), identity.ino())
                .unwrap()
        );
        assert_eq!(std::fs::read(root.join("owned-moved")).unwrap(), b"owned");
        assert!(std::fs::read_dir(&root).unwrap().any(|entry| {
            let entry = entry.unwrap();
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".tmux-agent-quarantine-")
                && std::fs::read(entry.path()).unwrap() == b"foreign"
        }));
        std::fs::remove_dir_all(root).unwrap();
    }
}
