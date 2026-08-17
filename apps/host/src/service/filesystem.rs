use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ffi::{OsStr, OsString},
    fs::{self, File, Metadata},
    io::{ErrorKind, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt},
        io::AsRawFd,
    },
    path::{Component, Path, PathBuf},
    process::{Child, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tmux_agent_protocol::v1;
use tokio::{sync::mpsc, time::sleep};
use uuid::Uuid;

use super::{SequencerControl, broadcast_control_event, emit_event};

mod download;
mod editor_io;
mod failure;
mod listing;
mod listing_page;
mod mutations;
mod open_stream;
mod path_policy;
mod terminal_upload;
pub(crate) use failure::FileFailure;
pub(crate) use terminal_upload::UploadCommitFailure;
mod watch_fallback;
mod watch_service;
use failure::{cancelled, stale_generation, stale_page_token};
use listing::{NEVER_CANCELLED, resolve_watch_directory};
use listing_page::DirectoryPageCache;
use mutations::mutation_metadata;
use path_policy::{AnchoredMetadata, AnchoredPath, RootCapability, descriptor_path};
use watch_fallback::FallbackTarget;
use watch_service::Watch;

pub(super) const MAX_TEXT_BYTES: u64 = 10 * 1024 * 1024;
pub(super) const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 10_000;
const DEFAULT_DIRECTORY_PAGE: usize = 4096;
const MAX_WATCHES: usize = 128;
const MAX_TRANSFER_CHUNK: usize = 1024 * 1024;
/// How long the parked fallback poller waits before re-checking `closed`.
const IDLE_PARK: Duration = Duration::from_millis(500);

struct Transfer {
    source: TransferSource,
    offset: u64,
    total: Option<u64>,
    hasher: blake3::Hasher,
    source_generation: Option<u64>,
}

enum TransferSource {
    File(File),
    Archive {
        child: Child,
        stdout: ChildStdout,
        _directory: File,
    },
}

struct Upload {
    file: File,
    temporary: AnchoredPath,
    target: AnchoredPath,
    _root_capability: RootCapability,
    root_token: String,
    metadata_path: PathBuf,
    offset: u64,
    total: u64,
    hasher: blake3::Hasher,
    permissions: fs::Permissions,
}

pub(super) struct FileService {
    generation: AtomicU64,
    watches: Mutex<HashMap<String, Watch>>,
    transfers: Mutex<HashMap<String, Transfer>>,
    uploads: Mutex<HashMap<String, Upload>>,
    terminal_uploads: Mutex<HashMap<String, terminal_upload::TerminalUpload>>,
    native_watcher: Mutex<Option<RecommendedWatcher>>,
    /// Woken when a watch registration lands on the polling fallback, so the
    /// poller parks instead of ticking while every watch is native.
    fallback_signal: tokio::sync::Notify,
    /// Ordered point-in-time directory views, so a later page slices one
    /// instead of re-stating the whole remote directory.
    pages: DirectoryPageCache,
}

impl FileService {
    pub(super) fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            watches: Mutex::new(HashMap::new()),
            transfers: Mutex::new(HashMap::new()),
            uploads: Mutex::new(HashMap::new()),
            terminal_uploads: Mutex::new(HashMap::new()),
            native_watcher: Mutex::new(None),
            fallback_signal: tokio::sync::Notify::new(),
            pages: DirectoryPageCache::default(),
        }
    }

    fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }
}

