use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};

fn environment(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

pub fn default_runtime_dir() -> PathBuf {
    resolved_runtime_dir(environment, unsafe { libc::geteuid() })
}

fn resolved_runtime_dir(
    environment: impl Fn(&str) -> Option<OsString>,
    uid: libc::uid_t,
) -> PathBuf {
    if let Some(path) = environment("ADE_HOST_RUNTIME_DIR") {
        return PathBuf::from(path);
    }
    if let Some(path) = environment("XDG_RUNTIME_DIR") {
        return PathBuf::from(path).join("tmux-agent-ide");
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = environment("HOME") {
        return PathBuf::from(home).join("Library/Caches/dev.dev.tmux-agent-ide/runtime");
    }
    PathBuf::from(format!("/tmp/tmux-agent-ide-{uid}"))
}

pub fn default_socket_path() -> PathBuf {
    default_runtime_dir().join("host.sock")
}

/// The directory this process is actually using.
///
/// The daemon's is decided by its socket's parent, which `--socket` can put
/// somewhere the environment would never resolve — and its state did not follow
/// it there: `agents.json`, the session order and the fallback mailbox all
/// asked the environment again, independently. That is the same class of split
/// as M13-E003, one process wide instead of two processes wide. The daemon
/// adopts its directory once, at startup, and everything below it agrees by
/// construction.
static ADOPTED_RUNTIME_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn adopt_runtime_dir(runtime: &Path) {
    let _ = ADOPTED_RUNTIME_DIR.set(runtime.to_path_buf());
}

pub fn runtime_dir() -> PathBuf {
    ADOPTED_RUNTIME_DIR
        .get()
        .cloned()
        .unwrap_or_else(default_runtime_dir)
}

/// Where the daemon records the runtime directory it actually chose.
///
/// M13-E003: a hook and the daemon resolved the runtime directory from
/// `XDG_RUNTIME_DIR`, which is *not* the same in the two contexts that matter.
/// The desktop starts the daemon over a non-interactive `ssh` command, where
/// systemd's user environment is not applied and the variable is unset, so the
/// daemon lived in `/tmp/tmux-agent-ide-<uid>`. The hooks run inside the user's
/// tmux server, whose global environment carries `XDG_RUNTIME_DIR`, so
/// `hook ingest` looked in `/run/user/<uid>/tmux-agent-ide`, found no socket,
/// and wrote its events to a fallback mailbox in that other directory that no
/// daemon has ever read. Every lifecycle event on the field machine was
/// delivered correctly and filed somewhere nobody was listening.
///
/// `HOME` is the one variable that *is* the same in both contexts — an ssh
/// command, a login shell and a tmux pane all agree on it — so the daemon
/// leaves a pointer under it and the hook reads it. Deliberately not removed on
/// shutdown: a stopped daemon's last directory is also the right place to leave
/// a fallback event for it to find when it comes back.
fn runtime_pointer_path(environment: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let home = PathBuf::from(environment("HOME")?);
    if cfg!(target_os = "macos") {
        Some(home.join("Library/Caches/dev.dev.tmux-agent-ide/daemon-runtime-dir"))
    } else {
        Some(home.join(".local/state/tmux-agent-ide/daemon-runtime-dir"))
    }
}

pub fn record_runtime_dir(runtime: &Path) -> anyhow::Result<()> {
    publish_runtime_dir(runtime, environment, unsafe { libc::geteuid() })
}

fn publish_runtime_dir(
    runtime: &Path,
    environment: impl Fn(&str) -> Option<OsString> + Copy,
    uid: libc::uid_t,
) -> anyhow::Result<()> {
    if !placed_by_environment(runtime, environment, uid) {
        return Ok(());
    }
    let Some(pointer) = runtime_pointer_path(environment) else {
        return Ok(());
    };
    let parent = pointer.parent().context("runtime pointer has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".daemon-runtime-dir-{}", std::process::id()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(runtime.as_os_str().as_encoded_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, &pointer)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Where a daemon on this machine could be, best first — normally two entries.
///
/// This process's own directory leads, because a daemon sitting in it is
/// unambiguously the one this process belongs to. The recorded pointer comes
/// next and *ends* the list: a daemon that published where it is has answered
/// the question, and reaching past that answer into directories nobody claimed
/// is how a process starts touching files it does not own. The guesses below
/// are for one case only — no daemon on this machine has published yet — which
/// after this change means a helper old enough to predate the pointer.
///
/// An explicit `ADE_HOST_RUNTIME_DIR` is answered exactly and alone: it is how
/// every test fixture isolates itself, and a fixture that fell back to a
/// neighbouring directory would talk to the developer's real daemon.
pub fn runtime_dir_candidates() -> Vec<PathBuf> {
    candidate_runtime_dirs(environment, unsafe { libc::geteuid() })
}

fn candidate_runtime_dirs(
    environment: impl Fn(&str) -> Option<OsString> + Copy,
    uid: libc::uid_t,
) -> Vec<PathBuf> {
    let mut candidates = vec![resolved_runtime_dir(environment, uid)];
    if environment("ADE_HOST_RUNTIME_DIR").is_some() {
        return candidates;
    }
    if let Some(recorded) = recorded_runtime_dir(environment) {
        add_unique(&mut candidates, recorded);
        return candidates;
    }
    if let Some(path) = environment("XDG_RUNTIME_DIR") {
        add_unique(&mut candidates, PathBuf::from(path).join("tmux-agent-ide"));
    }
    // Both platform defaults, not only this platform's: `XDG_RUNTIME_DIR` set
    // on a Mac takes the resolution above away from the cache directory, and a
    // daemon that had it unset is then unreachable with nothing to fall back
    // on — the asymmetry Linux does not have.
    if let Some(home) = environment("HOME") {
        add_unique(
            &mut candidates,
            PathBuf::from(home).join("Library/Caches/dev.dev.tmux-agent-ide/runtime"),
        );
    }
    add_unique(
        &mut candidates,
        PathBuf::from(format!("/run/user/{uid}/tmux-agent-ide")),
    );
    add_unique(
        &mut candidates,
        PathBuf::from(format!("/tmp/tmux-agent-ide-{uid}")),
    );
    candidates
}

/// Whether `runtime` is where this process's own environment places a daemon,
/// rather than somewhere it was pinned — by `ADE_HOST_RUNTIME_DIR` or by an
/// explicit `--socket`.
///
/// The one predicate that decides whether a process owns the shared pointer
/// under `HOME`. Every test fixture on this machine pins its directory one of
/// those two ways while inheriting the developer's real `HOME`: a pinned
/// process that *wrote* the pointer would send the user's hooks to a directory
/// that is deleted when the lane ends, and one that *read* it would sweep — and
/// `consume` deletes — the user's own pending events. Stated once, because it
/// was stated twice and omitted on the read.
fn placed_by_environment(
    runtime: &Path,
    environment: impl Fn(&str) -> Option<OsString> + Copy,
    uid: libc::uid_t,
) -> bool {
    environment("ADE_HOST_RUNTIME_DIR").is_none()
        && runtime == resolved_runtime_dir(environment, uid)
}

/// The directory a daemon last published, for a process entitled to read it.
pub fn published_runtime_dir() -> Option<PathBuf> {
    let uid = unsafe { libc::geteuid() };
    placed_by_environment(&runtime_dir(), environment, uid)
        .then(|| recorded_runtime_dir(environment))
        .flatten()
}

/// Read back as the bytes it was written as. A path is not text: decoding it
/// lossily produces a path that exists nowhere, and trimming it corrupts the
/// legal ones that end in a space.
fn recorded_runtime_dir(environment: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let recorded = fs::read(runtime_pointer_path(environment)?).ok()?;
    (!recorded.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&recorded).to_owned()))
}

fn add_unique(candidates: &mut Vec<PathBuf>, path: PathBuf) {
    if !candidates.contains(&path) {
        candidates.push(path);
    }
}

/// Where an event goes when no daemon answered anywhere.
///
/// A directory that has held a daemon is worth more than the one this process
/// would have picked: the daemon that comes back is the one that reads it.
pub fn fallback_runtime_dir(candidates: &[PathBuf]) -> PathBuf {
    candidates
        .iter()
        .find(|candidate| candidate.join("daemon.json").exists())
        .or_else(|| candidates.iter().find(|candidate| candidate.is_dir()))
        .or_else(|| candidates.first())
        .cloned()
        .unwrap_or_else(default_runtime_dir)
}

/// A Unix socket bind must fit `sockaddr_un::sun_path` including its
/// terminator: 104 bytes on Darwin, 108 on Linux. The default runtime
/// directory is short, but `ADE_HOST_RUNTIME_DIR` and `XDG_RUNTIME_DIR` can
/// both point somewhere deep, and the kernel's own refusal
/// (`path must be shorter than SUN_LEN`) never named the limit or the length.
/// This is the same bound the desktop already enforces for SSH control sockets
/// (M10-E040); M10-E058 is it going unchecked on the daemon's own socket.
const SOCKET_PATH_BYTES: usize = if cfg!(target_os = "macos") { 104 } else { 108 };

pub fn check_socket_path_length(path: &Path) -> anyhow::Result<()> {
    let length = path.as_os_str().as_encoded_bytes().len();
    if length >= SOCKET_PATH_BYTES {
        bail!(
            "daemon socket path is {length} bytes, but this platform allows at most \
             {} including the terminator: {}. Set ADE_HOST_RUNTIME_DIR to a shorter directory.",
            SOCKET_PATH_BYTES - 1,
            path.display()
        );
    }
    Ok(())
}

pub fn prepare_runtime_dir(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("runtime path is not a real directory: {}", path.display());
        }
    } else {
        fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    let mode = fs::metadata(path)?.permissions().mode() & 0o777;
    if mode != 0o700 {
        bail!("runtime directory must be mode 0700, got {mode:04o}");
    }
    Ok(())
}

pub fn ensure_private_socket(path: &Path) -> anyhow::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() {
        bail!("daemon endpoint is not a Unix socket: {}", path.display());
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o600 {
        bail!("daemon socket must be mode 0600, got {mode:04o}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_within_the_platform_bound_is_accepted() {
        let path = PathBuf::from(format!(
            "/tmp/{}/host.sock",
            "a".repeat(SOCKET_PATH_BYTES - "/tmp//host.sock".len() - 1)
        ));
        assert_eq!(
            path.as_os_str().as_encoded_bytes().len(),
            SOCKET_PATH_BYTES - 1
        );
        check_socket_path_length(&path).unwrap();
    }

    #[test]
    fn socket_path_at_or_over_the_bound_is_refused_with_the_limit_and_length() {
        let path = PathBuf::from(format!(
            "/tmp/{}/host.sock",
            "a".repeat(SOCKET_PATH_BYTES - "/tmp//host.sock".len())
        ));
        let error = check_socket_path_length(&path).unwrap_err().to_string();
        assert!(error.contains(&SOCKET_PATH_BYTES.to_string()), "{error}");
        assert!(error.contains("ADE_HOST_RUNTIME_DIR"), "{error}");
    }

    #[test]
    fn runtime_directory_is_private() {
        let root = std::env::temp_dir().join(format!("ade-path-test-{}", uuid::Uuid::new_v4()));
        prepare_runtime_dir(&root).unwrap();
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir(root).unwrap();
    }

    /// The exact split that lost every hook event on the field machine: the
    /// daemon was started over `ssh` with no `XDG_RUNTIME_DIR`, the hooks ran
    /// inside a tmux server that had one, and the two never named the same
    /// directory. The candidate list has to close the gap from *either* side.
    #[test]
    fn a_hook_with_xdg_set_still_reaches_a_daemon_started_without_it() {
        let home = std::env::temp_dir().join(format!("ade-pointer-{}", uuid::Uuid::new_v4()));
        let daemon_runtime = home.join("daemon-runtime");
        fs::create_dir_all(&daemon_runtime).unwrap();
        let hook_environment = |name: &str| match name {
            "HOME" => Some(OsString::from(home.as_os_str())),
            "XDG_RUNTIME_DIR" => Some(OsString::from("/run/user/4242")),
            _ => None,
        };

        // Before any daemon has published: the directories one could have
        // chosen, on either platform, because a hook cannot know which of them
        // a helper too old to publish picked.
        let guesses = candidate_runtime_dirs(hook_environment, 4242);
        assert_eq!(
            guesses.first(),
            Some(&PathBuf::from("/run/user/4242/tmux-agent-ide")),
            "this process's own resolution still comes first"
        );
        for guess in ["/tmp/tmux-agent-ide-4242", "/run/user/4242/tmux-agent-ide"] {
            assert!(guesses.contains(&PathBuf::from(guess)), "{guesses:?}");
        }
        assert!(guesses.contains(&home.join("Library/Caches/dev.dev.tmux-agent-ide/runtime")));

        // And once a daemon has: its answer, and nothing else. The guesses stop
        // — a directory nobody claimed is one this process must not connect to,
        // file events in, or (as the daemon) delete from.
        let pointer = runtime_pointer_path(hook_environment).unwrap();
        fs::create_dir_all(pointer.parent().unwrap()).unwrap();
        fs::write(&pointer, daemon_runtime.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(
            candidate_runtime_dirs(hook_environment, 4242),
            vec![
                PathBuf::from("/run/user/4242/tmux-agent-ide"),
                daemon_runtime
            ],
            "the recorded directory must end the list"
        );
        fs::remove_dir_all(home).unwrap();
    }

    /// A fixture must not publish. Every lane in `tests/` pins its runtime
    /// directory — with `ADE_HOST_RUNTIME_DIR` or an explicit `--socket` — while
    /// inheriting the developer's real `HOME`, so a daemon that published
    /// whatever directory it was given would point that developer's own hooks
    /// at a directory that is deleted when the lane ends. This one was found
    /// after it had already happened on the machine the fix was written on.
    #[test]
    fn only_a_daemon_this_environment_placed_itself_publishes_where_it_is() {
        let home = std::env::temp_dir().join(format!("ade-publish-{}", uuid::Uuid::new_v4()));
        let fixture = home.join("fixture-runtime");
        fs::create_dir_all(&home).unwrap();
        let unpinned = |name: &str| match name {
            "HOME" => Some(OsString::from(home.as_os_str())),
            "XDG_RUNTIME_DIR" => Some(OsString::from(home.as_os_str())),
            _ => None,
        };
        let pointer = runtime_pointer_path(unpinned).unwrap();
        let pinned = |name: &str| match name {
            "HOME" => Some(OsString::from(home.as_os_str())),
            "ADE_HOST_RUNTIME_DIR" => Some(OsString::from(fixture.as_os_str())),
            _ => None,
        };

        publish_runtime_dir(&fixture, pinned, 11).unwrap();
        assert!(
            !pointer.exists(),
            "a pinned fixture published its directory"
        );
        // Pinned the other way: the directory came from `--socket`, not from
        // anything this environment would have resolved.
        publish_runtime_dir(&fixture, unpinned, 11).unwrap();
        assert!(
            !pointer.exists(),
            "a daemon published a directory its environment does not name"
        );

        // And a pinned process must not *read* it either: the sweep that
        // follows the pointer deletes what it finds, so a fixture that read the
        // developer's pointer would drain their pending events. This is the
        // same predicate, and it was once stated only on the write.
        adopt_runtime_dir(&fixture);
        assert!(
            !placed_by_environment(&fixture, pinned, 11),
            "a pinned fixture claimed the pointer it must not read"
        );

        publish_runtime_dir(&home.join("tmux-agent-ide"), unpinned, 11).unwrap();
        assert_eq!(
            fs::read(&pointer).unwrap(),
            home.join("tmux-agent-ide").as_os_str().as_encoded_bytes(),
            "the real daemon's own directory was not published"
        );
        // And it round-trips as bytes rather than as text.
        assert!(candidate_runtime_dirs(unpinned, 11).contains(&home.join("tmux-agent-ide")));
        fs::remove_dir_all(home).unwrap();
    }

    /// A fixture that names its directory must never be widened into a scan:
    /// the neighbouring candidate on a developer's machine is their real daemon.
    #[test]
    fn an_explicit_runtime_directory_is_the_only_candidate() {
        let environment = |name: &str| match name {
            "ADE_HOST_RUNTIME_DIR" => Some(OsString::from("/fixture/runtime")),
            "XDG_RUNTIME_DIR" => Some(OsString::from("/run/user/7")),
            "HOME" => Some(OsString::from("/home/someone")),
            _ => None,
        };
        assert_eq!(
            candidate_runtime_dirs(environment, 7),
            vec![PathBuf::from("/fixture/runtime")]
        );
    }

    #[test]
    fn an_undeliverable_event_waits_where_a_daemon_has_lived() {
        let root = std::env::temp_dir().join(format!("ade-fallback-{}", uuid::Uuid::new_v4()));
        let empty = root.join("empty");
        let used = root.join("used");
        fs::create_dir_all(&empty).unwrap();
        fs::create_dir_all(&used).unwrap();
        fs::write(used.join("daemon.json"), b"{}").unwrap();
        assert_eq!(
            fallback_runtime_dir(&[root.join("missing"), empty.clone(), used.clone()]),
            used
        );
        // Nothing has ever run: the first directory that exists still beats a
        // path this process would have to create.
        assert_eq!(
            fallback_runtime_dir(&[root.join("missing"), empty.clone()]),
            empty
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_default_runtime_uses_user_cache_instead_of_shared_tmp() {
        if std::env::var_os("ADE_HOST_RUNTIME_DIR").is_none()
            && std::env::var_os("XDG_RUNTIME_DIR").is_none()
            && let Some(home) = std::env::var_os("HOME")
        {
            assert_eq!(
                default_runtime_dir(),
                PathBuf::from(home).join("Library/Caches/dev.dev.tmux-agent-ide/runtime")
            );
        }
    }
}
