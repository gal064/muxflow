use std::{
    env, fs,
    io::{BufReader, Read},
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use tmux_agent_protocol::{
    HELPER_VERSION, HOST_CAPABILITIES, envelope, read_frame_sync,
    v1::{self, envelope::Payload},
    write_frame_sync,
};
use uuid::Uuid;

const CHUNK_BYTES: usize = 1024 * 1024;
const FIVE_GIB: u64 = 5 * 1024 * 1024 * 1024;
const CLEANUP_THRESHOLD: u64 = 1024 * 1024 * 1024;

#[derive(Clone)]
enum Mode {
    Local { host: String },
    Ssh { config: String, target: String },
}

struct Bridge {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_request: u64,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug)]
struct UploadResult {
    bytes: u64,
    digest: String,
    final_path: String,
}

#[derive(Debug)]
struct DownloadResult {
    bytes: u64,
    digest: String,
    max_chunk: usize,
}

fn main() -> Result<(), String> {
    let arguments: Vec<String> = env::args().collect();
    let (action, mode) = parse_arguments(&arguments)?;
    match action.as_str() {
        "acceptance" => run_acceptance(mode),
        "parent-swap" => run_parent_swap(mode),
        _ => Err("action must be acceptance or parent-swap".into()),
    }
}

fn parse_arguments(arguments: &[String]) -> Result<(String, Mode), String> {
    let action = arguments.get(1).ok_or(
        "usage: transfer-test-driver <acceptance|parent-swap> <local HOST|ssh CONFIG TARGET>",
    )?;
    let mode = match arguments.get(2).map(String::as_str) {
        Some("local") => Mode::Local {
            host: arguments.get(3).ok_or("host binary is required")?.clone(),
        },
        Some("ssh") => Mode::Ssh {
            config: arguments.get(3).ok_or("SSH config is required")?.clone(),
            target: arguments.get(4).ok_or("SSH target is required")?.clone(),
        },
        _ => return Err("connection must be local or ssh".into()),
    };
    Ok((action.clone(), mode))
}

fn spawn_bridge(mode: &Mode, bulk: bool) -> Result<Child, String> {
    let mut command = match mode {
        Mode::Local { host } => {
            let mut command = Command::new(host);
            command.args(["bridge", "--stdio"]);
            command
        }
        Mode::Ssh { config, target } => {
            let mut command = Command::new("ssh");
            command.args(["-F", config, "-T"]);
            if bulk {
                command.args(["-o", "ControlMaster=no", "-o", "ControlPath=none"]);
            }
            command
                .arg(target)
                .arg("$HOME/.local/bin/muxflow-host bridge --stdio");
            command
        }
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("could not spawn bridge: {error}"))
}

