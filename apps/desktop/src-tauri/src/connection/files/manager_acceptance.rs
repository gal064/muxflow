use std::{
    fs,
    io::BufReader,
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tauri::ipc::{Channel, InvokeResponseBody};
use tmux_agent_protocol::{
    HELPER_VERSION, HOST_CAPABILITIES, envelope, read_frame_sync,
    v1::{self, envelope::Payload},
    write_frame_sync,
};
use uuid::Uuid;

use super::{
    download_manager::enqueue_acceptance_download,
    scheduler::{BulkBinding, acceptance_engine_counts, cancel_transfer, engine_test_lock},
    upload_manager::enqueue_acceptance_upload,
};
use crate::connection::{
    ConnectionSpec, TerminalClient,
    transport::{SshLease, acquire_control_master, spawn_bridge},
};

struct ControlLane {
    child: Child,
    _lease: Option<SshLease>,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_request: u64,
}

impl Drop for ControlLane {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl ControlLane {
    fn connect(connection: &ConnectionSpec) -> Result<(Self, v1::ServerHello), String> {
        let lease = acquire_control_master(connection)?;
        let mut child = spawn_bridge(connection, lease.as_ref())?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or("control bridge stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("control bridge stdout unavailable")?;
        let mut reader = BufReader::new(stdout);
        write_frame_sync(
            &mut stdin,
            &envelope(
                1,
                0,
                Payload::ClientHello(v1::ClientHello {
                    desktop_version: env!("CARGO_PKG_VERSION").into(),
                    requested_capabilities: HOST_CAPABILITIES,
                    expected_helper_version: HELPER_VERSION.into(),
                    bulk_connection: false,
                    ..Default::default()
                }),
            ),
        )
        .map_err(|error| error.to_string())?;
        let frame = read_frame_sync(&mut reader)
            .map_err(|error| error.to_string())?
            .ok_or("control bridge closed during handshake")?;
        let Some(Payload::ServerHello(hello)) = frame.payload else {
            return Err("control bridge omitted ServerHello".into());
        };
        if hello.read_only {
            return Err(format!(
                "control bridge is read-only: {}",
                hello.incompatibility
            ));
        }
        Ok((
            Self {
                child,
                _lease: lease,
                stdin,
                reader,
                next_request: 2,
            },
            hello,
        ))
    }

    fn request(&mut self, request: v1::Request) -> Result<v1::Response, String> {
        let request_id = self.next_request;
        self.next_request = self
            .next_request
            .checked_add(1)
            .ok_or("manager control request ID overflow")?;
        write_frame_sync(
            &mut self.stdin,
            &envelope(request_id, 0, Payload::Request(request)),
        )
        .map_err(|error| error.to_string())?;
        loop {
            let frame = read_frame_sync(&mut self.reader)
                .map_err(|error| error.to_string())?
                .ok_or("manager control bridge disconnected")?;
            if frame.request_id == request_id
                && let Some(Payload::Response(response)) = frame.payload
            {
                if !response.ok {
                    return Err(format!(
                        "{}: {}",
                        response.error_code, response.display_message
                    ));
                }
                return Ok(response);
            }
        }
    }

    fn active_root(&mut self) -> Result<v1::ActiveRoot, String> {
        let snapshot = self
            .request(v1::Request {
                operation: v1::Operation::Subscribe.into(),
                scope: "full".into(),
                ..Default::default()
            })?
            .snapshot
            .ok_or("manager subscribe omitted snapshot")?;
        let pane = snapshot
            .panes
            .iter()
            .find(|pane| pane.active)
            .or_else(|| snapshot.panes.first())
            .ok_or("manager fixture has no tmux pane")?;
        self.request(v1::Request {
            operation: v1::Operation::ResolveActiveRoot.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: Uuid::new_v4().to_string(),
                pane_id: pane.id.clone(),
                expected_server_identity: snapshot.server_identity,
                expected_topology_generation: snapshot.generation,
                ..Default::default()
            }),
            ..Default::default()
        })?
        .file
        .and_then(|file| file.active_root)
        .ok_or("manager active-root response omitted payload".into())
    }

    fn probe_root(&mut self, active: &v1::ActiveRoot) -> Result<(), String> {
        self.request(v1::Request {
            operation: v1::Operation::ListDirectory.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: Uuid::new_v4().to_string(),
                root: active.root.clone(),
                root_token: active.root_token.clone(),
                page_size: 1,
                ..Default::default()
            }),
            ..Default::default()
        })
        .map(|_| ())
    }
}

