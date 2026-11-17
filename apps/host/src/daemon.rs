use std::{
    fs,
    io::{ErrorKind, Read},
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, bail};
use serde::Deserialize;
use tokio::{
    net::{UnixListener, UnixStream},
    sync::mpsc,
    time::{sleep, timeout},
};

use tmux_agent_protocol::{
    PROTOCOL_MAJOR, envelope, read_frame,
    v1::{self, envelope::Payload},
    validate_host_contract, write_frame,
};

use crate::{
    diagnostics::{RuntimeDiagnostics, SafeErrorClass, write_safe_log},
    paths, service,
};

pub async fn run(socket_path: PathBuf) -> anyhow::Result<()> {
    let helper_build_digest = crate::build_identity::digest()?.to_owned();
    let runtime = socket_path
        .parent()
        .context("daemon socket has no parent directory")?;
    paths::check_socket_path_length(&socket_path)?;
    paths::prepare_runtime_dir(runtime)?;
    // Resolve the process's temporary and durable roots before any subsystem
    // opens state. Explicit socket fixtures keep both roots together.
    paths::adopt_socket_path(&socket_path)?;
    paths::prepare_runtime_dir(&paths::state_dir())?;

    if socket_path.exists() {
        let metadata = fs::symlink_metadata(&socket_path)?;
        if !metadata.file_type().is_socket() {
            bail!(
                "refusing to replace non-socket path {}",
                socket_path.display()
            );
        }
        if UnixStream::connect(&socket_path).await.is_ok() {
            bail!(
                "host daemon is already running at {}",
                socket_path.display()
            );
        }
        fs::remove_file(&socket_path).context("remove stale daemon socket")?;
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind daemon socket {}", socket_path.display()))?;
    paths::ensure_private_socket(&socket_path)?;
    let diagnostics = match RuntimeDiagnostics::open(runtime) {
        Ok(diagnostics) => diagnostics,
        Err(error) => {
            drop(listener);
            let _ = fs::remove_file(&socket_path);
            return Err(error).context("initialize private runtime diagnostics");
        }
    };
    diagnostics.install_process_recorder();
    let metadata_path = runtime.join("daemon.json");
    let executable = fs::canonicalize(std::env::current_exe()?)?;
    let process_start_time = process_start_time(std::process::id())?;
    fs::write(
        &metadata_path,
        serde_json::to_vec(&serde_json::json!({
            "pid": std::process::id(),
            "helperVersion": tmux_agent_protocol::HELPER_VERSION,
            "helperBuildDigest": helper_build_digest,
            "protocolMajor": tmux_agent_protocol::PROTOCOL_MAJOR,
            "processStartTime": process_start_time,
            "executable": executable,
        }))?,
    )?;
    fs::set_permissions(&metadata_path, fs::Permissions::from_mode(0o600))?;

    // Replay what hooks left in the mailbox while no daemon ran, off the
    // runtime, so the socket answers from the start: a daemon started under a
    // busy disk used to be silent for the whole drain, and a bridge retires a
    // silent daemon. The order still holds. A live hook drains the mailbox
    // under the same lock before it is applied, so it waits for this drain
    // instead of overtaking it, and the timer below does the same before
    // maintenance.
    let startup_drain = tokio::task::spawn_blocking({
        let diagnostics = diagnostics.clone();
        move || {
            if service::agents::ingest_fallbacks().is_err() {
                diagnostics.record(SafeErrorClass::HookFallbackIngestionFailed);
                write_safe_log(SafeErrorClass::HookFallbackIngestionFailed);
            }
        }
    });

    // The chats that were running when the last daemon stopped go on from
    // where they are, with nobody connected to ask for them.
    tokio::task::spawn_blocking(service::chat::adopt_running);

    // A persistence failure can recover without a daemon restart (for example,
    // after transient quota pressure). Keep draining the durable mailbox while
    // this daemon is alive; live ingest also drains synchronously before it
    // accepts a newer event, so the timer can never be an overtaking lane.
    let hook_retry = tokio::spawn(async {
        let mut temporaries_swept: Option<std::time::Instant> = None;
        loop {
            sleep(Duration::from_secs(2)).await;
            let sweep_temporaries = temporaries_swept
                .is_none_or(|swept| swept.elapsed() >= STALE_TEMPORARY_SWEEP_INTERVAL);
            if sweep_temporaries {
                temporaries_swept = Some(std::time::Instant::now());
            }
            let _ = tokio::task::spawn_blocking(move || {
                if sweep_temporaries {
                    remove_stale_temporaries(&paths::state_dir(), std::time::SystemTime::now());
                }
                let _ = service::agents::ingest_fallbacks();
                // After the drain, never before it: maintenance withdraws
                // states that nothing has confirmed, and a hook already sitting
                // in the durable mailbox is a confirmation. Sweeping first
                // would withdraw a state the very next line was about to
                // restore, and publish both.
                service::agents::maintain();
                // Snoozes end on the host's clock, connected or not.
                service::history::maintain();
            })
            .await;
        }
    });
    // Optional features with daemon-lifetime work of their own, such as
    // automations; like the hook timer, they run with no client connected.
    // They start once the mailbox is drained: an automation that closes an
    // idle agent's tab must not judge it by state the mailbox is about to
    // change.
    let plugins = tokio::spawn(async move {
        let _ = startup_drain.await;
        service::plugins::start()
    });

    let (shutdown_tx, mut shutdown_rx) = mpsc::unbounded_channel();
    loop {
        tokio::select! {
        _ = shutdown_rx.recv() => break,
        accepted = listener.accept() => match accepted {
            Ok((stream, _)) => {
                let shutdown_tx = shutdown_tx.clone();
                let diagnostics = diagnostics.clone();
                diagnostics.connection_accepted();
                tokio::spawn(async move {
                    let result = service::serve_with_shutdown(stream, Some(shutdown_tx)).await;
                    diagnostics.connection_ended(result.is_err());
                    if result.is_err() {
                        write_safe_log(SafeErrorClass::HostConnectionEnded);
                    }
                });
            }
            Err(_) => {
                diagnostics.record(SafeErrorClass::DaemonAcceptFailed);
                write_safe_log(SafeErrorClass::DaemonAcceptFailed);
                sleep(Duration::from_millis(50)).await;
            }
        }
        }
    }
    hook_retry.abort();
    plugins.abort();
    if let Ok(plugin_tasks) = plugins.await {
        for task in plugin_tasks {
            task.abort();
        }
    }
    drop(listener);
    // A hot voice sidecar holds ~1 GB; kill it here rather than let
    // `kill_on_drop` race the runtime's teardown.
    service::voice::shutdown().await;
    // Flush coalesced operational counters before cooperative shutdown. This
    // is ordered against the background writer and never runs on hot paths.
    let _ = diagnostics.flush();
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(&metadata_path);
    Ok(())
}

pub async fn stop(socket_path: PathBuf) -> anyhow::Result<()> {
    let mut stream = match UnixStream::connect(&socket_path).await {
        Ok(stream) => stream,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) if error.kind() == ErrorKind::ConnectionRefused => {
            remove_stale_endpoint(&socket_path)?;
            return Ok(());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("connect daemon socket {}", socket_path.display()));
        }
    };
    if let Err(handshake_error) = write_frame(
        &mut stream,
        &envelope(
            1,
            0,
            Payload::ClientHello(v1::ClientHello {
                bulk_connection: false,
                ..Default::default()
            }),
        ),
    )
    .await
    {
        drop(stream);
        return remove_if_stale_after_transport_error(&socket_path, handshake_error.into()).await;
    }
    let hello = match read_frame(&mut stream).await {
        Ok(Some(hello)) => hello,
        Ok(None) => {
            drop(stream);
            return remove_if_stale_after_transport_error(
                &socket_path,
                anyhow::anyhow!("daemon closed during handshake"),
            )
            .await;
        }
        Err(error) => {
            drop(stream);
            return remove_if_stale_after_transport_error(&socket_path, error.into()).await;
        }
    };
    if hello.protocol_major != PROTOCOL_MAJOR
        || !matches!(hello.payload, Some(Payload::ServerHello(_)))
    {
        bail!("daemon protocol is incompatible with cooperative shutdown");
    }
    write_frame(
        &mut stream,
        &envelope(
            2,
            0,
            Payload::Request(v1::Request {
                operation: v1::Operation::ShutdownDaemon.into(),
                ..Default::default()
            }),
        ),
    )
    .await?;
    loop {
        let response = match read_frame(&mut stream).await {
            Ok(Some(response)) => response,
            Ok(None) => {
                drop(stream);
                return remove_if_stale_after_transport_error(
                    &socket_path,
                    anyhow::anyhow!("daemon closed before shutdown acknowledgement"),
                )
                .await;
            }
            Err(error) => {
                drop(stream);
                return remove_if_stale_after_transport_error(&socket_path, error.into()).await;
            }
        };
        if response.request_id == 2 {
            let Some(Payload::Response(response)) = response.payload else {
                bail!("daemon returned an invalid shutdown acknowledgement");
            };
            if !response.ok {
                bail!("daemon refused shutdown: {}", response.display_message);
            }
            return Ok(());
        }
    }
}

