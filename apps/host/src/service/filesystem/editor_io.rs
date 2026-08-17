use super::*;

impl FileService {
    #[cfg(test)]
    pub(crate) fn begin_file_write(
        &self,
        root: &str,
        path: &str,
        transfer_id: &str,
        operation_id: &str,
        total: u64,
        expected_generation: u64,
    ) -> anyhow::Result<()> {
        let token = root_token(root)?;
        self.begin_file_write_authorized(
            root,
            &token,
            path,
            transfer_id,
            operation_id,
            total,
            expected_generation,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn begin_file_write_authorized(
        &self,
        root: &str,
        root_token: &str,
        path: &str,
        transfer_id: &str,
        operation_id: &str,
        total: u64,
        expected_generation: u64,
    ) -> anyhow::Result<()> {
        validate_transfer_id(transfer_id)?;
        validate_token("operation ID", operation_id)?;
        if total > MAX_TEXT_BYTES {
            bail!("text content exceeds the 10 MiB editor limit");
        }
        if self.uploads.lock().unwrap().contains_key(transfer_id) {
            bail!("file-write transfer ID is already active");
        }
        let root = RootCapability::validate(root, root_token)?;
        let (logical_target, target) = root.resolve_existing(path)?;
        reject_root_target(root.logical_root(), &logical_target)?;
        let (logical_write_target, _) = root.regular_file_target(&logical_target, &target)?;
        let target_anchor = root.anchor(&logical_write_target)?;
        let metadata = target_anchor.open_file()?.metadata()?;
        // The generation the editor opened this file at, enforced rather than
        // accepted and dropped.
        //
        // This parameter arrived, was named `_expected_generation`, and was
        // discarded — so a file changed underneath an open editor was silently
        // clobbered on save. It reads as a guarantee to anyone who sees the
        // signature, and the read side of this feature was rebuilt around it:
        // `open_stream` re-stats its own descriptor and refuses a file that
        // moved while it was being opened, precisely so the editor could carry
        // a generation into the write. This is where that carry is redeemed.
        //
        // The *leaf's* generation, from the logical name the caller opened —
        // not the write target's. For a symlink those are two different inodes,
        // and the leaf is what every producer of an on-screen fact reports:
        // directory listings, precise watch events, and the open the editor
        // holds. Comparing against the resolved target instead would refuse
        // every save of a symlinked file.
        //
        // Zero means the caller claimed no generation — the wire field is
        // optional and a first write has nothing to compare — and is not a
        // mismatch.
        if expected_generation != 0 {
            let leaf =
                metadata_for_anchored(&root.stable_root(), &target, &logical_target)?.generation;
            if leaf != expected_generation {
                return Err(stale_generation("file changed since it was opened"));
            }
        }
        let temporary = target_anchor.sibling(OsString::from(format!(
            ".tmux-ide-save-{transfer_id}.partial"
        )))?;
        let file = temporary.create_file(metadata.permissions().mode() & 0o7777)?;
        self.uploads.lock().unwrap().insert(
            transfer_id.to_owned(),
            Upload {
                file,
                temporary,
                target: target_anchor,
                root_token: root.token().to_owned(),
                metadata_path: logical_target,
                _root_capability: root,
                offset: 0,
                total,
                hasher: blake3::Hasher::new(),
                permissions: metadata.permissions(),
            },
        );
        Ok(())
    }

    pub(crate) fn write_file_chunk(
        &self,
        transfer_id: &str,
        offset: u64,
        data: &[u8],
    ) -> anyhow::Result<u64> {
        validate_transfer_id(transfer_id)?;
        if data.len() > MAX_TRANSFER_CHUNK {
            bail!("file-write chunk exceeds 1 MiB");
        }
        let mut uploads = self.uploads.lock().unwrap();
        let upload = uploads
            .get_mut(transfer_id)
            .context("file-write transfer is not active")?;
        if offset != upload.offset {
            bail!(
                "stale file-write offset: expected {}, received {offset}",
                upload.offset
            );
        }
        let next = upload
            .offset
            .checked_add(data.len() as u64)
            .context("file-write byte counter overflow")?;
        if next > upload.total {
            bail!("file-write chunk exceeds declared byte count");
        }
        upload.file.write_all(data)?;
        upload.hasher.update(data);
        upload.offset = next;
        Ok(next)
    }

    #[cfg(test)]
    pub(crate) fn commit_file_write(
        &self,
        transfer_id: &str,
        expected_blake3: &str,
    ) -> anyhow::Result<v1::FileMetadata> {
        self.commit_file_write_authorized(transfer_id, expected_blake3)
            .map(|(metadata, _)| metadata)
    }

    pub(crate) fn commit_file_write_authorized(
        &self,
        transfer_id: &str,
        expected_blake3: &str,
    ) -> anyhow::Result<(v1::FileMetadata, String)> {
        validate_transfer_id(transfer_id)?;
        let mut uploads = self.uploads.lock().unwrap();
        let upload = uploads
            .get(transfer_id)
            .context("file-write transfer is not active")?;
        if upload.offset != upload.total {
            bail!(
                "file-write is incomplete: received {} of {} bytes",
                upload.offset,
                upload.total
            );
        }
        let actual_blake3 = upload.hasher.finalize().to_hex().to_string();
        if expected_blake3.is_empty() || actual_blake3 != expected_blake3 {
            let upload = uploads.remove(transfer_id).expect("upload exists");
            let _ = upload.temporary.unlink(false);
            bail!("file-write BLAKE3 verification failed");
        }
        let mut upload = uploads.remove(transfer_id).expect("upload exists");
        let result = (|| -> anyhow::Result<(v1::FileMetadata, String)> {
            upload.file.flush()?;
            upload.file.sync_all()?;
            let mut content = Vec::new();
            upload.temporary.open_file()?.read_to_end(&mut content)?;
            if content.len() as u64 != upload.total
                || content.contains(&0)
                || std::str::from_utf8(&content).is_err()
            {
                bail!("editor writes require the declared valid UTF-8 text without NUL bytes");
            }
            upload.file.set_permissions(upload.permissions)?;
            upload.temporary.rename_to_replace(&upload.target)?;
            upload.target.sync_parent()?;
            self.next_generation();
            let metadata = mutation_metadata(&upload.target, &upload.metadata_path)?;
            Ok((metadata, upload.root_token.clone()))
        })();
        if result.is_err() {
            let _ = upload.temporary.unlink(false);
        }
        result
    }

    pub(crate) fn cancel_file_write(&self, transfer_id: &str) -> anyhow::Result<()> {
        validate_transfer_id(transfer_id)?;
        if let Some(upload) = self.uploads.lock().unwrap().remove(transfer_id) {
            let _ = upload.temporary.unlink(false);
        }
        Ok(())
    }
}
