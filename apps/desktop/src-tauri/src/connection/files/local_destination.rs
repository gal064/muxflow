use std::{
    ffi::{CString, OsStr},
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::MetadataExt,
        io::{AsRawFd, FromRawFd},
    },
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use tmux_agent_protocol::{PublicationOutcome, PublishFailure, PublishResult, Published};
use uuid::Uuid;

use super::{
    destination_lease::{DestinationLease, FileIdentity, InspectedDestination, reserve_name},
    download_manager::DownloadCollisionPolicy,
};

pub(super) use super::destination_lease::DestinationReservations;

pub(super) struct PreparedDestination {
    _reservation: Arc<ReservedDestination>,
    directory: File,
    directory_path: PathBuf,
    directory_identity: FileIdentity,
    final_name: CString,
    final_display: PathBuf,
    partial_name: CString,
    transaction_name: CString,
    overwrite_identity: Option<FileIdentity>,
    collision: DownloadCollisionPolicy,
}

pub(super) struct ReservedDestination {
    directory: File,
    directory_path: PathBuf,
    directory_identity: FileIdentity,
    final_name: CString,
    final_display: PathBuf,
    overwrite_identity: Option<FileIdentity>,
    collision: DownloadCollisionPolicy,
    _lease: DestinationLease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Explicit fault-injection points are constructed only by tests.
enum LocalPublishFault {
    DirectoryFsync,
    Cleanup,
    RollbackCleanup,
    RollbackSubstitution,
}

const MAX_LOCAL_JOURNAL_BYTES: u64 = 4096;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum LocalJournalState {
    #[default]
    Prepared,
    Published,
    Reconciled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalPublishJournal {
    schema_version: u32,
    state: LocalJournalState,
    final_name: Vec<u8>,
    partial_name: Vec<u8>,
    new_identity: FileIdentity,
    original_identity: Option<FileIdentity>,
}

struct LocalJournalHandle {
    file: File,
    value: LocalPublishJournal,
}

impl Drop for LocalJournalHandle {
    fn drop(&mut self) {
        // Close also releases flock, but an explicit unlock makes the
        // crash-recovery handoff deterministic before another thread scans the
        // directory under a heavily parallel test or reconnect workload.
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl ReservedDestination {
    pub(super) fn reserve(
        path: &Path,
        collision: DownloadCollisionPolicy,
        reservations: Arc<DestinationReservations>,
    ) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err("download destination path must be absolute".into());
        }
        let directory_path = path
            .parent()
            .ok_or("download destination has no parent")?
            .to_owned();
        let directory_name = c_string(directory_path.as_os_str(), "destination parent")?;
        // SAFETY: directory_name is live for this call and the returned fd is
        // immediately owned by File.
        let fd = unsafe {
            libc::open(
                directory_name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(format!(
                "destination parent is unavailable or unsafe: {}",
                io::Error::last_os_error()
            ));
        }
        // SAFETY: fd was freshly returned by open and is uniquely owned.
        let directory = unsafe { File::from_raw_fd(fd) };
        let metadata = directory.metadata().map_err(|error| error.to_string())?;
        let directory_identity = FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        let requested = path
            .file_name()
            .ok_or("download destination has no basename")?;
        let reserved = reserve_name(
            &reservations,
            &directory,
            directory_identity,
            &directory_path,
            requested,
            collision,
            |candidate| {
                let candidate = c_string(candidate, "destination basename")?;
                Ok(
                    metadata_at(&directory, &candidate)?.map(|entry| InspectedDestination {
                        identity: entry.identity,
                        regular: entry.regular,
                    }),
                )
            },
        )?;
        let final_display =
            directory_path.join(std::ffi::OsStr::from_bytes(reserved.final_name.as_bytes()));
        Ok(Self {
            directory,
            directory_path,
            directory_identity,
            final_name: reserved.final_name,
            final_display,
            overwrite_identity: reserved.overwrite_identity,
            collision,
            _lease: reserved.lease,
        })
    }

    #[cfg(test)]
    pub(super) fn final_path(&self) -> &Path {
        &self.final_display
    }

    #[cfg(test)]
    fn release_as(&self, owner: Uuid) {
        self._lease.release_as(owner);
    }
}

impl PreparedDestination {
    #[cfg(test)]
    pub(super) fn open(path: &Path, collision: DownloadCollisionPolicy) -> Result<Self, String> {
        let reserved = Arc::new(ReservedDestination::reserve(
            path,
            collision,
            Arc::default(),
        )?);
        Self::prepare(reserved)
    }

    pub(super) fn prepare(reserved: Arc<ReservedDestination>) -> Result<Self, String> {
        let directory = reserved
            .directory
            .try_clone()
            .map_err(|error| error.to_string())?;
        recover_local_transactions(&directory)?;
        let transaction_id = Uuid::new_v4();
        let partial_name = CString::new(format!(".tmux-agent-download-{transaction_id}.partial"))
            .expect("UUID partial name contains no NUL");
        let transaction_name = CString::new(format!(
            ".tmux-agent-download-transaction-{transaction_id}.json"
        ))
        .expect("UUID transaction name contains no NUL");
        let prepared = Self {
            _reservation: Arc::clone(&reserved),
            directory,
            directory_path: reserved.directory_path.clone(),
            directory_identity: reserved.directory_identity,
            final_name: reserved.final_name.clone(),
            final_display: reserved.final_display.clone(),
            partial_name,
            transaction_name,
            overwrite_identity: reserved.overwrite_identity,
            collision: reserved.collision,
        };
        prepared.probe_writable()?;
        Ok(prepared)
    }

    pub(super) fn final_path(&self) -> &Path {
        &self.final_display
    }

    pub(super) fn ensure_available(&self, required: u64) -> Result<(), String> {
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: the directory fd and output pointer are valid.
        let result = unsafe { libc::fstatvfs(self.directory.as_raw_fd(), stats.as_mut_ptr()) };
        if result != 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        // SAFETY: successful fstatvfs initialized the value.
        let stats = unsafe { stats.assume_init() };
        let available = u128::from(stats.f_bavail).saturating_mul(u128::from(stats.f_frsize));
        let available = u64::try_from(available).unwrap_or(u64::MAX);
        if available < required {
            return Err(format!(
                "insufficient destination space: need {required} bytes, have {available}"
            ));
        }
        Ok(())
    }

    pub(super) fn create_partial(&self) -> Result<File, String> {
        // SAFETY: directory fd and C string are valid. O_NOFOLLOW plus
        // O_EXCL prevents link following and substitution.
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                self.partial_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(format!(
                "destination preflight failed: {}",
                io::Error::last_os_error()
            ));
        }
        // SAFETY: openat returned a new uniquely owned fd.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub(super) fn cleanup_partial(&self) -> Result<(), String> {
        let id = journal_uuid(self.transaction_name.as_bytes())
            .ok_or("download transaction journal name is not app-owned")?;
        OwnedDeleteLeaf::Partial(id).validate(&self.partial_name)?;
        // SAFETY: directory fd and C string remain valid.
        let result =
            unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.partial_name.as_ptr(), 0) };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(format!("local partial cleanup failed: {error}"))
        }
    }

    pub(super) fn publish(&self) -> PublishResult<()> {
        self.publish_with_fault(None)
    }

    fn publish_with_fault(&self, fault: Option<LocalPublishFault>) -> PublishResult<()> {
        self.validate_namespace().map_err(not_published)?;
        let new_identity = metadata_at(&self.directory, &self.partial_name)
            .map_err(not_published)?
            .ok_or_else(|| not_published("download partial disappeared before publication".into()))?
            .identity;
        let had_original = match self.collision {
            DownloadCollisionPolicy::OverwriteConfirmed => match self.overwrite_identity {
                Some(expected) => {
                    let current = metadata_at(&self.directory, &self.final_name)
                        .map_err(not_published)?
                        .ok_or_else(|| {
                            not_published("confirmed overwrite destination disappeared".into())
                        })?;
                    if !current.regular || current.identity != expected {
                        return Err(not_published(
                            "confirmed overwrite destination was substituted; refusing overwrite"
                                .into(),
                        ));
                    }
                    true
                }
                None => false,
            },
            DownloadCollisionPolicy::Fail | DownloadCollisionPolicy::Rename => false,
        };
        let mut journal = self
            .create_transaction_journal(new_identity)
            .map_err(not_published)?;
        let rename_result = if had_original {
            exchange_at(&self.directory, &self.partial_name, &self.final_name)
        } else {
            rename_at(&self.directory, &self.partial_name, &self.final_name, true)
        };
        if let Err(error) = rename_result {
            let _ = self.delete_transaction_journal(&journal.file);
            return Err(not_published(error));
        }
        let durable = if matches!(
            fault,
            Some(
                LocalPublishFault::DirectoryFsync
                    | LocalPublishFault::RollbackCleanup
                    | LocalPublishFault::RollbackSubstitution
            )
        ) {
            Err("injected destination directory fsync failure".into())
        } else {
            self.directory.sync_all().map_err(|error| error.to_string())
        };
        if let Err(error) = durable {
            return Err(self.rollback_publication(new_identity, had_original, &error, fault));
        }
        if let Err(error) = rewrite_local_journal(&mut journal, LocalJournalState::Published) {
            return Err(self.rollback_publication(new_identity, had_original, &error, fault));
        }
        let mut cleanup_errors = Vec::new();
        if had_original {
            if fault == Some(LocalPublishFault::Cleanup) {
                cleanup_errors.push("original destination backup retained".into());
            } else {
                if let Err(error) = self.cleanup_partial() {
                    cleanup_errors.push(error);
                }
                if let Err(error) = self.directory.sync_all() {
                    cleanup_errors.push(format!("published backup cleanup fsync failed: {error}"));
                }
            }
        }
        if let Err(error) = rewrite_local_journal(&mut journal, LocalJournalState::Reconciled) {
            cleanup_errors.push(format!(
                "published transaction reconciliation failed: {error}"
            ));
        } else if cleanup_errors.is_empty()
            && let Err(error) = self.delete_transaction_journal(&journal.file)
        {
            cleanup_errors.push(format!(
                "published transaction journal cleanup failed: {error}"
            ));
        }
        Ok(Published {
            value: (),
            cleanup_error: (!cleanup_errors.is_empty()).then(|| cleanup_errors.join("; ")),
        })
    }

    fn create_transaction_journal(
        &self,
        new_identity: FileIdentity,
    ) -> Result<LocalJournalHandle, String> {
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                self.transaction_name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(format!(
                "could not create download transaction journal: {}",
                io::Error::last_os_error()
            ));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        let mut handle = LocalJournalHandle {
            file,
            value: LocalPublishJournal {
                schema_version: 1,
                state: LocalJournalState::Prepared,
                final_name: self.final_name.as_bytes().to_vec(),
                partial_name: self.partial_name.as_bytes().to_vec(),
                new_identity,
                original_identity: self.overwrite_identity,
            },
        };
        rewrite_local_journal(&mut handle, LocalJournalState::Prepared)?;
        self.directory
            .sync_all()
            .map_err(|error| error.to_string())?;
        Ok(handle)
    }

    fn delete_transaction_journal(&self, journal: &File) -> Result<(), String> {
        let expected = journal.metadata().map_err(|error| error.to_string())?;
        identity_preserving_delete(
            &self.directory,
            &self.transaction_name,
            FileIdentity {
                device: expected.dev(),
                inode: expected.ino(),
            },
            OwnedDeleteLeaf::Journal(
                journal_uuid(self.transaction_name.as_bytes())
                    .ok_or("download transaction journal name is not app-owned")?,
            ),
        )?;
        self.directory.sync_all().map_err(|error| error.to_string())
    }

    fn rollback_publication(
        &self,
        new_identity: FileIdentity,
        had_original: bool,
        cause: &str,
        fault: Option<LocalPublishFault>,
    ) -> PublishFailure {
        let quarantine = CString::new(format!(
            ".tmux-agent-download-rollback-{}.partial",
            Uuid::new_v4()
        ))
        .expect("UUID rollback name contains no NUL");
        if let Err(error) = rename_at(&self.directory, &self.final_name, &quarantine, true) {
            return unknown(format!("{cause}; destination quarantine failed: {error}"));
        }
        if fault == Some(LocalPublishFault::RollbackSubstitution) {
            let preserved = CString::new(format!(
                ".tmux-agent-download-test-preserved-{}.partial",
                Uuid::new_v4()
            ))
            .expect("UUID test-preserved name contains no NUL");
            if let Err(error) = rename_at(&self.directory, &quarantine, &preserved, true) {
                return unknown(format!(
                    "{cause}; injected rollback substitution setup failed: {error}"
                ));
            }
            let fd = unsafe {
                libc::openat(
                    self.directory.as_raw_fd(),
                    quarantine.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd < 0 {
                return unknown(format!(
                    "{cause}; injected rollback substitution create failed: {}",
                    io::Error::last_os_error()
                ));
            }
            let mut foreign = unsafe { File::from_raw_fd(fd) };
            if let Err(error) = std::io::Write::write_all(&mut foreign, b"foreign-substitute") {
                return unknown(format!(
                    "{cause}; injected rollback substitution write failed: {error}"
                ));
            }
        }
        let current = match metadata_at(&self.directory, &quarantine) {
            Ok(Some(metadata)) => metadata.identity,
            Ok(None) => {
                return unknown(format!("{cause}; quarantined destination disappeared"));
            }
            Err(error) => {
                return unknown(format!("{cause}; quarantine inspection failed: {error}"));
            }
        };
        if current != new_identity {
            return unknown(format!(
                "{cause}; substituted destination retained in quarantine"
            ));
        }
        if had_original
            && let Err(error) =
                rename_at(&self.directory, &self.partial_name, &self.final_name, true)
        {
            return unknown(format!(
                "{cause}; original destination restore failed: {error}"
            ));
        }
        if fault == Some(LocalPublishFault::RollbackCleanup) {
            return not_published(format!(
                "{cause}; injected verified replacement quarantine cleanup failure"
            ));
        }
        if let Err(error) =
            validate_generated_uuid_leaf(&quarantine, ".tmux-agent-download-rollback-", ".partial")
        {
            return unknown(format!("{cause}; invalid owned rollback leaf: {error}"));
        }
        let result = unsafe { libc::unlinkat(self.directory.as_raw_fd(), quarantine.as_ptr(), 0) };
        if result != 0 {
            return not_published(format!(
                "{cause}; verified replacement retained for cleanup: {}",
                io::Error::last_os_error()
            ));
        }
        let _ = self.directory.sync_all();
        not_published(cause.to_owned())
    }

    fn probe_writable(&self) -> Result<(), String> {
        let probe = CString::new(format!(".tmux-agent-preflight-{}", Uuid::new_v4()))
            .expect("UUID probe contains no NUL");
        // SAFETY: descriptor and C string are valid and O_EXCL makes ownership
        // of the probe unambiguous.
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                probe.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(format!(
                "destination is not writable: {}",
                io::Error::last_os_error()
            ));
        }
        unsafe { libc::close(fd) };
        validate_generated_uuid_leaf(&probe, ".tmux-agent-preflight-", "")?;
        // SAFETY: the probe was just created by this process in the anchored
        // directory. Cleanup is descriptor-relative.
        let unlinked = unsafe { libc::unlinkat(self.directory.as_raw_fd(), probe.as_ptr(), 0) };
        if unlinked != 0 {
            return Err(format!(
                "destination probe cleanup failed: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    fn validate_namespace(&self) -> Result<(), String> {
        let metadata = std::fs::symlink_metadata(&self.directory_path)
            .map_err(|_| "destination parent changed during download")?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.dev() != self.directory_identity.device
            || metadata.ino() != self.directory_identity.inode
        {
            return Err("destination parent changed during download".into());
        }
        Ok(())
    }
}

fn journal_uuid(name: &[u8]) -> Option<Uuid> {
    let text = std::str::from_utf8(name).ok()?;
    let id = text
        .strip_prefix(".tmux-agent-download-transaction-")?
        .strip_suffix(".json")?;
    Uuid::parse_str(id).ok()
}

fn rewrite_local_journal(
    handle: &mut LocalJournalHandle,
    state: LocalJournalState,
) -> Result<(), String> {
    handle.value.state = state;
    let bytes = serde_json::to_vec(&handle.value).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_LOCAL_JOURNAL_BYTES {
        return Err("download transaction journal exceeds its private bound".into());
    }
    handle
        .file
        .seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    handle.file.set_len(0).map_err(|error| error.to_string())?;
    handle
        .file
        .write_all(&bytes)
        .map_err(|error| error.to_string())?;
    handle.file.sync_all().map_err(|error| error.to_string())
}

fn recover_local_transactions(directory: &File) -> Result<(), String> {
    for name in super::local_staging::directory_entry_names(directory)? {
        let text = name.to_string_lossy();
        let Some(id) = text
            .strip_prefix(".tmux-agent-download-transaction-")
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|id| Uuid::parse_str(id).is_ok())
        else {
            continue;
        };
        let journal_id = Uuid::parse_str(id).map_err(|error| error.to_string())?;
        let name_c = c_string(&name, "download transaction journal")?;
        let Some(metadata) = metadata_at(directory, &name_c)? else {
            continue;
        };
        if !metadata.regular
            || metadata.uid != unsafe { libc::geteuid() }
            || metadata.mode & 0o077 != 0
        {
            continue;
        }
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name_c.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            continue;
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
                continue;
            }
            return Err(error.to_string());
        }
        let opened = file.metadata().map_err(|error| error.to_string())?;
        let journal_identity = FileIdentity {
            device: opened.dev(),
            inode: opened.ino(),
        };
        if journal_identity != metadata.identity
            || !opened.is_file()
            || opened.uid() != unsafe { libc::geteuid() }
            || opened.mode() & 0o077 != 0
            || opened.len() > MAX_LOCAL_JOURNAL_BYTES
        {
            continue;
        }
        let mut bytes = Vec::with_capacity(opened.len() as usize);
        file.take(MAX_LOCAL_JOURNAL_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_LOCAL_JOURNAL_BYTES {
            continue;
        }
        let journal: LocalPublishJournal =
            match serde_json::from_slice::<LocalPublishJournal>(&bytes) {
                Ok(value)
                    if value.schema_version == 1
                        && validate_leaf_basename(&value.final_name).is_ok()
                        && value.partial_name == expected_partial_name(journal_id).as_bytes() =>
                {
                    value
                }
                _ => continue,
            };
        let final_name = CString::new(journal.final_name)
            .map_err(|_| "download journal final name contains NUL")?;
        let partial_name = CString::new(journal.partial_name)
            .map_err(|_| "download journal partial name contains NUL")?;
        let target_is_new = metadata_at(directory, &final_name)?
            .is_some_and(|current| current.regular && current.identity == journal.new_identity);
        if target_is_new {
            if let Some(original) = journal.original_identity
                && metadata_at(directory, &partial_name)?
                    .is_some_and(|current| current.identity == original)
            {
                identity_preserving_delete(
                    directory,
                    &partial_name,
                    original,
                    OwnedDeleteLeaf::Partial(journal_id),
                )?;
            }
        } else if metadata_at(directory, &partial_name)?
            .is_some_and(|current| current.identity == journal.new_identity)
        {
            identity_preserving_delete(
                directory,
                &partial_name,
                journal.new_identity,
                OwnedDeleteLeaf::Partial(journal_id),
            )?;
        }
        identity_preserving_delete(
            directory,
            &name_c,
            journal_identity,
            OwnedDeleteLeaf::Journal(journal_id),
        )?;
        directory.sync_all().map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn identity_preserving_delete(
    directory: &File,
    name: &CString,
    expected: FileIdentity,
    allowed: OwnedDeleteLeaf,
) -> Result<bool, String> {
    allowed.validate(name)?;
    let quarantine = CString::new(format!(
        ".tmux-agent-download-quarantine-{}.partial",
        Uuid::new_v4()
    ))
    .expect("UUID quarantine contains no NUL");
    validate_generated_uuid_leaf(&quarantine, ".tmux-agent-download-quarantine-", ".partial")?;
    rename_at(directory, name, &quarantine, true)?;
    let current = metadata_at(directory, &quarantine)?;
    if !current.is_some_and(|metadata| metadata.identity == expected) {
        return Ok(false);
    }
    if unsafe { libc::unlinkat(directory.as_raw_fd(), quarantine.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    Ok(true)
}

#[derive(Clone, Copy)]
enum OwnedDeleteLeaf {
    Journal(Uuid),
    Partial(Uuid),
}

impl OwnedDeleteLeaf {
    fn validate(self, actual: &CString) -> Result<(), String> {
        validate_leaf_basename(actual.as_bytes())?;
        let expected = match self {
            Self::Journal(id) => expected_journal_name(id),
            Self::Partial(id) => expected_partial_name(id),
        };
        if actual.as_bytes() != expected.as_bytes() {
            return Err("refusing descriptor deletion for an unowned download leaf".into());
        }
        Ok(())
    }
}

fn expected_partial_name(id: Uuid) -> CString {
    CString::new(format!(".tmux-agent-download-{id}.partial"))
        .expect("UUID partial name contains no NUL")
}

fn expected_journal_name(id: Uuid) -> CString {
    CString::new(format!(".tmux-agent-download-transaction-{id}.json"))
        .expect("UUID journal name contains no NUL")
}

fn validate_leaf_basename(name: &[u8]) -> Result<(), String> {
    if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') {
        return Err("download transaction leaf must be one validated basename".into());
    }
    Ok(())
}

fn validate_generated_uuid_leaf(name: &CString, prefix: &str, suffix: &str) -> Result<(), String> {
    validate_leaf_basename(name.as_bytes())?;
    let text = name
        .to_str()
        .map_err(|_| "owned download leaf is not UTF-8")?;
    let id = text
        .strip_prefix(prefix)
        .and_then(|value| value.strip_suffix(suffix))
        .ok_or("owned download leaf has an invalid type")?;
    Uuid::parse_str(id)
        .map(|_| ())
        .map_err(|_| "owned download leaf has an invalid UUID".to_owned())
}

fn not_published(message: String) -> PublishFailure {
    PublishFailure {
        outcome: PublicationOutcome::NotPublished,
        message,
    }
}

fn unknown(message: String) -> PublishFailure {
    PublishFailure {
        outcome: PublicationOutcome::Unknown,
        message,
    }
}

struct EntryMetadata {
    identity: FileIdentity,
    regular: bool,
    uid: libc::uid_t,
    mode: libc::mode_t,
}

fn metadata_at(directory: &File, name: &CString) -> Result<Option<EntryMetadata>, String> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: descriptor, C string, and output pointer are valid.
    let result = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error.to_string())
        };
    }
    // SAFETY: successful fstatat initialized stat.
    let stat = unsafe { stat.assume_init() };
    Ok(Some(EntryMetadata {
        identity: FileIdentity {
            device: u64::try_from(stat.st_dev)
                .map_err(|_| "destination device identity is invalid".to_owned())?,
            inode: stat.st_ino,
        },
        regular: (stat.st_mode & libc::S_IFMT) == libc::S_IFREG,
        uid: stat.st_uid,
        mode: stat.st_mode,
    }))
}

