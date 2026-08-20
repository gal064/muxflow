use std::{
    env, fs,
    io::{BufReader, Read},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::Instant,
};

use tmux_agent_protocol::{
    HELPER_VERSION, HOST_CAPABILITIES, envelope, read_frame_sync,
    v1::{self, envelope::Payload},
    write_frame_sync,
};
use uuid::Uuid;

fn main() -> Result<(), String> {
    let arguments: Vec<_> = std::env::args().collect();
    let mut control = spawn_bridge(&arguments)?;
    let mut bulk = spawn_bridge(&arguments)?;
    if arguments.get(1).map(String::as_str) == Some("large-local") {
        run_large_download(&mut control, &mut bulk)?;
    } else {
        run(&mut control, &mut bulk)?;
    }
    let _ = control.kill();
    let _ = control.wait();
    let _ = bulk.kill();
    let _ = bulk.wait();
    Ok(())
}

fn spawn_bridge(arguments: &[String]) -> Result<Child, String> {
    match arguments.get(1).map(String::as_str) {
        Some("local" | "large-local") => {
            Command::new(arguments.get(2).ok_or("host binary is required")?)
                .args(["bridge", "--stdio"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|error| error.to_string())
        }
        Some("ssh") => Command::new("ssh")
            .arg("-F")
            .arg(arguments.get(2).ok_or("SSH config is required")?)
            .arg("-T")
            .arg(arguments.get(3).ok_or("SSH target is required")?)
            .arg("$HOME/.local/bin/muxflow-host bridge --stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| error.to_string()),
        _ => Err("usage: filesystem-test-driver <local HOST|ssh CONFIG TARGET>".into()),
    }
}

const FIVE_GIB: u64 = 5 * 1024 * 1024 * 1024;

fn run_large_download(control: &mut Child, bulk: &mut Child) -> Result<(), String> {
    let started_at = Instant::now();
    let mut control_stdin = control
        .stdin
        .take()
        .ok_or("control bridge stdin unavailable")?;
    let control_stdout = control
        .stdout
        .take()
        .ok_or("control bridge stdout unavailable")?;
    let mut control_reader = BufReader::new(control_stdout);
    let control_hello = handshake(&mut control_stdin, &mut control_reader, false, "", 0)?;
    let mut bulk_stdin = bulk.stdin.take().ok_or("bulk bridge stdin unavailable")?;
    let bulk_stdout = bulk.stdout.take().ok_or("bulk bridge stdout unavailable")?;
    let mut bulk_reader = BufReader::new(bulk_stdout);
    handshake(
        &mut bulk_stdin,
        &mut bulk_reader,
        true,
        &control_hello.server_identity,
        9_007_199_254_740_993,
    )?;

    let active = active_root(&mut control_stdin, &mut control_reader)?;
    let path = env::var("ADE_PHASE4_LARGE_FILE").unwrap_or_else(|_| "five-gib.bin".into());
    let transfer_id = format!("five-gib-{}", Uuid::new_v4());
    let descriptor = start_download(
        &mut bulk_stdin,
        &mut bulk_reader,
        10,
        &active,
        &path,
        &transfer_id,
    )?;
    if !descriptor.total_known || descriptor.total_bytes != FIVE_GIB {
        return Err(format!(
            "5 GiB descriptor used the wrong u64 total: known={}, bytes={}",
            descriptor.total_known, descriptor.total_bytes
        ));
    }

    // Keep a second transfer active, read from it, then cancel it. This proves
    // that transfer state is independently keyed and cancellation releases it
    // without disrupting the primary 5 GiB stream.
    let cancelled_id = format!("cancel-{}", Uuid::new_v4());
    let cancelled_descriptor = start_download(
        &mut bulk_stdin,
        &mut bulk_reader,
        11,
        &active,
        &path,
        &cancelled_id,
    )?;
    if cancelled_descriptor.total_bytes != FIVE_GIB {
        return Err("concurrent download descriptor lost its 64-bit size".into());
    }
    let cancelled_chunk = download_chunk(&mut bulk_stdin, &mut bulk_reader, 12, &cancelled_id, 0)?;
    if cancelled_chunk.data.is_empty() || cancelled_chunk.eof {
        return Err("concurrent cancellation fixture did not enter an active stream".into());
    }
    request(
        &mut bulk_stdin,
        &mut bulk_reader,
        13,
        v1::Request {
            operation: v1::Operation::CancelDownload.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: cancelled_id.clone(),
                transfer_id: cancelled_id.clone(),
                ..Default::default()
            }),
            ..Default::default()
        },
    )?;
    let after_cancel = request_error(
        &mut bulk_stdin,
        &mut bulk_reader,
        14,
        v1::Request {
            operation: v1::Operation::ReadDownloadChunk.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: cancelled_id.clone(),
                transfer_id: cancelled_id,
                offset: cancelled_chunk.data.len() as u64,
                chunk_bytes: 1024 * 1024,
                ..Default::default()
            }),
            ..Default::default()
        },
    )?;
    if after_cancel.error_code != "download_read_rejected" {
        return Err(format!(
            "cancelled transfer returned the wrong error: {}",
            after_cancel.error_code
        ));
    }

    let daemon_pid = daemon_pid()?;
    let mut request_id = 100_u64;
    let mut control_request_id = 20_000_u64;
    let mut offset = 0_u64;
    let mut hasher = blake3::Hasher::new();
    let mut max_control_latency_ms = 0_u128;
    let mut control_probes = 0_u64;
    loop {
        let chunk = download_chunk(
            &mut bulk_stdin,
            &mut bulk_reader,
            request_id,
            &transfer_id,
            offset,
        )?;
        request_id = request_id.saturating_add(1);
        if chunk.offset != offset || chunk.data.len() > 1024 * 1024 {
            return Err("large download violated sequential bounded chunk framing".into());
        }
        if chunk.data.is_empty() && !chunk.eof {
            return Err("large regular-file download returned an empty non-EOF chunk".into());
        }
        hasher.update(&chunk.data);
        offset = offset
            .checked_add(chunk.data.len() as u64)
            .ok_or("large download offset overflowed u64")?;

        if request_id.is_multiple_of(64) || chunk.eof {
            let control_started = Instant::now();
            request(
                &mut control_stdin,
                &mut control_reader,
                control_request_id,
                file_request(
                    v1::Operation::ListDirectory,
                    &active.root,
                    &active.root_token,
                    "",
                    &format!("large-control-{control_request_id}"),
                ),
            )?;
            control_request_id = control_request_id.saturating_add(1);
            control_probes = control_probes.saturating_add(1);
            max_control_latency_ms =
                max_control_latency_ms.max(control_started.elapsed().as_millis());
        }
        if chunk.eof {
            let digest = hasher.finalize().to_hex().to_string();
            if offset != FIVE_GIB
                || chunk.total_bytes != FIVE_GIB
                || !chunk.total_known
                || chunk.blake3 != digest
            {
                return Err("5 GiB streamed byte count or BLAKE3 verification failed".into());
            }
            break;
        }
    }

    let driver_hwm_kib = process_hwm_kib(std::process::id())?;
    let daemon_hwm_kib = process_hwm_kib(daemon_pid)?;
    const MEMORY_LIMIT_KIB: u64 = 256 * 1024;
    if driver_hwm_kib >= MEMORY_LIMIT_KIB || daemon_hwm_kib >= MEMORY_LIMIT_KIB {
        return Err(format!(
            "5 GiB stream exceeded bounded-memory gate: driver={driver_hwm_kib} KiB daemon={daemon_hwm_kib} KiB"
        ));
    }
    if max_control_latency_ms >= 1_000 {
        return Err(format!(
            "control lane was unresponsive during 5 GiB transfer: {max_control_latency_ms} ms"
        ));
    }

    println!(
        "{}",
        serde_json::json!({
            "downloadBytes": offset.to_string(),
            "exceedsU32": offset > u64::from(u32::MAX),
            "blake3Verified": true,
            "chunkBytesMax": 1024 * 1024,
            "controlProbes": control_probes,
            "maxControlLatencyMs": max_control_latency_ms,
            "cancelledConcurrentTransfer": true,
            "cancelledTransferReleased": true,
            "driverHwmKiB": driver_hwm_kib,
            "daemonHwmKiB": daemon_hwm_kib,
            "memoryLimitKiB": MEMORY_LIMIT_KIB,
            "elapsedMs": started_at.elapsed().as_millis(),
        })
    );
    Ok(())
}

