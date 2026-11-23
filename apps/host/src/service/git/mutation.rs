use super::path::WorktreeRoot;
use super::runner::{
    GIT_MUTATION_DEADLINE, GitOutput, git_output_with_deadline, git_path_cancellable,
    git_paths_cancellable, success_output,
};
use super::status::read_status_cancellable;
use super::*;

pub(super) fn mutate_hunk(
    root: &str,
    repository: &v1::GitRepository,
    request: &v1::GitRequest,
    mutation: v1::GitMutationKind,
    cancellation: &AtomicBool,
) -> anyhow::Result<GitOutput> {
    let current = read_diff(
        root,
        repository.clone(),
        request,
        DiffAudience::Mutation,
        Some(cancellation),
    )?;
    if current.source_generation != request.expected_source_generation {
        bail!("stale source diff generation");
    }
    if current.binary || current.hunk_count == 0 {
        bail!("complete-hunk operations require a textual diff");
    }
    let patch = extract_hunk_patch(&current.patch, request.hunk_index)?;
    if mutation == v1::GitMutationKind::DiscardHunk
        && v1::GitDiffTarget::try_from(request.diff_target).unwrap_or_default()
            == v1::GitDiffTarget::Staged
    {
        return discard_staged_hunk(root, &patch, cancellation);
    }
    let args: &[&str] = match mutation {
        v1::GitMutationKind::StageHunk => {
            &["apply", "--cached", "--recount", "--whitespace=nowarn"]
        }
        v1::GitMutationKind::UnstageHunk => &[
            "apply",
            "--cached",
            "--reverse",
            "--recount",
            "--whitespace=nowarn",
        ],
        v1::GitMutationKind::DiscardHunk => {
            &["apply", "--reverse", "--recount", "--whitespace=nowarn"]
        }
        _ => unreachable!(),
    };
    let refs: Vec<_> = args.iter().map(OsStr::new).collect();
    git_output_with_deadline(
        root,
        &refs,
        Some(&patch),
        Some(cancellation),
        GIT_MUTATION_DEADLINE,
    )
}

fn discard_staged_hunk(
    root: &str,
    patch: &[u8],
    cancellation: &AtomicBool,
) -> anyhow::Result<GitOutput> {
    let worktree_check = [
        OsStr::new("apply"),
        OsStr::new("--reverse"),
        OsStr::new("--check"),
        OsStr::new("--recount"),
        OsStr::new("--whitespace=nowarn"),
    ];
    let index_check = [
        OsStr::new("apply"),
        OsStr::new("--cached"),
        OsStr::new("--reverse"),
        OsStr::new("--check"),
        OsStr::new("--recount"),
        OsStr::new("--whitespace=nowarn"),
    ];
    for (args, action) in [
        (&worktree_check[..], "selected hunk worktree precheck"),
        (&index_check[..], "selected hunk index precheck"),
    ] {
        let checked = git_output_with_deadline(
            root,
            args,
            Some(patch),
            Some(cancellation),
            GIT_MUTATION_DEADLINE,
        )?;
        ensure_success(&checked, action)?;
    }

    let reverse_worktree = [
        OsStr::new("apply"),
        OsStr::new("--reverse"),
        OsStr::new("--recount"),
        OsStr::new("--whitespace=nowarn"),
    ];
    let worktree = git_output_with_deadline(
        root,
        &reverse_worktree,
        Some(patch),
        Some(cancellation),
        GIT_MUTATION_DEADLINE,
    )?;
    ensure_success(&worktree, "discard selected staged hunk from worktree")?;

    let reverse_index = [
        OsStr::new("apply"),
        OsStr::new("--cached"),
        OsStr::new("--reverse"),
        OsStr::new("--recount"),
        OsStr::new("--whitespace=nowarn"),
    ];
    let index_result = git_output_with_deadline(
        root,
        &reverse_index,
        Some(patch),
        Some(cancellation),
        GIT_MUTATION_DEADLINE,
    );
    if let Ok(output) = &index_result
        && ensure_success(output, "discard selected staged hunk from index").is_ok()
    {
        return index_result;
    }

    // Restore the worktree if the second half did not complete. Rollback is
    // deliberately independent of the transport cancellation token.
    let forward_worktree = [
        OsStr::new("apply"),
        OsStr::new("--recount"),
        OsStr::new("--whitespace=nowarn"),
    ];
    let rollback = git_output_with_deadline(
        root,
        &forward_worktree,
        Some(patch),
        None,
        GIT_MUTATION_DEADLINE,
    );
    let rollback_error = match rollback {
        Ok(output) => ensure_success(&output, "rollback staged-hunk worktree change").err(),
        Err(error) => Some(error),
    };
    let operation_error = match index_result {
        Ok(output) => {
            ensure_success(&output, "discard selected staged hunk from index").unwrap_err()
        }
        Err(error) => error,
    };
    if let Some(rollback_error) = rollback_error {
        bail!("{operation_error}; rollback failed: {rollback_error}");
    }
    Err(operation_error)
}

