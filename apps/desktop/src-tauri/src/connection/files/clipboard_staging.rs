use std::{
    ffi::OsString,
    fs::File,
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use serde_json::{Value, json};
use uuid::Uuid;

use super::local_staging::{
    LocalOwnedDirectory, clipboard_cache_path, lock_file, try_lock_file_exclusive,
};

const MAX_PNG_BYTES: u64 = 25 * 1024 * 1024;
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const RETENTION_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const RETENTION_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 8_192;
const MAX_IMAGE_PIXELS: u64 = 16_777_216;

pub(super) fn stage_clipboard_png(png_bytes: &[u8]) -> Result<Value, String> {
    if png_bytes.len() as u64 > MAX_PNG_BYTES {
        return Err("clipboard PNG exceeds the 25 MiB encoded-image limit".into());
    }
    if png_bytes.len() < PNG_SIGNATURE.len() || &png_bytes[..PNG_SIGNATURE.len()] != PNG_SIGNATURE {
        return Err("clipboard image does not have a valid PNG signature".into());
    }
    validate_png_dimensions(png_bytes)?;
    let home = std::env::var_os("HOME").ok_or("HOME is unavailable")?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return Err("HOME must be absolute".into());
    }
    let directory = clipboard_cache_path(&home);
    let staging = LocalOwnedDirectory::open_clipboard_cache(&home)?;
    cleanup_owned_clipboard(&staging)?;
    let id = Uuid::new_v4();
    let file_name = OsString::from(format!("{id}.png"));
    let path = directory.join(&file_name);
    let mut file = staging.create_private(&file_name)?;
    lock_file(&file, libc::LOCK_EX)?;
    let mut written = 0_u64;
    for chunk in png_bytes.chunks(64 * 1024) {
        written = written
            .checked_add(chunk.len() as u64)
            .ok_or("clipboard PNG byte counter overflow")?;
        if written > MAX_PNG_BYTES {
            let _ = staging.unlink(&file_name);
            return Err("clipboard PNG exceeds the 25 MiB encoded-image limit".into());
        }
        file.write_all(chunk).map_err(|error| error.to_string())?;
    }
    file.sync_all().map_err(|error| error.to_string())?;
    if !staging.current_namespace_matches() {
        let _ = staging.unlink(&file_name);
        return Err("clipboard staging directory was replaced while writing".into());
    }
    Ok(json!({
        "path": path,
        "name": clipboard_destination_name(id),
        "sizeBytes": png_bytes.len().to_string(),
        "blake3": blake3::hash(png_bytes).to_hex().to_string(),
    }))
}

fn validate_png_dimensions(png: &[u8]) -> Result<(), String> {
    if png.len() < 24 || &png[12..16] != b"IHDR" {
        return Err("clipboard PNG is missing a valid IHDR header".into());
    }
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or("clipboard PNG pixel count overflow")?;
    if width == 0
        || height == 0
        || width > MAX_IMAGE_DIMENSION
        || height > MAX_IMAGE_DIMENSION
        || pixels > MAX_IMAGE_PIXELS
    {
        return Err("clipboard image exceeds the 8192px or 16,777,216-pixel limit".into());
    }
    Ok(())
}

pub(super) fn lock_owned_source(path: &Path, file: File) -> Result<Option<Arc<File>>, String> {
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(None);
    };
    let expected = clipboard_cache_path(&PathBuf::from(home));
    if path.parent() != Some(expected.as_path())
        || !path
            .file_name()
            .is_some_and(|name| owned_clipboard_name(&name.to_string_lossy()))
    {
        return Ok(None);
    }
    lock_file(&file, libc::LOCK_SH)?;
    Ok(Some(Arc::new(file)))
}

fn cleanup_owned_clipboard(directory: &LocalOwnedDirectory) -> Result<(), String> {
    let now = SystemTime::now();
    let mut total = 0_u64;
    let mut candidates = Vec::new();
    for (name, metadata) in directory.entries()? {
        if !owned_clipboard_name(&name.to_string_lossy())
            || !metadata.regular
            || metadata.symlink
            || metadata.uid != unsafe { libc::geteuid() }
        {
            continue;
        }
        let file = match directory.open_readonly(&name) {
            Ok(file) => file,
            Err(_) => continue,
        };
        if !try_lock_file_exclusive(&file)? {
            continue;
        }
        let opened = file.metadata().map_err(|error| error.to_string())?;
        if opened.dev() != metadata.device || opened.ino() != metadata.inode {
            continue;
        }
        total = total.saturating_add(opened.len());
        candidates.push((
            name,
            opened.len(),
            opened.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            file,
        ));
    }
    candidates.sort_by_key(|(_, _, modified, _)| *modified);
    for (name, size, modified, file) in candidates {
        let old = now.duration_since(modified).unwrap_or_default() >= RETENTION_AGE;
        if !old && total <= RETENTION_BYTES {
            continue;
        }
        let opened = file.metadata().map_err(|error| error.to_string())?;
        if directory.quarantine_and_delete(&name, opened.dev(), opened.ino())? {
            total = total.saturating_sub(size);
        }
    }
    Ok(())
}

fn owned_clipboard_name(name: &str) -> bool {
    name.strip_suffix(".png")
        .is_some_and(|id| Uuid::parse_str(id).is_ok())
}