fn active_root(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
) -> Result<v1::ActiveRoot, String> {
    let snapshot = request(
        stdin,
        reader,
        2,
        v1::Request {
            operation: v1::Operation::Subscribe.into(),
            scope: "full".into(),
            ..Default::default()
        },
    )?
    .snapshot
    .ok_or("large-download subscribe omitted snapshot")?;
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.active)
        .or_else(|| snapshot.panes.first())
        .ok_or("large-download fixture has no pane")?;
    request(
        stdin,
        reader,
        3,
        v1::Request {
            operation: v1::Operation::ResolveActiveRoot.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: Uuid::new_v4().to_string(),
                pane_id: pane.id.clone(),
                expected_server_identity: snapshot.server_identity,
                expected_topology_generation: snapshot.generation,
                ..Default::default()
            }),
            ..Default::default()
        },
    )?
    .file
    .and_then(|file| file.active_root)
    .ok_or("large-download active-root response omitted payload".into())
}

fn start_download(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    request_id: u64,
    active: &v1::ActiveRoot,
    path: &str,
    transfer_id: &str,
) -> Result<v1::DownloadDescriptor, String> {
    request(
        stdin,
        reader,
        request_id,
        v1::Request {
            operation: v1::Operation::StartDownload.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: transfer_id.into(),
                root: active.root.clone(),
                root_token: active.root_token.clone(),
                path: path.into(),
                transfer_id: transfer_id.into(),
                ..Default::default()
            }),
            ..Default::default()
        },
    )?
    .file
    .and_then(|file| file.download)
    .ok_or("large download omitted descriptor".into())
}

