use super::*;

impl FileService {
    pub(super) fn reconcile_terminal_upload_in_directory(
        &self,
        directory: StagingDirectory,
        transfer_id: &str,
    ) -> anyhow::Result<v1::UploadDescriptor> {
        validate_transfer_id(transfer_id)?;
        Uuid::parse_str(transfer_id).context("terminal upload transfer ID must be a UUID")?;
        let manifest_name = owned_manifest_name(transfer_id);
        let manifest_metadata = directory.metadata(&manifest_name)?;
        if !manifest_metadata.is_file()
            || manifest_metadata.file_type().is_symlink()
            || manifest_metadata.uid() != unsafe { libc::geteuid() }
            || manifest_metadata.len() > MAX_MANIFEST_BYTES
        {
            bail!("completed upload ownership manifest is unsafe");
        }
        let mut manifest_file = directory.open_readonly(&manifest_name)?;
        lock_shared(&manifest_file)?;
        let opened_manifest = manifest_file.metadata()?;
        if !opened_manifest.is_file()
            || opened_manifest.uid() != unsafe { libc::geteuid() }
            || opened_manifest.dev() != manifest_metadata.dev()
            || opened_manifest.ino() != manifest_metadata.ino()
            || opened_manifest.len() > MAX_MANIFEST_BYTES
        {
            bail!("completed upload ownership manifest changed while opening");
        }
        let mut bytes = Vec::with_capacity(opened_manifest.len() as usize);
        std::io::Read::by_ref(&mut manifest_file)
            .take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            bail!("completed upload ownership manifest exceeds its bound");
        }
        let manifest: OwnedUploadManifest = serde_json::from_slice(&bytes)?;
        if manifest.schema_version != 2
            || manifest.transfer_id != transfer_id
            || validate_destination_basename(&manifest.final_name).is_err()
            || manifest.blake3.len() != 64
        {
            bail!("completed upload ownership manifest does not match the transfer");
        }
        let target_name = OsString::from(&manifest.final_name);
        let mut target = directory.open_readonly(&target_name)?;
        let metadata = target.metadata()?;
        if !metadata.is_file()
            || metadata.dev() != manifest.device
            || metadata.ino() != manifest.inode
            || metadata.len() != manifest.size
        {
            bail!("completed upload was replaced after commit");
        }
        let mut hasher = blake3::Hasher::new();
        let mut buffer = vec![0_u8; MAX_TRANSFER_CHUNK];
        let mut size = 0_u64;
        loop {
            let count = target.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            size = size
                .checked_add(count as u64)
                .context("reconciled upload byte counter overflow")?;
        }
        let digest = hasher.finalize().to_hex().to_string();
        if size != manifest.size || digest != manifest.blake3 {
            bail!("completed upload no longer matches its verified bytes");
        }
        Ok(v1::UploadDescriptor {
            transfer_id: transfer_id.to_owned(),
            destination_name: manifest.final_name,
            total_bytes: manifest.size,
            final_path: directory
                .path()
                .join(&target_name)
                .to_string_lossy()
                .into_owned(),
            verified: true,
            blake3: digest,
            cleanup_status: v1::CleanupStatus::NotNeeded.into(),
            ..Default::default()
        })
    }
}