pub(crate) fn extract_hunk_patch(patch: &[u8], requested: u32) -> anyhow::Result<Vec<u8>> {
    let mut header = Vec::new();
    let mut hunks: Vec<Vec<u8>> = Vec::new();
    let mut current = None;
    for line in patch.split_inclusive(|byte| *byte == b'\n') {
        if line.starts_with(b"@@ ") {
            if let Some(value) = current.take() {
                hunks.push(value);
            }
            current = Some(line.to_vec());
        } else if let Some(value) = current.as_mut() {
            value.extend_from_slice(line);
        } else {
            header.extend_from_slice(line);
        }
    }
    if let Some(value) = current {
        hunks.push(value);
    }
    header.extend_from_slice(
        hunks
            .get(requested as usize)
            .ok_or_else(|| anyhow::anyhow!("hunk index is out of range"))?,
    );
    Ok(header)
}

pub(super) fn unstage_file(
    root: &str,
    initial: bool,
    request: &v1::GitRequest,
    cancellation: &AtomicBool,
) -> anyhow::Result<GitOutput> {
    if initial {
        git_path_cancellable(
            root,
            &[b"rm", b"--cached", b"--quiet"],
            &request.path,
            cancellation,
        )
    } else if request.original_path.is_empty() {
        git_path_cancellable(
            root,
            &[b"restore", b"--staged"],
            &request.path,
            cancellation,
        )
    } else {
        git_paths_cancellable(
            root,
            &[b"restore", b"--staged"],
            &[&request.path, &request.original_path],
            Some(cancellation),
        )
    }
}

pub(super) fn discard_file(
    root: &str,
    identity: &RepositoryIdentity,
    initial: bool,
    request: &v1::GitRequest,
    cancellation: &AtomicBool,
) -> anyhow::Result<GitOutput> {
    let target = v1::GitDiffTarget::try_from(request.diff_target).unwrap_or_default();
    let current = read_status_cancellable(root, identity, Some(cancellation))?;
    if target == v1::GitDiffTarget::Staged {
        if initial {
            let output = git_path_cancellable(
                root,
                &[b"rm", b"--cached", b"--quiet"],
                &request.path,
                cancellation,
            )?;
            ensure_success(&output, "git rm --cached")?;
            remove_untracked(root, &request.path)?;
            return Ok(output);
        }
        if current
            .entries
            .iter()
            .find(|entry| entry.path == request.path)
            .is_some_and(|entry| entry.index_status == "A")
        {
            let output = git_path_cancellable(
                root,
                &[b"rm", b"--cached", b"--quiet"],
                &request.path,
                cancellation,
            )?;
            ensure_success(&output, "git rm --cached")?;
            remove_untracked(root, &request.path)?;
            Ok(output)
        } else if request.original_path.is_empty() {
            git_path_cancellable(
                root,
                &[b"restore", b"--source=HEAD", b"--staged", b"--worktree"],
                &request.path,
                cancellation,
            )
        } else {
            git_paths_cancellable(
                root,
                &[b"restore", b"--source=HEAD", b"--staged", b"--worktree"],
                &[&request.path, &request.original_path],
                Some(cancellation),
            )
        }
    } else {
        let tracked = git_path_cancellable(
            root,
            &[b"ls-files", b"--error-unmatch"],
            &request.path,
            cancellation,
        )?;
        if tracked.status.success() {
            git_path_cancellable(
                root,
                &[b"restore", b"--worktree"],
                &request.path,
                cancellation,
            )
        } else {
            remove_untracked(root, &request.path)?;
            Ok(success_output())
        }
    }
}

fn remove_untracked(root: &str, path: &[u8]) -> anyhow::Result<()> {
    WorktreeRoot::capture(root)?.entry(path)?.unlink_file()
}

/// Whether a mutation acts on the whole status rather than one path.
pub(super) fn is_bulk(mutation: v1::GitMutationKind) -> bool {
    matches!(
        mutation,
        v1::GitMutationKind::StageAll
            | v1::GitMutationKind::UnstageAll
            | v1::GitMutationKind::DiscardAll
    )
}

/// An entry a bulk mutation may touch: never a conflict, whose staging would
/// mark it resolved, nor a submodule pointer, nor an ignored file.
fn bulk_eligible(entry: &v1::GitStatusEntry) -> bool {
    !entry.ignored && !entry.conflicted && !entry.submodule
}

fn has_worktree_change(entry: &v1::GitStatusEntry) -> bool {
    entry.untracked || entry.worktree_kind != v1::GitChangeKind::Unspecified as i32
}