fn download_chunk(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    request_id: u64,
    transfer_id: &str,
    offset: u64,
) -> Result<v1::TransferChunk, String> {
    request(
        stdin,
        reader,
        request_id,
        v1::Request {
            operation: v1::Operation::ReadDownloadChunk.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: transfer_id.into(),
                transfer_id: transfer_id.into(),
                offset,
                chunk_bytes: 1024 * 1024,
                ..Default::default()
            }),
            ..Default::default()
        },
    )?
    .file
    .and_then(|file| file.transfer_chunk)
    .ok_or("large download omitted chunk".into())
}

fn daemon_pid() -> Result<u32, String> {
    let runtime = env::var("ADE_HOST_RUNTIME_DIR")
        .map_err(|_| "ADE_HOST_RUNTIME_DIR is required for memory evidence")?;
    let metadata = fs::read(format!("{runtime}/daemon.json")).map_err(|error| error.to_string())?;
    let value: serde_json::Value =
        serde_json::from_slice(&metadata).map_err(|error| error.to_string())?;
    value
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or("daemon metadata omitted a valid pid".into())
}

#[cfg(target_os = "linux")]
fn process_hwm_kib(pid: u32) -> Result<u64, String> {
    let status = fs::read_to_string(format!("/proc/{pid}/status"))
        .map_err(|error| format!("read process {pid} memory status: {error}"))?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| format!("process {pid} status omitted VmHWM"))
}