/// Stops the daemon cooperatively, or, when it cannot cooperate within two
/// seconds, retires it by its verified PID. Shared by the bridge replacing a
/// different build and by `daemon-stop --force` in the remote install scripts.
pub async fn stop_or_retire(socket_path: &Path) -> anyhow::Result<()> {
    let stopped = timeout(Duration::from_secs(2), stop(socket_path.to_owned())).await;
    if matches!(stopped, Ok(Ok(()))) {
        return Ok(());
    }
    let retired = retire_verified(socket_path).await;
    // A slow but cooperative daemon can exit after the timeout and delete its
    // metadata before the retire reads it; an endpoint that no longer accepts
    // connections is the stop that was asked for.
    if retired.is_err() && UnixStream::connect(socket_path).await.is_err() {
        return Ok(());
    }
    retired.context("retire host daemon after cooperative shutdown failed")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DaemonMetadata {
    pid: u32,
    process_start_time: u64,
    executable: PathBuf,
}

/**
 * Retires a daemon that cannot speak the current cooperative-shutdown protocol.
 *
 * The caller must already have established that the daemon is older. This
 * function independently proves the PID still names the process recorded by
 * the private runtime metadata before sending a bounded SIGTERM.
 */
pub async fn retire_verified(socket_path: &Path) -> anyhow::Result<()> {
    let runtime = socket_path
        .parent()
        .context("daemon socket has no parent directory")?;
    let metadata_path = runtime.join("daemon.json");
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&metadata_path)
        .context("open daemon metadata without following links")?;
    let file_metadata = file.metadata().context("inspect daemon metadata")?;
    if !file_metadata.file_type().is_file()
        || file_metadata.uid() != unsafe { libc::geteuid() }
        || file_metadata.mode() & 0o077 != 0
    {
        bail!("refusing unsafe daemon metadata");
    }
    let mut encoded = Vec::new();
    file.by_ref().take(16 * 1024).read_to_end(&mut encoded)?;
    let recorded: DaemonMetadata =
        serde_json::from_slice(&encoded).context("decode daemon metadata")?;
    let live_start =
        process_start_time(recorded.pid).context("verify daemon process start time")?;
    let live_executable = process_executable(recorded.pid).context("verify daemon executable")?;
    if live_start != recorded.process_start_time || live_executable != recorded.executable {
        bail!("daemon metadata no longer identifies the live process");
    }

    let pid = i32::try_from(recorded.pid).context("daemon PID exceeds pid_t")?;
    // SAFETY: the PID's start time and executable were matched immediately
    // above; SIGTERM requests the daemon's ordinary process cleanup path.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        return Err(std::io::Error::last_os_error()).context("terminate verified daemon");
    }
    for _ in 0..100 {
        match process_start_time(recorded.pid) {
            Err(_) => {
                return remove_if_stale_after_transport_error(
                    socket_path,
                    anyhow::anyhow!("retired daemon left a live endpoint"),
                )
                .await;
            }
            Ok(start) if start != recorded.process_start_time => {
                bail!("daemon PID was reused while waiting for retirement");
            }
            Ok(_) => sleep(Duration::from_millis(20)).await,
        }
    }
    bail!("verified daemon did not exit after SIGTERM")
}