fn clipboard_destination_name(id: Uuid) -> String {
    format!("clipboard-{id}.png")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        ffi::CString,
        os::unix::fs::{PermissionsExt, symlink},
    };

    #[test]
    fn successive_destinations_are_unique_and_agent_compatible() {
        let first = clipboard_destination_name(Uuid::new_v4());
        let second = clipboard_destination_name(Uuid::new_v4());
        assert_ne!(first, second);
        for name in [first, second] {
            assert!(name.starts_with("clipboard-") && name.ends_with(".png"));
            assert!(
                name.bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
            );
        }
    }

    #[test]
    fn raw_png_hard_limit_is_checked_before_staging_side_effects() {
        let oversized = vec![0_u8; MAX_PNG_BYTES as usize + 1];
        assert_eq!(
            stage_clipboard_png(&oversized).unwrap_err(),
            "clipboard PNG exceeds the 25 MiB encoded-image limit"
        );
    }

    #[test]
    fn png_dimensions_are_bounded_before_staging() {
        let mut header = [0_u8; 24];
        header[..8].copy_from_slice(PNG_SIGNATURE);
        header[12..16].copy_from_slice(b"IHDR");
        header[16..20].copy_from_slice(&2_u32.to_be_bytes());
        header[20..24].copy_from_slice(&3_u32.to_be_bytes());
        validate_png_dimensions(&header).unwrap();
        header[16..20].copy_from_slice(&8_193_u32.to_be_bytes());
        assert!(
            validate_png_dimensions(&header)
                .unwrap_err()
                .contains("8192px")
        );
    }

    #[test]
    fn retention_skips_locked_and_foreign_entries() {
        let directory =
            std::env::temp_dir().join(format!("clipboard-retention-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let staging = LocalOwnedDirectory::open(&directory).unwrap();
        let active_name = OsString::from(format!("{}.png", Uuid::new_v4()));
        let active_path = directory.join(&active_name);
        let active = staging.create_private(&active_name).unwrap();
        active.set_len(RETENTION_BYTES + 1).unwrap();
        lock_file(&active, libc::LOCK_SH).unwrap();
        let stale_name = OsString::from(format!("{}.png", Uuid::new_v4()));
        let stale_path = directory.join(&stale_name);
        staging
            .create_private(&stale_name)
            .unwrap()
            .set_len(RETENTION_BYTES + 1)
            .unwrap();
        let old_name = OsString::from(format!("{}.png", Uuid::new_v4()));
        let old_path = directory.join(&old_name);
        drop(staging.create_private(&old_name).unwrap());
        let old_path_c = CString::new(old_path.as_os_str().as_encoded_bytes()).unwrap();
        let old_times = [libc::timespec {
            tv_sec: 1,
            tv_nsec: 0,
        }; 2];
        assert_eq!(
            unsafe { libc::utimensat(libc::AT_FDCWD, old_path_c.as_ptr(), old_times.as_ptr(), 0) },
            0
        );
        let foreign_target = directory.join("foreign-target");
        std::fs::write(&foreign_target, b"foreign").unwrap();
        let foreign_link = directory.join(format!("{}.png", Uuid::new_v4()));
        symlink(&foreign_target, &foreign_link).unwrap();
        cleanup_owned_clipboard(&staging).unwrap();
        assert!(active_path.exists());
        assert!(!stale_path.exists() && !old_path.exists());
        assert!(foreign_link.is_symlink());
        assert_eq!(std::fs::read(&foreign_target).unwrap(), b"foreign");
        drop(active);
        // An independent process may transiently hold a cooperative lock on a
        // cache entry under full-suite load. Retention is intentionally
        // retryable on each staging pass, so prove bounded eventual cleanup
        // without weakening the locked-entry assertion above.
        for _ in 0..10 {
            cleanup_owned_clipboard(&staging).unwrap();
            if !active_path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!active_path.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn existing_cache_parent_permissions_are_never_changed() {
        let root = std::env::temp_dir().join(format!("cache-parent-mode-{}", Uuid::new_v4()));
        #[cfg(target_os = "macos")]
        let cache = root.join("Library/Caches");
        #[cfg(not(target_os = "macos"))]
        let cache = root.join(".cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _staging = LocalOwnedDirectory::open_clipboard_cache(&root).unwrap();
        assert_eq!(
            std::fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
            0o755
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clipboard_cache_symlink_substitution_preserves_foreign_inode_and_mode() {
        let root = std::env::temp_dir().join(format!("cache-symlink-{}", Uuid::new_v4()));
        #[cfg(target_os = "macos")]
        let cache = root.join("Library/Caches");
        #[cfg(not(target_os = "macos"))]
        let cache = root.join(".cache");
        let foreign = root.join("foreign");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir(&foreign).unwrap();
        std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o755)).unwrap();
        #[cfg(target_os = "macos")]
        let app_name = "dev.dev.tmux-agent-ide";
        #[cfg(not(target_os = "macos"))]
        let app_name = "tmux-agent-ide";
        symlink(&foreign, cache.join(app_name)).unwrap();
        assert!(LocalOwnedDirectory::open_clipboard_cache(&root).is_err());
        assert_eq!(
            std::fs::metadata(&foreign).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(std::fs::read_dir(&foreign).unwrap().next().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