fn connect(
    mode: &Mode,
    bulk: bool,
    expected_server: &str,
    connection_epoch: u64,
) -> Result<(Bridge, v1::ServerHello), String> {
    let mut child = spawn_bridge(mode, bulk)?;
    let mut stdin = child.stdin.take().ok_or("bridge stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("bridge stdout unavailable")?;
    let mut reader = BufReader::new(stdout);
    write_frame_sync(
        &mut stdin,
        &envelope(
            1,
            0,
            Payload::ClientHello(v1::ClientHello {
                desktop_version: "phase7-deterministic-driver".into(),
                requested_capabilities: HOST_CAPABILITIES,
                expected_helper_version: HELPER_VERSION.into(),
                bulk_connection: bulk,
                expected_server_identity: expected_server.into(),
                connection_epoch,
            }),
        ),
    )
    .map_err(|error| error.to_string())?;
    let frame = read_frame_sync(&mut reader)
        .map_err(|error| error.to_string())?
        .ok_or("bridge closed during handshake")?;
    let Some(Payload::ServerHello(hello)) = frame.payload else {
        return Err("bridge omitted ServerHello".into());
    };
    if hello.read_only {
        return Err(format!("helper is read-only: {}", hello.incompatibility));
    }
    if bulk
        && (hello.server_identity != expected_server || hello.connection_epoch != connection_epoch)
    {
        return Err("bulk identity/epoch was not echoed exactly".into());
    }
    Ok((
        Bridge {
            child,
            stdin,
            reader,
            next_request: 2,
        },
        hello,
    ))
}

fn run_acceptance(mode: Mode) -> Result<(), String> {
    let transfer_bytes = env::var("ADE_PHASE7_BYTES")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()
        .map_err(|error| format!("invalid ADE_PHASE7_BYTES: {error}"))?
        .unwrap_or(FIVE_GIB);
    if transfer_bytes == 0 {
        return Err("ADE_PHASE7_BYTES must be nonzero".into());
    }
    let started = Instant::now();
    let (mut control, hello) = connect(&mode, false, "", 0)?;
    let active = active_root(&mut control)?;
    let epoch = 9_007_199_254_740_993;
    let (mut upload_lane, _) = connect(&mode, true, &hello.server_identity, epoch)?;
    let (mut download_lane, _) = connect(&mode, true, &hello.server_identity, epoch)?;

    let mut matrix = fault_matrix(&mut control, &mut upload_lane)?;
    two_lane_rename_uploads(&mut upload_lane, &mut download_lane)?;
    matrix["twoLaneRenameDistinct"] = serde_json::Value::Bool(true);
    let upload_id = Uuid::new_v4().to_string();
    let upload_name = format!("phase7-five-gib-{upload_id}.bin");
    let descriptor = prepare_upload(
        &mut upload_lane,
        &upload_id,
        &upload_name,
        transfer_bytes,
        v1::CollisionPolicy::Fail,
        true,
        false,
    )?;
    if descriptor.total_bytes != transfer_bytes || !descriptor.final_path.is_empty() {
        return Err("upload preflight exposed a path or changed its exact u64 total".into());
    }

    // Once an active partial exceeds the cleanup threshold, a preflight on the
    // other independent bridge must not unlink it. This specifically guards
    // process/connection-local active registries.
    let warmup_target = if transfer_bytes > CLEANUP_THRESHOLD {
        CLEANUP_THRESHOLD + 1
    } else {
        0
    };
    let zeroes = vec![0_u8; CHUNK_BYTES];
    let mut upload_offset = 0_u64;
    let mut upload_hasher = blake3::Hasher::new();
    while upload_offset < warmup_target {
        let amount = usize::try_from((warmup_target - upload_offset).min(CHUNK_BYTES as u64))
            .map_err(|_| "warmup chunk did not fit usize")?;
        write_upload(
            &mut upload_lane,
            &upload_id,
            upload_offset,
            transfer_bytes,
            &zeroes[..amount],
        )?;
        upload_hasher.update(&zeroes[..amount]);
        upload_offset += amount as u64;
    }
    let cross_bridge_cleanup_safe = if warmup_target > 0 {
        let cleanup_id = Uuid::new_v4().to_string();
        let cleanup_name = format!("cleanup-trigger-{cleanup_id}");
        prepare_upload(
            &mut download_lane,
            &cleanup_id,
            &cleanup_name,
            1,
            v1::CollisionPolicy::Fail,
            false,
            false,
        )?;
        cancel_upload(&mut download_lane, &cleanup_id)?;
        true
    } else {
        false
    };

    let download_name = env::var("ADE_PHASE7_DOWNLOAD_FILE")
        .unwrap_or_else(|_| "phase7-five-gib-source.bin".into());
    let download_id = format!("download-{}", Uuid::new_v4());
    let download_descriptor =
        start_download(&mut download_lane, &active, &download_name, &download_id)?;
    if !download_descriptor.total_known || download_descriptor.total_bytes != transfer_bytes {
        return Err(format!(
            "download preflight changed its exact u64 total: expected {transfer_bytes}, got {}",
            download_descriptor.total_bytes
        ));
    }

    let (result_tx, result_rx) = mpsc::channel();
    let upload_tx = result_tx.clone();
    let upload_thread = std::thread::spawn(move || {
        let result = finish_upload(
            upload_lane,
            upload_id,
            transfer_bytes,
            upload_offset,
            upload_hasher,
            zeroes,
        );
        let _ = upload_tx.send(("upload", result.map(TransferResult::Upload)));
    });
    let download_thread = std::thread::spawn(move || {
        let result = finish_download(download_lane, download_id, transfer_bytes);
        let _ = result_tx.send(("download", result.map(TransferResult::Download)));
    });

    let mut upload_result = None;
    let mut download_result = None;
    let mut max_control_latency_ms = 0_u128;
    let mut control_probes = 0_u64;
    while upload_result.is_none() || download_result.is_none() {
        let probe_started = Instant::now();
        list_root(&mut control, &active)?;
        max_control_latency_ms = max_control_latency_ms.max(probe_started.elapsed().as_millis());
        control_probes += 1;
        match result_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(("upload", Ok(TransferResult::Upload(result)))) => upload_result = Some(result),
            Ok(("download", Ok(TransferResult::Download(result)))) => {
                download_result = Some(result)
            }
            Ok((name, Err(error))) => return Err(format!("{name} lane failed: {error}")),
            Ok((name, _)) => return Err(format!("{name} lane returned mismatched evidence")),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => return Err(format!("transfer evidence channel failed: {error}")),
        }
    }
    upload_thread.join().map_err(|_| "upload thread panicked")?;
    download_thread
        .join()
        .map_err(|_| "download thread panicked")?;
    let upload = upload_result.unwrap();
    let download = download_result.unwrap();
    if upload.bytes != transfer_bytes
        || download.bytes != transfer_bytes
        || upload.digest != download.digest
        || max_control_latency_ms >= 1_500
    {
        return Err("final byte/digest/control-responsiveness gate failed".into());
    }

    println!(
        "{}",
        serde_json::json!({
            "uploadBytes": upload.bytes.to_string(),
            "downloadBytes": download.bytes.to_string(),
            "exceedsU32": transfer_bytes > u64::from(u32::MAX),
            "uploadBlake3": upload.digest,
            "downloadBlake3": download.digest,
            "blake3Verified": true,
            "uploadFinalPath": upload.final_path,
            "chunkBytesMax": download.max_chunk,
            "boundedChunkBytes": CHUNK_BYTES,
            "independentBulkConnections": 2,
            "controlProbes": control_probes,
            "maxControlLatencyMs": max_control_latency_ms,
            "crossBridgeCleanupExercised": warmup_target > 0,
            "crossBridgeCleanupSafe": cross_bridge_cleanup_safe,
            "faultMatrix": matrix,
            "driverHwmKiB": process_hwm_kib(std::process::id())?,
            "elapsedMs": started.elapsed().as_millis(),
        })
    );
    Ok(())
}

