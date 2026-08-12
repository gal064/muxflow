use super::*;

impl FileService {
    #[cfg(test)]
    pub(super) fn mutate(
        &self,
        request: &v1::FileServiceRequest,
    ) -> anyhow::Result<v1::FileMetadata> {
        let mut request = request.clone();
        if request.root_token.is_empty() {
            request.root_token = root_token(&request.root)?;
        }
        self.mutate_cancellable(&request, &AtomicBool::new(false))
    }

    pub(crate) fn mutate_cancellable(
        &self,
        request: &v1::FileServiceRequest,
        cancellation: &AtomicBool,
    ) -> anyhow::Result<v1::FileMetadata> {
        validate_token("operation ID", &request.operation_id)?;
        check_cancelled(cancellation)?;
        let root = RootCapability::validate(&request.root, &request.root_token)?;
        let kind = v1::FileMutationKind::try_from(request.mutation).unwrap_or_default();
        match kind {
            v1::FileMutationKind::Create => self.create(&root, request),
            v1::FileMutationKind::Rename | v1::FileMutationKind::Move => {
                self.rename(&root, request)
            }
            v1::FileMutationKind::Duplicate => self.duplicate(&root, request, cancellation),
            v1::FileMutationKind::Delete => {
                self.delete(&root, request, cancellation)?;
                Ok(v1::FileMetadata {
                    path: request.path.clone(),
                    ..Default::default()
                })
            }
            _ => bail!("file mutation kind is required"),
        }
    }

    fn create(
        &self,
        root: &RootCapability,
        request: &v1::FileServiceRequest,
    ) -> anyhow::Result<v1::FileMetadata> {
        let (logical_target, _) = root.resolve_new(&request.path)?;
        let anchored_target = root.anchor(&logical_target)?;
        let target = anchored_target.path();
        if target.exists() {
            bail!(
                "destination_exists: refusing to overwrite without an explicit destination action"
            );
        }
        if request.create_directory {
            fs::create_dir(&target)?;
        } else {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
        }
        self.next_generation();
        metadata_for_anchored(&root.stable_root(), &target, &logical_target)
    }

