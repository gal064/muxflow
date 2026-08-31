use std::{
    cmp::Ordering,
    fs::{self, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, bail};
use tmux_agent_protocol::{
    HELPER_VERSION, HOST_CAPABILITIES, PROTOCOL_MAJOR, envelope, missing_host_capabilities,
    read_frame,
    v1::{self, envelope::Payload},
    write_frame,
};
use tokio::{
    io::{self, AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::Command,
    sync::oneshot,
    time::{sleep, timeout},
};

use crate::daemon;

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

/// Why a bridge stopped pumping — the one field of its exit line that tells a
/// normal teardown apart from a daemon that vanished under a live client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    /// The client released stdin and the daemon then closed its side: the
    /// ordinary end of a connection the desktop walked away from.
    StdinEof,
    /// The daemon closed the socket while the client was still attached.
    DaemonEof,
    /// The client left and the daemon went quiet without ever closing.
    DrainIdle,
    /// The connection or the pump itself failed.
    Error,
}

impl ExitReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::StdinEof => "stdin-eof",
            Self::DaemonEof => "daemon-eof",
            Self::DrainIdle => "drain-idle",
            Self::Error => "error",
        }
    }
}

pub async fn run(socket_path: PathBuf, auto_start: bool) -> anyhow::Result<ExitReason> {
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
    let reason = loop {
        let read = if draining {
            match timeout(DRAIN_IDLE, socket_read.read(&mut buffer)).await {
                Ok(read) => read?,
                // Nothing from the daemon for a whole idle window after the
                // client already left: stop waiting for a close that may
                // never come.
                Err(_elapsed) => break ExitReason::DrainIdle,
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
            // A close after the client already left is the ordinary teardown.
            // One while it is still attached is the daemon going away under a
            // live desktop, which is a different incident entirely.
            //
            // The stdin closure and the close it provokes can land in the same
            // poll — the upload pump shuts the socket down before it reports
            // stdin gone — and `select!` picks between ready arms at random.
            // Asking the channel directly is what keeps an ordinary departure
            // from being filed as a vanished daemon on the losing coin flip.
            break if draining || stdin_closed.try_recv().is_ok() {
                ExitReason::StdinEof
            } else {
                ExitReason::DaemonEof
            };
        }
        stdout.write_all(&buffer[..read]).await?;
        stdout.flush().await?;
    };
    stdout.flush().await?;
    // The daemon side is finished; a pump still parked on a live stdin has
    // nothing left to deliver to. Aborting the task does not cancel the
    // blocking read underneath it — only leaving the process does; see the
    // bridge arm of `main`.
    upload.abort();
    let _ = upload.await;
    Ok(reason)
}

async fn connect(path: &Path, auto_start: bool) -> anyhow::Result<UnixStream> {
    if let Some(stream) = existing_daemon(path, auto_start).await? {
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

/// Uses a current daemon as-is and cooperatively retires one that is outdated —
/// or that could not prove it is not — before the packaged helper starts its
/// replacement. The probe has its own connection because the desktop must still
/// own the real connection's first ClientHello.
async fn existing_daemon(path: &Path, auto_start: bool) -> anyhow::Result<Option<UnixStream>> {
    let Ok(mut probe) = UnixStream::connect(path).await else {
        return Ok(None);
    };
    if !auto_start {
        return Ok(Some(probe));
    }
    // A probe that timed out, died mid-frame or answered with something other
    // than a ServerHello carries no evidence about versions. Failing the
    // connection on it made every later reconnect fail the same way until the
    // daemon was killed by hand, so an unproven daemon is retired exactly like
    // an outdated one. Only an affirmative ServerHello saying the daemon is
    // newer is allowed to refuse.
    let compatibility =
        match timeout(Duration::from_secs(2), daemon_compatibility(&mut probe)).await {
            Ok(Ok(compatibility)) => compatibility,
            // A transport failure or a timeout, in that order.
            Ok(Err(_)) | Err(_) => DaemonCompatibility::Unknown,
        };
    drop(probe);
    match compatibility {
        DaemonCompatibility::Compatible => return Ok(UnixStream::connect(path).await.ok()),
        DaemonCompatibility::AppOutdated => {
            bail!(
                "the running host daemon is newer than this app; update the app before reconnecting"
            )
        }
        DaemonCompatibility::Unknown | DaemonCompatibility::DaemonOutdated { force: false } => {
            let stopped = timeout(Duration::from_secs(2), daemon::stop(path.to_owned())).await;
            if !matches!(stopped, Ok(Ok(()))) {
                daemon::retire_verified(path)
                    .await
                    .context("retire incompatible host daemon after cooperative shutdown failed")?;
            }
        }
        DaemonCompatibility::DaemonOutdated { force: true } => {
            daemon::retire_verified(path)
                .await
                .context("retire old-protocol host daemon")?;
        }
    }
    for _ in 0..100 {
        if UnixStream::connect(path).await.is_err() {
            return Ok(None);
        }
        sleep(Duration::from_millis(20)).await;
    }
    bail!(
        "incompatible host daemon did not release {}",
        path.display()
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DaemonCompatibility {
    Compatible,
    DaemonOutdated { force: bool },
    AppOutdated,
    Unknown,
}

async fn daemon_compatibility(stream: &mut UnixStream) -> anyhow::Result<DaemonCompatibility> {
    write_frame(
        stream,
        &envelope(
            1,
            0,
            Payload::ClientHello(v1::ClientHello {
                desktop_version: HELPER_VERSION.into(),
                requested_capabilities: HOST_CAPABILITIES,
                expected_helper_version: HELPER_VERSION.into(),
                ..Default::default()
            }),
        ),
    )
    .await?;
    let response = read_frame(stream)
        .await?
        .context("daemon closed during compatibility probe")?;
    match response.protocol_major.cmp(&PROTOCOL_MAJOR) {
        Ordering::Less => return Ok(DaemonCompatibility::DaemonOutdated { force: true }),
        Ordering::Greater => return Ok(DaemonCompatibility::AppOutdated),
        Ordering::Equal => {}
    }
    let Some(Payload::ServerHello(hello)) = response.payload else {
        return Ok(DaemonCompatibility::Unknown);
    };
    match release_ordinal(&hello.helper_version).zip(release_ordinal(HELPER_VERSION)) {
        Some((daemon, current)) if daemon > current => return Ok(DaemonCompatibility::AppOutdated),
        Some((daemon, current)) if daemon < current => {
            return Ok(DaemonCompatibility::DaemonOutdated { force: false });
        }
        None if hello.helper_version != HELPER_VERSION => return Ok(DaemonCompatibility::Unknown),
        _ => {}
    }
    if missing_host_capabilities(hello.capabilities) != 0 {
        // Required bits are append-only. A same-release daemon missing one is
        // an older build of that release, never evidence that the app is old.
        return Ok(DaemonCompatibility::DaemonOutdated { force: false });
    }
    Ok(if hello.read_only {
        DaemonCompatibility::Unknown
    } else {
        DaemonCompatibility::Compatible
    })
}

fn release_ordinal(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let ordinal = (next()?, next()?, next()?);
    parts.next().is_none().then_some(ordinal)
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
    use tokio::net::UnixListener;

    /// A short root, not the default tempdir: the sockets bound below must stay
    /// under the platform's 104/108-byte limit, and macOS puts the default
    /// tempdir 50+ bytes deep under /var/folders.
    fn temporary_runtime() -> PathBuf {
        let root = PathBuf::from("/tmp").join(format!("ade-bridge-{}", uuid::Uuid::new_v4()));
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

    #[tokio::test]
    async fn an_old_running_daemon_is_stopped_before_the_packaged_one_starts() {
        let root = temporary_runtime();
        let socket = root.join("host.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (mut probe, _) = listener.accept().await.unwrap();
            let _ = read_frame(&mut probe).await.unwrap().unwrap();
            let mut hello = envelope(
                1,
                0,
                Payload::ServerHello(v1::ServerHello {
                    helper_version: HELPER_VERSION.into(),
                    capabilities: HOST_CAPABILITIES
                        & !tmux_agent_protocol::CAP_TMUX_EXECUTABLE_RESOLUTION,
                    ..Default::default()
                }),
            );
            hello.protocol_major = PROTOCOL_MAJOR;
            write_frame(&mut probe, &hello).await.unwrap();
            drop(probe);

            let (mut shutdown, _) = listener.accept().await.unwrap();
            let _ = read_frame(&mut shutdown).await.unwrap().unwrap();
            let mut hello = envelope(
                1,
                0,
                Payload::ServerHello(v1::ServerHello {
                    helper_version: HELPER_VERSION.into(),
                    ..Default::default()
                }),
            );
            hello.protocol_major = PROTOCOL_MAJOR;
            write_frame(&mut shutdown, &hello).await.unwrap();
            let request = read_frame(&mut shutdown).await.unwrap().unwrap();
            assert!(
                matches!(request.payload, Some(Payload::Request(v1::Request {
                operation,
                ..
            })) if operation == v1::Operation::ShutdownDaemon as i32)
            );
            write_frame(
                &mut shutdown,
                &envelope(
                    request.request_id,
                    0,
                    Payload::Response(v1::Response {
                        ok: true,
                        ..Default::default()
                    }),
                ),
            )
            .await
            .unwrap();
            drop(shutdown);
            drop(listener);
            fs::remove_file(&socket).unwrap();
        });

        assert!(
            existing_daemon(&root.join("host.sock"), true)
                .await
                .unwrap()
                .is_none()
        );
        server.await.unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    /// A daemon that cannot prove its version is retired, not refused forever.
    ///
    /// A probe that times out, hangs up mid-frame or answers with something
    /// other than a ServerHello says nothing about versions. Failing the
    /// connection on it left the daemon running, so every later reconnect
    /// failed exactly the same way until someone killed it by hand.
    #[tokio::test]
    async fn a_daemon_that_cannot_prove_its_version_is_retired_rather_than_refused() {
        let root = temporary_runtime();
        let socket = root.join("host.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (mut probe, _) = listener.accept().await.unwrap();
            let _ = read_frame(&mut probe).await.unwrap().unwrap();
            // No ServerHello at all: the daemon hangs up mid-probe.
            drop(probe);

            let (mut shutdown, _) = listener.accept().await.unwrap();
            answer_cooperative_shutdown(&mut shutdown).await;
            drop(shutdown);
            drop(listener);
            fs::remove_file(&socket).unwrap();
        });

        assert!(
            existing_daemon(&root.join("host.sock"), true)
                .await
                .unwrap()
                .is_none()
        );
        server.await.unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    /// The handshake and reply `daemon::stop` expects from a cooperating daemon.
    async fn answer_cooperative_shutdown(stream: &mut UnixStream) {
        let _ = read_frame(stream).await.unwrap().unwrap();
        let mut hello = envelope(
            1,
            0,
            Payload::ServerHello(v1::ServerHello {
                helper_version: HELPER_VERSION.into(),
                ..Default::default()
            }),
        );
        hello.protocol_major = PROTOCOL_MAJOR;
        write_frame(stream, &hello).await.unwrap();
        let request = read_frame(stream).await.unwrap().unwrap();
        assert!(
            matches!(request.payload, Some(Payload::Request(v1::Request {
                operation,
                ..
            })) if operation == v1::Operation::ShutdownDaemon as i32)
        );
        write_frame(
            stream,
            &envelope(
                request.request_id,
                0,
                Payload::Response(v1::Response {
                    ok: true,
                    ..Default::default()
                }),
            ),
        )
        .await
        .unwrap();
    }
}