fn two_lane_rename_uploads(first: &mut Bridge, second: &mut Bridge) -> Result<(), String> {
    let name = format!("two lane same name {}.bin", Uuid::new_v4());
    let first_id = Uuid::new_v4().to_string();
    let second_id = Uuid::new_v4().to_string();
    let first_bytes = b"first-lane";
    let second_bytes = b"second-lane-distinct";
    let first_descriptor = prepare_upload(
        first,
        &first_id,
        &name,
        first_bytes.len() as u64,
        v1::CollisionPolicy::Rename,
        false,
        false,
    )?;
    let second_descriptor = prepare_upload(
        second,
        &second_id,
        &name,
        second_bytes.len() as u64,
        v1::CollisionPolicy::Rename,
        false,
        false,
    )?;
    if first_descriptor.destination_name == second_descriptor.destination_name {
        return Err(format!(
            "two independent upload lanes reserved the same rename destination \
             (requested={name:?} first={:?} first_renamed={} second={:?} second_renamed={})",
            first_descriptor.destination_name,
            first_descriptor.collision_renamed,
            second_descriptor.destination_name,
            second_descriptor.collision_renamed,
        ));
    }
    write_upload(first, &first_id, 0, first_bytes.len() as u64, first_bytes)?;
    write_upload(
        second,
        &second_id,
        0,
        second_bytes.len() as u64,
        second_bytes,
    )?;
    let first_commit = commit_upload(
        first,
        &first_id,
        blake3::hash(first_bytes).to_hex().as_ref(),
    )?;
    let second_commit = commit_upload(
        second,
        &second_id,
        blake3::hash(second_bytes).to_hex().as_ref(),
    )?;
    if !first_commit.verified
        || !second_commit.verified
        || first_commit.final_path == second_commit.final_path
        || first_commit.blake3 == second_commit.blake3
    {
        return Err("two-lane rename upload verification was not distinct".into());
    }
    Ok(())
}

enum TransferResult {
    Upload(UploadResult),
    Download(DownloadResult),
}

fn finish_upload(
    mut lane: Bridge,
    transfer_id: String,
    total: u64,
    mut offset: u64,
    mut hasher: blake3::Hasher,
    zeroes: Vec<u8>,
) -> Result<UploadResult, String> {
    while offset < total {
        let amount = usize::try_from((total - offset).min(CHUNK_BYTES as u64))
            .map_err(|_| "upload chunk did not fit usize")?;
        write_upload(&mut lane, &transfer_id, offset, total, &zeroes[..amount])?;
        hasher.update(&zeroes[..amount]);
        offset += amount as u64;
    }
    let digest = hasher.finalize().to_hex().to_string();
    let committed = commit_upload(&mut lane, &transfer_id, &digest)?;
    if !committed.verified
        || committed.total_bytes != total
        || committed.blake3 != digest
        || committed.final_path.is_empty()
    {
        return Err("upload commit did not return exact verified evidence".into());
    }
    Ok(UploadResult {
        bytes: offset,
        digest,
        final_path: committed.final_path,
    })
}

