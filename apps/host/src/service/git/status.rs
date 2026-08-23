use super::*;
use anyhow::Context;
use prost::Message;
use std::{collections::HashSet, sync::atomic::AtomicBool};

use super::path::WorktreeRoot;
use super::runner::{GitMetadataCapability, git_output_cancellable};

/// The parts of a repository that identify it, and nothing else.
///
/// Deliberately not a `v1::GitRepository`: HEAD state is not known at discovery
/// and a protobuf value with three blank fields would be a value that lies.
/// Only `read_status_cancellable`, which reads HEAD authoritatively, builds the
/// wire type.
#[derive(Clone, Debug)]
pub(super) struct RepositoryIdentity {
    pub(super) repository_id: String,
    pub(super) worktree_root: String,
    pub(super) git_dir: Vec<u8>,
    pub(super) common_dir: Vec<u8>,
}

/// Discovers the repository's static identity in one Git process.
///
/// `rev-parse` answers each query in argument order, so the worktree root, the
/// git directory and the common directory arrive together. HEAD and the branch
/// name are deliberately *not* asked for here: `git status --porcelain=v2
/// --branch` already reports both authoritatively, and asking twice made a
/// warm status refresh cost five processes before it read anything.
pub(super) fn discover_repository(
    root: &str,
    logical_root: &str,
    root_identity: (u64, u64),
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<RepositoryIdentity> {
    let [worktree_root, git_dir, common_dir] = repository_paths(root, cancellation)?;
    let worktree_root = String::from_utf8_lossy(&worktree_root).into_owned();
    if worktree_root.as_bytes() != logical_root.as_bytes() {
        bail!("active root is not the requested Git worktree root");
    }
    let git_identity = directory_identity(&git_dir)?;
    let common_identity = directory_identity(&common_dir)?;
    Ok(RepositoryIdentity {
        repository_id: repository_identity(
            logical_root,
            root_identity,
            &git_dir,
            git_identity,
            &common_dir,
            common_identity,
        ),
        worktree_root,
        git_dir,
        common_dir,
    })
}

/// The worktree root, git directory and common directory, absolute.
///
/// One `rev-parse` answers all three, but its output is newline separated and a
/// directory name may contain a newline. When the batched answer is not exactly
/// three lines the query is repeated one at a time, where each answer is the
/// whole of its own output and cannot be mis-split.
fn repository_paths(root: &str, cancellation: Option<&AtomicBool>) -> anyhow::Result<[Vec<u8>; 3]> {
    const QUERIES: [&str; 3] = ["--show-toplevel", "--git-dir", "--git-common-dir"];
    let batched = runner::git_stdout_cancellable(
        root,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new(QUERIES[0]),
            OsStr::new(QUERIES[1]),
            OsStr::new(QUERIES[2]),
        ],
        cancellation,
    )?;
    let lines: Vec<_> = batched
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| trim_one_carriage_return(line.to_vec()))
        .collect();
    if let Ok(paths) = <[Vec<u8>; 3]>::try_from(lines) {
        return Ok(paths);
    }
    let mut separate = Vec::with_capacity(QUERIES.len());
    for query in QUERIES {
        let value = runner::git_stdout_cancellable(
            root,
            &[
                OsStr::new("rev-parse"),
                OsStr::new("--path-format=absolute"),
                OsStr::new(query),
            ],
            cancellation,
        )?;
        separate.push(trim_one_trailing_newline(value));
    }
    <[Vec<u8>; 3]>::try_from(separate)
        .map_err(|_| anyhow::anyhow!("git rev-parse omitted a repository path"))
}

fn directory_identity(path: &[u8]) -> anyhow::Result<(u64, u64)> {
    use std::os::unix::{fs::MetadataExt as _, io::FromRawFd as _};

    let path = std::ffi::CString::new(path).context("Git directory path contains NUL")?;
    // SAFETY: path is a live C string and successful open returns an owned fd.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .context("Git directory capability is unavailable");
    }
    // SAFETY: successful open returned a uniquely owned fd.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

/// The repository identity, derived in exactly one place.
///
/// Discovery computes it and capability validation recomputes it to prove the
/// metadata directories did not change underneath. Two hand-written copies of
/// this derivation that drifted apart would accept a capability describing a
/// different repository, so there is only ever one.
fn repository_identity(
    logical_root: &str,
    root_identity: (u64, u64),
    git_dir: &[u8],
    git_identity: (u64, u64),
    common_dir: &[u8],
    common_identity: (u64, u64),
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(logical_root.as_bytes());
    hasher.update(&root_identity.0.to_le_bytes());
    hasher.update(&root_identity.1.to_le_bytes());
    hasher.update(&[0]);
    hasher.update(git_dir);
    hasher.update(&git_identity.0.to_le_bytes());
    hasher.update(&git_identity.1.to_le_bytes());
    hasher.update(&[0]);
    hasher.update(common_dir);
    hasher.update(&common_identity.0.to_le_bytes());
    hasher.update(&common_identity.1.to_le_bytes());
    hasher.finalize().to_hex().to_string()
}