    fn rename(
        &self,
        root: &RootCapability,
        request: &v1::FileServiceRequest,
    ) -> anyhow::Result<v1::FileMetadata> {
        let (logical_source, _) = root.resolve_existing(&request.path)?;
        reject_root_target(root.logical_root(), &logical_source)?;
        let (logical_destination, _) = root.resolve_new(&request.destination)?;
        if logical_source == logical_destination {
            bail!("source and destination are identical");
        }
        let source_anchor = root.anchor(&logical_source)?;
        if fs::symlink_metadata(source_anchor.path())?.is_dir()
            && logical_destination.starts_with(&logical_source)
        {
            bail!("destination_inside_source: a directory cannot be moved into itself");
        }
        let destination_anchor = root.anchor(&logical_destination)?;
        let destination = destination_anchor.path();
        let backup = stage_destination(
            &destination,
            request.overwrite_confirmed,
            request.non_empty_confirmed,
        )?;
        let renamed = source_anchor.rename_to_noreplace(&destination_anchor);
        if let Err(error) = renamed {
            rollback_destination(&destination, backup.as_deref())?;
            if error
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::raw_os_error)
                == Some(libc::EXDEV)
            {
                bail!("cross-filesystem moves are not supported safely");
            }
            return Err(error);
        }
        // The rename already committed. Backup cleanup is maintenance and must
        // not turn a committed mutation into a false failure response.
        let _ = discard_backup(backup.as_deref());
        self.next_generation();
        metadata_for_anchored(&root.stable_root(), &destination, &logical_destination)
    }

    fn duplicate(
        &self,
        root: &RootCapability,
        request: &v1::FileServiceRequest,
        cancellation: &AtomicBool,
    ) -> anyhow::Result<v1::FileMetadata> {
        let (logical_source, _) = root.resolve_existing(&request.path)?;
        reject_root_target(root.logical_root(), &logical_source)?;
        let (logical_destination, _) = root.resolve_new(&request.destination)?;
        if logical_source == logical_destination {
            bail!("source and destination are identical");
        }
        let source_anchor = root.anchor(&logical_source)?;
        let destination_anchor = root.anchor(&logical_destination)?;
        let source = source_anchor.path();
        let destination = destination_anchor.path();
        let source_meta = fs::symlink_metadata(&source)?;
        if source_meta.is_dir() && logical_destination.starts_with(&logical_source) {
            bail!("destination_inside_source: a directory cannot be duplicated into itself");
        }
        let backup = stage_destination(
            &destination,
            request.overwrite_confirmed,
            request.non_empty_confirmed,
        )?;
        let result = if source_meta.file_type().is_symlink() {
            check_cancelled(cancellation).and_then(|()| copy_symlink_atomic(&source, &destination))
        } else if source_meta.is_dir() {
            copy_directory_atomic(&source, &destination, cancellation)
        } else if source_meta.is_file() {
            copy_file_atomic(
                &source,
                &destination,
                source_meta.permissions(),
                cancellation,
            )
        } else {
            Err(anyhow::anyhow!(
                "only regular files and directories can be duplicated"
            ))
        };
        if let Err(error) = result {
            rollback_destination(&destination, backup.as_deref())?;
            return Err(error);
        }
        let _ = discard_backup(backup.as_deref());
        self.next_generation();
        metadata_for_anchored(&root.stable_root(), &destination, &logical_destination)
    }

    fn delete(
        &self,
        root: &RootCapability,
        request: &v1::FileServiceRequest,
        cancellation: &AtomicBool,
    ) -> anyhow::Result<()> {
        let (logical_target, _) = root.resolve_existing(&request.path)?;
        reject_root_target(root.logical_root(), &logical_target)?;
        let target_anchor = root.anchor(&logical_target)?;
        let target = target_anchor.path();
        let metadata = target_anchor.metadata_no_follow()?;
        check_cancelled(cancellation)?;
        if metadata.file_type().is_symlink() || metadata.is_file() {
            target_anchor.unlink(false)?;
        } else if metadata.is_dir() {
            let non_empty = fs::read_dir(&target)?.next().transpose()?.is_some();
            if non_empty && !request.non_empty_confirmed {
                bail!("confirmation_required_non_empty_directory");
            }
            if non_empty {
                let parent = target.parent().context("delete target has no parent")?;
                let staged = parent.join(format!(".tmux-ide-delete-{}.partial", Uuid::new_v4()));
                rename_noreplace(&target, &staged)?;
                let result = remove_directory_cooperative(&staged, cancellation);
                if let Err(error) = result {
                    // The public delete committed at rename. Finish cleanup and
                    // report success if cleanup succeeds; never claim cancellation
                    // after the irreversible boundary.
                    fs::remove_dir_all(&staged).with_context(|| {
                        format!("delete committed but partial cleanup failed after: {error}")
                    })?;
                }
            } else {
                target_anchor.unlink(true)?;
            }
        } else {
            bail!("unsupported file type");
        }
        self.next_generation();
        Ok(())
    }
}

fn stage_destination(
    path: &Path,
    overwrite: bool,
    non_empty: bool,
) -> anyhow::Result<Option<PathBuf>> {
    if !path.exists() && fs::symlink_metadata(path).is_err() {
        return Ok(None);
    }
    if !overwrite {
        bail!("confirmation_required_destination_overwrite");
    }
    let metadata = fs::symlink_metadata(path)?;
    let expected_identity = (metadata.dev(), metadata.ino());
    if metadata.is_dir() {
        let has_entries = fs::read_dir(path)?.next().transpose()?.is_some();
        if has_entries && !non_empty {
            bail!("confirmation_required_non_empty_destination");
        }
    } else if !metadata.file_type().is_symlink() && !metadata.is_file() {
        bail!("unsupported overwrite destination type");
    }
    let parent = path.parent().context("destination has no parent")?;
    let backup = parent.join(format!(".tmux-ide-overwrite-{}.partial", Uuid::new_v4()));
    rename_noreplace(path, &backup)?;
    let staged = fs::symlink_metadata(&backup)?;
    if (staged.dev(), staged.ino()) != expected_identity {
        let _ = rename_noreplace(&backup, path);
        bail!("destination changed while it was being staged; retry from a fresh snapshot");
    }
    Ok(Some(backup))
}

