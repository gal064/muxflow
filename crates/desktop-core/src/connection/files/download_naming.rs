//! Choosing the basename a download lands under.
//!
//! A sibling of `local_destination` rather than a child of it: that module is
//! an fd-based atomic-publish state machine, and reaching name arithmetic
//! through it would only move the ownership problem. Two callers share this —
//! the backend's `Rename` collision policy, which asks through a directory
//! descriptor it already holds, and the save panel's suggested default, which
//! has only a path. They differ in nothing else, which is why the walk itself
//! lives here exactly once.

use std::{
    ffi::{CString, OsStr, OsString},
    fs::File,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        io::AsRawFd,
    },
    path::Path,
};

/// The one collision walk: `name`, `name (1)`, `name (2)`, … until something is
/// free, bounded the same way for every caller.
///
/// The two callers differ only in how they ask whether a name is taken — one
/// holds a directory descriptor and one has only a path — so that is the single
/// parameter. Writing the walk twice is how the panel's suggested name and the
/// backend's `Rename` policy would eventually disagree about the bound, the
/// starting index, or which entries count as occupied.
pub(super) fn first_free_name(
    requested: &OsStr,
    name_max: usize,
    mut taken: impl FnMut(&OsStr) -> Result<bool, String>,
) -> Result<OsString, String> {
    if !taken(requested)? {
        return Ok(requested.to_os_string());
    }
    for index in 1..=10_000 {
        let candidate = OsString::from_vec(renamed_name_bytes(requested, index, name_max)?);
        if !taken(&candidate)? {
            return Ok(candidate);
        }
    }
    Err("could not choose a non-colliding destination name".into())
}

/// The name the save panel should open with: the spelling the `Rename`
/// collision policy would eventually produce, applied *before* the panel opens
/// instead of after the transfer.
///
/// Runs the same `first_free_name` walk `choose_name`'s `Rename` policy runs,
/// so the panel's suggestion and the backend's eventual answer cannot drift.
/// It deliberately does not open a directory descriptor: this is a default the
/// user is about to confirm or overrule in the panel, and the authoritative
/// check against a swapped-out parent still happens in
/// `PreparedDestination::open`.
pub(super) fn suggest_non_colliding_name(
    directory: &Path,
    requested: &OsStr,
    reserved: impl Fn(&Path) -> bool,
) -> Result<OsString, String> {
    let bytes = requested.as_bytes();
    if bytes.is_empty()
        || bytes.contains(&b'/')
        || bytes.contains(&0)
        || requested == OsStr::new(".")
        || requested == OsStr::new("..")
    {
        return Err("download name must be a single path component".into());
    }
    let name_max = path_name_max(directory);
    if bytes.len() > name_max {
        return Err("destination basename exceeds filesystem NAME_MAX".into());
    }
    first_free_name(requested, name_max, |candidate| {
        let path = directory.join(candidate);
        // A name already promised to a save panel counts as taken even though
        // nothing occupies it yet: a download in flight holds only its
        // `.partial`, so absence from disk is not evidence that the name is
        // free, and two concurrent downloads of one file would otherwise be
        // offered the same name and silently overwrite each other.
        if reserved(&path) {
            return Ok(true);
        }
        // Any entry at all, symlinks included: the panel should step around a
        // dangling symlink the same way it steps around a file. Only
        // `NotFound` means free — treating `EACCES` or `ELOOP` as free is
        // exactly the "which entries count as occupied" drift the shared walk
        // exists to prevent, and it would suggest a name that is already
        // taken.
        match std::fs::symlink_metadata(&path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(format!(
                "could not inspect the Downloads directory: {error}"
            )),
        }
    })
}

/// `NAME_MAX` for a directory named by path rather than by descriptor. Falls
/// back to the POSIX floor when the filesystem will not say, which only ever
/// makes the suggested name shorter than it had to be.
fn path_name_max(directory: &Path) -> usize {
    const FALLBACK_NAME_MAX: usize = 255;
    let Ok(path) = CString::new(directory.as_os_str().as_bytes()) else {
        return FALLBACK_NAME_MAX;
    };
    // SAFETY: `path` is a live NUL-terminated C string for the duration of the call.
    let value = unsafe { libc::pathconf(path.as_ptr(), libc::_PC_NAME_MAX) };
    usize::try_from(value).unwrap_or(FALLBACK_NAME_MAX).max(1)
}

