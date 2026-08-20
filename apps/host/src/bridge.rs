use std::{
    fs::{self, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, bail};
use tokio::{
    io::{self, AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::Command,
    sync::oneshot,
    time::{sleep, timeout},
};

/// How long the bridge keeps draining the daemon after its own stdin closed.
///
/// Stdin closing means the client is gone: the desktop holds this process's
/// stdin open for the connection's whole life and only releases it by dying.
/// The daemon's teardown then closes the socket well inside this window, so
/// the timer normally never fires — it is the self-reaper for a daemon that
/// regresses into holding the socket open, which once left bridge processes
/// orphaned for days. The timer re-arms on every read, so a daemon still
/// actively flushing is never cut off.
const DRAIN_IDLE: Duration = Duration::from_secs(10);

pub async fn run(socket_path: PathBuf, auto_start: bool) -> anyhow::Result<()> {
    let stream = connect(&socket_path, auto_start).await?;
    let (mut socket_read, mut socket_write) = stream.into_split();
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();

    let (stdin_closed_tx, mut stdin_closed) = oneshot::channel::<()>();
    let upload = tokio::spawn(async move {
        let copied = io::copy(&mut stdin, &mut socket_write).await;
        let _ = socket_write.shutdown().await;
        let _ = stdin_closed_tx.send(());
        copied
    });

    let mut buffer = vec![0u8; 64 * 1024];
    let mut draining = false;
    loop {
        let read = if draining {
            match timeout(DRAIN_IDLE, socket_read.read(&mut buffer)).await {
                Ok(read) => read?,
                // Nothing from the daemon for a whole idle window after the
                // client already left: stop waiting for a close that may
                // never come.
                Err(_elapsed) => break,
            }
        } else {
            tokio::select! {
                read = socket_read.read(&mut buffer) => read?,
                _ = &mut stdin_closed => {
                    draining = true;
                    continue;
                }
            }
        };
        if read == 0 {
            break;
        }
        stdout.write_all(&buffer[..read]).await?;
        stdout.flush().await?;
    }
    stdout.flush().await?;
    // The daemon side is finished; a pump still parked on a live stdin has
    // nothing left to deliver to.
    upload.abort();
    let _ = upload.await;
    Ok(())
}

async fn connect(path: &Path, auto_start: bool) -> anyhow::Result<UnixStream> {
    if let Ok(stream) = UnixStream::connect(path).await {
        return Ok(stream);
    }
    if !auto_start {
        bail!("host daemon is not available at {}", path.display());
    }

    let executable = std::env::current_exe().context("resolve host helper executable")?;
    Command::new(executable)
        .arg("daemon")
        .arg("--socket")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(daemon_stderr(path))
        .spawn()
        .context("start host daemon")?;

    for _ in 0..100 {
        if let Ok(stream) = UnixStream::connect(path).await {
            return Ok(stream);
        }
        sleep(Duration::from_millis(20)).await;
    }
    bail!("host daemon did not create {}", path.display())
}

/// Bytes the daemon's stderr log keeps before the next spawn starts it over.
const MAX_DAEMON_STDERR_BYTES: u64 = 5 * 1024 * 1024;

/// Where a detached daemon's stderr goes.
///
/// It went to `/dev/null`. Every line this host writes about a pane it had to
/// evict, a resume tmux refused or a seed it discarded is written to stderr,
/// and in production none of them has ever been read: the daemon is spawned
/// detached from the process that started it, so its stderr had no reader and
/// was pointed at nothing. A file beside `diagnostics.json` — the same private
/// runtime directory, derived the same way, from this socket's parent — is the
/// difference between diagnosing a pane that froze an hour ago and asking the
/// user to reproduce it.
///
/// Rotation is a truncation at spawn time and nothing more. A log that grows
/// without bound in a cache directory is its own defect, and a daemon that
/// restarts is exactly the moment the previous run's tail stops being the
/// interesting one. Any failure here falls back to the old behaviour rather
/// than refusing to start a daemon over a log file.
fn daemon_stderr(socket_path: &Path) -> Stdio {
    let Some(runtime) = socket_path.parent() else {
        return Stdio::null();
    };
    let path = runtime.join("daemon.stderr.log");
    let oversized = fs::symlink_metadata(&path)
        .map(|metadata| metadata.is_file() && metadata.len() > MAX_DAEMON_STDERR_BYTES)
        .unwrap_or(false);
    let mut options = OpenOptions::new();
    options
        .create(true)
        .write(true)
        .mode(0o600)
        // The daemon's runtime directory is the user's own private directory;
        // refusing to follow a symlink out of it keeps a hostile or stale link
        // from redirecting this write somewhere else.
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    if oversized {
        options.truncate(true);
    } else {
        options.append(true);
    }
    options
        .open(&path)
        .map_or_else(|_| Stdio::null(), Stdio::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temporary_runtime() -> PathBuf {
        let root = std::env::temp_dir().join(format!("ade-bridge-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn the_daemon_log_is_private_appended_and_started_over_when_oversized() {
        let root = temporary_runtime();
        let socket = root.join("host.sock");
        let log = root.join("daemon.stderr.log");

        drop(daemon_stderr(&socket));
        assert_eq!(
            fs::metadata(&log).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::write(&log, b"the previous run").unwrap();
        drop(daemon_stderr(&socket));
        assert_eq!(
            fs::read(&log).unwrap(),
            b"the previous run",
            "an ordinary restart discarded the log it was meant to keep"
        );

        fs::write(&log, vec![b'x'; MAX_DAEMON_STDERR_BYTES as usize + 1]).unwrap();
        drop(daemon_stderr(&socket));
        assert_eq!(
            fs::metadata(&log).unwrap().len(),
            0,
            "an oversized log grew without bound"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A daemon that cannot get its log still starts. The log is diagnostics;
    /// refusing to run without one would trade a readable failure for a total
    /// one.
    #[test]
    fn a_symlinked_or_unwritable_log_falls_back_to_discarding_stderr() {
        let root = temporary_runtime();
        let outside = root.join("elsewhere");
        fs::write(&outside, b"untouched").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("daemon.stderr.log")).unwrap();
        drop(daemon_stderr(&root.join("host.sock")));
        assert_eq!(fs::read(&outside).unwrap(), b"untouched");
        fs::remove_dir_all(root).unwrap();
    }
}
