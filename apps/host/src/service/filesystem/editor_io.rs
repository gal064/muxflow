use super::*;

impl FileService {
    #[cfg(test)]
    pub(crate) fn read_file(&self, root: &str, path: &str) -> anyhow::Result<v1::FileContent> {
        let token = root_token(root)?;
        self.read_file_authorized(root, &token, path)
    }

    pub(crate) fn read_file_authorized(
        &self,
        root: &str,
        root_token: &str,
        path: &str,
    ) -> anyhow::Result<v1::FileContent> {
        let root = RootCapability::validate(root, root_token)?;
        let (logical_target, target) = root.resolve_existing(path)?;
        if logical_target == root.logical_root() {
            bail!("the active root itself cannot be opened as a file");
        }
        let (logical_opened, opened_path) = root.regular_file_target(&logical_target, &target)?;
        let opened_anchor = root.anchor(&logical_opened)?;
        let mut file = opened_anchor.open_file()?;
        let metadata = file.metadata()?;
        let mut item = metadata_for_anchored(&root.stable_root(), &target, &logical_target)?;
        apply_effective_metadata(&mut item, &metadata);
        if item.symlink {
            item.symlink_target_kind = v1::FileKind::File.into();
        }
        let size = metadata.len();
        let image = image_mime(&opened_path).is_some();
        item.image_preview_eligible = image && size <= MAX_IMAGE_BYTES;
        let kind = if image {
            v1::FileContentKind::Image
        } else if size > MAX_TEXT_BYTES {
            v1::FileContentKind::TooLarge
        } else {
            let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or_default());
            file.read_to_end(&mut bytes)?;
            if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
                v1::FileContentKind::Binary
            } else {
                v1::FileContentKind::Text
            }
        };
        Ok(v1::FileContent {
            generation: item.generation,
            metadata: Some(item),
            kind: kind.into(),
            content: Vec::new(),
        })
    }

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
        _expected_generation: u64,
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
        let write_target = target_anchor.path();
        let metadata = target_anchor.open_file()?.metadata()?;
        let parent = write_target.parent().context("file has no parent")?;
        let temporary = parent.join(format!(".tmux-ide-save-{transfer_id}.partial"));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(metadata.permissions().mode() & 0o7777)
            .open(&temporary)?;
        self.uploads.lock().unwrap().insert(
            transfer_id.to_owned(),
            Upload {
                file,
                temporary,
                target: write_target,
                _target_anchor: target_anchor,
                root_token: root.token().to_owned(),
                metadata_path: logical_target,
                root_capability: root,
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
            let _ = fs::remove_file(upload.temporary);
            bail!("file-write BLAKE3 verification failed");
        }
        let mut upload = uploads.remove(transfer_id).expect("upload exists");
        let result = (|| -> anyhow::Result<(v1::FileMetadata, String)> {
            upload.file.flush()?;
            upload.file.sync_all()?;
            let content = fs::read(&upload.temporary)?;
            if content.len() as u64 != upload.total
                || content.contains(&0)
                || std::str::from_utf8(&content).is_err()
            {
                bail!("editor writes require the declared valid UTF-8 text without NUL bytes");
            }
            fs::set_permissions(&upload.temporary, upload.permissions)?;
            let parent = upload.target.parent().context("file has no parent")?;
            fs::rename(&upload.temporary, &upload.target)?;
            File::open(parent)?.sync_all()?;
            self.next_generation();
            let metadata = metadata_for_anchored(
                &upload.root_capability.stable_root(),
                &upload.target,
                &upload.metadata_path,
            )?;
            Ok((metadata, upload.root_token.clone()))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&upload.temporary);
        }
        result
    }

    pub(crate) fn cancel_file_write(&self, transfer_id: &str) -> anyhow::Result<()> {
        validate_transfer_id(transfer_id)?;
        if let Some(upload) = self.uploads.lock().unwrap().remove(transfer_id) {
            let _ = fs::remove_file(upload.temporary);
        }
        Ok(())
    }
}
