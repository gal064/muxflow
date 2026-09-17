use super::*;

pub(super) fn validate_destination_basename(name: &str) -> anyhow::Result<()> {
    let path = Path::new(name);
    let mut components = path.components();
    let safe = matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && name.len() <= 255
        && !name.contains('\0')
        && !name.starts_with(".tmux-agent-");
    if !safe {
        bail!("upload destination must be one non-empty basename");
    }
    Ok(())
}

pub(super) fn choose_and_reserve_destination(
    directory: &StagingDirectory,
    name: &str,
    collision: v1::CollisionPolicy,
) -> anyhow::Result<(OsString, bool, OsString, File)> {
    if name.len() > directory.name_max()? {
        bail!("upload destination basename exceeds filesystem NAME_MAX");
    }
    let requested = OsString::from(name);
    let existing = directory.metadata(&requested);
    if matches!(&existing, Err(error) if is_not_found(error)) {
        if collision == v1::CollisionPolicy::Unspecified {
            bail!("collision policy is required");
        }
        if let Some((reservation_name, reservation_file)) =
            try_reserve_destination(directory, &requested)?
        {
            return Ok((requested, false, reservation_name, reservation_file));
        }
        if collision != v1::CollisionPolicy::Rename {
            bail!("upload destination is reserved by another active transfer");
        }
        return choose_renamed_reservation(directory, name, 1);
    }
    let existing = existing?;
    match collision {
        v1::CollisionPolicy::Fail | v1::CollisionPolicy::Unspecified => {
            bail!("upload destination already exists")
        }
        v1::CollisionPolicy::OverwriteConfirmed => {
            if !existing.is_file() || existing.file_type().is_symlink() {
                bail!("overwrite destination must be a regular file");
            }
            let Some((reservation_name, reservation_file)) =
                try_reserve_destination(directory, &requested)?
            else {
                bail!("upload destination is reserved by another active transfer");
            };
            Ok((requested, false, reservation_name, reservation_file))
        }
        v1::CollisionPolicy::Rename => choose_renamed_reservation(directory, name, 1),
    }
}

pub(super) fn choose_renamed_reservation(
    directory: &StagingDirectory,
    name: &str,
    first_index: usize,
) -> anyhow::Result<(OsString, bool, OsString, File)> {
    let name_max = directory.name_max()?;
    let path = Path::new(name);
    let stem = path.file_stem().and_then(OsStr::to_str).unwrap_or(name);
    let extension = path.extension().and_then(OsStr::to_str);
    for index in first_index..=10_000 {
        let candidate = renamed_basename(stem, extension, index, name_max)?;
        let candidate = OsString::from(candidate);
        if matches!(directory.metadata(&candidate), Err(error) if is_not_found(&error))
            && let Some((reservation_name, reservation_file)) =
                try_reserve_destination(directory, &candidate)?
        {
            return Ok((candidate, true, reservation_name, reservation_file));
        }
    }
    bail!("could not choose a non-colliding upload basename")
}

pub(super) fn renamed_basename(
    stem: &str,
    extension: Option<&str>,
    index: usize,
    name_max: usize,
) -> anyhow::Result<String> {
    let suffix = format!(" ({index})");
    if suffix.len() >= name_max {
        bail!("filesystem NAME_MAX is too small for collision suffix");
    }
    let extension = extension.filter(|value| suffix.len() + 2 + value.len() <= name_max);
    let extension_bytes = extension.map_or(0, |value| value.len() + 1);
    let budget = name_max - suffix.len() - extension_bytes;
    let mut boundary = stem.len().min(budget);
    while boundary > 0 && !stem.is_char_boundary(boundary) {
        boundary -= 1;
    }
    if boundary == 0 {
        bail!("upload destination basename cannot fit collision suffix");
    }
    let mut candidate = format!("{}{}", &stem[..boundary], suffix);
    if let Some(extension) = extension {
        candidate.push('.');
        candidate.push_str(extension);
    }
    debug_assert!(candidate.len() <= name_max && candidate.is_char_boundary(candidate.len()));
    Ok(candidate)
}

pub(super) fn try_reserve_destination(
    directory: &StagingDirectory,
    candidate: &OsStr,
) -> anyhow::Result<Option<(OsString, File)>> {
    let reservation_name = reservation_leaf_name(candidate);
    for _ in 0..2 {
        match directory.create_private(&reservation_name) {
            Ok(file) => {
                lock_exclusive(&file)?;
                return Ok(Some((reservation_name, file)));
            }
            Err(error) if is_already_exists(&error) => {
                let existing = match directory.open_readonly(&reservation_name) {
                    Ok(file) => file,
                    Err(_) => return Ok(None),
                };
                if !try_lock_exclusive(&existing)? {
                    return Ok(None);
                }
                // An unlocked reservation can only be a crashed helper's
                // unambiguous internal leaf. Remove it descriptor-relatively.
                directory.unlink(&reservation_name)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

pub(super) fn reservation_leaf_name(candidate: &OsStr) -> OsString {
    let hash = blake3::hash(candidate.as_encoded_bytes()).to_hex();
    OsString::from(format!("{RESERVATION_PREFIX}{hash}{RESERVATION_SUFFIX}"))
}