fn finish_download(
    mut lane: Bridge,
    transfer_id: String,
    expected: u64,
) -> Result<DownloadResult, String> {
    let mut offset = 0_u64;
    let mut max_chunk = 0_usize;
    let mut hasher = blake3::Hasher::new();
    loop {
        let response = lane_request(
            &mut lane,
            v1::Request {
                operation: v1::Operation::ReadDownloadChunk.into(),
                file: Some(v1::FileServiceRequest {
                    operation_id: transfer_id.clone(),
                    transfer_id: transfer_id.clone(),
                    offset,
                    chunk_bytes: CHUNK_BYTES as u32,
                    ..Default::default()
                }),
                ..Default::default()
            },
        )?;
        let chunk = response
            .file
            .and_then(|file| file.transfer_chunk)
            .ok_or("download response omitted chunk")?;
        if chunk.offset != offset || chunk.data.is_empty() && !chunk.eof {
            return Err("download returned a stale offset or empty non-EOF chunk".into());
        }
        max_chunk = max_chunk.max(chunk.data.len());
        if max_chunk > CHUNK_BYTES {
            return Err("download exceeded the bounded chunk size".into());
        }
        hasher.update(&chunk.data);
        offset = offset
            .checked_add(chunk.data.len() as u64)
            .ok_or("download offset overflow")?;
        if chunk.eof {
            let digest = hasher.finalize().to_hex().to_string();
            if !chunk.total_known
                || offset != expected
                || chunk.total_bytes != expected
                || chunk.blake3 != digest
            {
                return Err("download final byte/BLAKE3 verification failed".into());
            }
            return Ok(DownloadResult {
                bytes: offset,
                digest,
                max_chunk,
            });
        }
    }
}

