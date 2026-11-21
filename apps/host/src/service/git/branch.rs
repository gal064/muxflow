//! The branch view of a repository: what the working tree changes against the
//! default branch or against HEAD, the local branches, and switching between
//! them.
//!
//! "The default branch" is the remote's: `origin/HEAD` when the clone recorded
//! it, else `origin/main` or `origin/master`. A repository with none of them
//! has nothing to compare with, which is an answer, not an error.

use std::os::unix::ffi::OsStringExt;

use super::path::WorktreeRoot;
use super::*;

/// Files a summary lists. The totals still count every file.
const MAX_SUMMARY_FILES: usize = 2_000;
/// Branches a list carries.
const MAX_BRANCHES: usize = 500;
/// An untracked file's lines are counted only below this size, and only while
/// the summary has read less than [`UNTRACKED_READ_BUDGET`] in all.
const MAX_UNTRACKED_COUNTED: usize = 1024 * 1024;
const UNTRACKED_READ_BUDGET: usize = 16 * 1024 * 1024;

/// The default branch and where HEAD left it.
pub(super) struct Base {
    /// As Git names it: `origin/main`.
    pub(super) name: String,
    pub(super) merge_base: String,
}

fn stdout_line(output: &GitOutput) -> Option<String> {
    (output.status.success() && output.interrupted.is_none())
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|line| !line.is_empty())
}

fn git(root: &str, args: &[&str], cancellation: Option<&AtomicBool>) -> anyhow::Result<GitOutput> {
    let args: Vec<_> = args.iter().map(OsStr::new).collect();
    git_output_cancellable(root, &args, None, cancellation)
}

/// The remote's default branch, if the repository has one.
fn default_branch(root: &str, cancellation: Option<&AtomicBool>) -> anyhow::Result<Option<String>> {
    let head = git(
        root,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        cancellation,
    )?;
    if let Some(name) = stdout_line(&head).and_then(|line| {
        line.strip_prefix("refs/remotes/")
            .map(str::to_owned)
            .filter(|name| !name.ends_with("/HEAD"))
    }) {
        return Ok(Some(name));
    }
    for name in ["origin/main", "origin/master"] {
        let verified = git(
            root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/remotes/{name}^{{commit}}"),
            ],
            cancellation,
        )?;
        if stdout_line(&verified).is_some() {
            return Ok(Some(name.to_owned()));
        }
    }
    Ok(None)
}

/// The default branch and its merge base with HEAD. `None` when there is no
/// default branch; an error when HEAD shares no history with it.
pub(super) fn resolve_base(
    root: &str,
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<Option<Base>> {
    let Some(name) = default_branch(root, cancellation)? else {
        return Ok(None);
    };
    let merge_base = git(
        root,
        &["merge-base", "HEAD", &format!("refs/remotes/{name}")],
        cancellation,
    )?;
    let Some(merge_base) = stdout_line(&merge_base) else {
        bail!("HEAD shares no history with {name}");
    };
    if !merge_base.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Git returned an invalid merge base");
    }
    Ok(Some(Base { name, merge_base }))
}

/// The merge base a default-branch diff reads against.
pub(super) fn require_base(root: &str, cancellation: Option<&AtomicBool>) -> anyhow::Result<Base> {
    resolve_base(root, cancellation)?
        .ok_or_else(|| anyhow::anyhow!("this repository has no default branch to compare with"))
}

/// `git diff -z` records: `<a>\t<b>\t<path>\0` for numstat, and
/// `<status>\0<path>\0` for name-status.
fn numstat(output: &[u8]) -> HashMap<Vec<u8>, (Option<u32>, Option<u32>)> {
    output
        .split(|byte| *byte == 0)
        .filter_map(|record| {
            let mut fields = record.splitn(3, |byte| *byte == b'\t');
            let (added, removed, path) = (fields.next()?, fields.next()?, fields.next()?);
            let count = |value: &[u8]| std::str::from_utf8(value).ok()?.parse().ok();
            (!path.is_empty()).then(|| (path.to_vec(), (count(added), count(removed))))
        })
        .collect()
}

fn name_status(output: &[u8]) -> Vec<(v1::GitChangeKind, Vec<u8>)> {
    let mut fields = output.split(|byte| *byte == 0);
    let mut files = Vec::new();
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        if path.is_empty() {
            continue;
        }
        let kind = match status.first() {
            Some(b'A') => v1::GitChangeKind::Added,
            Some(b'D') => v1::GitChangeKind::Deleted,
            Some(b'T') => v1::GitChangeKind::TypeChanged,
            Some(b'U') => v1::GitChangeKind::Unmerged,
            _ => v1::GitChangeKind::Modified,
        };
        files.push((kind, path.to_vec()));
    }
    files
}

