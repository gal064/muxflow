use super::*;

/// How many ordered entries one point-in-time directory snapshot retains.
///
/// A directory larger than this is still fully pageable: the last page of a
/// snapshot hands back a token that carries its own resume key, so the next
/// page rebuilds one further snapshot rather than re-stating the whole
/// directory once per page.
pub(super) const SNAPSHOT_ENTRY_LIMIT: usize = MAX_DIRECTORY_ENTRIES;

/// How many snapshots one connection retains, and for how long.
const MAX_RETAINED_SNAPSHOTS: usize = 8;
const SNAPSHOT_TTL: Duration = Duration::from_secs(120);

/// The ordering key a directory listing pages through: directories first, then
/// the raw entry name bytes. Raw bytes, because a name that does not survive
/// UTF-8 replacement must still have exactly one position in the order.
pub(super) type EntryKey = (u8, Vec<u8>);

pub(super) fn entry_key(metadata: &v1::FileMetadata, name: &OsStr) -> EntryKey {
    (
        u8::from(metadata.kind != i32::from(v1::FileKind::Directory)),
        name.as_bytes().to_vec(),
    )
}

/// One point-in-time ordered view of a directory, retained so later pages slice
/// it instead of enumerating and stating the whole remote directory again.
pub(super) struct DirectorySnapshotPage {
    binding: PageBinding,
    entries: Vec<(EntryKey, v1::FileMetadata)>,
    /// The generation every page of this listing reports.
    ///
    /// One listing has one revision. Minting a fresh one per page gave an
    /// assembled multi-page listing a revision that described only its last
    /// page, which is a version number for something nobody was holding.
    generation: u64,
    /// The scan stopped at [`SNAPSHOT_ENTRY_LIMIT`], so entries after the last
    /// retained key exist and need one further scan.
    truncated: bool,
    created: Instant,
}

/// Everything a page token is bound to. A token that does not reproduce this
/// exact identity belongs to another server, root, or directory inode and is
/// refused rather than answered from the wrong place.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct PageBinding {
    server_identity: String,
    root_token: String,
    logical_path: Vec<u8>,
    device: u64,
    inode: u64,
}

