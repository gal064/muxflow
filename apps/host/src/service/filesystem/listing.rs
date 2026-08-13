use super::*;

impl FileService {
    #[cfg(test)]
    pub(crate) fn list_directory(
        &self,
        root: &str,
        path: &str,
        watch_id: &str,
    ) -> anyhow::Result<v1::DirectorySnapshot> {
        let token = root_token(root)?;
        self.list_directory_page_authorized(root, &token, path, watch_id, "", 0)
    }

    #[cfg(test)]
    pub(crate) fn list_directory_page(
        &self,
        root: &str,
        path: &str,
        watch_id: &str,
        page_token: &str,
        page_size: u32,
    ) -> anyhow::Result<v1::DirectorySnapshot> {
        let token = root_token(root)?;
        self.list_directory_page_authorized(root, &token, path, watch_id, page_token, page_size)
    }

    pub(crate) fn list_directory_page_authorized(
        &self,
        root: &str,
        root_token: &str,
        path: &str,
        watch_id: &str,
        page_token: &str,
        page_size: u32,
    ) -> anyhow::Result<v1::DirectorySnapshot> {
        let root = RootCapability::validate(root, root_token)?;
        list_directory_impl(
            &root,
            path,
            watch_id,
            self.next_generation(),
            page_token,
            page_size,
        )
    }
}

pub(super) fn list_directory_impl(
    root: &RootCapability,
    path: &str,
    watch_id: &str,
    generation: u64,
    page_token: &str,
    page_size: u32,
) -> anyhow::Result<v1::DirectorySnapshot> {
    let (logical_target, _) = root.resolve_new(path)?;
    let directory = if logical_target == root.logical_root() {
        root.open_root_directory()?
    } else {
        root.anchor(&logical_target)?.open_directory()?
    };
    let relative = logical_target
        .strip_prefix(root.logical_root())
        .expect("resolved path is in root");
    if relative.components().any(|component| {
        matches!(component, Component::Normal(value) if value == OsStr::new(".git") || value == OsStr::new("node_modules"))
    }) {
        bail!(".git, node_modules, and directory symlinks are collapsed and unwatched by default");
    }
    let page_size = if page_size == 0 {
        DEFAULT_DIRECTORY_PAGE
    } else {
        usize::try_from(page_size)
            .unwrap_or(MAX_DIRECTORY_ENTRIES)
            .clamp(1, MAX_DIRECTORY_ENTRIES)
    };
    let start = decode_page_token(page_token)?;
    let mut candidates = BTreeMap::<(u8, Vec<u8>), v1::FileMetadata>::new();
    let entries = if logical_target == root.logical_root() {
        root.directory_entries()?
    } else {
        root.anchor(&logical_target)?.directory_entries()?
    };
    for name in entries {
        let entry = AnchoredPath::in_directory(&directory, name.clone())?;
        let metadata = metadata_for_directory_entry(&entry, &logical_target.join(&name))?;
        let rank = u8::from(metadata.kind != i32::from(v1::FileKind::Directory));
        let key = (rank, name.as_bytes().to_vec());
        if start.as_ref().is_some_and(|start| key <= *start) {
            continue;
        }
        candidates.insert(key, metadata);
        if candidates.len() > page_size.saturating_add(1) {
            candidates.pop_last();
        }
    }
    let overflowed = candidates.len() > page_size;
    if overflowed {
        candidates.pop_last();
    }
    let next_page_token = if overflowed {
        candidates
            .last_key_value()
            .map(|(key, _)| encode_page_token(key))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let entries = candidates.into_values().collect();
    Ok(v1::DirectorySnapshot {
        watch_id: watch_id.to_owned(),
        root: root.logical_root().to_string_lossy().into_owned(),
        path: logical_target.to_string_lossy().into_owned(),
        generation,
        entries,
        overflowed,
        authoritative: true,
        next_page_token,
        complete: !overflowed,
    })
}

pub(super) fn resolve_watch_directory(
    root: &RootCapability,
    path: &str,
) -> anyhow::Result<(PathBuf, File)> {
    let (logical_target, stable_target) = root.resolve_existing(path)?;
    if fs::symlink_metadata(&stable_target)?
        .file_type()
        .is_symlink()
    {
        bail!("directory watch target must be a non-symlink directory");
    }
    let directory = if logical_target == root.logical_root() {
        root.open_root_directory()?
    } else {
        root.anchor(&logical_target)?.open_directory()?
    };
    let relative = logical_target
        .strip_prefix(root.logical_root())
        .expect("resolved path is in root");
    if relative.components().any(|component| {
        matches!(component, Component::Normal(value) if value == OsStr::new(".git") || value == OsStr::new("node_modules"))
    }) {
        bail!(".git, node_modules, and directory symlinks are collapsed and unwatched by default");
    }
    Ok((logical_target, directory))
}

fn encode_page_token(key: &(u8, Vec<u8>)) -> String {
    let mut value = format!("{}:", key.0);
    for byte in &key.1 {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

fn decode_page_token(value: &str) -> anyhow::Result<Option<(u8, Vec<u8>)>> {
    if value.is_empty() {
        return Ok(None);
    }
    let (rank, encoded) = value
        .split_once(':')
        .context("invalid directory page token")?;
    let rank = rank.parse::<u8>().context("invalid directory page token")?;
    if rank > 1 || encoded.len() % 2 != 0 || encoded.len() > 8192 {
        bail!("invalid directory page token");
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().chunks_exact(2) {
        let text = std::str::from_utf8(pair).context("invalid directory page token")?;
        bytes.push(u8::from_str_radix(text, 16).context("invalid directory page token")?);
    }
    Ok(Some((rank, bytes)))
}