fn discard_backup(backup: Option<&Path>) -> anyhow::Result<()> {
    if let Some(backup) = backup {
        remove_path(backup)?;
    }
    Ok(())
}

fn rollback_destination(destination: &Path, backup: Option<&Path>) -> anyhow::Result<()> {
    if let Some(backup) = backup {
        rename_noreplace(backup, destination).context(
            "destination changed during rollback; preserved backup instead of deleting a racer",
        )?;
    }
    Ok(())
}

fn remove_path(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn copy_file_atomic(
    source: &Path,
    destination: &Path,
    permissions: fs::Permissions,
    cancellation: &AtomicBool,
) -> anyhow::Result<()> {
    let parent = destination.parent().context("destination has no parent")?;
    let temporary = parent.join(format!(".tmux-ide-copy-{}.partial", Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(source)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let mut buffer = vec![0_u8; 256 * 1024];
        loop {
            check_cancelled(cancellation)?;
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            output.write_all(&buffer[..read])?;
        }
        output.sync_all()?;
        fs::set_permissions(&temporary, permissions)?;
        rename_noreplace(&temporary, destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn copy_symlink_atomic(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let parent = destination.parent().context("destination has no parent")?;
    let temporary = parent.join(format!(".tmux-ide-copy-{}.partial", Uuid::new_v4()));
    let link = fs::read_link(source)?;
    let result = (|| -> anyhow::Result<()> {
        std::os::unix::fs::symlink(link, &temporary)?;
        rename_noreplace(&temporary, destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn copy_directory(
    source: &Path,
    destination: &Path,
    cancellation: &AtomicBool,
) -> anyhow::Result<()> {
    check_cancelled(cancellation)?;
    let source_directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(source)?;
    let stable_source = descriptor_path(source_directory.as_raw_fd());
    fs::create_dir(destination)?;
    fs::set_permissions(destination, source_directory.metadata()?.permissions())?;
    let destination_directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(destination)?;
    let stable_destination = descriptor_path(destination_directory.as_raw_fd());
    for entry in fs::read_dir(&stable_source)? {
        check_cancelled(cancellation)?;
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        let target = stable_destination.join(entry.file_name());
        if metadata.file_type().is_symlink() {
            let link = fs::read_link(entry.path())?;
            std::os::unix::fs::symlink(link, target)?;
            continue;
        }
        if metadata.is_dir() {
            copy_directory(&entry.path(), &target, cancellation)?;
        } else if metadata.is_file() {
            copy_file_atomic(&entry.path(), &target, metadata.permissions(), cancellation)?;
        } else {
            bail!("unsupported file type while duplicating directory");
        }
    }
    Ok(())
}

fn copy_directory_atomic(
    source: &Path,
    destination: &Path,
    cancellation: &AtomicBool,
) -> anyhow::Result<()> {
    let parent = destination.parent().context("destination has no parent")?;
    let temporary = parent.join(format!(".tmux-ide-copy-{}.partial", Uuid::new_v4()));
    let result = copy_directory(source, &temporary, cancellation).and_then(|()| {
        check_cancelled(cancellation)?;
        rename_noreplace(&temporary, destination)?;
        Ok(())
    });
    if result.is_err() && temporary.exists() {
        let _ = fs::remove_dir_all(temporary);
    }
    result
}

fn remove_directory_cooperative(path: &Path, cancellation: &AtomicBool) -> anyhow::Result<()> {
    check_cancelled(cancellation)?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)?;
    let stable = descriptor_path(directory.as_raw_fd());
    for entry in fs::read_dir(stable)? {
        check_cancelled(cancellation)?;
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            remove_directory_cooperative(&entry.path(), cancellation)?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    fs::remove_dir(path)?;
    Ok(())
}

fn check_cancelled(cancellation: &AtomicBool) -> anyhow::Result<()> {
    if cancellation.load(Ordering::Acquire) {
        bail!("cancelled: file mutation was cancelled");
    }
    Ok(())
}