fn fault_matrix(control: &mut Bridge, bulk: &mut Bridge) -> Result<serde_json::Value, String> {
    let id = Uuid::new_v4().to_string();
    let control_rejection = lane_error(
        control,
        prepare_request(
            &id,
            "control-body",
            1,
            v1::CollisionPolicy::Fail,
            false,
            false,
        ),
    )?;
    require_error(
        &control_rejection,
        "bulk_connection_required",
        "independent bulk",
    )?;

    let large_id = Uuid::new_v4().to_string();
    let large = lane_error(
        bulk,
        prepare_request(
            &large_id,
            "unconfirmed-large",
            500 * 1024 * 1024 + 1,
            v1::CollisionPolicy::Fail,
            false,
            false,
        ),
    )?;
    require_error(&large, "upload_preflight_rejected", "500 MiB")?;

    let invalid_id = Uuid::new_v4().to_string();
    let invalid = lane_error(
        bulk,
        prepare_request(
            &invalid_id,
            "../escape",
            1,
            v1::CollisionPolicy::Fail,
            false,
            false,
        ),
    )?;
    require_error(&invalid, "upload_preflight_rejected", "basename")?;

    let png_id = Uuid::new_v4().to_string();
    let png = lane_error(
        bulk,
        prepare_request(
            &png_id,
            "oversized.png",
            25 * 1024 * 1024 + 1,
            v1::CollisionPolicy::Fail,
            false,
            true,
        ),
    )?;
    require_error(&png, "upload_preflight_rejected", "25 MiB")?;

    let collision_name = format!("opaque ' ; $()-{}.txt", Uuid::new_v4());
    let collision_id = Uuid::new_v4().to_string();
    prepare_upload(
        bulk,
        &collision_id,
        &collision_name,
        5,
        v1::CollisionPolicy::Fail,
        false,
        false,
    )?;
    write_upload(bulk, &collision_id, 0, 5, b"hello")?;
    commit_upload(
        bulk,
        &collision_id,
        blake3::hash(b"hello").to_hex().as_ref(),
    )?;
    let collide_id = Uuid::new_v4().to_string();
    let collision = lane_error(
        bulk,
        prepare_request(
            &collide_id,
            &collision_name,
            1,
            v1::CollisionPolicy::Fail,
            false,
            false,
        ),
    )?;
    require_error(&collision, "upload_preflight_rejected", "already exists")?;
    let rename_id = Uuid::new_v4().to_string();
    let renamed = prepare_upload(
        bulk,
        &rename_id,
        &collision_name,
        1,
        v1::CollisionPolicy::Rename,
        false,
        false,
    )?;
    if !renamed.collision_renamed || renamed.destination_name == collision_name {
        return Err("collision rename was not deterministic and explicit".into());
    }
    cancel_upload(bulk, &rename_id)?;

    let offset_id = Uuid::new_v4().to_string();
    prepare_upload(
        bulk,
        &offset_id,
        "bad-offset",
        2,
        v1::CollisionPolicy::Fail,
        false,
        false,
    )?;
    let offset_error = lane_error(bulk, write_request(&offset_id, 1, 2, vec![1]))?;
    require_error(&offset_error, "upload_chunk_rejected", "offset")?;
    cancel_upload(bulk, &offset_id)?;

    let chunk_id = Uuid::new_v4().to_string();
    prepare_upload(
        bulk,
        &chunk_id,
        "oversized-chunk",
        CHUNK_BYTES as u64 + 1,
        v1::CollisionPolicy::Fail,
        false,
        false,
    )?;
    let chunk_error = lane_error(
        bulk,
        write_request(
            &chunk_id,
            0,
            CHUNK_BYTES as u64 + 1,
            vec![0; CHUNK_BYTES + 1],
        ),
    )?;
    require_error(&chunk_error, "upload_chunk_rejected", "1 MiB")?;
    cancel_upload(bulk, &chunk_id)?;

    let digest_name = format!("bad-digest-{}", Uuid::new_v4());
    let digest_id = Uuid::new_v4().to_string();
    prepare_upload(
        bulk,
        &digest_id,
        &digest_name,
        3,
        v1::CollisionPolicy::Fail,
        false,
        false,
    )?;
    write_upload(bulk, &digest_id, 0, 3, b"bad")?;
    let digest_error = lane_error(bulk, commit_request(&digest_id, "wrong"))?;
    require_error(&digest_error, "upload_commit_not_published", "BLAKE3")?;
    let retry_id = Uuid::new_v4().to_string();
    prepare_upload(
        bulk,
        &retry_id,
        &digest_name,
        1,
        v1::CollisionPolicy::Fail,
        false,
        false,
    )?;
    cancel_upload(bulk, &retry_id)?;

    let cancelled_name = format!("cancelled-{}", Uuid::new_v4());
    let cancelled_id = Uuid::new_v4().to_string();
    prepare_upload(
        bulk,
        &cancelled_id,
        &cancelled_name,
        2,
        v1::CollisionPolicy::Fail,
        false,
        false,
    )?;
    write_upload(bulk, &cancelled_id, 0, 1, b"x")?;
    cancel_upload(bulk, &cancelled_id)?;
    let after_cancel = lane_error(bulk, write_request(&cancelled_id, 1, 2, vec![b'y']))?;
    require_error(&after_cancel, "upload_chunk_rejected", "not active")?;
    let reuse_id = Uuid::new_v4().to_string();
    prepare_upload(
        bulk,
        &reuse_id,
        &cancelled_name,
        1,
        v1::CollisionPolicy::Fail,
        false,
        false,
    )?;
    cancel_upload(bulk, &reuse_id)?;

    Ok(serde_json::json!({
        "controlBodiesRejected": true,
        "largeConfirmationRequired": true,
        "basenameTraversalRejected": true,
        "pngLimitEnforced": true,
        "shellSensitiveNameOpaque": true,
        "collisionFailAndRename": true,
        "staleOffsetRejected": true,
        "oversizedChunkRejected": true,
        "badDigestCleaned": true,
        "cancelCleanedAndReleased": true,
    }))
}