/// An untracked file's lines and whether it is binary, as Git would count
/// them. Lines are left uncounted past the read budget or the size bound.
fn untracked_lines(root: &WorktreeRoot, path: &[u8], budget: &mut usize) -> (u32, bool) {
    let Some(entry) = root.entry(path).ok() else {
        return (0, false);
    };
    let Some(metadata) = entry.metadata().ok().flatten() else {
        return (0, false);
    };
    if !metadata.is_file() || metadata.is_symlink() {
        return (0, false);
    }
    let size = metadata.len() as usize;
    if size > MAX_UNTRACKED_COUNTED || size > *budget {
        return (0, entry.sample_is_binary().unwrap_or(false));
    }
    let Some(content) = entry.read(Some(MAX_UNTRACKED_COUNTED)).ok().flatten() else {
        return (0, false);
    };
    *budget = budget.saturating_sub(content.len());
    if content[..content.len().min(8192)].contains(&0) {
        return (0, true);
    }
    let lines = content.iter().filter(|byte| **byte == b'\n').count()
        + usize::from(content.last().is_some_and(|byte| *byte != b'\n'));
    (lines.try_into().unwrap_or(u32::MAX), false)
}

/// The last commit, for a summary of what is not committed: its oid, and the
/// tree to diff against, which is the empty tree while HEAD has no commit yet.
fn head_base(root: &str, cancellation: Option<&AtomicBool>) -> anyhow::Result<(String, String)> {
    let head = git(
        root,
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
        cancellation,
    )?;
    if let Some(oid) = stdout_line(&head) {
        return Ok((oid.clone(), oid));
    }
    // Asked of Git, since the empty tree's id depends on the object format.
    let empty = git(
        root,
        &["hash-object", "-t", "tree", "/dev/null"],
        cancellation,
    )?;
    let Some(tree) = stdout_line(&empty) else {
        bail!("Git could not name the empty tree");
    };
    Ok((String::new(), tree))
}