impl PageBinding {
    pub(super) fn capture(
        root: &RootCapability,
        logical_target: &Path,
        directory: &File,
    ) -> anyhow::Result<Self> {
        let metadata = directory.metadata()?;
        Ok(Self {
            server_identity: crate::service::snapshot::server_identity(),
            root_token: root.token().to_owned(),
            logical_path: logical_target.as_os_str().as_bytes().to_vec(),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    /// The binding's identity, including the generation the token continues.
    ///
    /// Generation is inside the digest rather than beside it so a token cannot
    /// be edited to claim a different listing revision than the one it was
    /// issued for.
    fn digest(&self, generation: u64) -> String {
        let mut hasher = blake3::Hasher::new();
        for part in [
            self.server_identity.as_bytes(),
            self.root_token.as_bytes(),
            self.logical_path.as_slice(),
        ] {
            hasher.update(&(part.len() as u64).to_le_bytes());
            hasher.update(part);
        }
        hasher.update(&self.device.to_le_bytes());
        hasher.update(&self.inode.to_le_bytes());
        hasher.update(&generation.to_le_bytes());
        hasher.finalize().to_hex()[..16].to_owned()
    }
}

/// Where a requested page resumes from.
pub(super) struct PageCursor {
    /// The snapshot this token was issued against, when it is still retained.
    pub(super) snapshot_id: String,
    /// Index of the first entry of this page within that snapshot.
    pub(super) index: usize,
    /// The generation the listing this token continues was issued under, so
    /// every page of it reports one revision even after the snapshot expires.
    pub(super) generation: u64,
    /// The last key already delivered. Self-describing, so a token whose
    /// snapshot has expired still resumes exactly rather than restarting.
    pub(super) resume_after: Option<EntryKey>,
}

const TOKEN_PREFIX: &str = "p1";

pub(super) fn encode_page_token(
    binding: &PageBinding,
    snapshot_id: &str,
    index: usize,
    generation: u64,
    resume_after: &EntryKey,
) -> String {
    format!(
        "{TOKEN_PREFIX}:{}:{snapshot_id}:{index}:{generation}:{}:{}",
        binding.digest(generation),
        resume_after.0,
        hex_encode(&resume_after.1),
    )
}

pub(super) fn decode_page_token(
    binding: &PageBinding,
    value: &str,
) -> anyhow::Result<Option<PageCursor>> {
    if value.is_empty() {
        return Ok(None);
    }
    let mut parts = value.splitn(7, ':');
    let invalid = || stale_page_token("invalid directory page token");
    let prefix = parts.next().ok_or_else(invalid)?;
    let digest = parts.next().ok_or_else(invalid)?;
    let snapshot_id = parts.next().ok_or_else(invalid)?;
    let index = parts.next().ok_or_else(invalid)?;
    let generation = parts.next().ok_or_else(invalid)?;
    let rank = parts.next().ok_or_else(invalid)?;
    let name = parts.next().ok_or_else(invalid)?;
    if prefix != TOKEN_PREFIX {
        return Err(invalid());
    }
    let index = index.parse::<usize>().map_err(|_| invalid())?;
    let generation = generation.parse::<u64>().map_err(|_| invalid())?;
    let rank = rank.parse::<u8>().map_err(|_| invalid())?;
    if digest != binding.digest(generation) {
        return Err(stale_page_token(
            "directory page token belongs to another server, root, directory, or listing",
        ));
    }
    if rank > 1 || snapshot_id.len() > 64 {
        return Err(invalid());
    }
    Ok(Some(PageCursor {
        snapshot_id: snapshot_id.to_owned(),
        index,
        generation,
        resume_after: Some((rank, hex_decode(name)?)),
    }))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

fn hex_decode(value: &str) -> anyhow::Result<Vec<u8>> {
    let invalid = || stale_page_token("invalid directory page token");
    if !value.len().is_multiple_of(2) || value.len() > 8192 {
        return Err(invalid());
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let text = std::str::from_utf8(pair).map_err(|_| invalid())?;
        bytes.push(u8::from_str_radix(text, 16).map_err(|_| invalid())?);
    }
    Ok(bytes)
}

/// The connection's bounded set of retained directory snapshots.
#[derive(Default)]
pub(super) struct DirectoryPageCache {
    snapshots: Mutex<HashMap<String, DirectorySnapshotPage>>,
}

/// One page sliced out of a retained snapshot.
pub(super) struct RetainedPage {
    pub(super) entries: Vec<v1::FileMetadata>,
    pub(super) next: Option<(usize, EntryKey)>,
    pub(super) snapshot_id: String,
    pub(super) generation: u64,
}

impl DirectoryPageCache {
    /// Retains an ordered snapshot and returns its identity.
    pub(super) fn insert(
        &self,
        binding: PageBinding,
        entries: Vec<(EntryKey, v1::FileMetadata)>,
        generation: u64,
        truncated: bool,
    ) -> String {
        let snapshot_id = Uuid::new_v4().simple().to_string();
        let mut snapshots = self.snapshots.lock().unwrap();
        snapshots.retain(|_, page| page.created.elapsed() <= SNAPSHOT_TTL);
        while snapshots.len() >= MAX_RETAINED_SNAPSHOTS {
            let Some(oldest) = snapshots
                .iter()
                .min_by_key(|(_, page)| page.created)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            snapshots.remove(&oldest);
        }
        snapshots.insert(
            snapshot_id.clone(),
            DirectorySnapshotPage {
                binding,
                entries,
                generation,
                truncated,
                created: Instant::now(),
            },
        );
        snapshot_id
    }

    /// Slices one page out of a retained snapshot, or `None` when the snapshot
    /// is gone, bound elsewhere, or exhausted past its retained entries.
    pub(super) fn page(
        &self,
        binding: &PageBinding,
        cursor: &PageCursor,
        page_size: usize,
    ) -> Option<RetainedPage> {
        let mut snapshots = self.snapshots.lock().unwrap();
        // Swept on the way in, not only on the way out: a session that opens
        // one large directory and then goes idle would otherwise keep its
        // retained entries resident until something else inserted one.
        snapshots.retain(|_, page| page.created.elapsed() <= SNAPSHOT_TTL);
        let page = snapshots.get(&cursor.snapshot_id)?;
        if page.binding != *binding
            || page.generation != cursor.generation
            || page.created.elapsed() > SNAPSHOT_TTL
        {
            return None;
        }
        if cursor.index > page.entries.len() {
            return None;
        }
        let window = &page.entries[cursor.index..];
        if window.is_empty() {
            // Exhausted. A truncated snapshot still has entries after it, so
            // the caller must scan once more from the token's resume key.
            return (!page.truncated).then(|| RetainedPage {
                entries: Vec::new(),
                next: None,
                snapshot_id: cursor.snapshot_id.clone(),
                generation: page.generation,
            });
        }
        let taken = window.len().min(page_size);
        let entries = window[..taken]
            .iter()
            .map(|(_, metadata)| metadata.clone())
            .collect();
        let next_index = cursor.index + taken;
        // A truncated snapshot's last retained page still advertises a next
        // page: its cursor lands on the retained end, where `page` reports
        // exhaustion and the caller resumes from the key that token carries.
        let has_more = next_index < page.entries.len() || page.truncated;
        Some(RetainedPage {
            entries,
            next: has_more.then(|| (next_index, window[taken - 1].0.clone())),
            snapshot_id: cursor.snapshot_id.clone(),
            generation: page.generation,
        })
    }
}
