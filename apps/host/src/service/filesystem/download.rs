use super::*;

impl FileService {
    #[cfg(test)]
    pub(crate) fn start_download(
        &self,
        root: &str,
        path: &str,
        folder: bool,
        transfer_id: &str,
        expected_generation: u64,
    ) -> anyhow::Result<v1::DownloadDescriptor> {
        let token = root_token(root)?;
        self.start_download_authorized(root, &token, path, folder, transfer_id, expected_generation)
    }

    pub(crate) fn start_download_authorized(
        &self,
        root: &str,
        root_token: &str,
        path: &str,
        folder: bool,
        transfer_id: &str,
        expected_generation: u64,
    ) -> anyhow::Result<v1::DownloadDescriptor> {
        validate_transfer_id(transfer_id)?;
        if self.transfers.lock().unwrap().contains_key(transfer_id) {
            bail!("transfer ID is already active");
        }
        let root = RootCapability::validate(root, root_token)?;
        let (logical_source, _) = root.resolve_new(path)?;
        reject_root_target(root.logical_root(), &logical_source)?;
        let source_anchor = root.anchor(&logical_source)?;
        let leaf_metadata = source_anchor.metadata_no_follow()?;
        let (source_handle, total, file_generation, source_mode, folder_archive, suggested_name) =
            if folder {
                if !leaf_metadata.is_dir() || leaf_metadata.is_symlink() {
                    bail!("folder download source is not a directory");
                }
                let name = logical_source.file_name().context("folder has no name")?;
                let directory = source_anchor.open_directory()?;
                let directory_fd = directory.as_raw_fd();
                let mut command = Command::new("tar");
                command
                    .arg("--format=pax")
                    .args(["-cf", "-", "--"])
                    .arg(".")
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                // SAFETY: the child performs only async-signal-safe fchdir before
                // exec. Its cwd remains bound to the authorized directory inode
                // even if every pathname leading to it is concurrently replaced.
                unsafe {
                    use std::os::unix::process::CommandExt as _;
                    command.pre_exec(move || {
                        if libc::fchdir(directory_fd) < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
                let mut child = command
                    .spawn()
                    .context("failed to start folder download archive")?;
                let stdout = child
                    .stdout
                    .take()
                    .context("folder archive stdout unavailable")?;
                set_nonblocking(&stdout)?;
                (
                    TransferSource::Archive {
                        child,
                        stdout,
                        _directory: directory,
                    },
                    None,
                    leaf_metadata.generation(),
                    leaf_metadata.mode(),
                    true,
                    format!("{}.tar", name.to_string_lossy()),
                )
            } else {
                let file = if leaf_metadata.is_symlink() {
                    let (_, source) = root.resolve_existing(path)?;
                    let (logical_opened, _) = root.regular_file_target(&logical_source, &source)?;
                    root.anchor(&logical_opened)?.open_file()?
                } else {
                    source_anchor.open_file()?
                };
                let opened_metadata = file.metadata()?;
                let generation = metadata_generation(&opened_metadata);
                if expected_generation != 0 && expected_generation != generation {
                    bail!("stale_file_generation: file changed before transfer opened");
                }
                let name = logical_source
                    .file_name()
                    .context("file has no name")?
                    .to_string_lossy()
                    .into_owned();
                (
                    TransferSource::File(file),
                    Some(opened_metadata.len()),
                    generation,
                    opened_metadata.permissions().mode(),
                    false,
                    name,
                )
            };
        self.transfers.lock().unwrap().insert(
            transfer_id.to_owned(),
            Transfer {
                source: source_handle,
                offset: 0,
                total,
                hasher: blake3::Hasher::new(),
                source_generation: (!folder_archive).then_some(file_generation),
            },
        );
        Ok(v1::DownloadDescriptor {
            transfer_id: transfer_id.to_owned(),
            suggested_name,
            total_bytes: total.unwrap_or_default(),
            folder_archive,
            mode: source_mode,
            file_generation,
            total_known: total.is_some(),
        })
    }

    pub(crate) fn read_download_chunk(
        &self,
        transfer_id: &str,
        offset: u64,
        requested: u32,
    ) -> anyhow::Result<v1::TransferChunk> {
        validate_transfer_id(transfer_id)?;
        let mut transfers = self.transfers.lock().unwrap();
        let transfer = transfers
            .get_mut(transfer_id)
            .context("transfer is not active")?;
        if offset != transfer.offset {
            bail!(
                "stale transfer offset: expected {}, received {offset}",
                transfer.offset
            );
        }
        let chunk_size = usize::try_from(requested)
            .unwrap_or(MAX_TRANSFER_CHUNK)
            .clamp(1, MAX_TRANSFER_CHUNK);
        let mut data = vec![0; chunk_size];
        let count = match &mut transfer.source {
            TransferSource::File(file) => file.read(&mut data)?,
            TransferSource::Archive { stdout, .. } => match stdout.read(&mut data) {
                Ok(count) => count,
                Err(error) if error.kind() == ErrorKind::WouldBlock => 0,
                Err(error) => return Err(error.into()),
            },
        };
        data.truncate(count);
        if count == 0 && transfer.total.is_none() {
            let TransferSource::Archive { child, .. } = &mut transfer.source else {
                unreachable!()
            };
            match child.try_wait()? {
                None => {
                    return Ok(v1::TransferChunk {
                        transfer_id: transfer_id.to_owned(),
                        offset,
                        total_known: false,
                        ..Default::default()
                    });
                }
                Some(status) if status.success() => transfer.total = Some(transfer.offset),
                Some(status) => bail!("folder archive failed with status {status}"),
            }
        }
        transfer.hasher.update(&data);
        transfer.offset = transfer.offset.saturating_add(count as u64);
        let eof = transfer.total.is_some_and(|total| transfer.offset == total);
        if eof
            && let (TransferSource::File(file), Some(expected_generation)) =
                (&transfer.source, transfer.source_generation)
            && metadata_generation(&file.metadata()?) != expected_generation
        {
            bail!("download source changed during transfer");
        }
        if count == 0 && !eof && matches!(transfer.source, TransferSource::File(_)) {
            bail!("download source changed or became unreadable during transfer");
        }
        let result = v1::TransferChunk {
            transfer_id: transfer_id.to_owned(),
            offset,
            data,
            eof,
            total_bytes: transfer.total.unwrap_or_default(),
            blake3: if eof {
                transfer.hasher.finalize().to_hex().to_string()
            } else {
                String::new()
            },
            total_known: transfer.total.is_some(),
        };
        if eof {
            transfers.remove(transfer_id).expect("transfer exists");
        }
        Ok(result)
    }

    pub(crate) fn cancel_download(&self, transfer_id: &str) -> anyhow::Result<()> {
        validate_transfer_id(transfer_id)?;
        if let Some(mut transfer) = self.transfers.lock().unwrap().remove(transfer_id)
            && let TransferSource::Archive { child, .. } = &mut transfer.source
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }
}
