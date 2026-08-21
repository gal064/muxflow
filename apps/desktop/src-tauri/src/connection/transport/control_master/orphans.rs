use super::*;

/// Sweeps control-master debris left behind by earlier runs of this app.
///
/// A graceful quit closes its own masters and unlinks its own sockets, but a
/// SIGTERM, a SIGKILL or a crash never reaches that path: the `ssh -M -N`
/// master keeps running and its per-process namespace directory keeps sitting
/// under the temporary root. Nothing in the dead process can clean that up, so
/// the next launch does it — once, in the background, and only for namespaces
/// whose owning pid is provably gone.
pub(crate) fn spawn_orphan_reaper() {
    let _ = thread::Builder::new()
        .name("ssh-orphan-reaper".into())
        .spawn(|| sweep_orphan_namespaces(&mut shutdown_orphaned_master));
}

/// The temporary roots a control socket can live under.
///
/// `ssh_profile_control_socket` prefers `std::env::temp_dir()` and falls back
/// to `/tmp` when the preferred root leaves no room for the AF_UNIX bind, so
/// both have to be swept. The fallback is dropped when it is the preferred
/// root, which is the normal case outside macOS.
pub(super) fn orphan_sweep_roots() -> Vec<PathBuf> {
    let preferred = std::env::temp_dir();
    let short = PathBuf::from("/tmp");
    if preferred == short {
        return vec![preferred];
    }
    vec![preferred, short]
}

fn sweep_orphan_namespaces(shutdown: &mut impl FnMut(&Path)) {
    for root in orphan_sweep_roots() {
        let base = root.join(format!("muxflow-{}", unsafe { libc::geteuid() }));
        sweep_orphan_base(&base, shutdown);
    }
}

/// Tells any master still bound to `socket` to exit before the socket is
/// unlinked. A master that already died makes `ssh -O exit` fail immediately,
/// which is exactly the outcome the caller wants, so the result is ignored.
fn shutdown_orphaned_master(socket: &Path) {
    let mut command = ssh_base(None);
    command
        .arg("-S")
        .arg(socket)
        .args(["-O", "exit"])
        .arg("muxflow-orphan-reap");
    let _ = run_control_command(command, CONTROL_COMMAND_TIMEOUT, &|| false);
}

pub(super) fn sweep_orphan_base(base: &Path, shutdown: &mut impl FnMut(&Path)) {
    // The reaper only ever deletes, so it validates and never repairs: a base
    // that is not an owned, private, non-symlink directory is left untouched
    // rather than chmodded into something this sweep is willing to walk.
    if !is_private_directory(base) {
        return;
    }
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(namespace_directory_pid) else {
            continue;
        };
        if pid == std::process::id() || !namespace_owner_is_dead(pid) {
            continue;
        }
        reap_namespace_directory(&entry.path(), shutdown);
    }
}

fn is_private_directory(directory: &Path) -> bool {
    fs::symlink_metadata(directory).is_ok_and(|metadata| {
        metadata.file_type().is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0
    })
}

/// Parses `ssh-<pid>-<nonce>` as written by `process_socket_namespace`, where
/// the nonce is the first eight characters of a simple-form UUID. Anything else
/// in the base directory belongs to someone else and is not ours to delete.
pub(super) fn namespace_directory_pid(name: &str) -> Option<u32> {
    let (pid, nonce) = name.strip_prefix("ssh-")?.rsplit_once('-')?;
    if pid.is_empty() || !pid.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if nonce.len() != 8
        || !nonce
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    pid.parse().ok()
}

