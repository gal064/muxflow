use std::{
    collections::HashMap,
    ffi::{CString, OsStr},
    fs::File,
    io,
    os::unix::ffi::OsStrExt,
    os::unix::{
        fs::MetadataExt,
        io::{AsRawFd, FromRawFd},
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use uuid::Uuid;

use super::{
    download_manager::DownloadCollisionPolicy,
    download_naming::{directory_name_max, first_free_name},
};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct FileIdentity {
    pub(super) device: u64,
    pub(super) inode: u64,
}

pub(super) struct InspectedDestination {
    pub(super) identity: FileIdentity,
    pub(super) regular: bool,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct DestinationKey {
    directory: FileIdentity,
    leaf: Vec<u8>,
}

struct DestinationReservation {
    owner: Uuid,
    display: PathBuf,
}

#[derive(Default)]
struct ReservationState {
    entries: HashMap<DestinationKey, DestinationReservation>,
    generation: u64,
}

#[derive(Default)]
pub(super) struct DestinationReservations {
    state: Mutex<ReservationState>,
}

pub(super) struct DestinationLease {
    registry: Arc<DestinationReservations>,
    key: DestinationKey,
    owner: Uuid,
}

pub(super) struct ReservedName {
    pub(super) final_name: CString,
    pub(super) overwrite_identity: Option<FileIdentity>,
    pub(super) lease: DestinationLease,
}

impl Drop for DestinationLease {
    fn drop(&mut self) {
        self.registry.release(&self.key, self.owner);
    }
}

impl DestinationReservations {
    fn try_reserve(
        self: &Arc<Self>,
        directory: &File,
        directory_identity: FileIdentity,
        directory_path: &Path,
        leaf: &OsStr,
    ) -> Result<Option<DestinationLease>, String> {
        loop {
            let (generation, reserved_leaves) = {
                let state = self
                    .state
                    .lock()
                    .map_err(|_| "download destination reservation registry is unavailable")?;
                (
                    state.generation,
                    state
                        .entries
                        .keys()
                        .filter(|key| key.directory == directory_identity)
                        .map(|key| key.leaf.clone())
                        .collect::<Vec<_>>(),
                )
            };
            if reserved_leaves
                .iter()
                .any(|reserved| reserved.as_slice() == leaf.as_bytes())
            {
                return Ok(None);
            }
            if !reserved_leaves.is_empty()
                && filesystem_matches_any(directory, &reserved_leaves, leaf)?
            {
                return Ok(None);
            }

            let key = DestinationKey {
                directory: directory_identity,
                leaf: leaf.as_bytes().to_vec(),
            };
            let owner = Uuid::new_v4();
            let mut state = self
                .state
                .lock()
                .map_err(|_| "download destination reservation registry is unavailable")?;
            if state.generation != generation {
                continue;
            }
            state.entries.insert(
                key.clone(),
                DestinationReservation {
                    owner,
                    display: directory_path.join(leaf),
                },
            );
            state.generation = state.generation.wrapping_add(1);
            return Ok(Some(DestinationLease {
                registry: Arc::clone(self),
                key,
                owner,
            }));
        }
    }

    fn contains(
        &self,
        directory: &File,
        directory_identity: FileIdentity,
        leaf: &OsStr,
    ) -> Result<bool, String> {
        let reserved_leaves = self
            .state
            .lock()
            .map_err(|_| "download destination reservation registry is unavailable")?
            .entries
            .keys()
            .filter(|key| key.directory == directory_identity)
            .map(|key| key.leaf.clone())
            .collect::<Vec<_>>();
        if reserved_leaves
            .iter()
            .any(|reserved| reserved.as_slice() == leaf.as_bytes())
        {
            return Ok(true);
        }
        if reserved_leaves.is_empty() {
            Ok(false)
        } else {
            filesystem_matches_any(directory, &reserved_leaves, leaf)
        }
    }

    fn release(&self, key: &DestinationKey, owner: Uuid) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state
            .entries
            .get(key)
            .is_some_and(|reservation| reservation.owner == owner)
        {
            state.entries.remove(key);
            state.generation = state.generation.wrapping_add(1);
        }
    }

    pub(super) fn contains_display_path(&self, path: &Path) -> bool {
        self.state
            .lock()
            .map(|state| state.entries.values().any(|entry| entry.display == path))
            .unwrap_or(true)
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.state.lock().unwrap().entries.len()
    }
}

impl DestinationLease {
    #[cfg(test)]
    pub(super) fn release_as(&self, owner: Uuid) {
        self.registry.release(&self.key, owner);
    }
}

struct SemanticProbe {
    parent: File,
    directory: Option<File>,
    directory_name: CString,
    directory_identity: Option<FileIdentity>,
    leaves: Vec<CString>,
    armed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum SemanticProbeFault {
    AfterDirectoryCreate,
    AfterLeafCreate,
}

impl Drop for SemanticProbe {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

impl SemanticProbe {
    fn create(parent: &File, fault: Option<SemanticProbeFault>) -> Result<Self, String> {
        let parent = parent.try_clone().map_err(|error| error.to_string())?;
        let directory_name = CString::new(format!(".tmux-agent-name-probe-{}", Uuid::new_v4()))
            .expect("UUID probe name contains no NUL");
        // SAFETY: the parent descriptor and generated C string are valid.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), directory_name.as_ptr(), 0o700) } != 0 {
            return Err(format!(
                "could not create destination semantic probe: {}",
                io::Error::last_os_error()
            ));
        }
        // The guard is armed immediately after successful creation. Every
        // later error path either proves/removes this exact inode or reports
        // cleanup failure and rejects admission.
        let mut probe = Self {
            parent,
            directory: None,
            directory_name,
            directory_identity: None,
            leaves: Vec::new(),
            armed: true,
        };
        let setup = (|| {
            let identity = metadata_identity_at(&probe.parent, &probe.directory_name)?
                .ok_or("destination semantic probe disappeared after creation")?;
            probe.directory_identity = Some(identity);
            if fault == Some(SemanticProbeFault::AfterDirectoryCreate) {
                return Err("injected failure after semantic probe directory creation".into());
            }
            // SAFETY: the parent descriptor and generated C string are valid.
            let fd = unsafe {
                libc::openat(
                    probe.parent.as_raw_fd(),
                    probe.directory_name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(format!(
                    "could not open destination semantic probe: {}",
                    io::Error::last_os_error()
                ));
            }
            // SAFETY: fd is a new uniquely owned descriptor.
            let directory = unsafe { File::from_raw_fd(fd) };
            let metadata = directory.metadata().map_err(|error| error.to_string())?;
            let opened_identity = FileIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            };
            if opened_identity != identity {
                return Err("destination semantic probe directory was substituted".into());
            }
            probe.directory = Some(directory);
            Ok(())
        })();
        if let Err(error) = setup {
            let cleanup = probe.cleanup();
            return Err(with_cleanup_error(error, cleanup));
        }
        Ok(probe)
    }

    fn add_leaf(&mut self, leaf: &[u8]) -> Result<(), String> {
        let name = CString::new(leaf).map_err(|_| "destination basename contains a NUL byte")?;
        let directory = self
            .directory
            .as_ref()
            .ok_or("destination semantic probe directory is unavailable")?;
        // SAFETY: the probe descriptor and candidate C string are valid.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(format!(
                "could not create destination semantic probe leaf: {}",
                io::Error::last_os_error()
            ));
        }
        // The O_EXCL-created name is owned even if closing the descriptor
        // later fails; arm its cleanup before performing any further work.
        self.leaves.push(name);
        // SAFETY: fd is a new uniquely owned descriptor.
        drop(unsafe { File::from_raw_fd(fd) });
        Ok(())
    }

    fn contains(&self, candidate: &OsStr) -> Result<bool, String> {
        let name = CString::new(candidate.as_bytes())
            .map_err(|_| "destination basename contains a NUL byte")?;
        let directory = self
            .directory
            .as_ref()
            .ok_or("destination semantic probe directory is unavailable")?;
        Ok(metadata_identity_at(directory, &name)?.is_some())
    }

    fn cleanup(&mut self) -> Result<(), String> {
        if !self.armed {
            return Ok(());
        }
        let mut errors = Vec::new();
        if let Some(directory) = &self.directory {
            let leaves = std::mem::take(&mut self.leaves);
            for leaf in leaves {
                // SAFETY: the private probe directory and O_EXCL-created leaf
                // are owned by this guard.
                if unsafe { libc::unlinkat(directory.as_raw_fd(), leaf.as_ptr(), 0) } != 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::NotFound {
                        errors.push(format!("could not remove semantic probe leaf: {error}"));
                        self.leaves.push(leaf);
                    }
                }
            }
        }
        let identity_matches = match self.directory_identity {
            Some(expected) => {
                metadata_identity_at(&self.parent, &self.directory_name)? == Some(expected)
            }
            None => false,
        };
        if !identity_matches {
            errors.push("destination semantic probe directory was substituted".into());
        } else if unsafe {
            libc::unlinkat(
                self.parent.as_raw_fd(),
                self.directory_name.as_ptr(),
                libc::AT_REMOVEDIR,
            )
        } != 0
        {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::NotFound {
                errors.push(format!(
                    "could not remove destination semantic probe: {error}"
                ));
            }
        } else {
            self.armed = false;
        }
        if errors.is_empty() {
            self.armed = false;
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

fn filesystem_matches_any(
    parent: &File,
    reserved: &[Vec<u8>],
    candidate: &OsStr,
) -> Result<bool, String> {
    filesystem_matches_any_with_fault(parent, reserved, candidate, None)
}

fn filesystem_matches_any_with_fault(
    parent: &File,
    reserved: &[Vec<u8>],
    candidate: &OsStr,
    fault: Option<SemanticProbeFault>,
) -> Result<bool, String> {
    let mut probe = SemanticProbe::create(parent, fault)?;
    let comparison = (|| {
        for leaf in reserved {
            probe.add_leaf(leaf)?;
            if fault == Some(SemanticProbeFault::AfterLeafCreate) {
                return Err("injected failure after semantic probe leaf creation".into());
            }
        }
        probe.contains(candidate)
    })();
    let cleanup = probe.cleanup();
    match cleanup {
        Ok(()) => comparison,
        Err(error) => Err(with_cleanup_error(
            comparison
                .err()
                .unwrap_or_else(|| "destination semantic probe cleanup failed".into()),
            Err(error),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_fault_leaves_no_residue(fault: SemanticProbeFault) {
        let root = std::env::temp_dir().join(format!("ade-dl-probe-fault-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let directory = File::open(&root).unwrap();
        let reserved = vec![b"report.pdf".to_vec()];
        let error = filesystem_matches_any_with_fault(
            &directory,
            &reserved,
            OsStr::new("REPORT.PDF"),
            Some(fault),
        )
        .unwrap_err();
        assert!(error.contains("injected failure"));
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn directory_setup_failure_cleans_exact_probe_inode() {
        assert_fault_leaves_no_residue(SemanticProbeFault::AfterDirectoryCreate);
    }

    #[test]
    fn leaf_setup_failure_cleans_leaf_and_exact_probe_inode() {
        assert_fault_leaves_no_residue(SemanticProbeFault::AfterLeafCreate);
    }
}

fn with_cleanup_error(primary: String, cleanup: Result<(), String>) -> String {
    match cleanup {
        Ok(()) => primary,
        Err(error) => format!("{primary}; cleanup failed: {error}"),
    }
}

fn metadata_identity_at(directory: &File, name: &CString) -> Result<Option<FileIdentity>, String> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: descriptor, C string, and output pointer are valid.
    let status = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status != 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error.to_string())
        };
    }
    // SAFETY: successful fstatat initialized stat.
    let stat = unsafe { stat.assume_init() };
    Ok(Some(FileIdentity {
        device: identity_component(stat.st_dev, "device")?,
        inode: identity_component(stat.st_ino, "inode")?,
    }))
}

fn identity_component<T>(value: T, label: &str) -> Result<u64, String>
where
    T: TryInto<u64>,
{
    value
        .try_into()
        .map_err(|_| format!("invalid destination {label} identity"))
}

/// Chooses and exclusively leases the exact final leaf. Filesystem inspection
/// happens outside the registry lock; only the final insert is serialized.
pub(super) fn reserve_name(
    reservations: &Arc<DestinationReservations>,
    directory: &std::fs::File,
    directory_identity: FileIdentity,
    directory_path: &Path,
    requested: &OsStr,
    collision: DownloadCollisionPolicy,
    mut inspect: impl FnMut(&OsStr) -> Result<Option<InspectedDestination>, String>,
) -> Result<ReservedName, String> {
    let name_max = directory_name_max(directory)?;
    if requested.as_bytes().len() > name_max {
        return Err("destination basename exceeds filesystem NAME_MAX".into());
    }
    if requested.as_bytes().contains(&0) {
        return Err("destination basename contains a NUL byte".into());
    }
    if collision != DownloadCollisionPolicy::Rename {
        let existing = inspect(requested)?;
        let overwrite_identity = match (collision, existing) {
            (DownloadCollisionPolicy::Fail, Some(_)) => {
                return Err("destination already exists".into());
            }
            (DownloadCollisionPolicy::OverwriteConfirmed, Some(entry)) if entry.regular => {
                Some(entry.identity)
            }
            (DownloadCollisionPolicy::OverwriteConfirmed, Some(_)) => {
                return Err("overwrite destination must be a regular file".into());
            }
            (_, None) => None,
            (DownloadCollisionPolicy::Rename, _) => unreachable!(),
        };
        let Some(lease) =
            reservations.try_reserve(directory, directory_identity, directory_path, requested)?
        else {
            return Err("download destination is already reserved by another transfer".into());
        };
        return Ok(ReservedName {
            final_name: CString::new(requested.as_bytes())
                .map_err(|_| "destination basename contains a NUL byte")?,
            overwrite_identity,
            lease,
        });
    }

    let mut externally_reserved = Vec::new();
    loop {
        let chosen = first_free_name(requested, name_max, |candidate| {
            Ok(inspect(candidate)?.is_some()
                || reservations.contains(directory, directory_identity, candidate)?
                || externally_reserved
                    .iter()
                    .any(|name: &std::ffi::OsString| name == candidate))
        })?;
        let Some(lease) =
            reservations.try_reserve(directory, directory_identity, directory_path, &chosen)?
        else {
            // Another thread won this exact leaf between the read and insert.
            // Restart the shared bounded naming walk against its new state.
            externally_reserved.push(chosen);
            continue;
        };
        return Ok(ReservedName {
            final_name: CString::new(chosen.as_bytes())
                .map_err(|_| "destination basename contains a NUL byte")?,
            overwrite_identity: None,
            lease,
        });
    }
}
