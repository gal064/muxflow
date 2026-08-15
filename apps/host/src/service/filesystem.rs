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
mod listing;
mod mutations;
mod path_policy;
mod terminal_upload;
pub(crate) use terminal_upload::UploadCommitFailure;
mod watch_service;
use listing::{list_directory_impl, resolve_watch_directory};
use mutations::mutation_metadata;
use path_policy::{AnchoredMetadata, AnchoredPath, RootCapability, descriptor_path};
use watch_service::Watch;

pub(super) const MAX_TEXT_BYTES: u64 = 10 * 1024 * 1024;
pub(super) const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 10_000;
const DEFAULT_DIRECTORY_PAGE: usize = 4096;
const MAX_WATCHES: usize = 128;
const MAX_TRANSFER_CHUNK: usize = 1024 * 1024;

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
    polling_fallback: AtomicBool,
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
            polling_fallback: AtomicBool::new(false),
        }
    }

    fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub(super) fn next_root_generation(&self) -> u64 {
        self.next_generation()
    }
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
        generation: metadata_generation(effective),
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
        let metadata = fs::symlink_metadata(entry.path())?;
        value = value.wrapping_add(watch_entry_fingerprint(&entry.file_name(), &metadata));
    }
    Ok(value)
}

fn watch_entry_fingerprint(name: &std::ffi::OsStr, metadata: &Metadata) -> u64 {
    let name = blake3::hash(name.as_bytes());
    let mut name_bytes = [0_u8; 8];
    name_bytes.copy_from_slice(&name.as_bytes()[..8]);
    u64::from_le_bytes(name_bytes) ^ metadata_generation(metadata).rotate_left(29)
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
mod tests {
    use super::watch_service::{
        FALLBACK_SCAN_ENTRY_BUDGET, FallbackScan, fold_fingerprint_records, precise_file_events,
        scan_fallback_shard_async, scan_fallback_shard_with_limits, watch_matches_events,
    };
    use super::*;

    fn fixture() -> (PathBuf, FileService) {
        #[cfg(target_os = "macos")]
        let temporary_root = Path::new("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let temporary_root = std::env::temp_dir();
        let root = temporary_root.join(format!("ade-files-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        (root, FileService::new())
    }

    /// Three populations, and the difference between them is the whole rule.
    ///
    /// Repository plumbing and platform scratch files are *hidden*: VS Code's
    /// `files.exclude` defaults, which is the explorer this one is compared
    /// against. `node_modules` is *shown and collapsed*, because VS Code shows
    /// it too and people open it on purpose. Every other dotfile — `.env`,
    /// `.gitignore` — is an ordinary file the user edits, and hiding those
    /// would be a different product.
    #[test]
    fn listing_hides_repository_plumbing_keeps_dotfiles_and_collapses_heavy_directories() {
        let (root, service) = fixture();
        fs::write(root.join(".env"), "ok").unwrap();
        fs::write(root.join(".DS_Store"), "noise").unwrap();
        fs::write(root.join("Thumbs.db"), "noise").unwrap();
        for hidden in [".git", ".svn", ".hg", "CVS"] {
            fs::create_dir(root.join(hidden)).unwrap();
        }
        fs::create_dir(root.join("node_modules")).unwrap();
        fs::create_dir(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("linked")).unwrap();
        let snapshot = service
            .list_directory(root.to_str().unwrap(), "", "watch")
            .unwrap();
        let named = |name: &str| snapshot.entries.iter().find(|item| item.name == name);

        assert!(named(".env").is_some(), "ordinary dotfiles stay visible");
        for hidden in [".git", ".svn", ".hg", "CVS", ".DS_Store", "Thumbs.db"] {
            assert!(named(hidden).is_none(), "{hidden} must not be listed");
        }
        for name in ["node_modules", "linked"] {
            assert!(
                !named(name).expect("shown, just not expandable").expandable,
                "{name} stays visible and collapsed"
            );
        }
        // Hidden is not the same as merely absent from one view: the path is
        // refused too, so nothing lists or watches it by asking directly.
        for hidden in [".git", ".svn", ".hg", "CVS", "node_modules"] {
            assert!(
                service
                    .list_directory(root.to_str().unwrap(), hidden, "watch")
                    .is_err(),
                "{hidden} must not be enterable by path"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    /// A hidden entry that consumed a page slot would make a page shorter than
    /// it claims, and one that became the page token would make the next page
    /// resume from a name no client was ever told about.
    #[test]
    fn hidden_entries_do_not_consume_page_slots_or_become_page_tokens() {
        let (root, service) = fixture();
        for name in ["a", "b", "c", "d"] {
            fs::write(root.join(name), name).unwrap();
        }
        for hidden in [".DS_Store", "Thumbs.db"] {
            fs::write(root.join(hidden), "noise").unwrap();
        }
        fs::create_dir(root.join(".git")).unwrap();
        let mut token = String::new();
        let mut names = Vec::new();
        let mut pages = 0;
        loop {
            let page = service
                .list_directory_page(root.to_str().unwrap(), "", "page", &token, 2)
                .unwrap();
            pages += 1;
            // The load-bearing assertion, and the reason it is not merely
            // `len() <= 2`: filtering *after* the page window would fill the
            // window with hidden entries and hand back a short page that still
            // claims more to come. Every page but the last is full.
            assert!(
                page.complete || page.entries.len() == 2,
                "a non-final page must be full, got {} entries",
                page.entries.len()
            );
            names.extend(page.entries.into_iter().map(|entry| entry.name));
            if page.complete {
                break;
            }
            token = page.next_page_token;
        }
        assert_eq!(names, vec!["a", "b", "c", "d"]);
        assert_eq!(pages, 2, "four visible entries at two per page");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_pages_are_bounded_stable_and_complete() {
        let (root, service) = fixture();
        for name in ["c", "a", "e", "b", "d"] {
            fs::write(root.join(name), name).unwrap();
        }
        let mut token = String::new();
        let mut names = Vec::new();
        loop {
            let page = service
                .list_directory_page(root.to_str().unwrap(), "", "page", &token, 2)
                .unwrap();
            assert!(page.entries.len() <= 2);
            names.extend(page.entries.into_iter().map(|entry| entry.name));
            if page.complete {
                break;
            }
            assert!(!page.next_page_token.is_empty());
            token = page.next_page_token;
        }
        assert_eq!(names, ["a", "b", "c", "d", "e"]);
        assert!(
            service
                .list_directory_page(root.to_str().unwrap(), "", "page", "bad", 2)
                .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn root_token_changes_when_same_path_is_replaced() {
        let (root, _service) = fixture();
        let first = root_token(root.to_str().unwrap()).unwrap();
        fs::remove_dir(&root).unwrap();
        fs::create_dir(&root).unwrap();
        let second = root_token(root.to_str().unwrap()).unwrap();
        assert_ne!(first, second);
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn stale_root_token_rejects_same_path_replacement_before_worker_open() {
        let (root, service) = fixture();
        fs::write(root.join("note"), "original").unwrap();
        let token = root_token(root.to_str().unwrap()).unwrap();
        let displaced = root.with_extension("displaced");
        fs::rename(&root, &displaced).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("note"), "replacement").unwrap();
        let error = service
            .read_file_authorized(root.to_str().unwrap(), &token, "note")
            .unwrap_err();
        assert!(error.to_string().contains("root snapshot token"));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(displaced).unwrap();
    }

    #[test]
    fn symlink_escape_and_destructive_confirmations_are_enforced() {
        let (root, service) = fixture();
        let outside = std::env::temp_dir().join(format!("ade-outside-{}", Uuid::new_v4()));
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret"), "no").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        assert!(
            service
                .read_file(root.to_str().unwrap(), "escape/secret")
                .is_err()
        );
        fs::write(root.join("kept"), "inside").unwrap();
        std::os::unix::fs::symlink(root.join("kept"), root.join("inside-link")).unwrap();
        service
            .mutate(&v1::FileServiceRequest {
                operation_id: "delete-link".into(),
                root: root.to_string_lossy().into_owned(),
                path: "inside-link".into(),
                mutation: v1::FileMutationKind::Delete.into(),
                ..Default::default()
            })
            .unwrap();
        assert!(
            root.join("kept").exists(),
            "deleting a symlink must retain its target"
        );
        fs::create_dir(root.join("full")).unwrap();
        fs::write(root.join("full/item"), "x").unwrap();
        let request = v1::FileServiceRequest {
            operation_id: "delete-1".into(),
            root: root.to_string_lossy().into_owned(),
            path: "full".into(),
            mutation: v1::FileMutationKind::Delete.into(),
            ..Default::default()
        };
        assert!(
            service
                .mutate(&request)
                .unwrap_err()
                .to_string()
                .contains("confirmation_required")
        );
        let mut confirmed = request;
        confirmed.non_empty_confirmed = true;
        service.mutate(&confirmed).unwrap();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn text_write_is_atomic_permission_preserving_and_bounded() {
        let (root, service) = fixture();
        let path = root.join("note.md");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        service
            .begin_file_write(root.to_str().unwrap(), "note.md", "write-1", "save-1", 3, 0)
            .unwrap();
        assert_eq!(service.write_file_chunk("write-1", 0, b"ne").unwrap(), 2);
        assert_eq!(service.write_file_chunk("write-1", 2, b"w").unwrap(), 3);
        let metadata = service
            .commit_file_write("write-1", blake3::hash(b"new").to_hex().as_ref())
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(metadata.mode & 0o777, 0o640);
        service
            .begin_file_write(root.to_str().unwrap(), "note.md", "write-2", "save-2", 1, 0)
            .unwrap();
        service.write_file_chunk("write-2", 0, &[0]).unwrap();
        assert!(
            service
                .commit_file_write("write-2", blake3::hash(&[0]).to_hex().as_ref())
                .is_err()
        );
        assert!(!root.join(".tmux-ide-save-write-2.partial").exists());
        service
            .begin_file_write(
                root.to_str().unwrap(),
                "note.md",
                "write-max",
                "save-max",
                MAX_TEXT_BYTES,
                0,
            )
            .unwrap();
        service.cancel_file_write("write-max").unwrap();
        assert!(
            service
                .begin_file_write(
                    root.to_str().unwrap(),
                    "note.md",
                    "write-over",
                    "save-over",
                    MAX_TEXT_BYTES + 1,
                    0,
                )
                .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancelled_file_write_cleans_partial_and_preserves_original() {
        let (root, service) = fixture();
        fs::write(root.join("note.txt"), "original").unwrap();
        service
            .begin_file_write(
                root.to_str().unwrap(),
                "note.txt",
                "cancel-me",
                "save",
                8,
                0,
            )
            .unwrap();
        service
            .write_file_chunk("cancel-me", 0, b"partial")
            .unwrap();
        service.cancel_file_write("cancel-me").unwrap();
        assert_eq!(
            fs::read_to_string(root.join("note.txt")).unwrap(),
            "original"
        );
        assert!(!root.join(".tmux-ide-save-cancel-me.partial").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn editor_commits_are_serialized_last_writer_wins() {
        let (root, service) = fixture();
        fs::write(root.join("note.txt"), "old").unwrap();
        let original = metadata_generation(&fs::metadata(root.join("note.txt")).unwrap());
        for (id, body) in [
            ("writer-a", b"one".as_slice()),
            ("writer-b", b"two".as_slice()),
        ] {
            service
                .begin_file_write(root.to_str().unwrap(), "note.txt", id, id, 3, original)
                .unwrap();
            service.write_file_chunk(id, 0, body).unwrap();
        }
        service
            .commit_file_write("writer-a", blake3::hash(b"one").to_hex().as_ref())
            .unwrap();
        service
            .commit_file_write("writer-b", blake3::hash(b"two").to_hex().as_ref())
            .unwrap();
        assert_eq!(fs::read_to_string(root.join("note.txt")).unwrap(), "two");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_overwrite_restores_the_original_destination() {
        let (root, service) = fixture();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        let _socket =
            std::os::unix::net::UnixListener::bind(source.join("unsupported.sock")).unwrap();
        fs::write(root.join("destination"), "original").unwrap();
        let result = service.mutate(&v1::FileServiceRequest {
            operation_id: "duplicate-failure".into(),
            root: root.to_string_lossy().into_owned(),
            path: "source".into(),
            destination: "destination".into(),
            mutation: v1::FileMutationKind::Duplicate.into(),
            overwrite_confirmed: true,
            ..Default::default()
        });
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(root.join("destination")).unwrap(),
            "original"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn case_only_rename_succeeds_without_overwrite_confirmation_and_reports_truthfully() {
        let (root, service) = fixture();
        fs::write(root.join("Case.txt"), "preserved").unwrap();
        let metadata = service
            .mutate(&v1::FileServiceRequest {
                operation_id: "case-only-rename".into(),
                root: root.to_string_lossy().into_owned(),
                path: "Case.txt".into(),
                destination: "case.txt".into(),
                mutation: v1::FileMutationKind::Rename.into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(metadata.name, "case.txt");
        assert_eq!(metadata.path, root.join("case.txt").to_string_lossy());
        assert_eq!(
            fs::read_to_string(root.join("case.txt")).unwrap(),
            "preserved"
        );
        let names = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, [OsString::from("case.txt")]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn download_stream_verifies_blake3_and_cleans_transfer() {
        let (root, service) = fixture();
        let content = vec![7_u8; 2 * 1024 * 1024 + 17];
        fs::write(root.join("large.bin"), &content).unwrap();
        let descriptor = service
            .start_download(root.to_str().unwrap(), "large.bin", false, "transfer-1", 0)
            .unwrap();
        assert_eq!(descriptor.total_bytes, content.len() as u64);
        let mut offset = 0;
        let mut received = Vec::new();
        let digest = loop {
            let chunk = service
                .read_download_chunk("transfer-1", offset, 128 * 1024)
                .unwrap();
            offset += chunk.data.len() as u64;
            received.extend_from_slice(&chunk.data);
            if chunk.eof {
                break chunk.blake3;
            }
        };
        assert_eq!(received, content);
        assert_eq!(digest, blake3::hash(&content).to_hex().to_string());
        assert!(
            service
                .read_download_chunk("transfer-1", offset, 1)
                .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn transfer_ids_cannot_be_used_as_paths_or_options() {
        let (root, service) = fixture();
        fs::write(root.join("file"), "data").unwrap();
        for invalid in [
            "../escape",
            "nested/id",
            "--checkpoint-action=exec=sh",
            ".",
            "x y",
        ] {
            assert!(
                service
                    .start_download(root.to_str().unwrap(), "file", false, invalid, 0)
                    .is_err()
            );
            assert!(
                service
                    .begin_file_write(root.to_str().unwrap(), "file", invalid, "operation", 4, 0)
                    .is_err()
            );
        }
        assert!(!root.parent().unwrap().join("escape.tar.partial").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn safe_file_symlink_open_and_save_preserve_the_link() {
        let (root, service) = fixture();
        fs::write(root.join("target.txt"), "old").unwrap();
        std::os::unix::fs::symlink("target.txt", root.join("link.txt")).unwrap();
        let content = service
            .read_file(root.to_str().unwrap(), "link.txt")
            .unwrap();
        let metadata = content.metadata.unwrap();
        assert!(metadata.symlink);
        assert_eq!(metadata.symlink_target_kind, v1::FileKind::File as i32);
        service
            .begin_file_write(
                root.to_str().unwrap(),
                "link.txt",
                "symlink-save",
                "save",
                3,
                metadata.generation,
            )
            .unwrap();
        service.write_file_chunk("symlink-save", 0, b"new").unwrap();
        service
            .commit_file_write("symlink-save", blake3::hash(b"new").to_hex().as_ref())
            .unwrap();
        assert_eq!(
            fs::read_link(root.join("link.txt")).unwrap(),
            PathBuf::from("target.txt")
        );
        assert_eq!(fs::read_to_string(root.join("target.txt")).unwrap(), "new");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_generation_rejects_same_size_replacement() {
        let (root, service) = fixture();
        fs::write(root.join("versioned"), "aaaa").unwrap();
        let generation = service
            .read_file(root.to_str().unwrap(), "versioned")
            .unwrap()
            .generation;
        fs::write(root.join("replacement"), "bbbb").unwrap();
        fs::rename(root.join("replacement"), root.join("versioned")).unwrap();
        let error = service
            .start_download(
                root.to_str().unwrap(),
                "versioned",
                false,
                "same-size",
                generation,
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("stale_file_generation"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_duplicate_preserves_dot_git_and_symlinks() {
        let (root, service) = fixture();
        fs::create_dir(root.join("source")).unwrap();
        fs::create_dir(root.join("source/.git")).unwrap();
        fs::write(root.join("source/.git/config"), "kept").unwrap();
        fs::write(root.join("source/file"), "body").unwrap();
        std::os::unix::fs::symlink("file", root.join("source/link")).unwrap();
        service
            .mutate(&v1::FileServiceRequest {
                operation_id: "duplicate-all".into(),
                root: root.to_string_lossy().into_owned(),
                root_token: root_token(root.to_str().unwrap()).unwrap(),
                path: "source".into(),
                destination: "copy".into(),
                mutation: v1::FileMutationKind::Duplicate.into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("copy/.git/config")).unwrap(),
            "kept"
        );
        assert_eq!(
            fs::read_link(root.join("copy/link")).unwrap(),
            PathBuf::from("file")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn timed_out_recursive_duplicate_is_cooperative_and_cleans_partial() {
        let (root, service) = fixture();
        fs::create_dir(root.join("source")).unwrap();
        let body = vec![0x5a; 1024 * 1024];
        for index in 0..64 {
            fs::write(root.join("source").join(index.to_string()), &body).unwrap();
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        let timeout = Arc::clone(&cancellation);
        let timer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            timeout.store(true, Ordering::Release);
        });
        let result = service.mutate_cancellable(
            &v1::FileServiceRequest {
                operation_id: "duplicate-timeout".into(),
                root: root.to_string_lossy().into_owned(),
                root_token: root_token(root.to_str().unwrap()).unwrap(),
                path: "source".into(),
                destination: "copy".into(),
                mutation: v1::FileMutationKind::Duplicate.into(),
                ..Default::default()
            },
            &cancellation,
        );
        timer.join().unwrap();
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        assert!(!root.join("copy").exists());
        assert!(!fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".partial")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disconnected_recursive_delete_leaves_no_app_owned_partial() {
        let (root, service) = fixture();
        fs::create_dir(root.join("victim")).unwrap();
        for index in 0..10_000 {
            fs::write(root.join("victim").join(index.to_string()), b"x").unwrap();
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        let disconnected = Arc::clone(&cancellation);
        let timer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(2));
            disconnected.store(true, Ordering::Release);
        });
        let result = service.mutate_cancellable(
            &v1::FileServiceRequest {
                operation_id: "delete-disconnect".into(),
                root: root.to_string_lossy().into_owned(),
                root_token: root_token(root.to_str().unwrap()).unwrap(),
                path: "victim".into(),
                mutation: v1::FileMutationKind::Delete.into(),
                non_empty_confirmed: true,
                ..Default::default()
            },
            &cancellation,
        );
        timer.join().unwrap();
        if let Err(error) = result {
            assert!(error.to_string().contains("cancelled"));
            assert!(root.join("victim").exists());
        } else {
            assert!(!root.join("victim").exists());
        }
        assert!(!fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".partial")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn folder_archive_is_streamed_and_preserves_explicit_contents() {
        let (root, service) = fixture();
        let folder = "--checkpoint=1";
        fs::create_dir(root.join(folder)).unwrap();
        fs::create_dir(root.join(folder).join(".git")).unwrap();
        fs::write(root.join(folder).join(".git/config"), "kept").unwrap();
        fs::write(root.join(folder).join("file"), "body").unwrap();
        std::os::unix::fs::symlink("file", root.join(folder).join("link")).unwrap();
        let descriptor = service
            .start_download(root.to_str().unwrap(), folder, true, "archive-safe", 0)
            .unwrap();
        assert!(descriptor.folder_archive);
        assert!(!descriptor.total_known);
        let mut archive = Vec::new();
        let mut offset = 0;
        loop {
            let chunk = service
                .read_download_chunk("archive-safe", offset, 64 * 1024)
                .unwrap();
            archive.extend_from_slice(&chunk.data);
            offset += chunk.data.len() as u64;
            if chunk.eof {
                assert!(chunk.total_known);
                assert_eq!(chunk.total_bytes, offset);
                break;
            }
            if chunk.data.is_empty() {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        let archive_path = root.join("result.tar");
        fs::write(&archive_path, archive).unwrap();
        let output = Command::new("tar")
            .args(["-tf"])
            .arg(&archive_path)
            .output()
            .unwrap();
        assert!(output.status.success());
        let listing = String::from_utf8(output.stdout).unwrap();
        assert!(listing.contains(".git/config"));
        assert!(listing.contains("link"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_watch_routing_is_independent_of_directory_entry_count() {
        let root_path = std::env::temp_dir().join(format!("ade-watch-route-{}", Uuid::new_v4()));
        fs::create_dir_all(root_path.join("huge")).unwrap();
        let root = Arc::new(RootCapability::capture(root_path.to_str().unwrap()).unwrap());
        let logical_root = root.logical_root().to_owned();
        let target_directory = root
            .anchor(&logical_root.join("huge"))
            .unwrap()
            .open_directory()
            .unwrap();
        let watch = Watch {
            root_token: root.token().to_owned(),
            root,
            path: logical_root.join("huge").to_string_lossy().into_owned(),
            target: logical_root.join("huge"),
            target_directory: Arc::new(target_directory),
            fallback_scan: Arc::new(Mutex::new(FallbackScan::new(0))),
        };
        let mut event = Event::new(notify::EventKind::Any);
        event.paths.push(watch.target.join("entry-249999"));
        assert!(watch_matches_events(&watch, &[Ok(event)], false));
        assert!(watch_matches_events(&watch, &[], true));
        assert!(!watch_matches_events(&watch, &[], false));
        fs::remove_dir_all(root_path).unwrap();
    }

    #[test]
    fn fallback_watch_registration_and_fingerprint_cover_all_change_shapes() {
        let (root, service) = fixture();
        fs::write(root.join("file"), "one").unwrap();
        let initial = watch_fingerprint(&root).unwrap();
        let snapshot = service
            .watch_directory(root.to_str().unwrap(), "", "fallback")
            .unwrap();
        assert!(snapshot.authoritative);
        assert!(service.polling_fallback.load(Ordering::Acquire));
        fs::write(root.join("file"), "longer").unwrap();
        let edited = watch_fingerprint(&root).unwrap();
        assert_ne!(initial, edited);
        fs::write(root.join("created"), "x").unwrap();
        let created = watch_fingerprint(&root).unwrap();
        assert_ne!(edited, created);
        fs::remove_file(root.join("created")).unwrap();
        assert_ne!(created, watch_fingerprint(&root).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fallback_fingerprint_covers_in_place_edits_beyond_4096_entries() {
        let root = std::env::temp_dir().join(format!("ade-fallback-large-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        for index in 0..=4096 {
            fs::write(root.join(format!("entry-{index:04}")), b"a").unwrap();
        }
        let before = watch_fingerprint(&root).unwrap();
        let scan = Mutex::new(FallbackScan::new(before));
        fs::write(root.join("entry-4096"), b"changed beyond old bound").unwrap();
        assert_ne!(before, watch_fingerprint(&root).unwrap());
        let mut shards = 0;
        loop {
            let shard =
                scan_fallback_shard_with_limits(&root, &scan, 64, Duration::from_secs(1)).unwrap();
            assert!(shard.processed <= 64);
            shards += 1;
            if shard.completed {
                assert!(shard.emit_authoritative);
                break;
            }
        }
        assert!(shards > 64, "the scanner must resume across bounded shards");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fallback_fingerprint_fold_covers_250k_records_with_a_hard_shard_cap() {
        let mut records = (0_u64..250_000).map(|value| value.wrapping_mul(31));
        let mut covered = 0;
        let mut shards = 0;
        loop {
            let (_, processed, completed) = fold_fingerprint_records(
                FALLBACK_SCAN_ENTRY_BUDGET,
                Duration::from_secs(1),
                || Ok(records.next()),
            )
            .unwrap();
            assert!(processed <= FALLBACK_SCAN_ENTRY_BUDGET);
            covered += processed;
            shards += 1;
            if completed {
                break;
            }
        }
        assert_eq!(covered, 250_000);
        assert!(shards > 100);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fallback_filesystem_scan_never_blocks_the_async_control_worker() {
        let root = std::env::temp_dir().join(format!("ade-fallback-latency-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let scan = Arc::new(Mutex::new(FallbackScan::new(
            watch_fingerprint(&root).unwrap(),
        )));
        let held_scan = Arc::clone(&scan);
        let ready = Arc::new(std::sync::Barrier::new(2));
        let held_ready = Arc::clone(&ready);
        let holder = std::thread::spawn(move || {
            let _guard = held_scan.lock().unwrap();
            held_ready.wait();
            std::thread::sleep(Duration::from_millis(100));
        });
        ready.wait();

        let scan_task = tokio::spawn(scan_fallback_shard_async(root.clone(), scan));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), async {
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(1)).await;
            })
            .await
            .is_ok(),
            "the current-thread runtime must stay responsive while filesystem work waits"
        );
        assert!(scan_task.await.unwrap().is_ok());
        holder.join().unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn deleted_file_events_use_absolute_root_and_nested_logical_paths() {
        let root_path = std::env::temp_dir().join(format!("ade-delete-path-{}", Uuid::new_v4()));
        fs::create_dir_all(root_path.join("nested")).unwrap();
        fs::write(root_path.join("removed"), "root").unwrap();
        fs::write(root_path.join("nested/removed"), "nested").unwrap();
        let root = Arc::new(RootCapability::capture(root_path.to_str().unwrap()).unwrap());
        let logical_root = root.logical_root().to_owned();
        let root_directory = root.open_root_directory().unwrap();
        let nested_directory = root
            .anchor(&logical_root.join("nested"))
            .unwrap()
            .open_directory()
            .unwrap();
        let root_watch = Watch {
            root_token: root.token().to_owned(),
            root: Arc::clone(&root),
            path: logical_root.to_string_lossy().into_owned(),
            target: logical_root.clone(),
            target_directory: Arc::new(root_directory),
            fallback_scan: Arc::new(Mutex::new(FallbackScan::new(0))),
        };
        let nested_watch = Watch {
            root_token: root.token().to_owned(),
            root,
            path: logical_root.join("nested").to_string_lossy().into_owned(),
            target: logical_root.join("nested"),
            target_directory: Arc::new(nested_directory),
            fallback_scan: Arc::new(Mutex::new(FallbackScan::new(0))),
        };
        let root_removed = root_path.join("removed");
        let nested_removed = root_path.join("nested/removed");
        fs::remove_file(&root_removed).unwrap();
        fs::remove_file(&nested_removed).unwrap();

        for (watch_id, watch, removed) in [
            ("root", &root_watch, &root_removed),
            ("nested", &nested_watch, &nested_removed),
        ] {
            let mut notify = Event::new(notify::EventKind::Any);
            let logical_removed = watch.target.join(removed.file_name().unwrap());
            notify.paths.push(logical_removed.clone());
            let events = precise_file_events(watch_id, watch, &[Ok(notify)]);
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].scope, logical_removed.to_string_lossy());
            let file = events[0].file.as_ref().unwrap();
            assert!(file.deleted);
            assert_eq!(
                file.metadata.as_ref().unwrap().path,
                logical_removed.to_string_lossy()
            );
        }
        fs::remove_dir_all(root_path).unwrap();
    }

    #[test]
    fn independent_watch_ids_are_reference_counted_per_directory() {
        let (root, service) = fixture();
        service
            .watch_directory(root.to_str().unwrap(), "", "explorer")
            .unwrap();
        service
            .watch_directory(root.to_str().unwrap(), "", "editor-parent")
            .unwrap();
        assert_eq!(service.watches.lock().unwrap().len(), 2);
        service.unwatch_directory("explorer").unwrap();
        assert!(
            service
                .watches
                .lock()
                .unwrap()
                .contains_key("editor-parent")
        );
        service.unwatch_directory("editor-parent").unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn native_watch_emits_precise_file_change_without_polling() {
        let root = std::env::temp_dir().join(format!("ade-watch-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let service = Arc::new(FileService::new());
        let closed = Arc::new(AtomicBool::new(false));
        let overflowed = Arc::new(AtomicBool::new(false));
        let (sender, mut receiver) = mpsc::channel(8);
        let _registration = crate::service::register_control_event_sink(sender.clone());
        service.spawn_watcher(Arc::clone(&closed), sender, overflowed);
        service
            .watch_directory(root.to_str().unwrap(), "", "watch-native")
            .unwrap();
        fs::write(root.join("created"), "event").unwrap();
        let expected = fs::canonicalize(&root)
            .unwrap()
            .join("created")
            .to_string_lossy()
            .into_owned();
        let observed = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(message) = receiver.recv().await {
                if matches!(message, SequencerControl::OrderedEvent(v1::HostEvent {
                    kind,
                    scope,
                    ..
                }) if kind == v1::EventKind::FileChanged as i32 && scope == expected)
                {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        closed.store(true, Ordering::Release);
        assert!(observed);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn native_watch_delete_events_keep_absolute_metadata_for_root_and_nested_paths() {
        let root = std::env::temp_dir().join(format!("ade-watch-delete-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("removed"), "root").unwrap();
        fs::write(root.join("nested/removed"), "nested").unwrap();
        let service = Arc::new(FileService::new());
        let closed = Arc::new(AtomicBool::new(false));
        let overflowed = Arc::new(AtomicBool::new(false));
        let (sender, mut receiver) = mpsc::channel(16);
        let _registration = crate::service::register_control_event_sink(sender.clone());
        service.spawn_watcher(Arc::clone(&closed), sender, overflowed);
        service
            .watch_directory(root.to_str().unwrap(), "", "watch-delete-root")
            .unwrap();
        service
            .watch_directory(root.to_str().unwrap(), "nested", "watch-delete-nested")
            .unwrap();

        let canonical_root = fs::canonicalize(&root).unwrap();
        let expected_root = canonical_root
            .join("removed")
            .to_string_lossy()
            .into_owned();
        let expected_nested = canonical_root
            .join("nested/removed")
            .to_string_lossy()
            .into_owned();
        fs::remove_file(&expected_root).unwrap();
        fs::remove_file(&expected_nested).unwrap();
        let mut expected = BTreeSet::from([expected_root, expected_nested]);
        let observed = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(message) = receiver.recv().await {
                let SequencerControl::OrderedEvent(event) = message else {
                    continue;
                };
                if event.kind != v1::EventKind::FileChanged as i32 {
                    continue;
                }
                let Some(file) = event.file else { continue };
                let Some(metadata) = file.metadata else {
                    continue;
                };
                if file.deleted && metadata.path == event.scope {
                    expected.remove(&metadata.path);
                }
                if expected.is_empty() {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        closed.store(true, Ordering::Release);
        assert!(observed);
        fs::remove_dir_all(root).unwrap();
    }
}
