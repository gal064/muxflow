use super::*;
use std::os::fd::AsRawFd;

#[cfg(test)]
fn crash_at(point: &str) {
    if std::env::var("ADE_PHASE7_UPLOAD_CRASH_POINT").as_deref() == Ok(point) {
        // SAFETY: the subprocess crash matrix deliberately terminates only
        // its own test process to model an uncatchable helper loss.
        unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
        // Linux tears the process down before `kill` returns to user code, but
        // macOS executes further instructions, and reaching the panic below
        // would replace the uncatchable loss being modelled with an unwind that
        // runs destructors — a strictly weaker crash than the test intends.
        // Park without unwinding until the signal lands, and keep the panic as a
        // loud bound in case the kill genuinely failed.
        std::thread::sleep(std::time::Duration::from_secs(10));
        unreachable!("SIGKILL did not terminate crash-test subprocess");
    }
}

#[cfg(not(test))]
#[inline]
fn crash_at(_point: &str) {}

pub(super) struct StagedManifest {
    pub(super) temporary_name: OsString,
    pub(super) final_name: OsString,
    pub(super) file: File,
    manifest: OwnedUploadManifest,
}

impl Drop for StagedManifest {
    fn drop(&mut self) {
        // Explicitly release the advisory lock before closing. A concurrent
        // fork can momentarily inherit the open file description before exec
        // applies O_CLOEXEC; relying only on close would leave recovery seeing
        // a false in-flight transaction during that window.
        // SAFETY: file remains live for the duration of Drop.
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

pub(super) fn stage_completed_upload_manifest(
    directory: &StagingDirectory,
    transfer_id: &str,
    target_name: &OsStr,
    target_file: &File,
    digest: &str,
    original_identity: Option<(u64, u64)>,
    fault: Option<PublicationFault>,
) -> anyhow::Result<StagedManifest> {
    let metadata = target_file.metadata()?;
    let final_name = target_name
        .to_str()
        .context("completed upload basename is not UTF-8")?;
    let manifest = OwnedUploadManifest {
        schema_version: 2,
        transfer_id: transfer_id.to_owned(),
        final_name: final_name.to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.len(),
        created_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        blake3: digest.to_owned(),
        transaction_state: UploadJournalState::Prepared,
        original_device: original_identity.map(|identity| identity.0),
        original_inode: original_identity.map(|identity| identity.1),
    };
    let bytes = serde_json::to_vec(&manifest)?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        bail!("owned-upload manifest exceeds its private bound");
    }
    let manifest_name = owned_manifest_name(transfer_id);
    let temporary_name = OsString::from(format!("{}.partial", manifest_name.to_string_lossy()));
    if fault == Some(PublicationFault::ManifestCreate) {
        bail!("not_published: injected manifest create failure");
    }
    let mut file = directory.create_private(&temporary_name)?;
    lock_exclusive(&file)?;
    let result = (|| -> anyhow::Result<StagedManifest> {
        if fault == Some(PublicationFault::ManifestWrite) {
            bail!("not_published: injected manifest write failure");
        }
        file.write_all(&bytes)?;
        if fault == Some(PublicationFault::ManifestFsync) {
            bail!("not_published: injected manifest fsync failure");
        }
        file.sync_all()?;
        set_private_file_mode(&file)?;
        // The prepared journal must itself be discoverable after a process or
        // host crash before publication can begin. Syncing only the journal
        // inode leaves its directory entry outside the durability contract.
        directory.sync()?;
        crash_at("prepared-journal-durable");
        Ok(StagedManifest {
            temporary_name: temporary_name.clone(),
            final_name: manifest_name,
            file,
            manifest,
        })
    })();
    if result.is_err() {
        let _ = directory.unlink(&temporary_name);
    }
    result
}

fn rewrite_upload_journal(
    staged: &mut StagedManifest,
    state: UploadJournalState,
    fault: Option<PublicationFault>,
) -> anyhow::Result<()> {
    let write_fault = matches!(
        (state, fault),
        (
            UploadJournalState::Published,
            Some(PublicationFault::PublishedJournalWrite)
        ) | (
            UploadJournalState::Reconciled,
            Some(PublicationFault::ReconciledJournalWrite)
        )
    );
    let fsync_fault = matches!(
        (state, fault),
        (
            UploadJournalState::Published,
            Some(PublicationFault::PublishedJournalFsync)
        ) | (
            UploadJournalState::Reconciled,
            Some(PublicationFault::ReconciledJournalFsync)
        )
    );
    if write_fault {
        bail!("injected upload transaction journal write failure");
    }
    staged.manifest.transaction_state = state;
    let bytes = serde_json::to_vec(&staged.manifest)?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        bail!("owned-upload transaction journal exceeds its private bound");
    }
    staged.file.seek(SeekFrom::Start(0))?;
    staged.file.set_len(0)?;
    staged.file.write_all(&bytes)?;
    if state == UploadJournalState::Published {
        crash_at("published-journal-written");
    }
    if fsync_fault {
        bail!("injected upload transaction journal fsync failure");
    }
    staged.file.sync_all()?;
    if state == UploadJournalState::Published {
        crash_at("published-journal-fsynced");
    }
    Ok(())
}