fn run_parent_swap(mode: Mode) -> Result<(), String> {
    let (control, hello) = connect(&mode, false, "", 0)?;
    drop(control);
    let (mut bulk, _) = connect(&mode, true, &hello.server_identity, 77)?;
    let transfer_id = Uuid::new_v4().to_string();
    let destination = format!("parent-swap-{transfer_id}");
    prepare_upload(
        &mut bulk,
        &transfer_id,
        &destination,
        4,
        v1::CollisionPolicy::Fail,
        false,
        false,
    )?;
    write_upload(&mut bulk, &transfer_id, 0, 4, b"swap")?;
    let staging = env::var("ADE_PHASE7_STAGING_DIR")
        .map(PathBuf::from)
        .map_err(|_| "ADE_PHASE7_STAGING_DIR is required for parent-swap")?;
    let backup = staging.with_file_name(format!("uploads-backup-{transfer_id}"));
    let attacker = staging.with_file_name(format!("uploads-attacker-{transfer_id}"));
    swap_staging_parent(&mode, &staging, &backup, &attacker)?;
    let commit = lane_error(
        &mut bulk,
        commit_request(&transfer_id, blake3::hash(b"swap").to_hex().as_ref()),
    )?;
    let commit_outcome = match commit.error_code.as_str() {
        "upload_commit_not_published" => "notPublished",
        "upload_commit_outcome_unknown" => "unknown",
        _ => "unexpected",
    };
    let commit_failed_closed = commit_outcome == "notPublished";
    let inspection = inspect_swapped_parent(&mode, &backup, &attacker, &destination, &transfer_id)?;
    restore_staging_parent(&mode, &staging, &backup, &attacker, &transfer_id)?;
    let reconciliation = lane_error(
        &mut bulk,
        v1::Request {
            operation: v1::Operation::ReconcileTerminalUpload.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: transfer_id.clone(),
                transfer_id: transfer_id.clone(),
                ..Default::default()
            }),
            ..Default::default()
        },
    )?;
    let reconciliation_absent = reconciliation.error_code == "upload_outcome_unavailable";
    let retry_id = Uuid::new_v4().to_string();
    let recovered = prepare_upload(
        &mut bulk,
        &retry_id,
        &format!("recovered-{retry_id}"),
        1,
        v1::CollisionPolicy::Fail,
        false,
        false,
    )
    .and_then(|_| cancel_upload(&mut bulk, &retry_id))
    .is_ok();
    let safe = commit_failed_closed
        && !inspection.attacker_published
        && !inspection.backup_published
        && !inspection.partial_leaked
        && reconciliation_absent
        && recovered;
    println!(
        "{}",
        serde_json::json!({
            "commitFailedClosed": commit_failed_closed,
            "commitOutcome": commit_outcome,
            "attackerDestinationPublished": inspection.attacker_published,
            "backupDestinationPublished": inspection.backup_published,
            "partialLeakedAfterParentSwap": inspection.partial_leaked,
            "reconciliationManifestAbsent": reconciliation_absent,
            "stagingRecovered": recovered,
            "parentSwapSafe": safe,
        })
    );
    if safe {
        Ok(())
    } else {
        Err("staging parent replacement left publication or cleanup residue".into())
    }
}

struct SwapInspection {
    attacker_published: bool,
    backup_published: bool,
    partial_leaked: bool,
}

fn swap_staging_parent(
    mode: &Mode,
    staging: &Path,
    backup: &Path,
    attacker: &Path,
) -> Result<(), String> {
    match mode {
        Mode::Local { .. } => {
            fs::rename(staging, backup).map_err(|error| error.to_string())?;
            fs::create_dir(attacker).map_err(|error| error.to_string())?;
            fs::set_permissions(attacker, fs::Permissions::from_mode(0o700))
                .map_err(|error| error.to_string())?;
            symlink(attacker, staging).map_err(|error| error.to_string())
        }
        Mode::Ssh { .. } => remote_shell(
            mode,
            &format!(
                "mv '{}' '{}'; mkdir -m 700 '{}'; ln -s '{}' '{}'",
                staging.display(),
                backup.display(),
                attacker.display(),
                attacker.display(),
                staging.display()
            ),
        ),
    }
}

fn inspect_swapped_parent(
    mode: &Mode,
    backup: &Path,
    attacker: &Path,
    destination: &str,
    transfer_id: &str,
) -> Result<SwapInspection, String> {
    let partial = backup.join(format!(".tmux-agent-upload-{transfer_id}.partial"));
    match mode {
        Mode::Local { .. } => Ok(SwapInspection {
            attacker_published: attacker.join(destination).exists(),
            backup_published: backup.join(destination).exists(),
            partial_leaked: partial.exists(),
        }),
        Mode::Ssh { .. } => {
            let output = remote_shell_output(
                mode,
                &format!(
                    "for path in '{}' '{}' '{}'; do if test -e \"$path\"; then printf 1; else printf 0; fi; done",
                    attacker.join(destination).display(),
                    backup.join(destination).display(),
                    partial.display()
                ),
            )?;
            let bytes = output.trim().as_bytes();
            if bytes.len() != 3 {
                return Err(format!("unexpected remote swap inspection: {output:?}"));
            }
            Ok(SwapInspection {
                attacker_published: bytes[0] == b'1',
                backup_published: bytes[1] == b'1',
                partial_leaked: bytes[2] == b'1',
            })
        }
    }
}