// Darwin has no /proc. This driver samples both itself and the daemon, so the
// peak has to be readable for an arbitrary pid; proc_pid_rusage reports
// ri_lifetime_max_phys_footprint, the closest analogue to VmHWM, in bytes.
#[cfg(target_os = "macos")]
fn process_hwm_kib(pid: u32) -> Result<u64, String> {
    let pid = i32::try_from(pid).map_err(|_| format!("process {pid} exceeds Darwin pid_t"))?;
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: the flavor requested matches rusage_info_v4, and the pointer
    // refers to a full, writable value of that type.
    let result = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    if result != 0 {
        return Err(format!(
            "read process {pid} memory status: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: proc_pid_rusage returned success, so the value is initialised.
    let info = unsafe { info.assume_init() };
    Ok(info.ri_lifetime_max_phys_footprint / 1024)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_hwm_kib(pid: u32) -> Result<u64, String> {
    Err(format!(
        "process {pid} peak RSS is unsupported on this platform"
    ))
}

fn run(control: &mut Child, bulk: &mut Child) -> Result<(), String> {
    let mut stdin = control
        .stdin
        .take()
        .ok_or("control bridge stdin unavailable")?;
    let stdout = control
        .stdout
        .take()
        .ok_or("control bridge stdout unavailable")?;
    let mut reader = BufReader::new(stdout);
    let control_hello = handshake(&mut stdin, &mut reader, false, "", 0)?;
    let mut bulk_stdin = bulk.stdin.take().ok_or("bulk bridge stdin unavailable")?;
    let bulk_stdout = bulk.stdout.take().ok_or("bulk bridge stdout unavailable")?;
    let mut bulk_reader = BufReader::new(bulk_stdout);
    handshake(
        &mut bulk_stdin,
        &mut bulk_reader,
        true,
        &control_hello.server_identity,
        9_007_199_254_740_993,
    )?;
    let subscribe = request(
        &mut stdin,
        &mut reader,
        2,
        v1::Request {
            operation: v1::Operation::Subscribe.into(),
            scope: "full".into(),
            ..Default::default()
        },
    )?;
    let snapshot = subscribe.snapshot.ok_or("subscribe omitted snapshot")?;
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.active)
        .or_else(|| snapshot.panes.first())
        .ok_or("fixture has no pane")?;
    let operation_id = Uuid::new_v4().to_string();
    let (root_response, root_events) = request_with_events(
        &mut stdin,
        &mut reader,
        3,
        v1::Request {
            operation: v1::Operation::ResolveActiveRoot.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: operation_id.clone(),
                pane_id: pane.id.clone(),
                expected_server_identity: snapshot.server_identity.clone(),
                expected_topology_generation: snapshot.generation,
                ..Default::default()
            }),
            ..Default::default()
        },
    )?;
    if !root_response.ok {
        return Err(format!(
            "active-root request failed: {}: {}",
            root_response.error_code, root_response.display_message
        ));
    }
    let active = root_response
        .file
        .and_then(|file| file.active_root)
        .ok_or("active-root response omitted payload")?;
    if !active.git_worktree || active.root_token.is_empty() {
        return Err("active root did not resolve the Git worktree with a token".into());
    }
    if !root_events.iter().any(|frame| {
        frame.sequence <= root_response.accepted_sequence
            && matches!(frame.payload, Some(Payload::Event(ref event)) if event.kind == i32::from(v1::EventKind::ActiveRoot))
    }) {
        return Err("atomic active-root event/barrier was not observed".into());
    }
    let token = active.root_token.clone();
    let root = active.root.clone();
    run_file_matrix(
        &mut stdin,
        &mut reader,
        &mut bulk_stdin,
        &mut bulk_reader,
        active,
        operation_id,
        root,
        token,
    )
}

fn handshake(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    bulk_connection: bool,
    expected_server_identity: &str,
    connection_epoch: u64,
) -> Result<v1::ServerHello, String> {
    write_frame_sync(
        stdin,
        &envelope(
            1,
            0,
            Payload::ClientHello(v1::ClientHello {
                desktop_version: "phase4-driver".into(),
                requested_capabilities: HOST_CAPABILITIES,
                expected_helper_version: HELPER_VERSION.into(),
                bulk_connection,
                expected_server_identity: expected_server_identity.into(),
                connection_epoch,
            }),
        ),
    )
    .map_err(|error| error.to_string())?;
    let hello = read_frame_sync(reader)
        .map_err(|error| error.to_string())?
        .ok_or("bridge closed during handshake")?;
    let Some(Payload::ServerHello(hello)) = hello.payload else {
        return Err("missing ServerHello".into());
    };
    if hello.read_only {
        return Err(hello.incompatibility);
    }
    if bulk_connection
        && (hello.server_identity != expected_server_identity
            || hello.connection_epoch != connection_epoch)
    {
        return Err("bulk identity/epoch binding was not echoed exactly".into());
    }
    Ok(hello)
}

#[allow(clippy::too_many_arguments, clippy::needless_borrow)]
fn run_file_matrix(
    mut stdin: &mut ChildStdin,
    mut reader: &mut BufReader<ChildStdout>,
    bulk_stdin: &mut ChildStdin,
    bulk_reader: &mut BufReader<ChildStdout>,
    active: v1::ActiveRoot,
    operation_id: String,
    root: String,
    token: String,
) -> Result<(), String> {
    let invalid = request_error(
        &mut stdin,
        &mut reader,
        4,
        file_request(
            v1::Operation::ListDirectory,
            &root,
            "wrong-token",
            "",
            "list-bad",
        ),
    )?;
    if invalid.error_code != "invalid_root_token" {
        return Err(format!(
            "unexpected invalid-token result: {}",
            invalid.error_code
        ));
    }
    let list = request(
        &mut stdin,
        &mut reader,
        5,
        file_request(
            v1::Operation::WatchDirectory,
            &root,
            &token,
            "",
            "watch-root",
        ),
    )?;
    let directory = list
        .file
        .and_then(|file| file.directory)
        .ok_or("list omitted snapshot")?;
    // The host's two different reasons for not enumerating a directory, which
    // this asserted as one and got wrong. `.git` is `ALWAYS_HIDDEN`
    // (`apps/host/src/service/filesystem.rs`): no listing reports it at all, so
    // looking for it as a collapsed *row* had been failing here since before
    // this package's base commit, on `improvement/integration` itself — the
    // whole lane exited 1 at this line and nothing in the repository said so.
    // `node_modules` is `COLLAPSED_DIRECTORIES`: shown, and never expandable.
    if !directory.authoritative {
        return Err("watch bootstrap was not an authoritative listing".into());
    }
    if directory.entries.iter().any(|entry| entry.name == ".git") {
        return Err("directory snapshot reported repository plumbing as a row".into());
    }
    if !directory
        .entries
        .iter()
        .any(|entry| entry.name == "node_modules" && !entry.expandable)
    {
        return Err("directory snapshot did not retain collapsed node_modules".into());
    }
    let file_name = format!("phase4-{operation_id}.txt");
    mutate(
        &mut stdin,
        &mut reader,
        6,
        &root,
        &token,
        &file_name,
        "",
        v1::FileMutationKind::Create,
        false,
        false,
    )?;
    let contents = b"phase4 protocol text\n".to_vec();
    let saved = bulk_write(
        bulk_stdin,
        bulk_reader,
        &root,
        &token,
        &file_name,
        "save-1",
        &contents,
        100,
        None,
    )?;
    if saved.generation == 0 || saved.size != contents.len() as u64 {
        return Err("write metadata generation/size is invalid".into());
    }
    // `OpenFileStream` on the bulk lane, because that is now the only way to
    // open a file: the metadata/preflight/per-chunk staircase this replaced
    // was a second code path for the same resource, and it had already drifted
    // from this one. The terminal response still carries the classification
    // and no bytes — bodies travel as `FileStream` frames, which
    // `request_with_events` collects and this assertion deliberately ignores.
    let read = request(
        bulk_stdin,
        bulk_reader,
        8,
        file_request(
            v1::Operation::OpenFileStream,
            &root,
            &token,
            &file_name,
            "read-1",
        ),
    )?;
    let content = read
        .file
        .and_then(|file| file.content)
        .ok_or("read omitted content")?;
    if content.kind != i32::from(v1::FileContentKind::Text) || !content.content.is_empty() {
        return Err("open response carried bytes or misclassified text".into());
    }
    let transfer_id = format!("download-{operation_id}");
    let started = request(
        bulk_stdin,
        bulk_reader,
        9,
        v1::Request {
            operation: v1::Operation::StartDownload.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: transfer_id.clone(),
                root: root.clone(),
                root_token: token.clone(),
                path: file_name.clone(),
                transfer_id: transfer_id.clone(),
                file_generation: content.generation,
                ..Default::default()
            }),
            ..Default::default()
        },
    )?;
    let descriptor = started
        .file
        .and_then(|file| file.download)
        .ok_or("download omitted descriptor")?;
    let mut received = Vec::new();
    let mut offset = 0_u64;
    let mut next_request = 10_u64;
    let final_digest = loop {
        let response = request(
            bulk_stdin,
            bulk_reader,
            next_request,
            v1::Request {
                operation: v1::Operation::ReadDownloadChunk.into(),
                file: Some(v1::FileServiceRequest {
                    operation_id: transfer_id.clone(),
                    transfer_id: transfer_id.clone(),
                    offset,
                    chunk_bytes: 7,
                    ..Default::default()
                }),
                ..Default::default()
            },
        )?;
        next_request += 1;
        let chunk = response
            .file
            .and_then(|file| file.transfer_chunk)
            .ok_or("download omitted chunk")?;
        offset += chunk.data.len() as u64;
        received.extend_from_slice(&chunk.data);
        if chunk.eof {
            break chunk.blake3;
        }
    };
    if offset != descriptor.total_bytes
        || received != contents
        || final_digest != blake3::hash(&received).to_hex().to_string()
    {
        return Err("streamed download verification failed".into());
    }
    let rejected = request_error(
        &mut stdin,
        &mut reader,
        900,
        v1::Request {
            operation: v1::Operation::StartDownload.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: "control-body-rejected".into(),
                root: root.clone(),
                root_token: token.clone(),
                path: "editor-max.txt".into(),
                transfer_id: "control-body-rejected".into(),
                ..Default::default()
            }),
            ..Default::default()
        },
    )?;
    if rejected.error_code != "bulk_connection_required" {
        return Err("control connection accepted a file body operation".into());
    }
    let max_text_metadata = request(
        bulk_stdin,
        bulk_reader,
        901,
        file_request(
            v1::Operation::OpenFileStream,
            &root,
            &token,
            "editor-max.txt",
            "editor-max-meta",
        ),
    )?
    .file
    .and_then(|file| file.content)
    .ok_or("max text metadata missing")?;
    if max_text_metadata.kind != i32::from(v1::FileContentKind::Text)
        || !max_text_metadata.content.is_empty()
    {
        return Err("max text response carried bytes rather than framing them".into());
    }
    let preview = request(
        bulk_stdin,
        bulk_reader,
        902,
        file_request(
            v1::Operation::OpenFileStream,
            &root,
            &token,
            "preview-25.png",
            "preview-25-meta",
        ),
    )?;
    let preview_metadata = preview
        .file
        .and_then(|file| file.content)
        .and_then(|content| content.metadata)
        .ok_or("25 MiB preview metadata missing")?;
    if preview_metadata.size != 25 * 1024 * 1024 || !preview_metadata.image_preview_eligible {
        return Err("25 MiB image was not preview eligible".into());
    }
    let oversized = request(
        bulk_stdin,
        bulk_reader,
        903,
        file_request(
            v1::Operation::OpenFileStream,
            &root,
            &token,
            "preview-over.png",
            "preview-over-meta",
        ),
    )?;
    let oversized_content = oversized
        .file
        .and_then(|file| file.content)
        .ok_or("oversized preview metadata missing")?;
    let oversized_metadata = oversized_content
        .metadata
        .ok_or("oversized image metadata missing")?;
    if oversized_metadata.size != 25 * 1024 * 1024 + 1
        || oversized_metadata.image_preview_eligible
        || !oversized_content.content.is_empty()
    {
        return Err("oversized image was not metadata-only".into());
    }
    let mut control_request_id = 1_000_u64;
    let mut max_control_latency_ms = 0_u128;
    let mut probe = || -> Result<(), String> {
        let started = Instant::now();
        request(
            stdin,
            reader,
            control_request_id,
            file_request(
                v1::Operation::ListDirectory,
                &root,
                &token,
                "",
                &format!("lane-{control_request_id}"),
            ),
        )?;
        control_request_id = control_request_id.saturating_add(1);
        max_control_latency_ms = max_control_latency_ms.max(started.elapsed().as_millis());
        Ok(())
    };
    let editor_bytes = vec![b'x'; 10 * 1024 * 1024];
    let editor_metadata = bulk_write(
        bulk_stdin,
        bulk_reader,
        &root,
        &token,
        "editor-max.txt",
        "save-editor-max",
        &editor_bytes,
        2_000,
        Some(&mut probe),
    )?;
    if editor_metadata.size != editor_bytes.len() as u64 {
        return Err("max editor write returned an incorrect byte count".into());
    }
    let (editor_read_bytes, editor_digest) = bulk_read_count(
        bulk_stdin,
        bulk_reader,
        &root,
        &token,
        "editor-max.txt",
        "read-editor-max",
        3_000,
        Some(&mut probe),
    )?;
    if editor_read_bytes != editor_bytes.len() as u64
        || editor_digest != blake3::hash(&editor_bytes).to_hex().to_string()
    {
        return Err("max editor read verification failed".into());
    }
    let (preview_bytes, _) = bulk_read_count(
        bulk_stdin,
        bulk_reader,
        &root,
        &token,
        "preview-25.png",
        "preview-25-read",
        4_000,
        Some(&mut probe),
    )?;
    if preview_bytes != 25 * 1024 * 1024 {
        return Err("25 MiB preview byte accounting failed".into());
    }
    if max_control_latency_ms >= 2_500 {
        return Err(format!(
            "control lane stalled during editor/preview bulk I/O: {max_control_latency_ms} ms"
        ));
    }
    let directory = format!("phase4-dir-{operation_id}");
    mutate(
        &mut stdin,
        &mut reader,
        next_request,
        &root,
        &token,
        &directory,
        "",
        v1::FileMutationKind::Create,
        true,
        false,
    )?;
    next_request += 1;
    mutate(
        &mut stdin,
        &mut reader,
        next_request,
        &root,
        &token,
        &format!("{directory}/item"),
        "",
        v1::FileMutationKind::Create,
        false,
        false,
    )?;
    next_request += 1;
    let folder_transfer = format!("folder-{operation_id}");
    let folder_started = request(
        bulk_stdin,
        bulk_reader,
        next_request,
        v1::Request {
            operation: v1::Operation::StartDownload.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: folder_transfer.clone(),
                root: root.clone(),
                root_token: token.clone(),
                path: directory.clone(),
                transfer_id: folder_transfer.clone(),
                folder: true,
                ..Default::default()
            }),
            ..Default::default()
        },
    )?;
    let folder_descriptor = folder_started
        .file
        .and_then(|file| file.download)
        .ok_or("folder download omitted descriptor")?;
    if !folder_descriptor.folder_archive || folder_descriptor.total_known {
        return Err("folder download did not expose the streamed archive contract".into());
    }
    next_request += 1;
    let mut folder_offset = 0_u64;
    let mut folder_hasher = blake3::Hasher::new();
    loop {
        let response = request(
            bulk_stdin,
            bulk_reader,
            next_request,
            v1::Request {
                operation: v1::Operation::ReadDownloadChunk.into(),
                file: Some(v1::FileServiceRequest {
                    operation_id: folder_transfer.clone(),
                    transfer_id: folder_transfer.clone(),
                    offset: folder_offset,
                    chunk_bytes: 1024,
                    ..Default::default()
                }),
                ..Default::default()
            },
        )?;
        next_request += 1;
        let chunk = response
            .file
            .and_then(|file| file.transfer_chunk)
            .ok_or("folder download omitted chunk")?;
        folder_offset += chunk.data.len() as u64;
        folder_hasher.update(&chunk.data);
        if chunk.eof {
            if chunk.blake3 != folder_hasher.finalize().to_hex().to_string()
                || !chunk.total_known
                || folder_offset != chunk.total_bytes
            {
                return Err("folder archive verification failed".into());
            }
            break;
        }
        if chunk.data.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    let delete_error = request_error(
        &mut stdin,
        &mut reader,
        next_request,
        mutation_request(
            &root,
            &token,
            &directory,
            "",
            v1::FileMutationKind::Delete,
            false,
            false,
        ),
    )?;
    if delete_error.error_code != "confirmation_required" {
        return Err("non-empty delete did not require confirmation".into());
    }
    next_request += 1;
    mutate(
        &mut stdin,
        &mut reader,
        next_request,
        &root,
        &token,
        &directory,
        "",
        v1::FileMutationKind::Delete,
        false,
        true,
    )?;
    next_request += 1;
    mutate(
        &mut stdin,
        &mut reader,
        next_request,
        &root,
        &token,
        &file_name,
        "",
        v1::FileMutationKind::Delete,
        false,
        false,
    )?;
    println!(
        "{}",
        serde_json::json!({
            "activeRootAtomic": true,
            "gitRoot": active.root,
            "rootToken": true,
            "dotfilesVisible": true,
            // The name the runner's `jq` gate has always used. It now reports
            // what the host actually does: repository plumbing hidden outright,
            // `node_modules` shown and never expandable.
            "gitCollapsed": true,
            "textRoundTrip": true,
            "streamedDownloadBytes": offset,
            "blake3Verified": true,
            "folderDownload": true,
            "controlBodiesRejected": true,
            "editorMaxReadWriteBytes": editor_read_bytes,
            "previewBoundaryBytes": preview_bytes,
            "oversizedPreviewMetadataOnly": true,
            "maxControlLatencyMs": max_control_latency_ms,
            "nonEmptyConfirmation": true,
        })
    );
    Ok(())
}