pub(super) fn directory_name_max(directory: &File) -> Result<usize, String> {
    // SAFETY: the descriptor is a live O_DIRECTORY file descriptor.
    let value = unsafe { libc::fpathconf(directory.as_raw_fd(), libc::_PC_NAME_MAX) };
    if value <= 0 {
        return Err("could not determine destination NAME_MAX".into());
    }
    usize::try_from(value).map_err(|_| "destination NAME_MAX is invalid".into())
}

fn renamed_name_bytes(requested: &OsStr, index: usize, name_max: usize) -> Result<Vec<u8>, String> {
    let path = Path::new(requested);
    let stem = path.file_stem().unwrap_or(requested).as_bytes();
    let suffix = format!(" ({index})");
    if suffix.len() >= name_max {
        return Err("filesystem NAME_MAX is too small for collision suffix".into());
    }
    let extension = path
        .extension()
        .map(OsStr::as_bytes)
        .filter(|value| suffix.len() + 2 + value.len() <= name_max);
    let extension_bytes = extension.map_or(0, |value| value.len() + 1);
    let budget = name_max - suffix.len() - extension_bytes;
    let boundary = if let Ok(text) = std::str::from_utf8(stem) {
        let mut boundary = text.len().min(budget);
        while boundary > 0 && !text.is_char_boundary(boundary) {
            boundary -= 1;
        }
        boundary
    } else {
        stem.len().min(budget)
    };
    if boundary == 0 {
        return Err("destination basename cannot fit collision suffix".into());
    }
    let mut candidate = stem[..boundary].to_vec();
    candidate.extend_from_slice(suffix.as_bytes());
    if let Some(extension) = extension {
        candidate.push(b'.');
        candidate.extend_from_slice(extension);
    }
    debug_assert!(candidate.len() <= name_max);
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use uuid::Uuid;

    #[test]
    fn rename_suffix_respects_name_max_and_utf8_boundaries() {
        let requested = format!("{}.tar.gz", "界".repeat(81));
        let requested = OsStr::new(&requested);
        let candidate = renamed_name_bytes(requested, 1, 255).unwrap();
        assert!(candidate.len() <= 255);
        let candidate = std::str::from_utf8(&candidate).unwrap();
        assert!(candidate.ends_with(" (1).gz"));
    }

    #[test]
    fn suggested_name_walks_past_collisions_using_the_same_rename_spelling() {
        let root = std::env::temp_dir().join(format!("ade-dl-suggest-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();

        // A free name is returned unchanged: the panel must not gratuitously
        // rename a file the user has never downloaded before.
        assert_eq!(
            suggest_non_colliding_name(&root, OsStr::new("report.pdf"), |_| false).unwrap(),
            OsStr::new("report.pdf")
        );

        // Three downloads of the same file, zero prompts.
        fs::write(root.join("report.pdf"), b"one").unwrap();
        assert_eq!(
            suggest_non_colliding_name(&root, OsStr::new("report.pdf"), |_| false).unwrap(),
            OsStr::new("report (1).pdf")
        );
        fs::write(root.join("report (1).pdf"), b"two").unwrap();
        assert_eq!(
            suggest_non_colliding_name(&root, OsStr::new("report.pdf"), |_| false).unwrap(),
            OsStr::new("report (2).pdf")
        );

        // A dangling symlink occupies the name as surely as a file does.
        std::os::unix::fs::symlink(root.join("missing"), root.join("dangling.txt")).unwrap();
        assert_eq!(
            suggest_non_colliding_name(&root, OsStr::new("dangling.txt"), |_| false).unwrap(),
            OsStr::new("dangling (1).txt")
        );

        // The suggestion is a basename in the Downloads directory, never a path:
        // a name carrying a separator or a traversal is refused outright.
        for rejected in ["", "a/b", "..", "."] {
            assert!(
                suggest_non_colliding_name(&root, OsStr::new(rejected), |_| false).is_err(),
                "accepted {rejected:?}"
            );
        }
        assert!(
            suggest_non_colliding_name(&root, OsStr::new(&"n".repeat(4096)), |_| false).is_err()
        );

        // A name promised to an earlier panel is taken even though nothing is
        // on disk under it yet — the in-flight download holds only a
        // `.partial`. Without this, two concurrent downloads of one file are
        // handed the same name and the second publish replaces the first.
        let promised = root.join("inflight.bin");
        assert_eq!(
            suggest_non_colliding_name(&root, OsStr::new("inflight.bin"), |candidate| candidate
                == promised)
            .unwrap(),
            OsStr::new("inflight (1).bin")
        );

        fs::remove_dir_all(&root).ok();
    }
}
