use super::listing_page::{
    EntryKey, PageBinding, SNAPSHOT_ENTRY_LIMIT, decode_page_token, encode_page_token, entry_key,
};
use super::*;

/// A directory read nothing can cancel: the watcher's own rescans, whose
/// lifetime is the watch rather than one client request.
pub(super) static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);

impl FileService {
    #[cfg(test)]
    pub(crate) fn list_directory(
        &self,
        root: &str,
        path: &str,
        watch_id: &str,
    ) -> anyhow::Result<v1::DirectorySnapshot> {
        let token = root_token(root)?;
        self.list_directory_page_authorized(root, &token, path, watch_id, "", 0, &NEVER_CANCELLED)
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
        self.list_directory_page_authorized(
            root,
            &token,
            path,
            watch_id,
            page_token,
            page_size,
            &NEVER_CANCELLED,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn list_directory_page_authorized(
        &self,
        root: &str,
        root_token: &str,
        path: &str,
        watch_id: &str,
        page_token: &str,
        page_size: u32,
        cancellation: &AtomicBool,
    ) -> anyhow::Result<v1::DirectorySnapshot> {
        let root = RootCapability::validate(root, root_token)?;
        self.list_directory_snapshot(
            &root,
            path,
            watch_id,
            self.next_generation(),
            page_token,
            page_size,
            cancellation,
        )
    }

    /// Reads one page of a directory, reusing a retained ordered snapshot when
    /// the caller is continuing a listing it already started.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn list_directory_snapshot(
        &self,
        root: &RootCapability,
        path: &str,
        watch_id: &str,
        generation: u64,
        page_token: &str,
        page_size: u32,
        cancellation: &AtomicBool,
    ) -> anyhow::Result<v1::DirectorySnapshot> {
        let (logical_target, directory) = open_listable_directory(root, path)?;
        let binding = PageBinding::capture(root, &logical_target, &directory)?;
        let page_size = normalize_page_size(page_size);
        let cursor = decode_page_token(&binding, page_token)?;

        // A retained snapshot answers without touching the filesystem again.
        // `page` returns `None` exactly when it cannot — expired, bound to
        // another directory, or exhausted past a truncated scan — and the
        // token's own resume key then drives one further bounded scan.
        if let Some(cursor) = &cursor
            && let Some(retained) = self.pages.page(&binding, cursor, page_size)
        {
            let next_page_token = retained.next.as_ref().map(|(index, key)| {
                encode_page_token(
                    &binding,
                    &retained.snapshot_id,
                    *index,
                    retained.generation,
                    key,
                )
            });
            return Ok(snapshot(
                root,
                &logical_target,
                watch_id,
                retained.generation,
                retained.entries,
                next_page_token,
            ));
        }

        // Every page of one listing reports the revision the listing started
        // with, so an assembled multi-page listing has a coherent one.
        let generation = cursor
            .as_ref()
            .map_or(generation, |cursor| cursor.generation);
        let resume_after = cursor.and_then(|cursor| cursor.resume_after);
        let (entries, truncated) = scan_ordered_entries(
            root,
            &logical_target,
            &directory,
            resume_after.as_ref(),
            cancellation,
        )?;
        let taken = entries.len().min(page_size);
        let has_more = entries.len() > taken || truncated;
        let page: Vec<v1::FileMetadata> = entries[..taken]
            .iter()
            .map(|(_, metadata)| metadata.clone())
            .collect();
        let next_page_token = if has_more {
            let last_key = entries[taken - 1].0.clone();
            let snapshot_id = self
                .pages
                .insert(binding.clone(), entries, generation, truncated);
            Some(encode_page_token(
                &binding,
                &snapshot_id,
                taken,
                generation,
                &last_key,
            ))
        } else {
            None
        };
        Ok(snapshot(
            root,
            &logical_target,
            watch_id,
            generation,
            page,
            next_page_token,
        ))
    }
}

fn normalize_page_size(page_size: u32) -> usize {
    if page_size == 0 {
        DEFAULT_DIRECTORY_PAGE
    } else {
        usize::try_from(page_size)
            .unwrap_or(MAX_DIRECTORY_ENTRIES)
            .clamp(1, MAX_DIRECTORY_ENTRIES)
    }
}

fn snapshot(
    root: &RootCapability,
    logical_target: &Path,
    watch_id: &str,
    generation: u64,
    entries: Vec<v1::FileMetadata>,
    next_page_token: Option<String>,
) -> v1::DirectorySnapshot {
    let overflowed = next_page_token.is_some();
    v1::DirectorySnapshot {
        watch_id: watch_id.to_owned(),
        root: root.logical_root().to_string_lossy().into_owned(),
        path: logical_target.to_string_lossy().into_owned(),
        generation,
        entries,
        overflowed,
        authoritative: true,
        next_page_token: next_page_token.unwrap_or_default(),
        complete: !overflowed,
        recovered_from_overflow: false,
    }
}

fn open_listable_directory(root: &RootCapability, path: &str) -> anyhow::Result<(PathBuf, File)> {
    let (logical_target, _) = root.resolve_new(path)?;
    let directory = if logical_target == root.logical_root() {
        root.open_root_directory()?
    } else {
        root.anchor(&logical_target)?.open_directory()?
    };
    ensure_enterable(root, &logical_target)?;
    Ok((logical_target, directory))
}

/// Enumerates and stats one bounded ordered window of a directory.
///
/// The ordered set is bounded at [`SNAPSHOT_ENTRY_LIMIT`] and entries at or
/// before `resume_after` are dropped rather than retained, so peak memory is a
/// window rather than the directory. Ranking an entry requires its metadata, so
/// one scan still stats every visible name once — but only once per window,
/// rather than once per page as an unbacked resume-by-key scan did.
fn scan_ordered_entries(
    root: &RootCapability,
    logical_target: &Path,
    directory: &File,
    resume_after: Option<&EntryKey>,
    cancellation: &AtomicBool,
) -> anyhow::Result<(Vec<(EntryKey, v1::FileMetadata)>, bool)> {
    let names = if logical_target == root.logical_root() {
        root.directory_entries()?
    } else {
        root.anchor(logical_target)?.directory_entries()?
    };
    let mut ordered = BTreeMap::<EntryKey, v1::FileMetadata>::new();
    let mut truncated = false;
    for name in names {
        if cancellation.load(Ordering::Acquire) {
            return Err(cancelled("directory listing"));
        }
        // Before the page window, not after it: the entry is not in this
        // listing at all, so it must not consume a page slot or become the
        // pagination token the next page resumes from.
        if is_always_hidden(&name) {
            continue;
        }
        let entry = AnchoredPath::in_directory(directory, name.clone())?;
        let metadata = metadata_for_directory_entry(&entry, &logical_target.join(&name))?;
        let key = entry_key(&metadata, &name);
        if resume_after.is_some_and(|resume_after| key <= *resume_after) {
            continue;
        }
        ordered.insert(key, metadata);
        if ordered.len() > SNAPSHOT_ENTRY_LIMIT {
            ordered.pop_last();
            truncated = true;
        }
    }
    Ok((ordered.into_iter().collect(), truncated))
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
    ensure_enterable(root, &logical_target)?;
    Ok((logical_target, directory))
}

/// Refuses a path that runs through a directory this service never enumerates.
///
/// Asking for the path directly is the way around a listing, so it answers the
/// same predicate the listing does. Otherwise a hidden directory would only be
/// absent from one view rather than hidden, and `expandable: false` on a
/// collapsed one would be a suggestion. This is also what keeps a watch off
/// those subtrees, which is why the guard was written in the first place:
/// `node_modules` is where the descriptor budget goes to die.
fn ensure_enterable(root: &RootCapability, logical_target: &Path) -> anyhow::Result<()> {
    let relative = logical_target
        .strip_prefix(root.logical_root())
        .expect("resolved path is in root");
    if relative.components().any(
        |component| matches!(component, Component::Normal(value) if is_never_enumerated(value)),
    ) {
        bail!(
            "hidden and collapsed directories, and directory symlinks, are not listed or watched"
        );
    }
    Ok(())
}
