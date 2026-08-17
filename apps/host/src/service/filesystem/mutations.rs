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
        if anchored_target.exists()? {
            bail!(
                "destination_exists: refusing to overwrite without an explicit destination action"
            );
        }
        if request.create_directory {
            anchored_target.create_directory(0o777)?;
        } else {
            anchored_target.create_file(0o666)?;
        }
        self.next_generation();
        mutation_metadata(&anchored_target, &logical_target)
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
        let source_metadata = source_anchor.metadata_no_follow()?;
        if source_metadata.is_dir() && logical_destination.starts_with(&logical_source) {
            bail!("destination_inside_source: a directory cannot be moved into itself");
        }
        let destination_anchor = root.anchor(&logical_destination)?;
        if is_case_only_same_entry(&source_anchor, &destination_anchor, &source_metadata)? {
            source_anchor.rename_to_replace(&destination_anchor)?;
            self.next_generation();
            return mutation_metadata(&destination_anchor, &logical_destination);
        }
        let backup = stage_destination(
            &destination_anchor,
            request.overwrite_confirmed,
            request.non_empty_confirmed,
        )?;
        let renamed = source_anchor.rename_to_noreplace(&destination_anchor);
        if let Err(error) = renamed {
            rollback_destination(&destination_anchor, backup.as_ref())?;
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
        let _ = discard_backup(backup.as_ref());
        self.next_generation();
        mutation_metadata(&destination_anchor, &logical_destination)
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
        let source_meta = source_anchor.metadata_no_follow()?;
        if source_meta.is_dir() && logical_destination.starts_with(&logical_source) {
            bail!("destination_inside_source: a directory cannot be duplicated into itself");
        }
        let backup = stage_destination(
            &destination_anchor,
            request.overwrite_confirmed,
            request.non_empty_confirmed,
        )?;
        let result = if source_meta.is_symlink() {
            check_cancelled(cancellation)
                .and_then(|()| copy_symlink_atomic(&source_anchor, &destination_anchor))
        } else if source_meta.is_dir() {
            copy_directory_atomic(&source_anchor, &destination_anchor, cancellation)
        } else if source_meta.is_file() {
            copy_file_atomic(
                &source_anchor,
                &destination_anchor,
                source_meta.permissions(),
                cancellation,
            )
        } else {
            Err(anyhow::anyhow!(
                "only regular files and directories can be duplicated"
            ))
        };
        if let Err(error) = result {
            rollback_destination(&destination_anchor, backup.as_ref())?;
            return Err(error);
        }
        let _ = discard_backup(backup.as_ref());
        self.next_generation();
        mutation_metadata(&destination_anchor, &logical_destination)
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
        let metadata = target_anchor.metadata_no_follow()?;
        check_cancelled(cancellation)?;
        if metadata.is_symlink() || metadata.is_file() {
            target_anchor.unlink(false)?;
        } else if metadata.is_dir() {
            let non_empty = !target_anchor.directory_entries()?.is_empty();
            if non_empty && !request.non_empty_confirmed {
                return Err(confirmation_required(
                    "confirmation_required_non_empty_directory",
                ));
            }
            if non_empty {
                let staged = target_anchor.sibling(OsString::from(format!(
                    ".tmux-ide-delete-{}.partial",
                    Uuid::new_v4()
                )))?;
                target_anchor.rename_to_noreplace(&staged)?;
                let result = remove_directory_cooperative(&staged, cancellation);
                if let Err(error) = result {
                    // The public delete committed at rename. Finish cleanup and
                    // report success if cleanup succeeds; never claim cancellation
                    // after the irreversible boundary.
                    remove_path(&staged).with_context(|| {
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
    path: &AnchoredPath,
    overwrite: bool,
    non_empty: bool,
) -> anyhow::Result<Option<AnchoredPath>> {
    if !path.exists()? {
        return Ok(None);
    }
    if !overwrite {
        return Err(confirmation_required(
            "confirmation_required_destination_overwrite",
        ));
    }
    let metadata = path.metadata_no_follow()?;
    let expected_identity = (metadata.device(), metadata.inode());
    if metadata.is_dir() {
        let has_entries = !path.directory_entries()?.is_empty();
        if has_entries && !non_empty {
            return Err(confirmation_required(
                "confirmation_required_non_empty_destination",
            ));
        }
    } else if !metadata.is_symlink() && !metadata.is_file() {
        bail!("unsupported overwrite destination type");
    }
    let backup = path.sibling(OsString::from(format!(
        ".tmux-ide-overwrite-{}.partial",
        Uuid::new_v4()
    )))?;
    path.rename_to_noreplace(&backup)?;
    let staged = backup.metadata_no_follow()?;
    if (staged.device(), staged.inode()) != expected_identity {
        let _ = backup.rename_to_noreplace(path);
        bail!("destination changed while it was being staged; retry from a fresh snapshot");
    }
    Ok(Some(backup))
}

fn discard_backup(backup: Option<&AnchoredPath>) -> anyhow::Result<()> {
    if let Some(backup) = backup {
        remove_path(backup)?;
    }
    Ok(())
}

fn rollback_destination(
    destination: &AnchoredPath,
    backup: Option<&AnchoredPath>,
) -> anyhow::Result<()> {
    if let Some(backup) = backup {
        backup.rename_to_noreplace(destination).context(
            "destination changed during rollback; preserved backup instead of deleting a racer",
        )?;
    }
    Ok(())
}

fn remove_path(path: &AnchoredPath) -> anyhow::Result<()> {
    let metadata = path.metadata_no_follow()?;
    if metadata.is_dir() && !metadata.is_symlink() {
        for name in path.directory_entries()? {
            remove_path(&path.child(name)?)?;
        }
        path.unlink(true)?;
    } else {
        path.unlink(false)?;
    }
    Ok(())
}

fn copy_file_atomic(
    source: &AnchoredPath,
    destination: &AnchoredPath,
    permissions: fs::Permissions,
    cancellation: &AtomicBool,
) -> anyhow::Result<()> {
    let temporary = destination.sibling(OsString::from(format!(
        ".tmux-ide-copy-{}.partial",
        Uuid::new_v4()
    )))?;
    let result = (|| -> anyhow::Result<()> {
        let mut input = source.open_file()?;
        let mut output = temporary.create_file(0o600)?;
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
        output.set_permissions(permissions)?;
        temporary.rename_to_noreplace(destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = temporary.unlink(false);
    }
    result
}

fn copy_symlink_atomic(source: &AnchoredPath, destination: &AnchoredPath) -> anyhow::Result<()> {
    let temporary = destination.sibling(OsString::from(format!(
        ".tmux-ide-copy-{}.partial",
        Uuid::new_v4()
    )))?;
    let link = source.read_link()?;
    let result = (|| -> anyhow::Result<()> {
        temporary.create_symlink(&link)?;
        temporary.rename_to_noreplace(destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = temporary.unlink(false);
    }
    result
}

fn copy_directory(
    source: &AnchoredPath,
    destination: &AnchoredPath,
    cancellation: &AtomicBool,
) -> anyhow::Result<()> {
    check_cancelled(cancellation)?;
    let source_permissions = source.open_directory()?.metadata()?.permissions();
    destination.create_directory(0o700)?;
    destination
        .open_directory()?
        .set_permissions(source_permissions)?;
    for name in source.directory_entries()? {
        check_cancelled(cancellation)?;
        let source_entry = source.child(name.clone())?;
        let target = destination.child(name)?;
        let metadata = source_entry.metadata_no_follow()?;
        if metadata.is_symlink() {
            target.create_symlink(&source_entry.read_link()?)?;
            continue;
        }
        if metadata.is_dir() {
            copy_directory(&source_entry, &target, cancellation)?;
        } else if metadata.is_file() {
            copy_file_atomic(&source_entry, &target, metadata.permissions(), cancellation)?;
        } else {
            bail!("unsupported file type while duplicating directory");
        }
    }
    Ok(())
}

fn copy_directory_atomic(
    source: &AnchoredPath,
    destination: &AnchoredPath,
    cancellation: &AtomicBool,
) -> anyhow::Result<()> {
    let temporary = destination.sibling(OsString::from(format!(
        ".tmux-ide-copy-{}.partial",
        Uuid::new_v4()
    )))?;
    let result = copy_directory(source, &temporary, cancellation).and_then(|()| {
        check_cancelled(cancellation)?;
        temporary.rename_to_noreplace(destination)?;
        Ok(())
    });
    if result.is_err() && temporary.exists().unwrap_or(false) {
        let _ = remove_path(&temporary);
    }
    result
}

fn remove_directory_cooperative(
    path: &AnchoredPath,
    cancellation: &AtomicBool,
) -> anyhow::Result<()> {
    check_cancelled(cancellation)?;
    for name in path.directory_entries()? {
        check_cancelled(cancellation)?;
        let entry = path.child(name)?;
        let metadata = entry.metadata_no_follow()?;
        if metadata.is_dir() && !metadata.is_symlink() {
            remove_directory_cooperative(&entry, cancellation)?;
        } else {
            entry.unlink(false)?;
        }
    }
    path.unlink(true)?;
    Ok(())
}

fn is_case_only_same_entry(
    source: &AnchoredPath,
    destination: &AnchoredPath,
    source_metadata: &AnchoredMetadata,
) -> anyhow::Result<bool> {
    if !source.same_parent(destination)? || source.leaf() == destination.leaf() {
        return Ok(false);
    }
    let Some(destination_metadata) = destination
        .exists()?
        .then(|| destination.metadata_no_follow())
        .transpose()?
    else {
        return Ok(false);
    };
    let same_folded_name = source.leaf().to_string_lossy().to_lowercase()
        == destination.leaf().to_string_lossy().to_lowercase();
    Ok(same_folded_name
        && (source_metadata.device(), source_metadata.inode())
            == (destination_metadata.device(), destination_metadata.inode()))
}

pub(super) fn mutation_metadata(
    anchor: &AnchoredPath,
    logical: &Path,
) -> anyhow::Result<v1::FileMetadata> {
    let metadata = anchor.metadata_no_follow()?;
    let symlink = metadata.is_symlink();
    let kind = if symlink {
        v1::FileKind::Symlink
    } else if metadata.is_dir() {
        v1::FileKind::Directory
    } else if metadata.is_file() {
        v1::FileKind::File
    } else {
        v1::FileKind::Other
    };
    let entry_name = logical.file_name().unwrap_or(logical.as_os_str());
    let name = entry_name.to_string_lossy().into_owned();
    let mime = image_mime(logical).unwrap_or_default().to_owned();
    Ok(v1::FileMetadata {
        path: logical.to_string_lossy().into_owned(),
        name: name.clone(),
        kind: kind.into(),
        size: metadata.len(),
        modified_unix_millis: metadata.modified_unix_millis(),
        mode: metadata.mode(),
        symlink,
        symlink_target: symlink
            .then(|| anchor.read_link().ok())
            .flatten()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default(),
        expandable: metadata.is_dir() && !is_never_enumerated(entry_name),
        generation: metadata.generation(),
        mime: mime.clone(),
        image_preview_eligible: metadata.is_file()
            && !mime.is_empty()
            && metadata.len() <= MAX_IMAGE_BYTES,
        symlink_target_kind: v1::FileKind::Unspecified.into(),
    })
}

fn check_cancelled(cancellation: &AtomicBool) -> anyhow::Result<()> {
    if cancellation.load(Ordering::Acquire) {
        bail!("cancelled: file mutation was cancelled");
    }
    Ok(())
}

#[cfg(test)]
mod descriptor_tests {
    use super::*;

    #[test]
    fn staging_and_recursive_copy_stay_on_captured_parent_after_late_swap() {
        #[cfg(target_os = "macos")]
        let temporary_root = PathBuf::from("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let temporary_root = std::env::temp_dir();
        let root = temporary_root.join(format!("ade-mutation-{}", Uuid::new_v4()));
        let outside = temporary_root.join(format!("ade-outside-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("parent/source/nested")).unwrap();
        fs::write(root.join("parent/source/nested/value"), "inside").unwrap();
        fs::create_dir(root.join("parent/destination")).unwrap();
        fs::write(root.join("parent/destination/original"), "original").unwrap();
        fs::create_dir_all(outside.join("destination")).unwrap();
        fs::write(outside.join("destination/foreign"), "foreign").unwrap();

        let capability = RootCapability::capture(root.to_str().unwrap()).unwrap();
        let source = capability.anchor(&root.join("parent/source")).unwrap();
        let destination = capability.anchor(&root.join("parent/destination")).unwrap();
        fs::rename(root.join("parent"), root.join("captured-parent")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("parent")).unwrap();

        let backup = stage_destination(&destination, true, true).unwrap();
        copy_directory_atomic(&source, &destination, &AtomicBool::new(false)).unwrap();
        discard_backup(backup.as_ref()).unwrap();

        assert_eq!(
            fs::read_to_string(root.join("captured-parent/destination/nested/value")).unwrap(),
            "inside"
        );
        assert_eq!(
            fs::read_to_string(outside.join("destination/foreign")).unwrap(),
            "foreign"
        );
        assert!(!outside.join("destination/nested").exists());
        fs::remove_file(root.join("parent")).unwrap();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