/// What the working tree changes against `base`, untracked files included:
/// the merge base with the default branch, or HEAD, where staged and unstaged
/// changes count alike.
pub(super) fn read_branch_summary(
    root: &str,
    base: v1::GitSummaryBase,
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<v1::GitBranchSummary> {
    let (name, oid, against, action) = match base {
        v1::GitSummaryBase::Head => {
            let (oid, against) = head_base(root, cancellation)?;
            ("HEAD".to_owned(), oid, against, "HEAD")
        }
        v1::GitSummaryBase::Unspecified => {
            let Some(base) = resolve_base(root, cancellation)? else {
                return Ok(v1::GitBranchSummary::default());
            };
            let against = base.merge_base.clone();
            (base.name, base.merge_base, against, "the default branch")
        }
    };
    let diff = |format: &str| -> anyhow::Result<Vec<u8>> {
        let output = git(
            root,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                format,
                "-z",
                &against,
                "--",
            ],
            cancellation,
        )?;
        ensure_success(&output, &format!("git diff against {action}"))?;
        if output.stdout_truncated {
            bail!("the changes against {action} exceed the bounded listing");
        }
        Ok(output.output.stdout)
    };
    let counts = numstat(&diff("--numstat")?);
    let mut files: Vec<v1::GitBranchFile> = name_status(&diff("--name-status")?)
        .into_iter()
        .map(|(kind, path)| {
            // Git counts a binary file as `-`.
            let (added, removed) = counts.get(&path).copied().unwrap_or((Some(0), Some(0)));
            v1::GitBranchFile {
                kind: kind.into(),
                added: added.unwrap_or_default(),
                removed: removed.unwrap_or_default(),
                binary: added.is_none(),
                path,
            }
        })
        .collect();
    let untracked = git(
        root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        cancellation,
    )?;
    ensure_success(&untracked, "list untracked files")?;
    if untracked.stdout_truncated {
        bail!("the untracked files exceed the bounded listing");
    }
    let worktree = WorktreeRoot::capture(root)?;
    let mut budget = UNTRACKED_READ_BUDGET;
    for path in untracked.stdout.split(|byte| *byte == 0) {
        // A nested repository is listed as its directory, which has no lines.
        if path.is_empty() || path.ends_with(b"/") {
            continue;
        }
        if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            bail!("Git branch summary cancelled");
        }
        let (added, binary) = untracked_lines(&worktree, path, &mut budget);
        files.push(v1::GitBranchFile {
            path: path.to_vec(),
            kind: v1::GitChangeKind::Untracked.into(),
            added,
            removed: 0,
            binary,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let (added, removed) = files.iter().fold((0u64, 0u64), |(a, r), file| {
        (a + u64::from(file.added), r + u64::from(file.removed))
    });
    let total_file_count = files.len() as u64;
    files.truncate(MAX_SUMMARY_FILES);
    Ok(v1::GitBranchSummary {
        base_ref: name,
        merge_base_oid: oid,
        files,
        total_file_count,
        added,
        removed,
    })
}

pub(super) fn read_branches(
    root: &str,
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<v1::GitBranchList> {
    let output = git(
        root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(HEAD)%(refname)",
            "refs/heads",
        ],
        cancellation,
    )?;
    ensure_success(&output, "list branches")?;
    let mut list = v1::GitBranchList::default();
    // Ref names hold no control characters, so a line is one ref.
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let (current, name) = match line.strip_prefix('*') {
            Some(name) => (true, name),
            None => (false, line.strip_prefix(' ').unwrap_or(line)),
        };
        let Some(name) = name.strip_prefix("refs/heads/") else {
            continue;
        };
        if current {
            list.current = name.to_owned();
        }
        if list.branches.len() < MAX_BRANCHES {
            list.branches.push(name.to_owned());
        } else {
            list.truncated = true;
        }
    }
    Ok(list)
}

/// Switches to a local branch. The name is checked to be one before Git sees
/// it, so it can never be read as an option or a revision expression.
pub(super) fn switch_branch(
    root: &str,
    branch: &str,
    cancellation: &AtomicBool,
) -> anyhow::Result<GitOutput> {
    if branch.is_empty() || branch.starts_with('-') || branch.bytes().any(|b| b.is_ascii_control())
    {
        bail!("branch name is invalid");
    }
    let reference = OsString::from_vec(format!("refs/heads/{branch}").into_bytes());
    // `show-ref --verify` takes only a full ref name, never an expression.
    let exists = git_output_cancellable(
        root,
        &[
            OsStr::new("show-ref"),
            OsStr::new("--verify"),
            OsStr::new("--quiet"),
            &reference,
        ],
        None,
        Some(cancellation),
    )?;
    if !exists.status.success() {
        bail!("there is no local branch named {branch}");
    }
    git_output_with_deadline(
        root,
        &[
            OsStr::new("switch"),
            OsStr::new("--no-guess"),
            OsStr::new(branch),
        ],
        None,
        Some(cancellation),
        runner::GIT_COMMIT_DEADLINE,
    )
}

impl GitService {
    pub(in crate::service) async fn branch_summary(
        &self,
        request: &v1::GitRequest,
        cancellation: Option<Arc<AtomicBool>>,
    ) -> anyhow::Result<v1::GitBranchSummary> {
        let (_, capabilities) = self.repository(request, cancellation.clone()).await?;
        require_matching_repository(request, &capabilities.identity.repository_id)?;
        let base = request.summary_base();
        tokio::task::spawn_blocking(move || {
            let _guard = capabilities.metadata.install();
            read_branch_summary(&capabilities.stable_root(), base, cancellation.as_deref())
        })
        .await
        .map_err(|error| anyhow::anyhow!("Git branch summary task failed: {error}"))?
    }

    pub(in crate::service) async fn branches(
        &self,
        request: &v1::GitRequest,
        cancellation: Option<Arc<AtomicBool>>,
    ) -> anyhow::Result<v1::GitBranchList> {
        let (_, capabilities) = self.repository(request, cancellation.clone()).await?;
        require_matching_repository(request, &capabilities.identity.repository_id)?;
        tokio::task::spawn_blocking(move || {
            let _guard = capabilities.metadata.install();
            read_branches(&capabilities.stable_root(), cancellation.as_deref())
        })
        .await
        .map_err(|error| anyhow::anyhow!("Git branch list task failed: {error}"))?
    }

    /// A checkout is a command like a commit: decided against a status this
    /// call read itself, under the repository lock, and answered with the one
    /// refresh it produces.
    pub(in crate::service) async fn checkout(
        &self,
        request: v1::GitRequest,
        connection_epoch: u64,
        cancellation: Arc<AtomicBool>,
    ) -> anyhow::Result<v1::GitCommandResult> {
        require_repository_id(&request)?;
        ensure_connection_epoch(&request, connection_epoch)?;
        let (coordinator, capabilities) = self
            .repository(&request, Some(Arc::clone(&cancellation)))
            .await?;
        let _guard = repository_lock(&capabilities.identity.repository_id)
            .lock_owned()
            .await;
        let _mutation = coordinator.begin_mutation();
        if cancellation.load(Ordering::Acquire) {
            bail!("cancelled before checkout");
        }
        let current = coordinator
            .status(
                &capabilities,
                Freshness::Exclusive,
                Some(Arc::clone(&cancellation)),
            )
            .await?;
        validate_status_generation(&request, &current)?;
        let execution_capabilities = Arc::clone(&capabilities);
        let executed = tokio::task::spawn_blocking(move || {
            let _metadata_guard = execution_capabilities.metadata.install();
            let root = execution_capabilities.stable_root();
            let pre_state = command_state(&root, &current);
            let outcome = switch_branch(&root, &request.branch, &cancellation);
            (outcome, pre_state)
        })
        .await
        .map_err(|error| anyhow::anyhow!("Git checkout task failed: {error}"))?;
        let (execution, pre_state) = executed;
        Ok(self
            .reconcile_command(&coordinator, &capabilities, execution, pre_state, false)
            .await)
    }
}