pub(super) fn validate_metadata_capability(
    logical_root: &str,
    root_identity: (u64, u64),
    repository: &RepositoryIdentity,
    capability: &GitMetadataCapability,
) -> anyhow::Result<()> {
    let (git_identity, common_identity) = capability.identities()?;
    let recomputed = repository_identity(
        logical_root,
        root_identity,
        &repository.git_dir,
        git_identity,
        &repository.common_dir,
        common_identity,
    );
    if recomputed != repository.repository_id {
        bail!("Git metadata directories changed while capturing repository capability");
    }
    Ok(())
}

pub(super) fn read_status_cancellable(
    root: &str,
    identity: &RepositoryIdentity,
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<v1::GitStatusSnapshot> {
    let output = git_output_cancellable(
        root,
        &[
            OsStr::new("status"),
            OsStr::new("--porcelain=v2"),
            OsStr::new("-z"),
            OsStr::new("--branch"),
            OsStr::new("--untracked-files=all"),
            OsStr::new("--ignored=matching"),
        ],
        None,
        cancellation,
    )?;
    ensure_success(&output, "git status")?;
    // Parsed before the truncation check: the branch header is the first thing
    // porcelain writes, so even a bounded snapshot can still say which branch
    // it is bounded for.
    let repository = repository_with_head(identity, &output.stdout);
    if output.stdout_truncated {
        let total_entry_count = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty() && !record.starts_with(b"# "))
            .count() as u64;
        return Ok(v1::GitStatusSnapshot {
            repository: Some(repository),
            source_generation: blake3::hash(&output.stdout).to_hex().to_string(),
            authoritative: false,
            oversized: true,
            total_entry_count,
            error: "Git status output exceeded the bounded control-lane limit; entry count is a lower bound".into(),
            ..Default::default()
        });
    }
    let mut entries = parse_porcelain_v2_z(&output.stdout)?;
    let copy_detection_incomplete = apply_copy_detection(root, &mut entries, cancellation)?;
    let mut binary = binary_paths(root, false, cancellation)?;
    binary.extend(binary_paths(root, true, cancellation)?);
    let worktree = WorktreeRoot::capture(root)?;
    for entry in &mut entries {
        if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            bail!("Git status refresh cancelled");
        }
        // A tracked entry with no worktree mode was absent when Git captured
        // status. Its leaf or any parent may therefore be gone legitimately.
        // Other entries retain the strict parent requirement so a concurrent
        // disappearance still invalidates their snapshot.
        let worktree_entry = if !entry.untracked && !entry.ignored && entry.worktree_mode == 0 {
            worktree.entry_if_parent_exists(&entry.path)?
        } else {
            Some(worktree.entry(&entry.path)?)
        };
        entry.binary = binary.contains(&entry.path);
        if entry.untracked && !entry.binary {
            entry.binary = match worktree_entry.as_ref() {
                Some(current) => current.sample_is_binary()?,
                None => false,
            };
        }
        entry.symlink = entry.index_mode == 0o120000
            || entry.worktree_mode == 0o120000
            || entry.head_mode == 0o120000;
        if !entry.symlink {
            entry.symlink = match worktree_entry.as_ref() {
                Some(current) => current
                    .metadata()?
                    .is_some_and(|metadata| metadata.is_symlink()),
                None => false,
            };
        }
    }
    let mut content_identity = blake3::Hasher::new();
    content_identity.update(&output.stdout);
    for entry in &entries {
        if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            bail!("Git status refresh cancelled");
        }
        content_identity.update(&entry.path);
        content_identity.update(&[0]);
        content_identity.update(&entry.original_path);
        content_identity.update(&[0]);
        // Hashing changed worktree entries makes content edits authoritative
        // even when porcelain XY/mode fields remain exactly the same.
        if entry.worktree_status != "." || entry.untracked || entry.conflicted {
            let worktree_entry = if !entry.untracked && !entry.ignored && entry.worktree_mode == 0 {
                worktree.entry_if_parent_exists(&entry.path)?
            } else {
                Some(worktree.entry(&entry.path)?)
            };
            if let Some(current) = worktree_entry {
                current.hash_identity(&mut content_identity, cancellation)?;
            } else {
                content_identity.update(b"missing\0");
            }
        }
    }
    Ok(bound_status_snapshot(v1::GitStatusSnapshot {
        repository: Some(repository),
        total_entry_count: entries.len() as u64,
        entries,
        authoritative: true,
        copy_detection_incomplete,
        source_generation: content_identity.finalize().to_hex().to_string(),
        ..Default::default()
    }))
}

