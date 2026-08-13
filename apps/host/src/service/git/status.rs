use super::*;
use anyhow::Context;
use prost::Message;
use std::{collections::HashSet, sync::atomic::AtomicBool};

use super::path::WorktreeRoot;
use super::runner::{GitMetadataCapability, git_output_cancellable};

pub(super) fn discover_repository(
    root: &str,
    logical_root: &str,
    root_identity: (u64, u64),
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<v1::GitRepository> {
    let top = runner::git_stdout_cancellable(
        root,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--show-toplevel"),
        ],
        cancellation,
    )?;
    let worktree_root = trim_one_newline(top);
    if worktree_root.as_bytes() != logical_root.as_bytes() {
        bail!("active root is not the requested Git worktree root");
    }
    let git_dir = trim_one_newline(runner::git_stdout_cancellable(
        root,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--git-dir"),
        ],
        cancellation,
    )?)
    .into_bytes();
    let common_dir = trim_one_newline(runner::git_stdout_cancellable(
        root,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--git-common-dir"),
        ],
        cancellation,
    )?)
    .into_bytes();
    let mut hasher = blake3::Hasher::new();
    hasher.update(logical_root.as_bytes());
    hasher.update(&root_identity.0.to_le_bytes());
    hasher.update(&root_identity.1.to_le_bytes());
    hasher.update(&[0]);
    hasher.update(&git_dir);
    let git_identity = directory_identity(&git_dir)?;
    hasher.update(&git_identity.0.to_le_bytes());
    hasher.update(&git_identity.1.to_le_bytes());
    hasher.update(&[0]);
    hasher.update(&common_dir);
    let common_identity = directory_identity(&common_dir)?;
    hasher.update(&common_identity.0.to_le_bytes());
    hasher.update(&common_identity.1.to_le_bytes());
    let head = git_output_cancellable(
        root,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--verify"),
            OsStr::new("HEAD"),
        ],
        None,
        cancellation,
    )?;
    let initial = !head.status.success();
    let head_oid = if initial {
        String::new()
    } else {
        trim_one_newline(head.output.stdout)
    };
    let symbolic = git_output_cancellable(
        root,
        &[
            OsStr::new("symbolic-ref"),
            OsStr::new("--quiet"),
            OsStr::new("--short"),
            OsStr::new("HEAD"),
        ],
        None,
        cancellation,
    )?;
    let detached_head = !initial && !symbolic.status.success();
    let head_name = if symbolic.status.success() {
        trim_one_newline(symbolic.output.stdout)
    } else {
        String::new()
    };
    Ok(v1::GitRepository {
        repository_id: hasher.finalize().to_hex().to_string(),
        worktree_root,
        git_dir,
        common_dir,
        initial,
        detached_head,
        head_name,
        head_oid,
    })
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

pub(super) fn validate_metadata_capability(
    logical_root: &str,
    root_identity: (u64, u64),
    repository: &v1::GitRepository,
    capability: &GitMetadataCapability,
) -> anyhow::Result<()> {
    let (git_identity, common_identity) = capability.identities()?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(logical_root.as_bytes());
    hasher.update(&root_identity.0.to_le_bytes());
    hasher.update(&root_identity.1.to_le_bytes());
    hasher.update(&[0]);
    hasher.update(&repository.git_dir);
    hasher.update(&git_identity.0.to_le_bytes());
    hasher.update(&git_identity.1.to_le_bytes());
    hasher.update(&[0]);
    hasher.update(&repository.common_dir);
    hasher.update(&common_identity.0.to_le_bytes());
    hasher.update(&common_identity.1.to_le_bytes());
    if hasher.finalize().to_hex().as_str() != repository.repository_id {
        bail!("Git metadata directories changed while capturing repository capability");
    }
    Ok(())
}

pub(super) fn read_status_cancellable(
    root: &str,
    mut repository: v1::GitRepository,
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
        entry.binary = binary.contains(&entry.path)
            || (entry.untracked && worktree.entry(&entry.path)?.sample_is_binary()?);
        entry.symlink = entry.index_mode == 0o120000
            || entry.worktree_mode == 0o120000
            || entry.head_mode == 0o120000;
        if !entry.symlink {
            validate_git_path(&entry.path)?;
            entry.symlink = worktree
                .entry(&entry.path)?
                .metadata()?
                .is_some_and(|metadata| metadata.is_symlink());
        }
    }
    for record in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|record| record.starts_with(b"# "))
    {
        if let Some(value) = record.strip_prefix(b"# branch.oid ") {
            repository.initial = value == b"(initial)";
            repository.head_oid = String::from_utf8_lossy(value).into_owned();
        } else if let Some(value) = record.strip_prefix(b"# branch.head ") {
            repository.detached_head = value == b"(detached)";
            repository.head_name = if repository.detached_head {
                String::new()
            } else {
                String::from_utf8_lossy(value).into_owned()
            };
        }
    }
    let mut identity = blake3::Hasher::new();
    identity.update(&output.stdout);
    for entry in &entries {
        if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            bail!("Git status refresh cancelled");
        }
        identity.update(&entry.path);
        identity.update(&[0]);
        identity.update(&entry.original_path);
        identity.update(&[0]);
        // Hashing changed worktree entries makes content edits authoritative
        // even when porcelain XY/mode fields remain exactly the same.
        if entry.worktree_status != "." || entry.untracked || entry.conflicted {
            worktree
                .entry(&entry.path)?
                .hash_identity(&mut identity, cancellation)?;
        }
    }
    Ok(bound_status_snapshot(v1::GitStatusSnapshot {
        repository: Some(repository),
        total_entry_count: entries.len() as u64,
        entries,
        authoritative: true,
        copy_detection_incomplete,
        source_generation: identity.finalize().to_hex().to_string(),
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

fn trim_one_newline(mut value: Vec<u8>) -> String {
    if value.ends_with(b"\n") {
        value.pop();
        if value.ends_with(b"\r") {
            value.pop();
        }
    }
    String::from_utf8_lossy(&value).into_owned()
}