fn restore_staging_parent(
    mode: &Mode,
    staging: &Path,
    backup: &Path,
    attacker: &Path,
    transfer_id: &str,
) -> Result<(), String> {
    let partial = backup.join(format!(".tmux-agent-upload-{transfer_id}.partial"));
    match mode {
        Mode::Local { .. } => {
            fs::remove_file(staging).map_err(|error| error.to_string())?;
            if partial.exists() {
                fs::remove_file(partial).map_err(|error| error.to_string())?;
            }
            fs::rename(backup, staging).map_err(|error| error.to_string())?;
            fs::remove_dir(attacker).map_err(|error| error.to_string())
        }
        Mode::Ssh { .. } => remote_shell(
            mode,
            &format!(
                "rm -f '{}' '{}'; mv '{}' '{}'; rmdir '{}'",
                staging.display(),
                partial.display(),
                backup.display(),
                staging.display(),
                attacker.display()
            ),
        ),
    }
}

fn remote_shell(mode: &Mode, script: &str) -> Result<(), String> {
    let output = remote_command(mode, script)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().into())
    }
}

fn remote_shell_output(mode: &Mode, script: &str) -> Result<String, String> {
    let output = remote_command(mode, script)?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    String::from_utf8(output.stdout).map_err(|error| error.to_string())
}

fn remote_command(mode: &Mode, script: &str) -> Result<std::process::Output, String> {
    let Mode::Ssh { config, target } = mode else {
        return Err("remote command requested in local mode".into());
    };
    Command::new("ssh")
        .args(["-F", config, target])
        .arg(script)
        .output()
        .map_err(|error| error.to_string())
}

fn active_root(control: &mut Bridge) -> Result<v1::ActiveRoot, String> {
    let snapshot = lane_request(
        control,
        v1::Request {
            operation: v1::Operation::Subscribe.into(),
            scope: "full".into(),
            ..Default::default()
        },
    )?
    .snapshot
    .ok_or("subscribe omitted snapshot")?;
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.active)
        .or_else(|| snapshot.panes.first())
        .ok_or("fixture has no tmux pane")?;
    lane_request(
        control,
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
    .ok_or("active-root response omitted payload".into())
}