/// A dropped Unix listener can leave a full kernel listen backlog of queued
/// connections that accept writes and then reset. Drain beyond Linux's common
/// 128-entry default; a live endpoint keeps replenishing the queue and is never
/// unlinked.
async fn remove_if_stale_after_transport_error(
    socket_path: &std::path::Path,
    original: anyhow::Error,
) -> anyhow::Result<()> {
    for _ in 0..256 {
        match UnixStream::connect(socket_path).await {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) if error.kind() == ErrorKind::ConnectionRefused => {
                remove_stale_endpoint(socket_path)?;
                return Ok(());
            }
            Ok(stream) => drop(stream),
            Err(_) => return Err(original),
        }
        sleep(Duration::from_millis(2)).await;
    }
    Err(original).context("daemon transport failed but endpoint still accepts connections")
}

pub async fn check(socket_path: PathBuf) -> anyhow::Result<()> {
    let mut stream = UnixStream::connect(&socket_path)
        .await
        .with_context(|| format!("connect daemon socket {}", socket_path.display()))?;
    write_frame(
        &mut stream,
        &envelope(
            1,
            0,
            Payload::ClientHello(v1::ClientHello {
                bulk_connection: false,
                ..Default::default()
            }),
        ),
    )
    .await?;
    let frame = read_frame(&mut stream)
        .await?
        .context("daemon closed during handshake")?;
    let envelope_major = frame.protocol_major;
    let Some(Payload::ServerHello(hello)) = frame.payload else {
        bail!("daemon did not return ServerHello");
    };
    validate_host_contract(envelope_major)?;
    if hello.helper_build_digest != crate::build_identity::digest()? {
        bail!("daemon helper build handshake is incompatible");
    }
    println!(
        "{}",
        serde_json::json!({
            "helperVersion": hello.helper_version,
            "helperBuildDigest": hello.helper_build_digest,
            "protocolMajor": PROTOCOL_MAJOR,
            "serverIdentity": hello.server_identity,
        })
    );
    Ok(())
}