fn rename_at(
    directory: &File,
    source: &CString,
    destination: &CString,
    exclusive: bool,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let result = unsafe {
        if exclusive {
            libc::syscall(
                libc::SYS_renameat2,
                directory.as_raw_fd(),
                source.as_ptr(),
                directory.as_raw_fd(),
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            ) as libc::c_int
        } else {
            libc::renameat(
                directory.as_raw_fd(),
                source.as_ptr(),
                directory.as_raw_fd(),
                destination.as_ptr(),
            )
        }
    };
    #[cfg(target_os = "macos")]
    let result = unsafe {
        if exclusive {
            libc::renameatx_np(
                directory.as_raw_fd(),
                source.as_ptr(),
                directory.as_raw_fd(),
                destination.as_ptr(),
                libc::RENAME_EXCL,
            )
        } else {
            libc::renameat(
                directory.as_raw_fd(),
                source.as_ptr(),
                directory.as_raw_fd(),
                destination.as_ptr(),
            )
        }
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let result = if exclusive {
        -1
    } else {
        unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                source.as_ptr(),
                directory.as_raw_fd(),
                destination.as_ptr(),
            )
        }
    };
    if result == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        if exclusive
            && matches!(
                error.raw_os_error(),
                Some(libc::ENOSYS) | Some(libc::EINVAL)
            )
        {
            Err("atomic no-replace rename is unavailable; refusing unsafe fallback".into())
        } else {
            Err(error.to_string())
        }
    }
}

#[cfg(target_os = "linux")]
fn exchange_at(directory: &File, left: &CString, right: &CString) -> Result<(), String> {
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            directory.as_raw_fd(),
            left.as_ptr(),
            directory.as_raw_fd(),
            right.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn exchange_at(directory: &File, left: &CString, right: &CString) -> Result<(), String> {
    let result = unsafe {
        libc::renameatx_np(
            directory.as_raw_fd(),
            left.as_ptr(),
            directory.as_raw_fd(),
            right.as_ptr(),
            libc::RENAME_SWAP,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error().to_string())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn exchange_at(_directory: &File, _left: &CString, _right: &CString) -> Result<(), String> {
    Err("transactional overwrite exchange is unavailable on this platform".into())
}

fn c_string(value: &OsStr, label: &str) -> Result<CString, String> {
    CString::new(value.as_bytes()).map_err(|_| format!("{label} contains a NUL byte"))
}

#[cfg(test)]
#[path = "local_destination/tests.rs"]
mod tests;