pub(super) fn mark_upload_journal_reconciled(
    file: &mut File,
    manifest: &mut OwnedUploadManifest,
) -> anyhow::Result<()> {
    manifest.transaction_state = UploadJournalState::Reconciled;
    let bytes = serde_json::to_vec(manifest)?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        bail!("owned-upload recovery journal exceeds its private bound");
    }
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}

/// Complete or roll back a transaction whose helper died after its prepared
/// journal became durable. Every decision is made from descriptor-relative
/// inode identities; namespace replacements are preserved.
pub(super) fn recover_upload_transactions(directory: &StagingDirectory) -> anyhow::Result<()> {
    for (journal_name, metadata) in directory.entries()? {
        let journal_text = journal_name.to_string_lossy();
        let Some(transfer_id) = journal_text
            .strip_prefix(OWNED_MANIFEST_PREFIX)
            .and_then(|name| name.strip_suffix(".json.partial"))
            .filter(|id| Uuid::parse_str(id).is_ok())
        else {
            continue;
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.len() > MAX_MANIFEST_BYTES
        {
            continue;
        }
        let mut journal_file = match directory.open_readwrite(&journal_name) {
            Ok(file) => file,
            Err(_) => continue,
        };
        if !try_lock_exclusive(&journal_file)? {
            continue;
        }
        let opened = journal_file.metadata()?;
        if opened.dev() != metadata.dev()
            || opened.ino() != metadata.ino()
            || opened.len() > MAX_MANIFEST_BYTES
        {
            continue;
        }
        let mut bytes = Vec::with_capacity(opened.len() as usize);
        std::io::Read::by_ref(&mut journal_file)
            .take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            continue;
        }
        let mut manifest: OwnedUploadManifest =
            match serde_json::from_slice::<OwnedUploadManifest>(&bytes) {
                Ok(manifest)
                    if manifest.transfer_id == transfer_id
                        && manifest.schema_version == 2
                        && validate_destination_basename(&manifest.final_name).is_ok() =>
                {
                    manifest
                }
                _ => continue,
            };
        let target_name = OsString::from(&manifest.final_name);
        let target_is_new = directory.metadata(&target_name).is_ok_and(|target| {
            target.is_file()
                && !target.file_type().is_symlink()
                && (target.dev(), target.ino()) == (manifest.device, manifest.inode)
                && target.len() == manifest.size
        });
        let partial_name = OsString::from(format!(
            "{PARTIAL_PREFIX}{}{PARTIAL_SUFFIX}",
            manifest.transfer_id
        ));
        if target_is_new {
            mark_upload_journal_reconciled(&mut journal_file, &mut manifest)?;
            let final_manifest = owned_manifest_name(&manifest.transfer_id);
            match directory.rename_noreplace(&journal_name, &final_manifest) {
                Ok(()) => {}
                Err(error) if is_already_exists(&error) => {
                    let _ = directory.quarantine_and_delete(
                        &journal_name,
                        opened.dev(),
                        opened.ino(),
                    )?;
                }
                Err(error) => return Err(error),
            }
            if let (Some(device), Some(inode)) = (manifest.original_device, manifest.original_inode)
                && let Ok(backup) = directory.metadata(&partial_name)
                && (backup.dev(), backup.ino()) == (device, inode)
            {
                let _ = directory.quarantine_and_delete(&partial_name, device, inode)?;
            }
        } else {
            if let Ok(partial) = directory.metadata(&partial_name)
                && (partial.dev(), partial.ino()) == (manifest.device, manifest.inode)
            {
                let _ =
                    directory.quarantine_and_delete(&partial_name, partial.dev(), partial.ino())?;
            }
            let _ = directory.quarantine_and_delete(&journal_name, opened.dev(), opened.ino())?;
        }
        let reservation_name = reservation_leaf_name(&target_name);
        if let Ok(reservation) = directory.open_readonly(&reservation_name)
            && try_lock_exclusive(&reservation)?
        {
            let expected = reservation.metadata()?;
            let current = directory.metadata(&reservation_name)?;
            if (current.dev(), current.ino()) == (expected.dev(), expected.ino()) {
                let _ = directory.quarantine_and_delete(
                    &reservation_name,
                    expected.dev(),
                    expected.ino(),
                )?;
            }
        }
        directory.sync()?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn record_completed_upload(
    directory: &StagingDirectory,
    transfer_id: &str,
    target_name: &OsStr,
    target_file: &File,
    digest: &str,
) -> anyhow::Result<()> {
    let staged = stage_completed_upload_manifest(
        directory,
        transfer_id,
        target_name,
        target_file,
        digest,
        None,
        None,
    )?;
    directory.rename_noreplace(&staged.temporary_name, &staged.final_name)?;
    directory.sync()
}

pub(super) fn validate_overwrite_identity(upload: &TerminalUpload) -> anyhow::Result<bool> {
    let current = upload.directory.metadata(&upload.target_name);
    match (upload.collision, upload.overwrite_identity, current) {
        (v1::CollisionPolicy::OverwriteConfirmed, Some(expected), Ok(metadata))
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && (metadata.dev(), metadata.ino()) == expected =>
        {
            Ok(true)
        }
        (v1::CollisionPolicy::OverwriteConfirmed, None, Err(error)) if is_not_found(&error) => {
            Ok(false)
        }
        (v1::CollisionPolicy::Fail | v1::CollisionPolicy::Rename, None, Err(error))
            if is_not_found(&error) =>
        {
            Ok(false)
        }
        (v1::CollisionPolicy::Unspecified, _, _) => {
            bail!("not_published: collision policy is required")
        }
        _ => bail!("not_published: upload destination changed before commit"),
    }
}

pub(super) fn publish_upload_transaction(
    upload: &TerminalUpload,
    manifest: &mut StagedManifest,
    had_original: bool,
    fault: Option<PublicationFault>,
) -> PublishResult<()> {
    if had_original {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        upload
            .directory
            .exchange(&upload.temporary_name, &upload.target_name)
            .map_err(|error| upload_not_published(error.to_string()))?;
    } else {
        upload
            .directory
            .rename_noreplace(&upload.temporary_name, &upload.target_name)
            .map_err(|error| upload_not_published(error.to_string()))?;
    }
    crash_at("target-renamed");
    let bookkeeping = (|| -> anyhow::Result<()> {
        if matches!(
            fault,
            Some(
                PublicationFault::PostRenameDirectoryFsync | PublicationFault::RollbackSubstitution
            )
        ) {
            bail!("injected post-rename directory fsync failure");
        }
        upload.directory.sync()?;
        crash_at("target-directory-fsynced");
        rewrite_upload_journal(manifest, UploadJournalState::Published, fault)?;
        upload
            .directory
            .rename_noreplace(&manifest.temporary_name, &manifest.final_name)?;
        crash_at("published-journal-renamed");
        if fault == Some(PublicationFault::PostJournalRenameDirectoryFsync) {
            bail!("injected post-journal-rename directory fsync failure");
        }
        upload.directory.sync()?;
        crash_at("published-journal-directory-fsynced");
        if !upload.directory.current_namespace_matches() {
            bail!("private staging directory was replaced during upload commit");
        }
        Ok(())
    })();
    if let Err(error) = bookkeeping {
        return Err(rollback_upload_publication(
            upload,
            manifest,
            had_original,
            error,
            fault,
        ));
    }

    let mut cleanup_errors = Vec::new();
    if let Err(error) = rewrite_upload_journal(manifest, UploadJournalState::Reconciled, fault) {
        cleanup_errors.push(format!(
            "published transaction reconciliation failed: {error}"
        ));
    }

    if had_original {
        if fault == Some(PublicationFault::Cleanup) {
            return Ok(Published {
                value: (),
                cleanup_error: Some(
                    [
                        cleanup_errors.join("; "),
                        "injected retained-backup cleanup failure".into(),
                    ]
                    .into_iter()
                    .filter(|value| !value.is_empty())
                    .collect::<Vec<_>>()
                    .join("; "),
                ),
            });
        }
        if let Err(error) = upload.directory.unlink(&upload.temporary_name) {
            if !is_not_found(&error) {
                cleanup_errors.push(format!("retained original-backup cleanup failed: {error}"));
            }
        } else {
            crash_at("backup-removed");
        }
        if let Err(error) = upload.directory.sync() {
            cleanup_errors.push(format!(
                "published but backup cleanup fsync failed: {error}"
            ));
        } else {
            crash_at("backup-removal-directory-fsynced");
        }
    }
    crash_at("cleanup-complete");
    Ok(Published {
        value: (),
        cleanup_error: (!cleanup_errors.is_empty()).then(|| cleanup_errors.join("; ")),
    })
}

fn rollback_upload_publication(
    upload: &TerminalUpload,
    manifest: &StagedManifest,
    had_original: bool,
    cause: anyhow::Error,
    fault: Option<PublicationFault>,
) -> PublishFailure {
    let quarantine = OsString::from(format!(".tmux-agent-rollback-{}.partial", Uuid::new_v4()));
    let expected = match upload.stream.file.metadata() {
        Ok(metadata) => (metadata.dev(), metadata.ino()),
        Err(error) => {
            return upload_unknown(format!("{cause}; uploaded inode unavailable: {error}"));
        }
    };
    if let Err(error) = upload
        .directory
        .rename_noreplace(&upload.target_name, &quarantine)
    {
        return upload_unknown(format!(
            "{cause}; published leaf quarantine failed: {error}"
        ));
    }
    if fault == Some(PublicationFault::RollbackSubstitution) {
        let preserved = OsString::from(format!(
            ".tmux-agent-test-preserved-{}.partial",
            Uuid::new_v4()
        ));
        if let Err(error) = upload.directory.rename_noreplace(&quarantine, &preserved) {
            return upload_unknown(format!(
                "{cause}; injected rollback substitution setup failed: {error}"
            ));
        }
        match upload.directory.create_private(&quarantine) {
            Ok(mut foreign) => {
                if let Err(error) = foreign.write_all(b"foreign-substitute") {
                    return upload_unknown(format!(
                        "{cause}; injected rollback substitution write failed: {error}"
                    ));
                }
            }
            Err(error) => {
                return upload_unknown(format!(
                    "{cause}; injected rollback substitution create failed: {error}"
                ));
            }
        }
    }
    let quarantined = match upload.directory.metadata(&quarantine) {
        Ok(metadata) => metadata,
        Err(error) => {
            return upload_unknown(format!("{cause}; quarantined leaf unavailable: {error}"));
        }
    };
    if (quarantined.dev(), quarantined.ino()) != expected {
        return upload_unknown(format!(
            "{cause}; destination was substituted and retained in quarantine"
        ));
    }
    if had_original
        && let Err(error) = upload
            .directory
            .rename_noreplace(&upload.temporary_name, &upload.target_name)
    {
        return upload_unknown(format!(
            "{cause}; original destination restore failed: {error}"
        ));
    }
    if let Err(error) = upload.directory.unlink(&quarantine) {
        return upload_not_published(format!(
            "{cause}; verified new leaf retained for cleanup: {error}"
        ));
    }
    let _ = quarantine_manifest_for_delete(&upload.directory, manifest);
    let _ = upload.directory.sync();
    upload_not_published(cause.to_string())
}

fn upload_not_published(message: String) -> PublishFailure {
    PublishFailure {
        outcome: PublicationOutcome::NotPublished,
        message,
    }
}

fn upload_unknown(message: String) -> PublishFailure {
    PublishFailure {
        outcome: PublicationOutcome::Unknown,
        message,
    }
}

fn quarantine_manifest_for_delete(
    directory: &StagingDirectory,
    manifest: &StagedManifest,
) -> anyhow::Result<()> {
    let quarantine = OsString::from(format!(
        ".tmux-agent-manifest-rollback-{}.partial",
        Uuid::new_v4()
    ));
    match directory.rename_noreplace(&manifest.final_name, &quarantine) {
        Ok(()) => {}
        Err(error) if is_not_found(&error) => return Ok(()),
        Err(error) => return Err(error),
    }
    let expected = manifest.file.metadata()?;
    let current = directory.metadata(&quarantine)?;
    if current.dev() == expected.dev() && current.ino() == expected.ino() {
        directory.unlink(&quarantine)?;
    }
    Ok(())
}