pub(super) fn bound_status_snapshot(mut snapshot: v1::GitStatusSnapshot) -> v1::GitStatusSnapshot {
    snapshot.total_entry_count = snapshot.entries.len() as u64;
    if snapshot.encoded_len() > MAX_GIT_STATUS_ENCODED {
        snapshot.entries.clear();
        snapshot.authoritative = false;
        snapshot.oversized = true;
        snapshot.error = format!(
            "Git status contains {} entries and exceeds the bounded control-lane limit",
            snapshot.total_entry_count
        );
    }
    snapshot
}

fn binary_paths(
    root: &str,
    cached: bool,
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<HashSet<Vec<u8>>> {
    let mut args = vec![
        OsStr::new("diff"),
        OsStr::new("--numstat"),
        OsStr::new("-z"),
    ];
    if cached {
        args.push(OsStr::new("--cached"));
    }
    let output = git_output_cancellable(root, &args, None, cancellation)?;
    ensure_success(&output, "git diff --numstat")?;
    let records: Vec<_> = output.stdout.split(|byte| *byte == 0).collect();
    let mut paths = HashSet::new();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        index += 1;
        if record.is_empty() {
            continue;
        }
        let mut fields = record.splitn(3, |byte| *byte == b'\t');
        let added = fields.next().unwrap_or_default();
        let deleted = fields.next().unwrap_or_default();
        let path = fields.next().unwrap_or_default();
        if path.is_empty() {
            // Rename/copy numstat records encode an empty path followed by
            // original and destination NUL records.
            index = index.saturating_add(1);
            if let Some(destination) = records.get(index)
                && added == b"-"
                && deleted == b"-"
            {
                paths.insert(destination.to_vec());
            }
            index = index.saturating_add(1);
        } else if added == b"-" && deleted == b"-" {
            paths.insert(path.to_vec());
        }
    }
    Ok(paths)
}

pub(super) fn apply_copy_detection(
    root: &str,
    entries: &mut [v1::GitStatusEntry],
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<bool> {
    const MAX_COPY_DETECTION_ENTRIES: usize = 512;
    let staged_candidates = entries
        .iter()
        .filter(|entry| matches!(entry.index_status.as_str(), "A" | "R" | "C"))
        .count();
    if staged_candidates == 0 {
        return Ok(false);
    }
    if staged_candidates > MAX_COPY_DETECTION_ENTRIES {
        return Ok(true);
    }
    let output = git_output_cancellable(
        root,
        &[
            OsStr::new("diff"),
            OsStr::new("--cached"),
            OsStr::new("--find-copies-harder"),
            OsStr::new("--name-status"),
            OsStr::new("-z"),
        ],
        None,
        cancellation,
    )?;
    ensure_success(&output, "git diff copy detection")?;
    let records: Vec<_> = output.stdout.split(|byte| *byte == 0).collect();
    let mut index = 0;
    while index + 2 < records.len() {
        let status = records[index];
        index += 1;
        if !status.starts_with(b"C") {
            index += if status.starts_with(b"R") { 2 } else { 1 };
            continue;
        }
        let original = records[index];
        let destination = records[index + 1];
        index += 2;
        if let Some(entry) = entries.iter_mut().find(|entry| entry.path == destination) {
            entry.index_status = "C".into();
            entry.original_path = original.to_vec();
            entry.index_kind = v1::GitChangeKind::Copied.into();
        }
    }
    Ok(false)
}

/// The wire repository, with HEAD taken from porcelain's branch header.
fn repository_with_head(identity: &RepositoryIdentity, porcelain: &[u8]) -> v1::GitRepository {
    let mut repository = v1::GitRepository {
        repository_id: identity.repository_id.clone(),
        worktree_root: identity.worktree_root.clone(),
        git_dir: identity.git_dir.clone(),
        common_dir: identity.common_dir.clone(),
        ..Default::default()
    };
    for record in porcelain
        .split(|byte| *byte == 0)
        .filter(|record| record.starts_with(b"# "))
    {
        if let Some(value) = record.strip_prefix(b"# branch.oid ") {
            // Porcelain writes the literal `(initial)` where a repository has no
            // commit yet. That is a state, not an object id.
            repository.initial = value == b"(initial)";
            repository.head_oid = if repository.initial {
                String::new()
            } else {
                String::from_utf8_lossy(value).into_owned()
            };
        } else if let Some(value) = record.strip_prefix(b"# branch.head ") {
            repository.detached_head = value == b"(detached)";
            repository.head_name = if repository.detached_head {
                String::new()
            } else {
                String::from_utf8_lossy(value).into_owned()
            };
        }
    }
    repository
}

/// `rev-parse` output is split on `\n`; only a stray `\r` can remain.
fn trim_one_carriage_return(mut value: Vec<u8>) -> Vec<u8> {
    if value.ends_with(b"\r") {
        value.pop();
    }
    value
}

fn trim_one_trailing_newline(mut value: Vec<u8>) -> Vec<u8> {
    if value.ends_with(b"\n") {
        value.pop();
    }
    trim_one_carriage_return(value)
}