fn remove_stale_endpoint(socket_path: &std::path::Path) -> anyhow::Result<()> {
    let runtime = socket_path
        .parent()
        .context("daemon socket has no parent directory")?;
    paths::prepare_runtime_dir(runtime)?;
    match fs::symlink_metadata(socket_path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(socket_path)
            .with_context(|| format!("remove stale daemon socket {}", socket_path.display()))?,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Ok(_) => bail!(
            "refusing to remove non-socket daemon endpoint {}",
            socket_path.display()
        ),
        Err(error) => return Err(error).context("inspect stale daemon socket"),
    }
    let metadata_path = runtime.join("daemon.json");
    match fs::symlink_metadata(&metadata_path) {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(&metadata_path).context("remove stale daemon metadata")?;
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Ok(_) => bail!("refusing to remove unsafe stale daemon metadata"),
        Err(error) => return Err(error).context("inspect stale daemon metadata"),
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn process_start_time(pid: u32) -> anyhow::Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let (_, tail) = stat
        .rsplit_once(") ")
        .context("invalid /proc process stat")?;
    tail.split_whitespace()
        .nth(19)
        .context("process stat omitted start time")?
        .parse()
        .context("invalid process start time")
}

#[cfg(target_os = "macos")]
fn process_start_time(pid: u32) -> anyhow::Result<u64> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let expected = std::mem::size_of::<libc::proc_bsdinfo>();
    let expected_i32 = i32::try_from(expected).context("proc_bsdinfo size exceeds i32")?;
    // SAFETY: `info` points to `expected` writable bytes and the requested
    // flavor returns `proc_bsdinfo` for the exact process ID.
    let written = unsafe {
        libc::proc_pidinfo(
            i32::try_from(pid).context("process ID exceeds Darwin pid_t")?,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            expected_i32,
        )
    };
    if written != expected_i32 {
        return Err(std::io::Error::last_os_error()).context("inspect Darwin process start time");
    }
    let info = unsafe { info.assume_init() };
    info.pbi_start_tvsec
        .checked_mul(1_000_000)
        .and_then(|value| value.checked_add(info.pbi_start_tvusec))
        .context("Darwin process start time overflow")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_start_time(_pid: u32) -> anyhow::Result<u64> {
    bail!("process start-time identity is unsupported on this platform")
}

#[cfg(target_os = "linux")]
fn process_executable(pid: u32) -> anyhow::Result<PathBuf> {
    let path = fs::read_link(format!("/proc/{pid}/exe"))?;
    let displayed = path.to_string_lossy();
    Ok(PathBuf::from(
        displayed.strip_suffix(" (deleted)").unwrap_or(&displayed),
    ))
}

#[cfg(target_os = "macos")]
fn process_executable(pid: u32) -> anyhow::Result<PathBuf> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let mut bytes = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `bytes` is writable for its declared length and proc_pidpath
    // writes at most that many bytes for the exact PID.
    let written = unsafe {
        libc::proc_pidpath(
            i32::try_from(pid).context("process ID exceeds Darwin pid_t")?,
            bytes.as_mut_ptr().cast(),
            u32::try_from(bytes.len()).context("process path buffer exceeds u32")?,
        )
    };
    if written <= 0 {
        return Err(std::io::Error::last_os_error()).context("inspect Darwin process executable");
    }
    bytes.truncate(usize::try_from(written).context("negative Darwin process path length")?);
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_executable(_pid: u32) -> anyhow::Result<PathBuf> {
    bail!("process executable identity is unsupported on this platform")
}

/// How often the maintenance pass lists the state folder for stale temporaries.
/// They pile up over days, so once a minute is plenty.
const STALE_TEMPORARY_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// How old a temporary has to be before it is taken for abandoned. A write
/// takes milliseconds, or tens of seconds on a stalled disk; never an hour.
const STALE_TEMPORARY_AGE: Duration = Duration::from_secs(60 * 60);

/// Removes the temporaries of state writes that never reached their rename:
/// `.hook-fallback-<uuid>.tmp` from a hook client killed by its agent's hook
/// timeout mid-write, and `.agents-<uuid>.tmp` from a daemon killed mid-persist.
/// Nothing else removes them, and one stalled afternoon left 138.
fn remove_stale_temporaries(state: &Path, now: std::time::SystemTime) {
    let Ok(entries) = fs::read_dir(state) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !(name.starts_with(".hook-fallback-") || name.starts_with(".agents-"))
            || !name.ends_with(".tmp")
        {
            continue;
        }
        // Not followed: a link here is not one of ours.
        let stale = entry.metadata().ok().is_some_and(|metadata| {
            metadata.file_type().is_file()
                && metadata
                    .modified()
                    .ok()
                    .and_then(|modified| now.duration_since(modified).ok())
                    .is_some_and(|age| age >= STALE_TEMPORARY_AGE)
        });
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::{fs::PermissionsExt, net::UnixListener},
    };

    #[test]
    fn only_temporaries_older_than_an_hour_are_removed() {
        let state = tempfile::tempdir().unwrap();
        let names = [
            ".hook-fallback-0b1c.tmp",
            ".agents-0b1c.tmp",
            ".hook-fallback-mailbox.lock",
            "hook-fallback-codex-7-00000000000000000000.pb",
            "agents.json",
            ".sessions-0b1c.tmp",
        ];
        for name in names {
            fs::write(state.path().join(name), b"x").unwrap();
        }
        let in_an_hour = std::time::SystemTime::now() + super::STALE_TEMPORARY_AGE;

        super::remove_stale_temporaries(state.path(), std::time::SystemTime::now());
        assert_eq!(fs::read_dir(state.path()).unwrap().count(), names.len());

        super::remove_stale_temporaries(state.path(), in_an_hour);
        let mut left = fs::read_dir(state.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        left.sort();
        assert_eq!(
            left,
            [
                ".hook-fallback-mailbox.lock",
                ".sessions-0b1c.tmp",
                "agents.json",
                "hook-fallback-codex-7-00000000000000000000.pb",
            ]
        );
    }

    #[tokio::test]
    async fn stop_is_idempotent_and_removes_only_a_stale_socket_endpoint() {
        let fixture = tempfile::tempdir().unwrap();
        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let socket = fixture.path().join("host.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        drop(listener);
        fs::write(fixture.path().join("daemon.json"), b"stale").unwrap();

        super::stop(socket.clone()).await.unwrap();
        assert!(!socket.exists());
        assert!(!fixture.path().join("daemon.json").exists());
        super::stop(socket).await.unwrap();
    }

    #[tokio::test]
    async fn stop_never_removes_a_non_socket_endpoint() {
        let fixture = tempfile::tempdir().unwrap();
        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let endpoint = fixture.path().join("host.sock");
        fs::write(&endpoint, b"foreign").unwrap();

        assert!(super::stop(endpoint.clone()).await.is_err());
        assert_eq!(fs::read(endpoint).unwrap(), b"foreign");
    }
}