fn event_channel() -> (Channel<Value>, mpsc::Receiver<Value>) {
    let (sender, receiver) = mpsc::channel();
    let channel = Channel::new(move |body| {
        if let InvokeResponseBody::Json(value) = body
            && let Ok(value) = serde_json::from_str(&value)
        {
            let _ = sender.send(value);
        }
        Ok(())
    });
    (channel, receiver)
}

fn connection_from_env() -> Result<ConnectionSpec, String> {
    match std::env::var("ADE_PHASE7_MANAGER_MODE").as_deref() {
        Ok("local") => Ok(ConnectionSpec::Local),
        Ok("ssh") => Ok(ConnectionSpec::Ssh {
            profile_id: "phase7-manager".into(),
            target: std::env::var("ADE_PHASE7_MANAGER_SSH_TARGET")
                .map_err(|_| "manager SSH target is required")?,
            config_path: Some(
                std::env::var("ADE_PHASE7_MANAGER_SSH_CONFIG")
                    .map_err(|_| "manager SSH config is required")?,
            ),
        }),
        _ => Err("ADE_PHASE7_MANAGER_MODE must be local or ssh".into()),
    }
}

fn terminal_event(events: &[Value]) -> Option<Value> {
    events
        .iter()
        .find(|event| event.get("terminal") == Some(&Value::Bool(true)))
        .cloned()
}

#[cfg(target_os = "linux")]
fn process_hwm_kib() -> Result<u64, String> {
    fs::read_to_string("/proc/self/status")
        .map_err(|error| error.to_string())?
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .ok_or("desktop manager process status omitted VmHWM".into())
}

#[cfg(target_os = "macos")]
fn process_hwm_kib() -> Result<u64, String> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if result != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let bytes = unsafe { usage.assume_init() }.ru_maxrss;
    u64::try_from(bytes)
        .map(|value| value / 1024)
        .map_err(|_| "desktop manager maximum RSS was negative".into())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_hwm_kib() -> Result<u64, String> {
    Err("desktop manager maximum RSS is unsupported on this platform".into())
}

#[test]
fn production_desktop_managers_transfer_exact_bytes_through_canonical_engine() {
    if std::env::var("ADE_PHASE7_MANAGER_ACCEPTANCE").as_deref() != Ok("1") {
        return;
    }
    let _serial = engine_test_lock();
    run_manager_acceptance().unwrap();
}

