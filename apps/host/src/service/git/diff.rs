use super::path::{WorktreeRoot, descriptor_path};
use super::runner::git_output_cancellable;
use super::*;
use std::os::unix::{ffi::OsStringExt, io::AsRawFd};

struct DiffContents {
    old: Vec<u8>,
    old_missing: bool,
    new: Vec<u8>,
    new_missing: bool,
    too_large: bool,
}

// A textual replacement can carry old + new once as Monaco contents and a
// second time in the patch. Keeping the source pair below 7 MiB leaves ample
// protobuf/frame headroom under the 16 MiB protocol ceiling.
const MAX_DIFF_SOURCE_PAIR: u64 = 7 * 1024 * 1024;
const MAX_SERIALIZED_DIFF_PAYLOAD: usize = 15 * 1024 * 1024;

/// Combined old + new bytes that may still ride the control lane.
///
/// The control connection also carries every keystroke and every terminal
/// frame. A megabyte of diff body in front of them is a visible typing stall,
/// so anything above this is described in the control response and re-read over
/// the independent bulk connection.
pub(super) const INLINE_DIFF_BODY_LIMIT: usize = 256 * 1024;

/// Who a diff is being read for.
///
/// The two audiences want opposite things, and they are never mixed: a hunk
/// mutation needs the raw patch and cannot defer anything, while the editor
/// needs content and never read the patch at all.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DiffAudience {
    /// The desktop editor. No patch; large bodies are referenced for the bulk
    /// lane rather than sent on the control lane.
    Client,
    /// A hunk mutation re-deriving its own patch, on the host, in process.
    Mutation,
}

impl DiffAudience {
    fn includes_patch(self) -> bool {
        self == Self::Mutation
    }

    /// Combined old + new bytes that may still be inlined in the response.
    fn inline_body_limit(self) -> usize {
        match self {
            Self::Client => INLINE_DIFF_BODY_LIMIT,
            Self::Mutation => usize::MAX,
        }
    }
}