fn list_root(control: &mut Bridge, active: &v1::ActiveRoot) -> Result<(), String> {
    lane_request(
        control,
        v1::Request {
            operation: v1::Operation::ListDirectory.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: Uuid::new_v4().to_string(),
                root: active.root.clone(),
                root_token: active.root_token.clone(),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .map(|_| ())
}

fn start_download(
    lane: &mut Bridge,
    active: &v1::ActiveRoot,
    path: &str,
    transfer_id: &str,
) -> Result<v1::DownloadDescriptor, String> {
    lane_request(
        lane,
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
    .ok_or("download preflight omitted descriptor".into())
}

fn prepare_upload(
    lane: &mut Bridge,
    transfer_id: &str,
    destination: &str,
    total: u64,
    collision: v1::CollisionPolicy,
    confirmed: bool,
    image_png: bool,
) -> Result<v1::UploadDescriptor, String> {
    lane_request(
        lane,
        prepare_request(
            transfer_id,
            destination,
            total,
            collision,
            confirmed,
            image_png,
        ),
    )?
    .file
    .and_then(|file| file.upload)
    .ok_or("upload preflight omitted descriptor".into())
}

fn prepare_request(
    transfer_id: &str,
    destination: &str,
    total: u64,
    collision: v1::CollisionPolicy,
    confirmed: bool,
    image_png: bool,
) -> v1::Request {
    v1::Request {
        operation: v1::Operation::PrepareTerminalUpload.into(),
        file: Some(v1::FileServiceRequest {
            operation_id: transfer_id.into(),
            transfer_id: transfer_id.into(),
            destination: destination.into(),
            source_name: destination.into(),
            total_bytes: total,
            collision_policy: collision.into(),
            large_upload_confirmed: confirmed,
            image_png,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn write_upload(
    lane: &mut Bridge,
    transfer_id: &str,
    offset: u64,
    total: u64,
    data: &[u8],
) -> Result<(), String> {
    let acknowledged = lane_request(
        lane,
        write_request(transfer_id, offset, total, data.to_vec()),
    )?
    .file
    .and_then(|file| file.transfer_chunk)
    .ok_or("upload chunk omitted acknowledgement")?;
    if acknowledged.offset != offset + data.len() as u64 {
        return Err("upload acknowledged an unexpected offset".into());
    }
    Ok(())
}

fn write_request(transfer_id: &str, offset: u64, total: u64, content: Vec<u8>) -> v1::Request {
    v1::Request {
        operation: v1::Operation::WriteTerminalUploadChunk.into(),
        file: Some(v1::FileServiceRequest {
            operation_id: transfer_id.into(),
            transfer_id: transfer_id.into(),
            offset,
            total_bytes: total,
            content,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn commit_upload(
    lane: &mut Bridge,
    transfer_id: &str,
    digest: &str,
) -> Result<v1::UploadDescriptor, String> {
    lane_request(lane, commit_request(transfer_id, digest))?
        .file
        .and_then(|file| file.upload)
        .ok_or("upload commit omitted descriptor".into())
}

fn commit_request(transfer_id: &str, digest: &str) -> v1::Request {
    v1::Request {
        operation: v1::Operation::CommitTerminalUpload.into(),
        file: Some(v1::FileServiceRequest {
            operation_id: transfer_id.into(),
            transfer_id: transfer_id.into(),
            blake3: digest.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn cancel_upload(lane: &mut Bridge, transfer_id: &str) -> Result<(), String> {
    let descriptor = lane_request(
        lane,
        v1::Request {
            operation: v1::Operation::CancelTerminalUpload.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: transfer_id.into(),
                transfer_id: transfer_id.into(),
                ..Default::default()
            }),
            ..Default::default()
        },
    )?
    .file
    .and_then(|file| file.upload)
    .ok_or("upload cancellation omitted cleanup result")?;
    if !descriptor.cleanup_error.is_empty() {
        return Err(format!(
            "upload cancellation cleanup failed: {}",
            descriptor.cleanup_error
        ));
    }
    Ok(())
}

fn lane_request(lane: &mut Bridge, request: v1::Request) -> Result<v1::Response, String> {
    let response = lane_response(lane, request)?;
    if response.ok {
        Ok(response)
    } else {
        Err(format!(
            "{}: {}",
            response.error_code, response.display_message
        ))
    }
}

fn lane_error(lane: &mut Bridge, request: v1::Request) -> Result<v1::Response, String> {
    let response = lane_response(lane, request)?;
    if response.ok {
        Err("request unexpectedly succeeded".into())
    } else {
        Ok(response)
    }
}

fn lane_response(lane: &mut Bridge, request: v1::Request) -> Result<v1::Response, String> {
    let request_id = lane.next_request;
    lane.next_request = lane
        .next_request
        .checked_add(1)
        .ok_or("request ID overflow")?;
    write_frame_sync(
        &mut lane.stdin,
        &envelope(request_id, 0, Payload::Request(request)),
    )
    .map_err(|error| error.to_string())?;
    loop {
        let frame = read_frame_sync(&mut lane.reader)
            .map_err(|error| error.to_string())?
            .ok_or("bridge disconnected")?;
        if frame.request_id == request_id
            && let Some(Payload::Response(response)) = frame.payload
        {
            return Ok(response);
        }
    }
}

fn require_error(response: &v1::Response, code: &str, text: &str) -> Result<(), String> {
    if response.error_code == code && response.display_message.contains(text) {
        Ok(())
    } else {
        Err(format!(
            "expected {code}/{text:?}, got {}/{}",
            response.error_code, response.display_message
        ))
    }
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

// Darwin has no /proc, so the peak resident set comes from getrusage. That
// reports the calling process only, which is exactly what the single caller
// asks for. ru_maxrss is bytes on Darwin and kilobytes on Linux, so the
// conversion is explicit rather than inherited from the Linux reading.
#[cfg(target_os = "macos")]
fn process_hwm_kib(pid: u32) -> Result<u64, String> {
    if pid != std::process::id() {
        return Err(format!(
            "process {pid} peak RSS is only readable for the calling process on Darwin"
        ));
    }
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: getrusage fills the whole rusage the pointer refers to.
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if result != 0 {
        return Err(format!(
            "read process {pid} memory status: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: getrusage returned success, so the value is initialised.
    let usage = unsafe { usage.assume_init() };
    u64::try_from(usage.ru_maxrss)
        .map(|bytes| bytes / 1024)
        .map_err(|_| format!("process {pid} reported a negative peak RSS"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_hwm_kib(pid: u32) -> Result<u64, String> {
    Err(format!(
        "process {pid} peak RSS is unsupported on this platform"
    ))
}

#[allow(dead_code)]
fn drain(mut reader: impl Read) -> Vec<u8> {
    let mut output = Vec::new();
    let _ = reader.read_to_end(&mut output);
    output
}
