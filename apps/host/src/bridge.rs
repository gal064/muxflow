use std::{
    fs::{self, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, bail};
use tmux_agent_protocol::{
    PROTOCOL_MAJOR, envelope, read_frame,
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
    if let ExistingDaemon::Connected(stream) = inspect_existing_daemon(path, auto_start).await? {
        return Ok(stream);
    }
    if !auto_start {
        bail!("host daemon is not available at {}", path.display());
    }

    // Binding the socket is the daemon-ownership election. Another helper can
    // win between the preflight above and our child reaching bind, so every
    // winner is re-probed before its stream is forwarded to the app. If that
    // winner is stale, retire it and hold a fresh election.
    for _ in 0..3 {
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

        match wait_for_spawned_daemon(path).await? {
            SpawnedDaemon::Connected(stream) => return Ok(stream),
            SpawnedDaemon::Retired => continue,
        }
    }
    bail!(
        "could not elect a current host daemon at {}",
        path.display()
    )
}

enum SpawnedDaemon {
    Connected(UnixStream),
    Retired,
}

async fn wait_for_spawned_daemon(path: &Path) -> anyhow::Result<SpawnedDaemon> {
    for _ in 0..100 {
        match inspect_existing_daemon(path, true).await? {
            ExistingDaemon::Connected(stream) => {
                return Ok(SpawnedDaemon::Connected(stream));
            }
            ExistingDaemon::Retired => return Ok(SpawnedDaemon::Retired),
            ExistingDaemon::Missing => {}
        }
        sleep(Duration::from_millis(20)).await;
    }
    bail!("host daemon did not create {}", path.display())
}

/// Verifies a daemon before returning a stream from that same process. With
/// auto-start enabled, an outdated or unproven daemon is cooperatively retired
/// before the packaged helper starts its replacement. The probe has its own
/// connection because the desktop must still own the real connection's first
/// ClientHello.
async fn inspect_existing_daemon(path: &Path, auto_start: bool) -> anyhow::Result<ExistingDaemon> {
    let Ok(mut probe) = UnixStream::connect(path).await else {
        return Ok(ExistingDaemon::Missing);
    };
    let probe_pid = probe
        .peer_cred()
        .context("inspect daemon build-probe peer credentials")?
        .pid()
        .context("daemon build-probe peer omitted its process ID")?;
    let same_build = matches!(
        timeout(Duration::from_secs(2), daemon_is_current_build(&mut probe)).await,
        Ok(Ok(true))
    );
    drop(probe);
    if same_build {
        return Ok(match UnixStream::connect(path).await {
            Ok(stream)
                if stream
                    .peer_cred()
                    .context("inspect forwarded daemon peer credentials")?
                    .pid()
                    == Some(probe_pid) =>
            {
                ExistingDaemon::Connected(stream)
            }
            // The socket owner changed after the probe. Never forward the
            // unverified peer; the caller will inspect the new owner on
            // its next pass.
            Ok(_) => ExistingDaemon::Missing,
            Err(_) => ExistingDaemon::Missing,
        });
    }
    if !auto_start {
        bail!("host daemon build differs and --no-start forbids replacing it");
    }
    let stopped = timeout(Duration::from_secs(2), daemon::stop(path.to_owned())).await;
    if !matches!(stopped, Ok(Ok(()))) {
        daemon::retire_verified(path)
            .await
            .context("retire host daemon after cooperative shutdown failed")?;
    }
    for _ in 0..100 {
        if UnixStream::connect(path).await.is_err() {
            return Ok(ExistingDaemon::Retired);
        }
        sleep(Duration::from_millis(20)).await;
    }
    bail!("incurrent host daemon did not release {}", path.display())
}

#[cfg(test)]
async fn existing_daemon(path: &Path, auto_start: bool) -> anyhow::Result<Option<UnixStream>> {
    Ok(match inspect_existing_daemon(path, auto_start).await? {
        ExistingDaemon::Connected(stream) => Some(stream),
        ExistingDaemon::Missing | ExistingDaemon::Retired => None,
    })
}

enum ExistingDaemon {
    Connected(UnixStream),
    Missing,
    Retired,
}

async fn daemon_is_current_build(stream: &mut UnixStream) -> anyhow::Result<bool> {
    write_frame(
        stream,
        &envelope(
            1,
            0,
            Payload::ClientHello(v1::ClientHello {
                ..Default::default()
            }),
        ),
    )
    .await?;
    let response = read_frame(stream)
        .await?
        .context("daemon closed during executable identity probe")?;
    let Some(Payload::ServerHello(hello)) = response.payload else {
        return Ok(false);
    };
    Ok(response.protocol_major == PROTOCOL_MAJOR
        && hello.helper_build_digest == crate::build_identity::digest()?)
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
    use tmux_agent_protocol::HELPER_VERSION;
    use tokio::net::UnixListener;

    /// A short root, not the default tempdir: the sockets bound below must stay
    /// under the platform's 104/108-byte limit, and macOS puts the default
    /// tempdir 50+ bytes deep under /var/folders.
    fn temporary_runtime() -> PathBuf {
        let root = PathBuf::from("/tmp").join(format!("ade-bridge-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[tokio::test]
    async fn an_unconnectable_socket_is_missing_not_a_retired_live_daemon() {
        let root = temporary_runtime();
        let socket = root.join("host.sock");
        drop(UnixListener::bind(&socket).unwrap());

        assert!(matches!(
            inspect_existing_daemon(&socket, true).await.unwrap(),
            ExistingDaemon::Missing
        ));
        fs::remove_dir_all(root).unwrap();
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
    async fn a_same_version_daemon_from_another_build_is_stopped() {
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
                    helper_build_digest: "0".repeat(64),
                    ..Default::default()
                }),
            );
            hello.protocol_major = PROTOCOL_MAJOR;
            write_frame(&mut probe, &hello).await.unwrap();
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

    #[tokio::test]
    async fn no_start_refuses_a_stale_build_without_retiring_it() {
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
                    helper_build_digest: "0".repeat(64),
                    ..Default::default()
                }),
            );
            hello.protocol_major = PROTOCOL_MAJOR;
            write_frame(&mut probe, &hello).await.unwrap();
            assert!(read_frame(&mut probe).await.unwrap().is_none());
        });

        let error = existing_daemon(&socket, false)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("--no-start"), "{error}");
        server.await.unwrap();
        assert!(socket.exists(), "no-start retired the daemon socket");
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_stale_process_that_wins_the_spawn_race_is_never_forwarded() {
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
                    helper_build_digest: "0".repeat(64),
                    ..Default::default()
                }),
            );
            hello.protocol_major = PROTOCOL_MAJOR;
            write_frame(&mut probe, &hello).await.unwrap();
            drop(probe);

            let (mut shutdown, _) = listener.accept().await.unwrap();
            answer_cooperative_shutdown(&mut shutdown).await;
            drop(shutdown);
            drop(listener);
            fs::remove_file(&socket).unwrap();
        });

        assert!(matches!(
            wait_for_spawned_daemon(&root.join("host.sock"))
                .await
                .unwrap(),
            SpawnedDaemon::Retired
        ));
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
