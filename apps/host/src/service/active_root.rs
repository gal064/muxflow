use std::{
    collections::HashMap,
    fs,
    os::unix::fs::MetadataExt,
    path::Path,
    process::{Command, Stdio},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, bail};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CacheKey {
    server_identity: String,
    pane_id: String,
    cwd: String,
    ancestry_generation: u64,
}

#[derive(Clone)]
struct CacheEntry {
    value: (String, bool),
    inserted: Instant,
}

static ROOT_CACHE: OnceLock<Mutex<HashMap<CacheKey, CacheEntry>>> = OnceLock::new();
const MAX_CACHE_ENTRIES: usize = 512;
const CACHE_TTL: Duration = Duration::from_secs(2);

pub(super) fn resolve_cached(
    server_identity: &str,
    pane_id: &str,
    cwd: &str,
    cancellation: &AtomicBool,
) -> anyhow::Result<(String, bool)> {
    resolve_cached_with(server_identity, pane_id, cwd, |cwd| {
        resolve_git_root(cwd, cancellation)
    })
}

fn resolve_cached_with(
    server_identity: &str,
    pane_id: &str,
    cwd: &str,
    resolver: impl FnOnce(&str) -> anyhow::Result<(String, bool)>,
) -> anyhow::Result<(String, bool)> {
    let canonical = fs::canonicalize(cwd).context("pane working directory is unavailable")?;
    if !canonical.is_dir() {
        bail!("pane working directory is not a directory");
    }
    let canonical = canonical.to_string_lossy().into_owned();
    let key = CacheKey {
        server_identity: server_identity.to_owned(),
        pane_id: pane_id.to_owned(),
        ancestry_generation: ancestry_generation(Path::new(&canonical)),
        cwd: canonical.clone(),
    };
    let mut cache = ROOT_CACHE.get_or_init(Default::default).lock().unwrap();
    cache.retain(|cached, entry| {
        entry.inserted.elapsed() <= CACHE_TTL
            && (cached.server_identity != key.server_identity
                || cached.pane_id != key.pane_id
                || cached.cwd != key.cwd
                || cached.ancestry_generation == key.ancestry_generation)
    });
    if let Some(value) = cache.get(&key).map(|entry| entry.value.clone()) {
        return Ok(value);
    }
    drop(cache);
    let value = resolver(&canonical)?;
    let mut cache = ROOT_CACHE.get_or_init(Default::default).lock().unwrap();
    if cache.len() >= MAX_CACHE_ENTRIES {
        cache.clear();
    }
    cache.insert(
        key,
        CacheEntry {
            value: value.clone(),
            inserted: Instant::now(),
        },
    );
    Ok(value)
}

fn ancestry_generation(mut path: &Path) -> u64 {
    let mut generation = 0_u64;
    if let Ok(metadata) = fs::metadata(path) {
        generation ^= metadata.dev().rotate_left(5);
        generation ^= metadata.ino().rotate_left(13);
        generation ^= (metadata.mtime() as u64).rotate_left(29);
        generation ^= (metadata.mtime_nsec() as u64).rotate_left(47);
    }
    loop {
        let marker = path.join(".git");
        if let Ok(metadata) = fs::symlink_metadata(marker) {
            generation ^= metadata.dev().rotate_left(3);
            generation ^= metadata.ino().rotate_left(17);
            generation ^= (metadata.mtime() as u64).rotate_left(37);
        }
        let Some(parent) = path.parent() else { break };
        if parent == path {
            break;
        }
        path = parent;
    }
    generation
}

fn resolve_git_root(cwd: &str, cancellation: &AtomicBool) -> anyhow::Result<(String, bool)> {
    if cancellation.load(Ordering::Acquire) {
        bail!("cancelled: active-root resolution cancelled");
    }
    let mut child = match Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return Ok((cwd.to_owned(), false)),
    };
    loop {
        if cancellation.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("cancelled: active-root Git probe cancelled");
        }
        if child.try_wait()?.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output()?;
    if output.status.success() {
        let text = String::from_utf8(output.stdout).context("Git root is not valid UTF-8")?;
        let root = fs::canonicalize(text.trim())?;
        if root.is_dir() {
            return Ok((root.to_string_lossy().into_owned(), true));
        }
    }
    Ok((cwd.to_owned(), false))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use uuid::Uuid;

    use super::*;

    #[test]
    fn repeated_pane_cwd_resolution_uses_cache() {
        let cwd = std::env::temp_dir().join(format!("ade-root-cache-{}", Uuid::new_v4()));
        fs::create_dir(&cwd).unwrap();
        let calls = AtomicUsize::new(0);
        let resolve = |cwd: &str| {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok((cwd.to_owned(), false))
        };
        let identity = Uuid::new_v4().to_string();
        resolve_cached_with(&identity, "%1", cwd.to_str().unwrap(), resolve).unwrap();
        resolve_cached_with(&identity, "%1", cwd.to_str().unwrap(), resolve).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        fs::remove_dir(cwd).unwrap();
    }

    #[test]
    fn ancestor_git_marker_add_and_remove_invalidate_cache() {
        let root = std::env::temp_dir().join(format!("ade-root-cache-git-{}", Uuid::new_v4()));
        let cwd = root.join("subdir");
        fs::create_dir_all(&cwd).unwrap();
        let calls = AtomicUsize::new(0);
        let resolve = |cwd: &str| {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok((cwd.to_owned(), false))
        };
        let identity = Uuid::new_v4().to_string();
        resolve_cached_with(&identity, "%2", cwd.to_str().unwrap(), resolve).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        resolve_cached_with(&identity, "%2", cwd.to_str().unwrap(), resolve).unwrap();
        fs::remove_dir(root.join(".git")).unwrap();
        resolve_cached_with(&identity, "%2", cwd.to_str().unwrap(), resolve).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 3);
        fs::remove_dir_all(root).unwrap();
    }
}