/// The generation of a root capability.
///
/// Derived from the token rather than counted, so the same root always answers
/// with the same generation and a different one always answers differently —
/// with no map to bound, evict, or accidentally clear underneath every root at
/// once. The token is already the capability's exact identity.
pub(super) fn root_generation(root_token: &str) -> u64 {
    let digest = blake3::hash(root_token.as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    // Never zero: a zero generation is the protobuf default, which callers read
    // as "the host said nothing".
    u64::from_le_bytes(bytes) | 1
}

impl Drop for FileService {
    fn drop(&mut self) {
        for (_, mut transfer) in self.transfers.get_mut().unwrap().drain() {
            if let TransferSource::Archive { child, .. } = &mut transfer.source {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        for (_, upload) in self.uploads.get_mut().unwrap().drain() {
            let _ = upload.temporary.unlink(false);
        }
        for (_, upload) in self.terminal_uploads.get_mut().unwrap().drain() {
            terminal_upload::cleanup_dropped_terminal_upload(upload);
        }
    }
}

/// Entries no listing ever reports, whatever asks for it.
///
/// VS Code's `files.exclude` defaults, adopted verbatim because the explorer is
/// the surface the user compares this one against. `node_modules` is
/// deliberately *not* here — VS Code shows it too, and this host has always
/// shown it collapsed; hiding a directory people open on purpose is a different
/// decision from hiding repository plumbing and scratch files nobody edits.
const ALWAYS_HIDDEN: &[&str] = &[".git", ".svn", ".hg", "CVS", ".DS_Store", "Thumbs.db"];

/// Directories the tree shows but never enumerates the inside of.
const COLLAPSED_DIRECTORIES: &[&str] = &["node_modules"];

/// Whether an entry of this name is omitted from every directory listing.
///
/// One predicate, because the pagination token, the watch registry and the
/// descend guard would otherwise each be deciding it separately. An entry that
/// no listing reports but that a directly requested path can still be listed
/// from is not hidden — it is merely absent from one view.
///
/// Deliberately *not* used by the emptiness checks that gate destructive
/// confirmations (`mutations.rs`): a directory holding nothing but a `.git` is
/// a directory holding a repository, and deleting it without asking because the
/// tree happens not to draw its contents is a different and much worse defect
/// than a confirmation prompt about something the user cannot see. Hidden means
/// "not shown", never "not there".
pub(super) fn is_always_hidden(name: &OsStr) -> bool {
    ALWAYS_HIDDEN
        .iter()
        .any(|hidden| name == OsStr::new(hidden))
}

/// Whether this service ever enumerates the contents of a directory so named.
///
/// The two reasons it does not are different — hidden from every listing, or
/// shown but not expandable — and every caller that asks has to treat them the
/// same: `expandable: false` and an unenterable path. A *symlinked* directory is
/// a third reason, decided per entry rather than by name, so it stays the
/// caller's own condition.
pub(super) fn is_never_enumerated(name: &OsStr) -> bool {
    is_always_hidden(name)
        || COLLAPSED_DIRECTORIES
            .iter()
            .any(|collapsed| name == OsStr::new(collapsed))
}

pub(super) fn root_token(root: &str) -> anyhow::Result<String> {
    Ok(RootCapability::capture(root)?.token().to_owned())
}

pub(super) fn validate_root_token(root: &str, token: &str) -> anyhow::Result<()> {
    RootCapability::validate(root, token)?;
    Ok(())
}

fn metadata_for_in_root(root: &Path, path: &Path) -> anyhow::Result<v1::FileMetadata> {
    let link_metadata = fs::symlink_metadata(path)?;
    let file_type = link_metadata.file_type();
    let symlink = file_type.is_symlink();
    let kind = if symlink {
        v1::FileKind::Symlink
    } else if link_metadata.is_dir() {
        v1::FileKind::Directory
    } else if link_metadata.is_file() {
        v1::FileKind::File
    } else {
        v1::FileKind::Other
    };
    let entry_name = path.file_name().unwrap_or(path.as_os_str());
    // The raw `OsStr`, not the lossy string: the hidden/collapsed predicate has
    // to see the bytes the filesystem gave, or a name that does not survive
    // UTF-8 replacement is asked about under a different identity than the one
    // `listing.rs` filters on.
    let name = entry_name.to_string_lossy().into_owned();
    let followed = if symlink {
        fs::canonicalize(path)
            .ok()
            .filter(|target| ensure_canonical_within(root, target).is_ok())
            .and_then(|target| fs::metadata(target).ok())
    } else {
        None
    };
    let effective = followed.as_ref().unwrap_or(&link_metadata);
    let symlink_target_kind = followed
        .as_ref()
        .map_or(v1::FileKind::Unspecified, |metadata| {
            if metadata.is_file() {
                v1::FileKind::File
            } else if metadata.is_dir() {
                v1::FileKind::Directory
            } else {
                v1::FileKind::Other
            }
        });
    let mime = image_mime(path).unwrap_or_default().to_owned();
    let collapsed = is_never_enumerated(entry_name) || symlink;
    Ok(v1::FileMetadata {
        path: path.to_string_lossy().into_owned(),
        name,
        kind: kind.into(),
        size: effective.len(),
        modified_unix_millis: effective
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| i64::try_from(value.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or_default(),
        mode: effective.permissions().mode(),
        symlink,
        symlink_target: symlink
            .then(|| fs::read_link(path).ok())
            .flatten()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default(),
        expandable: link_metadata.is_dir() && !collapsed,
        // The *leaf's* identity, never the followed target's, so one path has
        // one generation whichever way it was observed. A directory listing
        // cannot follow a symlink — it resolves entries descriptor-relative and
        // no-follow on purpose — so a followed generation here would disagree
        // with every listing of the same entry and make the editor re-read
        // every symlinked file it opened.
        generation: metadata_generation(&link_metadata),
        mime,
        image_preview_eligible: effective.is_file()
            && image_mime(path).is_some()
            && effective.len() <= MAX_IMAGE_BYTES
            && (!symlink || followed.is_some()),
        symlink_target_kind: symlink_target_kind.into(),
    })
}

fn metadata_for_anchored(
    root: &Path,
    anchored: &Path,
    logical: &Path,
) -> anyhow::Result<v1::FileMetadata> {
    let mut metadata = metadata_for_in_root(root, anchored)?;
    metadata.path = logical.to_string_lossy().into_owned();
    metadata.name = logical
        .file_name()
        .unwrap_or(logical.as_os_str())
        .to_string_lossy()
        .into_owned();
    Ok(metadata)
}

fn metadata_for_directory_entry(
    entry: &AnchoredPath,
    logical: &Path,
) -> anyhow::Result<v1::FileMetadata> {
    let metadata = entry.metadata_no_follow()?;
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
    let collapsed = is_never_enumerated(entry_name) || symlink;
    let mime = image_mime(logical).unwrap_or_default().to_owned();
    Ok(v1::FileMetadata {
        path: logical.to_string_lossy().into_owned(),
        name,
        kind: kind.into(),
        size: metadata.len(),
        modified_unix_millis: metadata.modified_unix_millis(),
        mode: metadata.mode(),
        symlink,
        symlink_target: symlink
            .then(|| entry.read_link().ok())
            .flatten()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default(),
        expandable: metadata.is_dir() && !collapsed,
        generation: metadata.generation(),
        mime,
        image_preview_eligible: metadata.is_file()
            && image_mime(logical).is_some()
            && metadata.len() <= MAX_IMAGE_BYTES,
        // Directory enumeration never follows a link. The target is resolved
        // only when a later authorized operation opens it descriptor-relative.
        symlink_target_kind: v1::FileKind::Unspecified.into(),
    })
}

fn metadata_generation(metadata: &Metadata) -> u64 {
    let mut value = metadata.dev().rotate_left(7) ^ metadata.ino();
    value ^= metadata.len().rotate_left(19);
    value ^= (metadata.mtime() as u64).rotate_left(31);
    value ^= (metadata.mtime_nsec() as u64).rotate_left(43);
    value
}

fn watch_fingerprint(path: &Path) -> anyhow::Result<u64> {
    let metadata = fs::metadata(path)?;
    let mut value = metadata_generation(&metadata);
    // Fold every entry into one scalar. Memory remains bounded while entries
    // beyond the native dirty-set limit still participate in fallback change
    // detection.
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        // An entry that vanished between `read_dir` and the stat is simply not
        // in this fingerprint. Failing the whole registration for it made
        // watching a directory something is actively writing into a coin flip.
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if let Some(entry_value) = watch_entry_fingerprint(&entry.file_name(), &metadata) {
            value = value.wrapping_add(entry_value);
        }
    }
    Ok(value)
}

/// One entry's contribution to a directory's change fingerprint, or `None` for
/// an entry no listing reports.
///
/// A hidden entry that moved the fingerprint would make the fallback poller a
/// second source of the churn the native watcher was just taught to filter: on
/// macOS every folder Finder has opened gains a `.DS_Store` that is rewritten
/// behind the user's back, and each rewrite would publish an authoritative
/// snapshot that the desktop answers with a full re-list — of a directory whose
/// listing does not contain the entry that changed.
fn watch_entry_fingerprint(name: &OsStr, metadata: &Metadata) -> Option<u64> {
    if is_always_hidden(name) {
        return None;
    }
    let name = blake3::hash(name.as_bytes());
    let mut name_bytes = [0_u8; 8];
    name_bytes.copy_from_slice(&name.as_bytes()[..8]);
    Some(u64::from_le_bytes(name_bytes) ^ metadata_generation(metadata).rotate_left(29))
}

fn apply_effective_metadata(item: &mut v1::FileMetadata, metadata: &Metadata) {
    item.size = metadata.len();
    item.modified_unix_millis = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| i64::try_from(value.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default();
    item.mode = metadata.permissions().mode();
    item.generation = metadata_generation(metadata);
}

fn request_path(root: &Path, path: &str) -> anyhow::Result<PathBuf> {
    if path.contains('\0') {
        bail!("path contains a NUL byte");
    }
    let path = Path::new(path);
    let candidate = if path.as_os_str().is_empty() {
        root.to_owned()
    } else if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    };
    if candidate
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("parent traversal is not allowed");
    }
    Ok(candidate)
}

fn ensure_canonical_within(root: &Path, path: &Path) -> anyhow::Result<()> {
    if path == root || path.starts_with(root) {
        Ok(())
    } else {
        bail!("path escapes the active root through a path or symlink")
    }
}

fn reject_root_target(root: &Path, target: &Path) -> anyhow::Result<()> {
    if root == target {
        bail!("the active root itself cannot be mutated");
    }
    Ok(())
}

fn image_mime(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
}

fn validate_token(label: &str, value: &str) -> anyhow::Result<()> {
    if value.is_empty() || value.len() > 256 || value.contains('\0') {
        bail!("invalid {label}");
    }
    Ok(())
}

fn validate_transfer_id(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        bail!("invalid transfer ID: expected 1-128 ASCII letters, digits, '-' or '_'");
    }
    Ok(())
}

fn set_nonblocking(file: &ChildStdout) -> anyhow::Result<()> {
    let fd = file.as_raw_fd();
    // SAFETY: fd belongs to the live ChildStdout and fcntl does not take ownership.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: same live descriptor; only the O_NONBLOCK status flag is added.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