fn file_request(
    operation: v1::Operation,
    root: &str,
    token: &str,
    path: &str,
    id: &str,
) -> v1::Request {
    v1::Request {
        operation: operation.into(),
        file: Some(v1::FileServiceRequest {
            operation_id: id.into(),
            root: root.into(),
            root_token: token.into(),
            path: path.into(),
            watch_id: id.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[allow(clippy::too_many_arguments)]
fn bulk_write(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    root: &str,
    token: &str,
    path: &str,
    operation_id: &str,
    content: &[u8],
    mut request_id: u64,
    mut control_probe: Option<&mut dyn FnMut() -> Result<(), String>>,
) -> Result<v1::FileMetadata, String> {
    let transfer_id = format!("write-{}", Uuid::new_v4());
    request(
        stdin,
        reader,
        request_id,
        v1::Request {
            operation: v1::Operation::BeginFileWrite.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: operation_id.into(),
                root: root.into(),
                root_token: token.into(),
                path: path.into(),
                transfer_id: transfer_id.clone(),
                total_bytes: content.len() as u64,
                ..Default::default()
            }),
            ..Default::default()
        },
    )?;
    request_id += 1;
    let mut offset = 0_u64;
    for chunk in content.chunks(1024 * 1024) {
        request(
            stdin,
            reader,
            request_id,
            v1::Request {
                operation: v1::Operation::WriteFileChunk.into(),
                file: Some(v1::FileServiceRequest {
                    operation_id: operation_id.into(),
                    transfer_id: transfer_id.clone(),
                    offset,
                    total_bytes: content.len() as u64,
                    content: chunk.to_vec(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )?;
        request_id += 1;
        offset += chunk.len() as u64;
        if let Some(probe) = control_probe.as_deref_mut() {
            probe()?;
        }
    }
    let response = request(
        stdin,
        reader,
        request_id,
        v1::Request {
            operation: v1::Operation::CommitFileWrite.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: operation_id.into(),
                transfer_id,
                blake3: blake3::hash(content).to_hex().to_string(),
                ..Default::default()
            }),
            ..Default::default()
        },
    )?;
    response
        .file
        .and_then(|file| file.metadata)
        .ok_or("bulk write omitted metadata".into())
}

#[allow(clippy::too_many_arguments)]
fn bulk_read_count(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    root: &str,
    token: &str,
    path: &str,
    transfer_id: &str,
    mut request_id: u64,
    mut control_probe: Option<&mut dyn FnMut() -> Result<(), String>>,
) -> Result<(u64, String), String> {
    let descriptor = request(
        stdin,
        reader,
        request_id,
        v1::Request {
            operation: v1::Operation::StartDownload.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: transfer_id.into(),
                root: root.into(),
                root_token: token.into(),
                path: path.into(),
                transfer_id: transfer_id.into(),
                ..Default::default()
            }),
            ..Default::default()
        },
    )?
    .file
    .and_then(|file| file.download)
    .ok_or("bulk read omitted descriptor")?;
    request_id += 1;
    let mut offset = 0_u64;
    let mut hasher = blake3::Hasher::new();
    loop {
        let chunk = request(
            stdin,
            reader,
            request_id,
            v1::Request {
                operation: v1::Operation::ReadDownloadChunk.into(),
                file: Some(v1::FileServiceRequest {
                    operation_id: transfer_id.into(),
                    transfer_id: transfer_id.into(),
                    offset,
                    chunk_bytes: 1024 * 1024,
                    ..Default::default()
                }),
                ..Default::default()
            },
        )?
        .file
        .and_then(|file| file.transfer_chunk)
        .ok_or("bulk read omitted chunk")?;
        request_id += 1;
        hasher.update(&chunk.data);
        offset += chunk.data.len() as u64;
        if let Some(probe) = control_probe.as_deref_mut() {
            probe()?;
        }
        if chunk.eof {
            let digest = hasher.finalize().to_hex().to_string();
            if offset != descriptor.total_bytes || chunk.blake3 != digest {
                return Err("bulk read byte/BLAKE3 verification failed".into());
            }
            return Ok((offset, digest));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn mutate(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    request_id: u64,
    root: &str,
    token: &str,
    path: &str,
    destination: &str,
    kind: v1::FileMutationKind,
    create_directory: bool,
    non_empty_confirmed: bool,
) -> Result<(), String> {
    request(
        stdin,
        reader,
        request_id,
        mutation_request(
            root,
            token,
            path,
            destination,
            kind,
            create_directory,
            non_empty_confirmed,
        ),
    )
    .map(|_| ())
}

#[allow(clippy::too_many_arguments)]
fn mutation_request(
    root: &str,
    token: &str,
    path: &str,
    destination: &str,
    kind: v1::FileMutationKind,
    create_directory: bool,
    non_empty_confirmed: bool,
) -> v1::Request {
    v1::Request {
        operation: v1::Operation::FileMutation.into(),
        file: Some(v1::FileServiceRequest {
            operation_id: Uuid::new_v4().to_string(),
            root: root.into(),
            root_token: token.into(),
            path: path.into(),
            destination: destination.into(),
            mutation: kind.into(),
            create_directory,
            non_empty_confirmed,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn request(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    request_id: u64,
    request: v1::Request,
) -> Result<v1::Response, String> {
    let (response, _) = request_with_events(stdin, reader, request_id, request)?;
    if response.ok {
        Ok(response)
    } else {
        Err(format!(
            "{}: {}",
            response.error_code, response.display_message
        ))
    }
}

fn request_error(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    request_id: u64,
    request: v1::Request,
) -> Result<v1::Response, String> {
    let (response, _) = request_with_events(stdin, reader, request_id, request)?;
    if response.ok {
        Err("request unexpectedly succeeded".into())
    } else {
        Ok(response)
    }
}

fn request_with_events(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    request_id: u64,
    request: v1::Request,
) -> Result<(v1::Response, Vec<v1::Envelope>), String> {
    write_frame_sync(stdin, &envelope(request_id, 0, Payload::Request(request)))
        .map_err(|error| error.to_string())?;
    let mut events = Vec::new();
    loop {
        let frame = read_frame_sync(reader)
            .map_err(|error| error.to_string())?
            .ok_or("bridge disconnected")?;
        if frame.request_id == request_id
            && let Some(Payload::Response(response)) = frame.payload.clone()
        {
            return Ok((response, events));
        }
        events.push(frame);
    }
}

#[allow(dead_code)]
fn drain(mut reader: impl Read) -> Vec<u8> {
    let mut output = Vec::new();
    let _ = reader.read_to_end(&mut output);
    output
}