/// `ESRCH` is the only answer that proves the namespace is abandoned. A live
/// pid, a pid owned by another user (`EPERM`), or any unexpected errno all
/// leave the directory alone.
fn namespace_owner_is_dead(pid: u32) -> bool {
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return false;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

fn reap_namespace_directory(directory: &Path, shutdown: &mut impl FnMut(&Path)) {
    if let Ok(entries) = fs::read_dir(directory) {
        for entry in entries.flatten() {
            let path = entry.path();
            if matches!(validated_control_socket_identity(&path), Ok(Some(_))) {
                shutdown(&path);
                let _ = fs::remove_file(&path);
            }
        }
    }
    // `remove_dir`, never `remove_dir_all`: a namespace that still holds
    // something this sweep did not recognise survives instead of being erased.
    let _ = fs::remove_dir(directory);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// A short root, not the default tempdir: one of these tests binds a real
    /// socket, and macOS puts the default tempdir 50+ bytes deep under
    /// `/var/folders`, past the Darwin `sun_path` limit.
    fn private_base() -> tempfile::TempDir {
        let base = tempfile::Builder::new()
            .prefix("ade-orph")
            .tempdir_in("/tmp")
            .unwrap();
        fs::set_permissions(base.path(), fs::Permissions::from_mode(0o700)).unwrap();
        base
    }

    /// A pid that has been spawned and reaped is the only pid this process can
    /// know is dead, rather than merely unlikely to exist.
    fn reaped_pid() -> u32 {
        let mut child = Command::new("/usr/bin/true").spawn().unwrap();
        child.wait().unwrap();
        child.id()
    }

    fn sweep_recording(base: &Path) -> Vec<PathBuf> {
        let mut shutdowns = Vec::new();
        sweep_orphan_base(base, &mut |socket: &Path| shutdowns.push(socket.to_owned()));
        shutdowns
    }

    #[test]
    fn namespace_directory_names_are_strictly_parsed() {
        assert_eq!(namespace_directory_pid("ssh-123-abcdef01"), Some(123));
        for rejected in [
            "ssh--abcdef01",
            "ssh-123-ABCDEF01",
            "ssh-123-abcdef0",
            "ssh-123-abcdef012",
            "ssh-12x-abcdef01",
            "notssh-123-abcdef01",
        ] {
            assert_eq!(namespace_directory_pid(rejected), None, "{rejected}");
        }
    }

    #[test]
    fn empty_namespace_of_a_dead_owner_is_removed() {
        let base = private_base();
        let orphan = base.path().join(format!("ssh-{}-abcdef01", reaped_pid()));
        fs::create_dir(&orphan).unwrap();

        assert!(sweep_recording(base.path()).is_empty());
        assert!(!orphan.exists(), "empty orphaned namespace survived");
    }

    #[test]
    fn orphaned_socket_is_shut_down_and_its_namespace_removed() {
        let base = private_base();
        let orphan = base.path().join(format!("ssh-{}-abcdef01", reaped_pid()));
        fs::create_dir(&orphan).unwrap();
        let socket = orphan.join("profile-0123.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();

        assert_eq!(sweep_recording(base.path()), vec![socket.clone()]);
        assert!(!socket.exists(), "orphaned socket was not unlinked");
        assert!(!orphan.exists(), "orphaned namespace survived");
        drop(listener);
    }

    #[test]
    fn unrecognised_file_keeps_its_namespace_alive() {
        let base = private_base();
        let orphan = base.path().join(format!("ssh-{}-abcdef01", reaped_pid()));
        fs::create_dir(&orphan).unwrap();
        let stranger = orphan.join("not-a-socket");
        fs::write(&stranger, b"unexpected").unwrap();

        assert!(sweep_recording(base.path()).is_empty());
        assert!(stranger.exists(), "reaper deleted a file it does not own");
        assert!(orphan.exists(), "non-empty namespace was removed");
    }

    #[test]
    fn live_owner_and_malformed_names_are_left_alone() {
        let base = private_base();
        let live = base
            .path()
            .join(format!("ssh-{}-abcdef01", std::process::id()));
        let malformed = base.path().join("ssh-12x-abcdef01");
        fs::create_dir(&live).unwrap();
        fs::create_dir(&malformed).unwrap();

        assert!(sweep_recording(base.path()).is_empty());
        assert!(live.exists(), "this process's own namespace was reaped");
        assert!(malformed.exists(), "a foreign directory was reaped");
    }
}
