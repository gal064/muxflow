use std::{
    io::Read,
    path::PathBuf,
    process::{Child, ChildStderr, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use super::ConnectionSpec;

mod control_master;
pub(super) use control_master::{
    SshLease, acquire_control_master, acquire_control_master_cancellable,
    acquire_control_master_for_socket, ssh_profile_control_socket,
};
pub(crate) use control_master::{close_all_control_masters, spawn_orphan_reaper};

pub(super) fn spawn_bridge(
    connection: &ConnectionSpec,
    ssh_lease: Option<&SshLease>,
) -> Result<Child, String> {
    let measured = matches!(connection, ConnectionSpec::Ssh { .. });
    if measured {
        crate::perf_log::record_remote_operation(
            crate::perf_log::RemoteOperation::InteractiveBridgeSpawnAttempt,
        );
    }
    let result = (|| {
        connection.validate()?;
        let mut command = match connection {
            ConnectionSpec::Local => {
                let mut command = Command::new(host_helper_path()?);
                command.args(["bridge", "--stdio"]);
                command
            }
            ConnectionSpec::Ssh {
                target,
                config_path,
                ..
            } => {
                let mut command = ssh_base(config_path.as_deref());
                command.arg("-T");
                ssh_lease
                    .ok_or("SSH bridge startup requires an acquired control-master lease")?
                    .configure(&mut command, target, config_path.as_deref())?;
                command
                    .arg(target)
                    .arg("$HOME/.local/bin/muxflow-host bridge --stdio");
                command
            }
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Keep the child's stderr instead of discarding it. Without this, every
            // way a bridge or daemon can fail to start — an over-long AF_UNIX socket
            // path, a refused SSH key, a missing helper — reached the user as the
            // single generic string "host closed during handshake" (M10-E058).
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("failed to start host bridge: {error}"))
    })();
    if measured {
        crate::perf_log::record_remote_operation(if result.is_ok() {
            crate::perf_log::RemoteOperation::InteractiveBridgeSpawnSuccess
        } else {
            crate::perf_log::RemoteOperation::InteractiveBridgeSpawnFailure
        });
    }
    result
}

/// Bound on retained bridge stderr. Large enough for a stack of `anyhow`
/// context lines or an OpenSSH refusal, small enough that a chatty or hostile
/// child cannot grow this buffer without limit.
const BRIDGE_STDERR_BYTES: usize = 2048;

/// Drains a bridge child's stderr on its own thread so the pipe can never fill
/// and block the child, retaining only the first [`BRIDGE_STDERR_BYTES`].
pub(super) struct BridgeStderr {
    buffer: Arc<Mutex<String>>,
    finished: Arc<Mutex<bool>>,
}

impl BridgeStderr {
    pub(super) fn capture(mut stderr: ChildStderr) -> Self {
        let buffer = Arc::new(Mutex::new(String::new()));
        let finished = Arc::new(Mutex::new(false));
        let writer = Arc::clone(&buffer);
        let done = Arc::clone(&finished);
        thread::spawn(move || {
            let mut raw = Vec::new();
            let mut chunk = [0_u8; 512];
            while let Ok(read) = stderr.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                if raw.len() < BRIDGE_STDERR_BYTES {
                    let room = BRIDGE_STDERR_BYTES - raw.len();
                    raw.extend_from_slice(&chunk[..read.min(room)]);
                }
            }
            *writer.lock().unwrap() = String::from_utf8_lossy(&raw).into_owned();
            *done.lock().unwrap() = true;
        });
        Self { buffer, finished }
    }

    /// Returns what the child wrote to stderr, waiting up to `grace` for it to
    /// flush. A failing child normally writes and exits immediately, so this
    /// wait is short; it exists only so a diagnostic is not lost to a race
    /// between the child's write and our own error path.
    pub(super) fn diagnostic(&self, grace: Duration) -> Option<String> {
        let deadline = Instant::now() + grace;
        loop {
            if *self.finished.lock().unwrap() {
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let text = self.buffer.lock().unwrap().trim().to_owned();
        (!text.is_empty()).then_some(text)
    }
}

/// Appends a bridge child's own stderr to a connection error, so a diagnosable
/// startup failure reaches the user as its real cause rather than as a generic
/// handshake message.
pub(super) fn with_bridge_diagnostic(error: String, stderr: Option<&BridgeStderr>) -> String {
    match stderr.and_then(|stderr| stderr.diagnostic(Duration::from_millis(300))) {
        Some(detail) => format!("{error}: {detail}"),
        None => error,
    }
}

pub(crate) fn spawn_bulk_bridge(
    connection: &ConnectionSpec,
    cancelled: &dyn Fn() -> bool,
) -> Result<Child, String> {
    let measured = matches!(connection, ConnectionSpec::Ssh { .. });
    if measured {
        crate::perf_log::record_remote_operation(
            crate::perf_log::RemoteOperation::BulkBridgeSpawnAttempt,
        );
    }
    let result = (|| {
        connection.validate()?;
        let mut command = match connection {
            ConnectionSpec::Local => {
                let mut command = Command::new(host_helper_path()?);
                command.args(["bridge", "--stdio"]);
                command
            }
            ConnectionSpec::Ssh {
                target,
                config_path,
                ..
            } => {
                let mut command = ssh_base(config_path.as_deref());
                command.arg("-T");
                // The pooled bridge process already owns a persistent bulk TCP
                // connection. A second SSH control master adds another process
                // and connection without improving reuse. Keep bulk isolated
                // from the interactive master and let the bridge pool own it.
                command.args(["-o", "ControlMaster=no", "-o", "ControlPath=none"]);
                command
                    .arg(target)
                    .arg("$HOME/.local/bin/muxflow-host bridge --stdio");
                command
            }
        };
        if cancelled() {
            return Err("bulk bridge establishment cancelled".into());
        }
        let child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("failed to start independent bulk bridge: {error}"))?;
        Ok(child)
    })();
    if measured {
        crate::perf_log::record_remote_operation(if result.is_ok() {
            crate::perf_log::RemoteOperation::BulkBridgeSpawnSuccess
        } else {
            crate::perf_log::RemoteOperation::BulkBridgeSpawnFailure
        });
    }
    result
}

pub(super) fn host_helper_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("ADE_HOST_HELPER_PATH") {
        return Ok(path.into());
    }
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut sibling_path = None;
    if let Some(parent) = current.parent() {
        let sibling = parent.join("muxflow-host");
        if sibling.is_file() {
            return Ok(sibling);
        }
        sibling_path = Some(sibling);
    }
    #[cfg(debug_assertions)]
    {
        for profile in ["debug", "release"] {
            let candidate = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../target")
                .join(profile)
                .join("muxflow-host");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    // The banner this produces is the only thing the user sees, so it names the
    // path that was checked and the command that fills it (P12-U004).
    Err(format!(
        "muxflow-host helper is not installed beside the desktop{}. Rebuild the app with `pnpm --dir apps/desktop tauri build --bundles app`, which stages the helper, or run `release/macos/build-package.sh` for the packaged flow.",
        sibling_path
            .map(|path| format!(" (looked for {})", path.display()))
            .unwrap_or_default()
    ))
}

fn ssh_base(config_path: Option<&str>) -> Command {
    let mut command = Command::new("ssh");
    if let Some(path) = config_path {
        command.arg("-F").arg(path);
    }
    command.args([
        "-o",
        "BatchMode=yes",
        // A 1 s probe with two allowances tears the session down after any two
        // second stall — a laptop lid, a Wi-Fi roam, a busy uplink — and the
        // reconnect that follows costs a full snapshot and a reseed of every
        // pane. 15 s with three allowances still notices a genuinely dead peer
        // inside a minute, which is what a keepalive is for.
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        // A reconnect attempted right after a wake meets a route that is still
        // black-holed, and the OS default leaves it stuck there for about 75 s —
        // long enough to outlive the backoff step that scheduled it. This bounds
        // the handshake and key exchange as well as the connect, so it has to
        // stay generous enough for a loaded host or a ProxyJump chain.
        "-o",
        "ConnectTimeout=10",
    ]);
    command
}

/// Options that suit the interactive control lane and only that lane.
///
/// Terminal output is highly repetitive text and compresses five to ten times
/// over, and keystrokes are the latency-critical traffic, so the control
/// connection wants compression and a low-delay class. The bulk lane wants
/// neither: compressing an already-compressed multi-gigabyte transfer is pure
/// CPU cost, and a bulk transfer is not low-delay traffic. These are properties
/// of the master connection, so they are applied where masters are created.
fn apply_control_lane_options(command: &mut Command) {
    command.args(["-o", "Compression=yes", "-o", "IPQoS=lowdelay"]);
}
