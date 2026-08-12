use super::*;

pub(super) fn cleanup_owned_staging(directory: &StagingDirectory) -> anyhow::Result<()> {
    recover_upload_transactions(directory)?;
    cleanup_owned_partials(directory)?;
    cleanup_completed_uploads(directory)
}

pub(super) fn cleanup_owned_partials(directory: &StagingDirectory) -> anyhow::Result<()> {
    let now = SystemTime::now();
    let mut candidates = Vec::new();
    let mut total = 0_u64;
    for (name, metadata) in directory.entries()? {
        if !owned_cleanup_partial_name(&name.to_string_lossy()) {
            continue;
        }
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            continue;
        }
        let file = match directory.open_readonly(&name) {
            Ok(file) => file,
            Err(_) => continue,
        };
        if !try_lock_exclusive(&file)? {
            continue;
        }
        let opened = file.metadata()?;
        if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
            continue;
        }
        total = total.saturating_add(metadata.len());
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        candidates.push((name, metadata.len(), modified, file));
    }
    candidates.sort_by_key(|(_, _, modified, _)| *modified);
    for (name, size, modified, file) in candidates {
        let old = now.duration_since(modified).unwrap_or_default() >= STALE_PARTIAL_AGE;
        if old || total > STALE_PARTIAL_TOTAL_BYTES {
            let current = directory.metadata(&name)?;
            let opened = file.metadata()?;
            if current.dev() != opened.dev() || current.ino() != opened.ino() {
                continue;
            }
            let opened = file.metadata()?;
            let _ = directory.quarantine_and_delete(&name, opened.dev(), opened.ino())?;
            total = total.saturating_sub(size);
        }
    }
    Ok(())
}

pub(super) fn cleanup_completed_uploads(directory: &StagingDirectory) -> anyhow::Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut candidates = Vec::new();
    let mut total = 0_u64;
    for (manifest_name, manifest_metadata) in directory.entries()? {
        let Some(manifest_id) = owned_manifest_id(&manifest_name.to_string_lossy()) else {
            continue;
        };
        if !manifest_metadata.is_file()
            || manifest_metadata.file_type().is_symlink()
            || manifest_metadata.uid() != unsafe { libc::geteuid() }
            || manifest_metadata.len() > MAX_MANIFEST_BYTES
        {
            continue;
        }
        let mut file = match directory.open_readwrite(&manifest_name) {
            Ok(file) => file,
            Err(_) => continue,
        };
        if !try_lock_exclusive(&file)? {
            continue;
        }
        let opened_manifest = file.metadata()?;
        if opened_manifest.len() > MAX_MANIFEST_BYTES
            || opened_manifest.dev() != manifest_metadata.dev()
            || opened_manifest.ino() != manifest_metadata.ino()
        {
            continue;
        }
        let mut bytes = Vec::with_capacity(opened_manifest.len() as usize);
        std::io::Read::by_ref(&mut file)
            .take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            continue;
        }
        let mut manifest: OwnedUploadManifest = match serde_json::from_slice(&bytes) {
            Ok(manifest) => manifest,
            Err(_) => continue,
        };
        if !matches!(manifest.schema_version, 1 | 2)
            || manifest.transfer_id != manifest_id
            || validate_destination_basename(&manifest.final_name).is_err()
        {
            continue;
        }
        let target_name = OsString::from(&manifest.final_name);
        let target_file = match directory.open_readonly(&target_name) {
            Ok(file) => file,
            Err(error) if is_not_found(&error) => {
                let _ = directory.quarantine_and_delete(
                    &manifest_name,
                    opened_manifest.dev(),
                    opened_manifest.ino(),
                )?;
                continue;
            }
            Err(error) => return Err(error),
        };
        let target_metadata = target_file.metadata()?;
        if !target_metadata.is_file()
            || target_metadata.file_type().is_symlink()
            || target_metadata.uid() != unsafe { libc::geteuid() }
            || target_metadata.dev() != manifest.device
            || target_metadata.ino() != manifest.inode
            || target_metadata.len() != manifest.size
        {
            // The user or another process replaced the path. Retire only the
            // unambiguously app-owned manifest and preserve the foreign leaf.
            let _ = directory.quarantine_and_delete(
                &manifest_name,
                opened_manifest.dev(),
                opened_manifest.ino(),
            )?;
            continue;
        }
        if manifest.schema_version == 2
            && manifest.transaction_state != UploadJournalState::Reconciled
        {
            mark_upload_journal_reconciled(&mut file, &mut manifest)?;
        }
        if let (Some(device), Some(inode)) = (manifest.original_device, manifest.original_inode) {
            let backup_name = OsString::from(format!(
                "{PARTIAL_PREFIX}{}{PARTIAL_SUFFIX}",
                manifest.transfer_id
            ));
            if let Ok(backup) = directory.metadata(&backup_name)
                && (backup.dev(), backup.ino()) == (device, inode)
            {
                let _ = directory.quarantine_and_delete(&backup_name, device, inode)?;
            }
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
        total = total.saturating_add(manifest.size);
        candidates.push((manifest_name, target_name, manifest, file, target_file));
    }
    candidates.sort_by_key(|(_, _, manifest, _, _)| manifest.created_unix_seconds);
    for (manifest_name, target_name, manifest, manifest_file, target_file) in candidates {
        let age = now.saturating_sub(manifest.created_unix_seconds);
        let old = age >= COMPLETED_STAGING_AGE.as_secs();
        let size_pressure = total > COMPLETED_STAGING_TOTAL_BYTES
            && age >= COMPLETED_STAGING_SIZE_MIN_AGE.as_secs();
        if !old && !size_pressure {
            continue;
        }
        let target_identity = target_file.metadata()?;
        let manifest_identity = manifest_file.metadata()?;
        if directory.quarantine_and_delete(
            &target_name,
            target_identity.dev(),
            target_identity.ino(),
        )? && directory.quarantine_and_delete(
            &manifest_name,
            manifest_identity.dev(),
            manifest_identity.ino(),
        )? {
            total = total.saturating_sub(manifest.size);
        }
    }
    directory.sync()?;
    Ok(())
}
