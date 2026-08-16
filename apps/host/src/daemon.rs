use std::{
    fs,
    io::ErrorKind,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, bail};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::mpsc,
    time::sleep,
};

use tmux_agent_protocol::{
    PROTOCOL_MAJOR, envelope, read_frame,
    v1::{self, envelope::Payload},
    write_frame,
};

use crate::{
    diagnostics::{RuntimeDiagnostics, SafeErrorClass, write_safe_log},
    paths, service,
};

pub async fn run(socket_path: PathBuf) -> anyhow::Result<()> {
    let runtime = socket_path
        .parent()
        .context("daemon socket has no parent directory")?;
    paths::check_socket_path_length(&socket_path)?;
    paths::prepare_runtime_dir(runtime)?;

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
    // Before anything reads it: this process's state belongs beside its
    // socket, not beside whatever its environment would have resolved.
    paths::adopt_runtime_dir(runtime);
    diagnostics.install_process_recorder();
    // Published before the first connection is accepted, so a hook that fires
    // the instant an agent starts can already find this directory rather than
    // the one its own environment would have derived (M13-E003). Non-fatal: a
    // daemon that cannot write the pointer still serves every client that
    // resolves the same directory it did, which is the common case.
    if let Err(error) = paths::record_runtime_dir(runtime) {
        eprintln!("could not record the runtime directory for hooks: {error}");
    }
    let metadata_path = runtime.join("daemon.json");
    let executable = fs::canonicalize(std::env::current_exe()?)?;
    let process_start_time = process_start_time(std::process::id())?;
    fs::write(
        &metadata_path,
        serde_json::to_vec(&serde_json::json!({
            "pid": std::process::id(),
            "helperVersion": tmux_agent_protocol::HELPER_VERSION,
            "protocolMajor": tmux_agent_protocol::PROTOCOL_MAJOR,
            "processStartTime": process_start_time,
            "executable": executable,
        }))?,
    )?;
    fs::set_permissions(&metadata_path, fs::Permissions::from_mode(0o600))?;

    if service::agents::ingest_fallbacks().is_err() {
        diagnostics.record(SafeErrorClass::HookFallbackIngestionFailed);
        write_safe_log(SafeErrorClass::HookFallbackIngestionFailed);
    }

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
    drop(listener);
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
                desktop_version: tmux_agent_protocol::HELPER_VERSION.into(),
                requested_capabilities: 0,
                expected_helper_version: String::new(),
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
                desktop_version: tmux_agent_protocol::HELPER_VERSION.into(),
                requested_capabilities: tmux_agent_protocol::HOST_CAPABILITIES,
                expected_helper_version: tmux_agent_protocol::HELPER_VERSION.into(),
                bulk_connection: false,
                ..Default::default()
            }),
        ),
    )
    .await?;
    let frame = read_frame(&mut stream)
        .await?
        .context("daemon closed during handshake")?;
    if frame.protocol_major != PROTOCOL_MAJOR {
        bail!("daemon protocol major is incompatible");
    }
    let Some(Payload::ServerHello(hello)) = frame.payload else {
        bail!("daemon did not return ServerHello");
    };
    if hello.read_only || hello.helper_version != tmux_agent_protocol::HELPER_VERSION {
        bail!("daemon helper version handshake is incompatible");
    }
    println!(
        "{}",
        serde_json::json!({
            "helperVersion": hello.helper_version,
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

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::{fs::PermissionsExt, net::UnixListener},
    };

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