fn has_index_change(entry: &v1::GitStatusEntry) -> bool {
    entry.index_kind != v1::GitChangeKind::Unspecified as i32
}

/// The unstaged and untracked entries of `status` that staging, discarding
/// or committing everything acts on: what the Changes list shows. Git names
/// an untracked repository inside the worktree as one directory entry;
/// staging it would add a gitlink and deleting it is not one file's discard,
/// so it is left alone.
pub(super) fn unstaged_entries<'a>(
    root: &str,
    status: &'a v1::GitStatusSnapshot,
) -> anyhow::Result<Vec<&'a v1::GitStatusEntry>> {
    let worktree = WorktreeRoot::capture(root)?;
    let mut entries = Vec::new();
    for entry in &status.entries {
        if !bulk_eligible(entry) || !has_worktree_change(entry) {
            continue;
        }
        if entry.untracked
            && let Some(found) = worktree.entry_if_parent_exists(&entry.path)?
            && found.is_directory()?
        {
            continue;
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// Paths for Git's `--pathspec-from-file=- --pathspec-file-nul`, which the
/// runner's `GIT_LITERAL_PATHSPECS` keeps literal.
fn pathspec_input(paths: &[&[u8]]) -> anyhow::Result<Vec<u8>> {
    let mut input = Vec::new();
    for path in paths {
        validate_git_path(path)?;
        input.extend_from_slice(path);
        input.push(0);
    }
    Ok(input)
}

fn git_with_paths(
    root: &str,
    args: &[&str],
    paths: &[&[u8]],
    cancellation: &AtomicBool,
) -> anyhow::Result<GitOutput> {
    let input = pathspec_input(paths)?;
    let mut refs: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    refs.extend([
        OsStr::new("--pathspec-from-file=-"),
        OsStr::new("--pathspec-file-nul"),
    ]);
    git_output_with_deadline(
        root,
        &refs,
        Some(&input),
        Some(cancellation),
        GIT_MUTATION_DEADLINE,
    )
}

/// Stages every unstaged and untracked path of `status`.
pub(super) fn stage_all(
    root: &str,
    status: &v1::GitStatusSnapshot,
    cancellation: &AtomicBool,
) -> anyhow::Result<GitOutput> {
    let paths: Vec<&[u8]> = unstaged_entries(root, status)?
        .iter()
        .map(|entry| entry.path.as_slice())
        .collect();
    if paths.is_empty() {
        bail!("there are no unstaged changes to stage");
    }
    git_with_paths(root, &["add", "-A"], &paths, cancellation)
}

/// Stage, unstage or discard every eligible entry of `status`, the snapshot
/// this mutation validated its generation against.
pub(super) fn mutate_all(
    root: &str,
    initial: bool,
    status: &v1::GitStatusSnapshot,
    mutation: v1::GitMutationKind,
    cancellation: &AtomicBool,
) -> anyhow::Result<GitOutput> {
    match mutation {
        v1::GitMutationKind::StageAll => stage_all(root, status, cancellation),
        v1::GitMutationKind::UnstageAll => {
            // A rename's original path is restored too, as for one file.
            let paths: Vec<&[u8]> = status
                .entries
                .iter()
                .filter(|entry| bulk_eligible(entry) && has_index_change(entry))
                .flat_map(|entry| {
                    std::iter::once(entry.path.as_slice()).chain(
                        (!entry.original_path.is_empty()).then_some(&entry.original_path[..]),
                    )
                })
                .collect();
            if paths.is_empty() {
                bail!("there are no staged changes to unstage");
            }
            // Without a HEAD, `rm --cached` would refuse any file whose
            // worktree also changed; `-f` with `--cached` never touches it.
            let args: &[&str] = if initial {
                &["rm", "--cached", "--quiet", "-f"]
            } else {
                &["restore", "--staged"]
            };
            git_with_paths(root, args, &paths, cancellation)
        }
        v1::GitMutationKind::DiscardAll => {
            let (untracked, tracked): (Vec<_>, Vec<_>) = unstaged_entries(root, status)?
                .into_iter()
                .partition(|entry| entry.untracked);
            if untracked.is_empty() && tracked.is_empty() {
                bail!("there are no unstaged changes to discard");
            }
            let mut output = success_output();
            if !tracked.is_empty() {
                let paths: Vec<&[u8]> = tracked.iter().map(|entry| entry.path.as_slice()).collect();
                output = git_with_paths(root, &["restore", "--worktree"], &paths, cancellation)?;
                ensure_success(&output, "git restore --worktree")?;
            }
            for entry in untracked {
                remove_untracked(root, &entry.path)?;
            }
            Ok(output)
        }
        _ => bail!("not a bulk Git mutation"),
    }
}