pub(super) fn read_diff(
    root: &str,
    repository: v1::GitRepository,
    request: &v1::GitRequest,
    audience: DiffAudience,
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<v1::GitDiff> {
    validate_git_path(&request.path)?;
    let target = v1::GitDiffTarget::try_from(request.diff_target).unwrap_or_default();
    if target == v1::GitDiffTarget::Unspecified {
        bail!("diff target is required");
    }
    let preclassified_binary = preclassify_binary(root, request, target, cancellation)?;
    if let Some(diff) = bounded_diff_metadata(
        root,
        &repository,
        request,
        target,
        preclassified_binary,
        false,
        cancellation,
    )? {
        return Ok(diff);
    }
    let mut args = vec![
        OsString::from("diff"),
        OsString::from("--no-ext-diff"),
        OsString::from("--no-textconv"),
        OsString::from("--full-index"),
        OsString::from("--binary"),
    ];
    if target == v1::GitDiffTarget::Staged {
        args.push(OsString::from("--cached"));
    }
    args.push(OsString::from("--"));
    if target == v1::GitDiffTarget::Staged
        && !request.original_path.is_empty()
        && request.original_path != request.path
    {
        args.push(OsString::from_vec(request.original_path.clone()));
    }
    args.push(OsString::from_vec(request.path.clone()));
    let refs: Vec<_> = args.iter().map(OsString::as_os_str).collect();
    let output = git_output_cancellable(root, &refs, None, cancellation)?;
    ensure_success(&output, "git diff")?;
    let mut patch = output.output.stdout;
    if patch.is_empty() && target == v1::GitDiffTarget::Unstaged {
        let worktree = WorktreeRoot::capture(root)?;
        let entry = worktree.entry(&request.path)?;
        if let Some(metadata) = entry.metadata()? {
            if metadata.is_symlink() {
                return bounded_diff_metadata(
                    root,
                    &repository,
                    request,
                    target,
                    true,
                    false,
                    cancellation,
                )?
                .ok_or_else(|| anyhow::anyhow!("untracked symlink diff classification failed"));
            }
            let pinned = entry.open_regular_for_git()?;
            let stable = descriptor_path(pinned.as_raw_fd());
            let output = git_output_cancellable(
                root,
                &[
                    OsStr::new("diff"),
                    OsStr::new("--no-index"),
                    OsStr::new("--no-ext-diff"),
                    OsStr::new("--no-textconv"),
                    OsStr::new("--"),
                    OsStr::new("/dev/null"),
                    stable.as_os_str(),
                ],
                None,
                cancellation,
            )?;
            if output.status.success() || output.status.code() == Some(1) {
                patch = rewrite_untracked_patch(&output.stdout, &request.path, metadata.mode());
            }
        }
    }
    let source_generation = blake3::hash(&patch).to_hex().to_string();
    let binary = patch.windows(16).any(|value| value == b"GIT binary patch")
        || patch.windows(13).any(|value| value == b"Binary files ")
        || patch
            .windows(18)
            .any(|value| value == b"Subproject commit ");
    let hunk_count = patch
        .split(|byte| *byte == b'\n')
        .filter(|line| line.starts_with(b"@@ "))
        .count()
        .try_into()
        .unwrap_or(u32::MAX);
    let contents = if binary {
        DiffContents {
            old: Vec::new(),
            old_missing: false,
            new: Vec::new(),
            new_missing: false,
            too_large: false,
        }
    } else {
        read_diff_contents(root, request, target, cancellation)?
    };
    let inlined_patch = if audience.includes_patch() {
        patch.len()
    } else {
        0
    };
    if contents
        .old
        .len()
        .saturating_add(contents.new.len())
        .saturating_add(inlined_patch)
        > MAX_SERIALIZED_DIFF_PAYLOAD
    {
        return bounded_diff_metadata(
            root,
            &repository,
            request,
            target,
            binary,
            true,
            cancellation,
        )?
        .ok_or_else(|| anyhow::anyhow!("Git diff exceeds the serialized frame budget"));
    }
    let bodies = DiffBodies::place(contents.old, contents.new, audience.inline_body_limit());
    Ok(v1::GitDiff {
        repository: Some(repository),
        target: target.into(),
        path: request.path.clone(),
        original_path: request.original_path.clone(),
        display_path: String::from_utf8_lossy(&request.path).into_owned(),
        old_content: bodies.old,
        new_content: bodies.new,
        old_content_ref: bodies.old_ref,
        new_content_ref: bodies.new_ref,
        patch: if audience.includes_patch() {
            patch
        } else {
            Vec::new()
        },
        source_generation,
        binary,
        too_large: contents.too_large,
        old_missing: contents.old_missing,
        new_missing: contents.new_missing,
        hunk_count,
    })
}

/// Where each side of a textual diff travels.
struct DiffBodies {
    old: Vec<u8>,
    new: Vec<u8>,
    old_ref: Option<v1::GitDiffContentRef>,
    new_ref: Option<v1::GitDiffContentRef>,
}

impl DiffBodies {
    fn place(old: Vec<u8>, new: Vec<u8>, inline_limit: usize) -> Self {
        if old.len().saturating_add(new.len()) <= inline_limit {
            return Self {
                old,
                new,
                old_ref: None,
                new_ref: None,
            };
        }
        Self {
            old_ref: content_ref(v1::GitDiffContentSide::Old, &old),
            new_ref: content_ref(v1::GitDiffContentSide::New, &new),
            old: Vec::new(),
            new: Vec::new(),
        }
    }
}

/// Binds a deferred body to its exact bytes. An empty side needs no round trip.
fn content_ref(side: v1::GitDiffContentSide, body: &[u8]) -> Option<v1::GitDiffContentRef> {
    if body.is_empty() {
        return None;
    }
    Some(v1::GitDiffContentRef {
        side: side.into(),
        size: body.len() as u64,
        content_digest: blake3::hash(body).to_hex().to_string(),
    })
}

/// Re-reads one side of a diff for the bulk lane.
///
/// Deliberately the same `read_side` the inline read uses: if the two ever
/// disagreed about where a side comes from, every deferred body would fail the
/// digest check the control response bound it to.
pub(super) fn read_diff_side(
    root: &str,
    request: &v1::GitRequest,
    side: v1::GitDiffContentSide,
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<Vec<u8>> {
    let target = v1::GitDiffTarget::try_from(request.diff_target).unwrap_or_default();
    if target == v1::GitDiffTarget::Unspecified {
        bail!("diff target is required");
    }
    if side == v1::GitDiffContentSide::Unspecified {
        bail!("Git diff content side is required");
    }
    validate_git_path(&request.path)?;
    if !request.original_path.is_empty() {
        // `read_side` resolves a staged rename through `original_path`, so it
        // reaches an object spec exactly as `path` does and is validated the
        // same way. The inline read does this at its own entry point.
        validate_git_path(&request.original_path)?;
    }
    Ok(read_side(root, request, target, side, cancellation)?.0)
}

/// Where one side of a diff comes from. The single source of that truth.
///
/// An unborn HEAD needs no special case: `git show HEAD:path` simply fails and
/// reports the side missing, which is exactly what an initial repository means.
fn read_side(
    root: &str,
    request: &v1::GitRequest,
    target: v1::GitDiffTarget,
    side: v1::GitDiffContentSide,
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<(Vec<u8>, bool)> {
    match (side, target) {
        (v1::GitDiffContentSide::Old, v1::GitDiffTarget::Staged) => git_object(
            root,
            b"HEAD",
            old_object_path(request, target),
            cancellation,
        ),
        (v1::GitDiffContentSide::Old, _) => {
            git_object(root, b":0", old_object_path(request, target), cancellation)
        }
        (v1::GitDiffContentSide::New, v1::GitDiffTarget::Staged) => {
            git_object(root, b":0", &request.path, cancellation)
        }
        (v1::GitDiffContentSide::New, _) => worktree_content(root, &request.path),
        (v1::GitDiffContentSide::Unspecified, _) => bail!("Git diff content side is required"),
    }
}

fn rewrite_untracked_patch(generated: &[u8], path: &[u8], mode: u32) -> Vec<u8> {
    let Some(hunk) = generated
        .split_inclusive(|byte| *byte == b'\n')
        .position(|line| line.starts_with(b"@@ "))
    else {
        return Vec::new();
    };
    let lines: Vec<_> = generated.split_inclusive(|byte| *byte == b'\n').collect();
    let a = quote_patch_path(b"a/", path);
    let b = quote_patch_path(b"b/", path);
    let mut patch = Vec::new();
    patch.extend_from_slice(b"diff --git ");
    patch.extend_from_slice(&a);
    patch.push(b' ');
    patch.extend_from_slice(&b);
    patch.push(b'\n');
    patch.extend_from_slice(format!("new file mode {:06o}\n", mode & 0o777777).as_bytes());
    patch.extend_from_slice(b"--- /dev/null\n+++ ");
    patch.extend_from_slice(&b);
    patch.push(b'\n');
    for line in &lines[hunk..] {
        patch.extend_from_slice(line);
    }
    patch
}

fn quote_patch_path(prefix: &[u8], path: &[u8]) -> Vec<u8> {
    let mut value = Vec::with_capacity(prefix.len() + path.len());
    value.extend_from_slice(prefix);
    value.extend_from_slice(path);
    if value
        .iter()
        .all(|byte| matches!(byte, b'!'..=b'~') && !matches!(byte, b'"' | b'\\'))
    {
        return value;
    }
    let mut quoted = vec![b'"'];
    for byte in value {
        match byte {
            b'"' | b'\\' => {
                quoted.push(b'\\');
                quoted.push(byte);
            }
            b'\n' => quoted.extend_from_slice(b"\\n"),
            b'\r' => quoted.extend_from_slice(b"\\r"),
            b'\t' => quoted.extend_from_slice(b"\\t"),
            b' '..=b'~' => quoted.push(byte),
            _ => quoted.extend_from_slice(format!("\\{byte:03o}").as_bytes()),
        }
    }
    quoted.push(b'"');
    quoted
}

fn bounded_diff_metadata(
    root: &str,
    repository: &v1::GitRepository,
    request: &v1::GitRequest,
    target: v1::GitDiffTarget,
    binary: bool,
    force_too_large: bool,
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<Option<v1::GitDiff>> {
    let old_path = old_object_path(request, target);
    let old_size = if target == v1::GitDiffTarget::Staged {
        object_size(root, b"HEAD", old_path, cancellation)?
    } else {
        object_size(root, b":0", old_path, cancellation)?
    };
    let new_size = if target == v1::GitDiffTarget::Staged {
        object_size(root, b":0", &request.path, cancellation)?
    } else {
        worktree_size(root, &request.path)?
    };
    let source_pair = old_size
        .unwrap_or_default()
        .saturating_add(new_size.unwrap_or_default());
    if !force_too_large
        && !binary
        && source_pair <= MAX_DIFF_SOURCE_PAIR
        && old_size.unwrap_or_default() <= MAX_DIFF_CONTENT as u64
        && new_size.unwrap_or_default() <= MAX_DIFF_CONTENT as u64
    {
        return Ok(None);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(repository.repository_id.as_bytes());
    hasher.update(&request.path);
    hasher.update(&old_size.unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(&new_size.unwrap_or(u64::MAX).to_be_bytes());
    if target == v1::GitDiffTarget::Staged {
        update_object_identity(&mut hasher, root, b"HEAD", old_path, cancellation)?;
        update_object_identity(&mut hasher, root, b":0", &request.path, cancellation)?;
    } else {
        update_object_identity(&mut hasher, root, b":0", old_path, cancellation)?;
        WorktreeRoot::capture(root)?
            .entry(&request.path)?
            .hash_identity(&mut hasher, cancellation)?;
    }
    Ok(Some(v1::GitDiff {
        repository: Some(repository.clone()),
        target: target.into(),
        path: request.path.clone(),
        original_path: request.original_path.clone(),
        display_path: String::from_utf8_lossy(&request.path).into_owned(),
        source_generation: hasher.finalize().to_hex().to_string(),
        binary,
        too_large: force_too_large || !binary,
        old_missing: old_size.is_none(),
        new_missing: new_size.is_none(),
        ..Default::default()
    }))
}

fn preclassify_binary(
    root: &str,
    request: &v1::GitRequest,
    target: v1::GitDiffTarget,
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<bool> {
    let mut args = vec![
        OsString::from("diff"),
        OsString::from("--numstat"),
        OsString::from("-z"),
    ];
    if target == v1::GitDiffTarget::Staged {
        args.push(OsString::from("--cached"));
    }
    args.push(OsString::from("--"));
    if target == v1::GitDiffTarget::Staged
        && !request.original_path.is_empty()
        && request.original_path != request.path
    {
        args.push(OsString::from_vec(request.original_path.clone()));
    }
    args.push(OsString::from_vec(request.path.clone()));
    let refs: Vec<_> = args.iter().map(OsString::as_os_str).collect();
    let output = git_output_cancellable(root, &refs, None, cancellation)?;
    ensure_success(&output, "git diff binary preclassification")?;
    if output.stdout.starts_with(b"-\t-") {
        return Ok(true);
    }
    if output.stdout.is_empty() && target == v1::GitDiffTarget::Unstaged {
        return WorktreeRoot::capture(root)?
            .entry(&request.path)?
            .sample_is_binary();
    }
    Ok(false)
}

fn update_object_identity(
    hasher: &mut blake3::Hasher,
    root: &str,
    prefix: &[u8],
    path: &[u8],
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    let mut spec = prefix.to_vec();
    spec.push(b':');
    spec.extend_from_slice(path);
    let output = git_output_cancellable(
        root,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--verify"),
            OsStr::from_bytes(&spec),
        ],
        None,
        cancellation,
    )?;
    if output.status.success() {
        hasher.update(&output.stdout);
    } else {
        hasher.update(b"missing\0");
    }
    Ok(())
}

fn object_size(
    root: &str,
    prefix: &[u8],
    path: &[u8],
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<Option<u64>> {
    let mut spec = prefix.to_vec();
    spec.push(b':');
    spec.extend_from_slice(path);
    let output = git_output_cancellable(
        root,
        &[
            OsStr::new("cat-file"),
            OsStr::new("-s"),
            OsStr::from_bytes(&spec),
        ],
        None,
        cancellation,
    )?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = std::str::from_utf8(&output.stdout)
        .map_err(|_| anyhow::anyhow!("Git object size was not UTF-8"))?
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("Git object size was invalid"))?;
    Ok(Some(value))
}

fn worktree_size(root: &str, path: &[u8]) -> anyhow::Result<Option<u64>> {
    Ok(WorktreeRoot::capture(root)?
        .entry(path)?
        .metadata()?
        .map(|metadata| metadata.len()))
}

fn read_diff_contents(
    root: &str,
    request: &v1::GitRequest,
    target: v1::GitDiffTarget,
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<DiffContents> {
    let (old, old_missing) = read_side(
        root,
        request,
        target,
        v1::GitDiffContentSide::Old,
        cancellation,
    )?;
    let (new, new_missing) = read_side(
        root,
        request,
        target,
        v1::GitDiffContentSide::New,
        cancellation,
    )?;
    let too_large = old.len() > MAX_DIFF_CONTENT || new.len() > MAX_DIFF_CONTENT;
    Ok(DiffContents {
        old: if too_large { Vec::new() } else { old },
        old_missing,
        new: if too_large { Vec::new() } else { new },
        new_missing,
        too_large,
    })
}

fn old_object_path(request: &v1::GitRequest, target: v1::GitDiffTarget) -> &[u8] {
    if target == v1::GitDiffTarget::Staged && !request.original_path.is_empty() {
        &request.original_path
    } else {
        &request.path
    }
}

fn git_object(
    root: &str,
    prefix: &[u8],
    path: &[u8],
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<(Vec<u8>, bool)> {
    let mut spec = prefix.to_vec();
    spec.push(b':');
    spec.extend_from_slice(path);
    let output = git_output_cancellable(
        root,
        &[OsStr::new("show"), OsStr::from_bytes(&spec)],
        None,
        cancellation,
    )?;
    Ok(if output.status.success() {
        (output.output.stdout, false)
    } else {
        (Vec::new(), true)
    })
}

fn worktree_content(root: &str, path: &[u8]) -> anyhow::Result<(Vec<u8>, bool)> {
    Ok(
        match WorktreeRoot::capture(root)?
            .entry(path)?
            .read(Some(MAX_DIFF_CONTENT))?
        {
            Some(content) => (content, false),
            None => (Vec::new(), true),
        },
    )
}
