use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::{fs, os::unix::fs::PermissionsExt};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use tmux_agent_protocol::{PublicationOutcome, PublishFailure, PublishResult, Published, v1};
use uuid::Uuid;

use super::{FileService, MAX_IMAGE_BYTES, MAX_TRANSFER_CHUNK, validate_transfer_id};

mod reconcile;
mod staging;
mod stream;
use staging::{
    StagingDirectory, lock_exclusive, lock_shared, set_private_file_mode, try_lock_exclusive,
};
use stream::UploadStream;

const LARGE_UPLOAD_BYTES: u64 = 500 * 1024 * 1024;
const STALE_PARTIAL_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const STALE_PARTIAL_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
const PARTIAL_PREFIX: &str = ".tmux-agent-upload-";
const PARTIAL_SUFFIX: &str = ".partial";
const RESERVATION_PREFIX: &str = ".tmux-agent-upload-reservation-";
const RESERVATION_SUFFIX: &str = ".lock";
const COMPLETED_STAGING_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const COMPLETED_STAGING_SIZE_MIN_AGE: Duration = Duration::from_secs(60 * 60);
const COMPLETED_STAGING_TOTAL_BYTES: u64 = 20 * 1024 * 1024 * 1024;
const OWNED_MANIFEST_PREFIX: &str = ".tmux-agent-owned-";
const OWNED_MANIFEST_SUFFIX: &str = ".json";
const MAX_MANIFEST_BYTES: u64 = 4096;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum UploadJournalState {
    #[default]
    Prepared,
    Published,
    Reconciled,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OwnedUploadManifest {
    schema_version: u32,
    transfer_id: String,
    final_name: String,
    device: u64,
    inode: u64,
    size: u64,
    created_unix_seconds: u64,
    #[serde(default)]
    blake3: String,
    #[serde(default)]
    transaction_state: UploadJournalState,
    #[serde(default)]
    original_device: Option<u64>,
    #[serde(default)]
    original_inode: Option<u64>,
}

pub(super) fn cleanup_dropped_terminal_upload(upload: TerminalUpload) {
    let _ = upload.directory.unlink(&upload.temporary_name);
    let _ = upload.directory.unlink(&upload.reservation_name);
}

pub(super) struct TerminalUpload {
    stream: UploadStream,
    directory: StagingDirectory,
    temporary_name: OsString,
    target_name: OsString,
    reservation_name: OsString,
    // The held exclusive lock makes the target reservation process-global,
    // including separate SSH helper processes.
    _reservation_file: File,
    collision: v1::CollisionPolicy,
    overwrite_identity: Option<(u64, u64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Explicit publication fault-injection points are test-driven.
enum PublicationFault {
    ManifestCreate,
    ManifestWrite,
    ManifestFsync,
    PostRenameDirectoryFsync,
    PublishedJournalWrite,
    PublishedJournalFsync,
    PostJournalRenameDirectoryFsync,
    ReconciledJournalWrite,
    ReconciledJournalFsync,
    Cleanup,
    RollbackSubstitution,
}

#[derive(Debug)]
pub(crate) struct UploadCommitFailure {
    pub(crate) outcome: PublicationOutcome,
    pub(crate) cleanup_failed: bool,
    pub(crate) message: String,
}

impl std::fmt::Display for UploadCommitFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for UploadCommitFailure {}

impl FileService {
    pub(crate) fn prepare_terminal_upload(
        &self,
        transfer_id: &str,
        destination_name: &str,
        total: u64,
        collision: v1::CollisionPolicy,
        large_upload_confirmed: bool,
        image_png: bool,
    ) -> anyhow::Result<v1::UploadDescriptor> {
        let directory = terminal_staging_directory()?;
        self.prepare_terminal_upload_in_directory(
            directory,
            transfer_id,
            destination_name,
            total,
            collision,
            large_upload_confirmed,
            image_png,
        )
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn prepare_terminal_upload_in(
        &self,
        directory: &Path,
        transfer_id: &str,
        destination_name: &str,
        total: u64,
        collision: v1::CollisionPolicy,
        large_upload_confirmed: bool,
        image_png: bool,
    ) -> anyhow::Result<v1::UploadDescriptor> {
        self.prepare_terminal_upload_in_directory(
            StagingDirectory::open(directory)?,
            transfer_id,
            destination_name,
            total,
            collision,
            large_upload_confirmed,
            image_png,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_terminal_upload_in_directory(
        &self,
        directory: StagingDirectory,
        transfer_id: &str,
        destination_name: &str,
        total: u64,
        collision: v1::CollisionPolicy,
        large_upload_confirmed: bool,
        image_png: bool,
    ) -> anyhow::Result<v1::UploadDescriptor> {
        validate_transfer_id(transfer_id)?;
        Uuid::parse_str(transfer_id).context("terminal upload transfer ID must be a UUID")?;
        validate_destination_basename(destination_name)?;
        if total > LARGE_UPLOAD_BYTES && !large_upload_confirmed {
            bail!(
                "large_upload_confirmation_required: uploads above 500 MiB require explicit confirmation"
            );
        }
        if image_png && total > MAX_IMAGE_BYTES {
            bail!("clipboard PNG exceeds the 25 MiB encoded-image limit");
        }
        if self
            .terminal_uploads
            .lock()
            .unwrap()
            .contains_key(transfer_id)
        {
            bail!("terminal upload transfer ID is already active");
        }

        let cleanup_error = cleanup_owned_staging(&directory)
            .err()
            .map(|error| error.to_string());
        let (target_name, collision_renamed, reservation_name, reservation_file) =
            choose_and_reserve_destination(&directory, destination_name, collision)?;
        let resolved_name = target_name.to_string_lossy().into_owned();
        let overwrite_identity = if collision == v1::CollisionPolicy::OverwriteConfirmed {
            match directory.metadata(&target_name) {
                Ok(metadata) => Some((metadata.dev(), metadata.ino())),
                Err(error) if is_not_found(&error) => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let available = directory.available_bytes()?;
        if available < total {
            bail!("insufficient destination space: need {total} bytes, have {available}");
        }
        let temporary_name =
            OsString::from(format!("{PARTIAL_PREFIX}{transfer_id}{PARTIAL_SUFFIX}"));
        let file = directory.create_private(&temporary_name)?;
        lock_exclusive(&file)?;
        self.terminal_uploads.lock().unwrap().insert(
            transfer_id.to_owned(),
            TerminalUpload {
                stream: UploadStream::new(file, total),
                directory,
                temporary_name,
                target_name,
                reservation_name,
                _reservation_file: reservation_file,
                collision,
                overwrite_identity,
            },
        );
        Ok(v1::UploadDescriptor {
            transfer_id: transfer_id.to_owned(),
            destination_name: resolved_name,
            total_bytes: total,
            collision_renamed,
            available_bytes: available,
            cleanup_status: if cleanup_error.is_some() {
                v1::CleanupStatus::Failed.into()
            } else {
                v1::CleanupStatus::NotNeeded.into()
            },
            cleanup_error: cleanup_error.unwrap_or_default(),
            ..Default::default()
        })
    }

    pub(crate) fn write_terminal_upload_chunk(
        &self,
        transfer_id: &str,
        offset: u64,
        data: &[u8],
    ) -> anyhow::Result<u64> {
        validate_transfer_id(transfer_id)?;
        let mut uploads = self.terminal_uploads.lock().unwrap();
        let upload = uploads
            .get_mut(transfer_id)
            .context("terminal upload is not active")?;
        upload.stream.write_chunk(offset, data)
    }

    pub(crate) fn commit_terminal_upload(
        &self,
        transfer_id: &str,
        expected_blake3: &str,
    ) -> Result<v1::UploadDescriptor, UploadCommitFailure> {
        self.commit_terminal_upload_with_fault(transfer_id, expected_blake3, None)
    }

    fn commit_terminal_upload_with_fault(
        &self,
        transfer_id: &str,
        expected_blake3: &str,
        fault: Option<PublicationFault>,
    ) -> Result<v1::UploadDescriptor, UploadCommitFailure> {
        validate_transfer_id(transfer_id).map_err(upload_commit_not_published)?;
        let mut upload = self
            .terminal_uploads
            .lock()
            .unwrap()
            .remove(transfer_id)
            .context("terminal upload is not active")
            .map_err(upload_commit_not_published)?;
        let mut preserve_partial_on_error = false;
        let mut failure_outcome = PublicationOutcome::NotPublished;
        let result = (|| -> anyhow::Result<v1::UploadDescriptor> {
            let digest = upload.stream.verified_digest(expected_blake3)?;
            use std::io::Write as _;
            upload.stream.file.flush()?;
            upload.stream.file.sync_all()?;
            if upload.stream.file.metadata()?.len() != upload.stream.total {
                bail!("terminal-upload final byte count verification failed");
            }
            set_private_file_mode(&upload.stream.file)?;
            if !upload.directory.current_namespace_matches() {
                bail!("private staging directory was replaced before upload commit");
            }
            let had_original = validate_overwrite_identity(&upload)?;
            let mut staged_manifest = stage_completed_upload_manifest(
                &upload.directory,
                transfer_id,
                &upload.target_name,
                &upload.stream.file,
                &digest,
                upload.overwrite_identity,
                fault,
            )?;
            let publish_result =
                publish_upload_transaction(&upload, &mut staged_manifest, had_original, fault);
            let published_cleanup = match publish_result {
                Ok(published) => published.cleanup_error.unwrap_or_default(),
                Err(error) => {
                    preserve_partial_on_error = error.outcome == PublicationOutcome::Unknown;
                    failure_outcome = error.outcome;
                    let _ = upload.directory.unlink(&staged_manifest.temporary_name);
                    let _ = upload.directory.unlink(&upload.reservation_name);
                    return Err(error.into());
                }
            };
            let reservation_cleanup = upload
                .directory
                .unlink(&upload.reservation_name)
                .err()
                .filter(|error| !is_not_found(error))
                .map(|error| error.to_string())
                .unwrap_or_default();
            let cleanup_error = [published_cleanup, reservation_cleanup]
                .into_iter()
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("; ");
            let final_path = upload.directory.path().join(&upload.target_name);
            Ok(v1::UploadDescriptor {
                transfer_id: transfer_id.to_owned(),
                destination_name: upload.target_name.to_string_lossy().into_owned(),
                total_bytes: upload.stream.total,
                final_path: final_path.to_string_lossy().into_owned(),
                verified: true,
                blake3: digest,
                cleanup_status: if cleanup_error.is_empty() {
                    v1::CleanupStatus::NotNeeded.into()
                } else {
                    v1::CleanupStatus::Retained.into()
                },
                cleanup_error,
                ..Default::default()
            })
        })();
        if let Err(error) = result {
            if !preserve_partial_on_error
                && let Err(cleanup) = upload.directory.unlink(&upload.temporary_name)
                && !is_not_found(&cleanup)
            {
                return Err(UploadCommitFailure {
                    outcome: failure_outcome,
                    cleanup_failed: true,
                    message: format!("{error}; partial cleanup also failed: {cleanup}"),
                });
            }
            let _ = upload.directory.unlink(&upload.reservation_name);
            return Err(UploadCommitFailure {
                outcome: failure_outcome,
                cleanup_failed: false,
                message: error.to_string(),
            });
        }
        result.map_err(upload_commit_not_published)
    }

    pub(crate) fn cancel_terminal_upload(&self, transfer_id: &str) -> anyhow::Result<String> {
        self.cancel_terminal_upload_with_fault(transfer_id, false)
    }

    fn cancel_terminal_upload_with_fault(
        &self,
        transfer_id: &str,
        inject_unlink_failure: bool,
    ) -> anyhow::Result<String> {
        validate_transfer_id(transfer_id)?;
        let Some(upload) = self.terminal_uploads.lock().unwrap().remove(transfer_id) else {
            return Ok(String::new());
        };
        let partial_cleanup = if inject_unlink_failure {
            Err(anyhow::anyhow!("injected partial unlink failure"))
        } else {
            upload.directory.unlink(&upload.temporary_name)
        };
        let reservation_cleanup = if inject_unlink_failure {
            Err(anyhow::anyhow!("injected reservation unlink failure"))
        } else {
            upload.directory.unlink(&upload.reservation_name)
        };
        Ok([partial_cleanup.err(), reservation_cleanup.err()]
            .into_iter()
            .flatten()
            .filter(|error| !is_not_found(error))
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("; "))
    }

    pub(crate) fn reconcile_terminal_upload(
        &self,
        transfer_id: &str,
    ) -> anyhow::Result<v1::UploadDescriptor> {
        self.reconcile_terminal_upload_in_directory(terminal_staging_directory()?, transfer_id)
    }

    #[cfg(test)]
    fn reconcile_terminal_upload_in(
        &self,
        staging_path: &Path,
        transfer_id: &str,
    ) -> anyhow::Result<v1::UploadDescriptor> {
        self.reconcile_terminal_upload_in_directory(
            StagingDirectory::open(staging_path)?,
            transfer_id,
        )
    }
}

fn upload_commit_not_published(error: anyhow::Error) -> UploadCommitFailure {
    UploadCommitFailure {
        outcome: PublicationOutcome::NotPublished,
        cleanup_failed: false,
        message: error.to_string(),
    }
}

fn terminal_staging_directory() -> anyhow::Result<StagingDirectory> {
    let home = std::env::var_os("HOME").context("HOME is unavailable")?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        bail!("HOME must be absolute");
    }
    StagingDirectory::open_upload_cache(&home)
}

#[cfg(test)]
fn ensure_cache_parent(path: &Path) -> anyhow::Result<()> {
    StagingDirectory::ensure_cache_path(path)
}

mod policy;
use policy::*;

mod retention;
use retention::*;

mod transaction;
use transaction::*;

fn owned_manifest_name(transfer_id: &str) -> OsString {
    OsString::from(format!(
        "{OWNED_MANIFEST_PREFIX}{transfer_id}{OWNED_MANIFEST_SUFFIX}"
    ))
}

fn owned_manifest_id(name: &str) -> Option<String> {
    let id = name
        .strip_prefix(OWNED_MANIFEST_PREFIX)?
        .strip_suffix(OWNED_MANIFEST_SUFFIX)?;
    Uuid::parse_str(id).ok().map(|_| id.to_owned())
}

fn owned_partial_name(name: &str) -> bool {
    name.strip_prefix(PARTIAL_PREFIX)
        .and_then(|name| name.strip_suffix(PARTIAL_SUFFIX))
        .is_some_and(|id| Uuid::parse_str(id).is_ok())
}

fn owned_cleanup_partial_name(name: &str) -> bool {
    owned_partial_name(name)
        || name
            .strip_prefix(RESERVATION_PREFIX)
            .and_then(|name| name.strip_suffix(RESERVATION_SUFFIX))
            .is_some_and(|hash| {
                hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        || name
            .strip_prefix(OWNED_MANIFEST_PREFIX)
            .and_then(|name| name.strip_suffix(".json.partial"))
            .is_some_and(|id| Uuid::parse_str(id).is_ok())
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    })
}

fn is_already_exists(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists)
    })
}

#[cfg(test)]
mod tests;