fn run_manager_acceptance() -> Result<(), String> {
    let bytes = std::env::var("ADE_PHASE7_BYTES")
        .map_err(|_| "ADE_PHASE7_BYTES is required")?
        .parse::<u64>()
        .map_err(|error| error.to_string())?;
    let source = PathBuf::from(
        std::env::var("ADE_PHASE7_MANAGER_UPLOAD_SOURCE")
            .map_err(|_| "manager upload source is required")?,
    );
    let download_destination = PathBuf::from(
        std::env::var("ADE_PHASE7_MANAGER_DOWNLOAD_DESTINATION")
            .map_err(|_| "manager download destination is required")?,
    );
    let download_source = std::env::var("ADE_PHASE7_MANAGER_DOWNLOAD_FILE")
        .map_err(|_| "manager download source is required")?;
    let result_path = PathBuf::from(
        std::env::var("ADE_PHASE7_MANAGER_RESULT")
            .map_err(|_| "manager result path is required")?,
    );
    if fs::metadata(&source)
        .map_err(|error| error.to_string())?
        .len()
        != bytes
    {
        return Err("manager upload fixture size differs from ADE_PHASE7_BYTES".into());
    }
    let connection = connection_from_env()?;
    let (mut control, hello) = ControlLane::connect(&connection)?;
    let active_root = control.active_root()?;
    let epoch = 9_007_199_254_740_995_u64;
    let client = Arc::new(TerminalClient::new());
    client
        .ready
        .store(true, std::sync::atomic::Ordering::Release);
    client
        .terminal_epoch
        .store(epoch, std::sync::atomic::Ordering::Release);
    *client.server_identity.lock().unwrap() = hello.server_identity.clone();
    let binding = BulkBinding::capture(client, hello.server_identity, epoch)?;

    let (upload_channel, upload_rx) = event_channel();
    let (download_channel, download_rx) = event_channel();
    let upload_id = enqueue_acceptance_upload(
        connection.clone(),
        binding.clone(),
        source.clone(),
        format!("phase7-manager-{}.bin", Uuid::new_v4()),
        upload_channel,
    )?;
    let download_id = enqueue_acceptance_download(
        connection.clone(),
        binding.clone(),
        active_root.root.clone(),
        active_root.root_token.clone(),
        download_source,
        download_destination.clone(),
        download_channel,
    )?;

    let active_deadline = Instant::now() + Duration::from_secs(10);
    let mut max_active = 0_usize;
    while Instant::now() < active_deadline {
        let (active, _) = acceptance_engine_counts();
        max_active = max_active.max(active);
        if active == 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if max_active != 2 {
        return Err("desktop manager jobs did not occupy exactly two canonical workers".into());
    }

    let (cancel_channel, cancel_rx) = event_channel();
    let cancel_id = enqueue_acceptance_upload(
        connection,
        binding,
        source,
        format!("phase7-manager-cancel-{}.bin", Uuid::new_v4()),
        cancel_channel,
    )?;
    let cancel_response = cancel_transfer(&cancel_id)?;

    let timeout = std::env::var("ADE_PHASE7_MANAGER_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1_200_u64);
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let mut upload_events = Vec::new();
    let mut download_events = Vec::new();
    let mut cancel_events = Vec::new();
    let mut control_probes = 0_u64;
    let mut max_control_latency = Duration::ZERO;
    while Instant::now() < deadline {
        upload_events.extend(upload_rx.try_iter());
        download_events.extend(download_rx.try_iter());
        cancel_events.extend(cancel_rx.try_iter());
        let (active, queued) = acceptance_engine_counts();
        max_active = max_active.max(active);
        if active > 2 {
            return Err(format!(
                "canonical manager engine exposed {active} active jobs"
            ));
        }
        if terminal_event(&upload_events).is_some()
            && terminal_event(&download_events).is_some()
            && terminal_event(&cancel_events).is_some()
            && active == 0
            && queued == 0
        {
            break;
        }
        let started = Instant::now();
        control.probe_root(&active_root)?;
        max_control_latency = max_control_latency.max(started.elapsed());
        control_probes += 1;
        std::thread::sleep(Duration::from_millis(50));
    }
    let upload = terminal_event(&upload_events).ok_or("manager upload omitted terminal event")?;
    let download =
        terminal_event(&download_events).ok_or("manager download omitted terminal event")?;
    let cancelled = terminal_event(&cancel_events).ok_or("queued manager cancel omitted event")?;
    let expected_bytes = bytes.to_string();
    for (label, event) in [("upload", &upload), ("download", &download)] {
        if event["state"] != "completed"
            || event["outcome"] != "published"
            || event["transferredBytes"].as_str() != Some(expected_bytes.as_str())
            || event["totalBytes"].as_str() != Some(expected_bytes.as_str())
        {
            return Err(format!(
                "manager {label} terminal contract mismatch: {event}"
            ));
        }
    }
    if upload["blake3"] != download["blake3"] {
        return Err("manager upload/download BLAKE3 values differ".into());
    }
    if cancelled["state"] != "cancelled"
        || cancelled["outcome"] != "notPublished"
        || cancelled.get("failureKind").is_some()
    {
        return Err(format!(
            "queued manager cancellation contract mismatch: {cancelled}"
        ));
    }
    let destination_size = fs::metadata(&download_destination)
        .map_err(|error| error.to_string())?
        .len();
    if destination_size != bytes {
        return Err(format!(
            "manager download published {destination_size} bytes, expected {bytes}"
        ));
    }
    let saw_states = |events: &[Value]| {
        ["queued", "running", "verifying", "completed"]
            .into_iter()
            .all(|state| events.iter().any(|event| event["state"] == state))
    };
    let result = json!({
        "desktopManager5GiB": bytes == 5_368_709_120,
        "uploadBytes": bytes.to_string(),
        "downloadBytes": destination_size.to_string(),
        "blake3Verified": true,
        "uploadBlake3": upload["blake3"],
        "downloadBlake3": download["blake3"],
        "uploadFinalPath": upload["destination"],
        "downloadFinalPath": download["destination"],
        "canonicalEngineMaxActive": max_active,
        "exactTwoWorkers": max_active == 2,
        "queuedCancellation": cancelled["state"] == "cancelled",
        "cancelDisposition": serde_json::to_value(cancel_response).map_err(|error| error.to_string())?,
        "uploadLifecycleComplete": saw_states(&upload_events),
        "downloadLifecycleComplete": saw_states(&download_events),
        "controlProbes": control_probes,
        "maxControlLatencyMs": max_control_latency.as_millis(),
        "desktopManagerHwmKiB": process_hwm_kib()?,
        "uploadTransferId": upload_id,
        "downloadTransferId": download_id,
    });
    fs::write(
        result_path,
        serde_json::to_vec_pretty(&result).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}
