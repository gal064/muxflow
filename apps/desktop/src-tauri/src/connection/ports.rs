//! Local port forwards from SSH hosts: the native half of the Ports tab.
//!
//! Each forward is its own `ssh -N -L` process, not a forward added to the
//! control master. A master forward outlives the client that asked for it,
//! needs a separate `-O cancel`, and vanishes silently when the master resets;
//! a dedicated process is removed by ending it and fails where it can be seen.
//! The price is one key-auth handshake per forward, the same trade the bulk
//! lane already makes.
//!
//! Nothing here is persisted. Forwards live exactly as long as this process:
//! see [`GUARD_SCRIPT`] for how that holds even through `kill -9`.

use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use super::{ConnectionSpec, acquire_control_master, transport::ssh_base};

/// Tells the renderer to re-read [`ports_list`]; the payload is the profile id.
const CHANGED_EVENT: &str = "port-forwards-changed";

/// How `ssh -v` reports our listener, bound or not. Only our own port counts:
/// the host's ssh config may add `LocalForward`s that ssh also binds (or fails
/// to, when the control master already holds them), and those must neither
/// make a forward `active` nor fail it. That is also why there is no
/// `ExitOnForwardFailure`: it would fail a forward over a port nobody typed.
fn listening_line(local_port: u16) -> String {
    format!("Local forwarding listening on 127.0.0.1 port {local_port}.")
}

fn bind_failure_prefix(local_port: u16) -> String {
    format!("bind [127.0.0.1]:{local_port}: ")
}

/// Lines of ssh's own diagnostics kept for a failed forward's message.
const DIAGNOSTIC_LINES: usize = 3;
const DIAGNOSTIC_LINE_CHARS: usize = 300;

/// Ties the forward's lifetime to a pipe this process holds.
///
/// The wrapper's stdin is a pipe whose only write end lives in Muxflow. The OS
/// closes it however Muxflow ends — quit, crash or `kill -9` — and the watcher
/// then kills ssh within milliseconds. Removing a forward takes the same path:
/// drop the pipe. Portable across Linux and macOS, unlike `PR_SET_PDEATHSIG`.
///
/// The watcher reads a duplicate (fd 3) because a non-interactive shell gives
/// an asynchronous list `/dev/null` as stdin unless it is redirected
/// explicitly. When ssh exits on its own, the watcher subshell is ended and its
/// `cat` lingers only until Muxflow drops the pipe for the failed forward.
const GUARD_SCRIPT: &str = r#"exec 3<&0
"$@" </dev/null 3<&- &
forward=$!
( cat >/dev/null; kill "$forward" 2>/dev/null ) <&3 >/dev/null 2>&1 &
watch=$!
exec 3<&-
wait "$forward"
status=$?
kill "$watch" 2>/dev/null
exit "$status""#;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ForwardState {
    Starting,
    Active,
    Failed,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PortForward {
    profile_id: String,
    remote_port: u16,
    local_port: u16,
    state: ForwardState,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DetectedPort {
    port: u16,
    /// Known only for the user's own processes; `ss -p` cannot name others.
    process: Option<String>,
}

struct Entry {
    id: u64,
    forward: PortForward,
    /// The guard pipe. Dropping it ends the forward.
    guard: Option<ChildStdin>,
}

type Notify = Arc<dyn Fn(&str) + Send + Sync>;

/// Every forward this process owns, across all hosts.
#[derive(Default)]
pub struct PortForwards(Arc<Registry>);

#[derive(Default)]
struct Registry {
    entries: Mutex<Vec<Entry>>,
    next_id: AtomicU64,
}

impl PortForwards {
    /// Ends every forward. The guard pipe would do this anyway when the process
    /// exits; doing it at quit keeps a graceful exit from depending on that.
    pub fn stop_all(&self) {
        self.0.entries.lock().unwrap().clear();
    }
}

impl Registry {
    fn start(
        self: &Arc<Self>,
        profile_id: &str,
        remote_port: u16,
        local_port: u16,
        mut command: Command,
        notify: Notify,
    ) -> Result<(), String> {
        if remote_port == 0 || local_port == 0 {
            return Err("ports must be between 1 and 65535".into());
        }
        let mut entries = self.entries.lock().unwrap();
        // A failed forward is retried by forwarding again, and its local port
        // is free, so failed entries never block a new one.
        entries.retain(|entry| {
            entry.forward.state != ForwardState::Failed
                || (entry.forward.local_port != local_port
                    && !(entry.forward.profile_id == profile_id
                        && entry.forward.remote_port == remote_port))
        });
        if entries.iter().any(|entry| {
            entry.forward.profile_id == profile_id && entry.forward.remote_port == remote_port
        }) {
            return Err(format!("port {remote_port} is already forwarded"));
        }
        if entries
            .iter()
            .any(|entry| entry.forward.local_port == local_port)
        {
            return Err(format!(
                "local port {local_port} is already used by another forward"
            ));
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("failed to start ssh: {error}"))?;
        let guard = child.stdin.take();
        let stderr = child.stderr.take().expect("stderr is piped");
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        entries.push(Entry {
            id,
            forward: PortForward {
                profile_id: profile_id.to_owned(),
                remote_port,
                local_port,
                state: ForwardState::Starting,
                error: None,
            },
            guard,
        });
        drop(entries);
        notify(profile_id);
        let registry = Arc::clone(self);
        let owner = profile_id.to_owned();
        if let Err(error) = thread::Builder::new()
            .name("port-forward".into())
            .spawn(move || registry.monitor(id, &owner, local_port, child, stderr, &notify))
        {
            self.entries.lock().unwrap().retain(|entry| entry.id != id);
            return Err(format!("failed to monitor ssh: {error}"));
        }
        Ok(())
    }

    /// Follows one forward's ssh until it exits: `active` once our listener is
    /// bound, `failed` with ssh's own words when the bind is refused or ssh
    /// ends while the forward is still wanted.
    fn monitor(
        &self,
        id: u64,
        profile_id: &str,
        local_port: u16,
        mut child: Child,
        stderr: impl Read,
        notify: &Notify,
    ) {
        let listening = listening_line(local_port);
        let bind_failure = bind_failure_prefix(local_port);
        let mut diagnostics: VecDeque<String> = VecDeque::with_capacity(DIAGNOSTIC_LINES);
        // Split on bytes: a `ProxyCommand` may write anything to stderr, and one
        // non-UTF-8 line must not end the watch.
        for line in BufReader::new(stderr).split(b'\n').map_while(Result::ok) {
            // ssh ends its log lines with `\r\n`.
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if line.starts_with(&bind_failure) {
                // ssh carries on without the forward; failing it drops the
                // guard, which ends ssh.
                if self.fail(id, line.chars().take(DIAGNOSTIC_LINE_CHARS).collect()) {
                    notify(profile_id);
                }
            } else if line.ends_with(&listening) {
                if self.update(id, |forward| {
                    if forward.state == ForwardState::Starting {
                        forward.state = ForwardState::Active;
                    }
                }) {
                    notify(profile_id);
                }
            } else if is_diagnostic(line) {
                if diagnostics.len() == DIAGNOSTIC_LINES {
                    diagnostics.pop_front();
                }
                diagnostics.push_back(line.chars().take(DIAGNOSTIC_LINE_CHARS).collect());
            }
        }
        let status = child.wait();
        let error = if diagnostics.is_empty() {
            match status {
                Ok(status) => format!("ssh exited ({status})"),
                Err(error) => format!("ssh exited: {error}"),
            }
        } else {
            Vec::from(diagnostics).join("; ")
        };
        if self.fail(id, error) {
            notify(profile_id);
        }
    }

    /// Marks a still-wanted forward failed and drops its guard, which ends its
    /// ssh (or releases the guard's lingering `cat` once ssh is gone). A
    /// forward that already failed keeps its first, more specific error.
    fn fail(&self, id: u64, error: String) -> bool {
        let mut entries = self.entries.lock().unwrap();
        let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) else {
            return false;
        };
        entry.guard = None;
        if entry.forward.state == ForwardState::Failed {
            return false;
        }
        entry.forward.state = ForwardState::Failed;
        entry.forward.error = Some(error);
        true
    }

    fn update(&self, id: u64, change: impl FnOnce(&mut PortForward)) -> bool {
        let mut entries = self.entries.lock().unwrap();
        let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) else {
            return false;
        };
        change(&mut entry.forward);
        true
    }

    /// Removes matching forwards; dropping their guards ends their ssh.
    fn stop(&self, matches: impl Fn(&PortForward) -> bool) -> bool {
        let mut entries = self.entries.lock().unwrap();
        let before = entries.len();
        entries.retain(|entry| !matches(&entry.forward));
        entries.len() != before
    }

    fn list(&self) -> Vec<PortForward> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .map(|entry| entry.forward.clone())
            .collect()
    }
}

/// ssh's `-v` output minus the chatter: what is left is what went wrong.
fn is_diagnostic(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty()
        && ![
            "debug",
            "OpenSSH_",
            "Authenticated to",
            "Transferred:",
            "Bytes per second",
        ]
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

fn ssh_parts(connection: &ConnectionSpec) -> Result<(&str, &str, Option<&str>), String> {
    connection.validate()?;
    match connection {
        ConnectionSpec::Ssh {
            profile_id,
            target,
            config_path,
        } => Ok((profile_id, target, config_path.as_deref())),
        ConnectionSpec::Local => Err("port forwarding needs an SSH host".into()),
    }
}

fn forward_command(
    target: &str,
    config_path: Option<&str>,
    remote_port: u16,
    local_port: u16,
) -> Command {
    let mut ssh = ssh_base(config_path);
    ssh.args([
        "-v",
        "-N",
        "-o",
        "ControlMaster=no",
        "-o",
        "ControlPath=none",
        "-L",
    ])
    // One explicit address here: bound as `localhost`, ssh settles for `::1`
    // alone when `127.0.0.1` is taken, and the forward looks healthy while
    // the browser reaches whatever holds the port. The Ports tab opens
    // `127.0.0.1` for the same reason. The far side stays `localhost` so
    // servers bound only to `::1` (Vite and others) are reachable.
    .arg(format!("127.0.0.1:{local_port}:localhost:{remote_port}"))
    .arg(target);
    guarded(&ssh)
}

fn guarded(inner: &Command) -> Command {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(GUARD_SCRIPT)
        .arg("muxflow-port-forward")
        .arg(inner.get_program())
        .args(inner.get_args());
    command
}

/// Lists listening TCP sockets: `ss` on Linux, `lsof` where there is no `ss`
/// (macOS).
const DETECT_COMMAND: &str = "ss -ltnHp 2>/dev/null || lsof -nP -iTCP -sTCP:LISTEN -Fcn";

/// Parses `ss -ltnHp` or `lsof -Fcn` into the ports worth suggesting:
/// unprivileged, reachable through a loopback forward, one row per port, the
/// user's own processes first.
pub(crate) fn parse_listening_ports(output: &str) -> Vec<DetectedPort> {
    let mut ports: Vec<DetectedPort> = Vec::new();
    let mut lsof_process: Option<String> = None;
    for line in output.lines() {
        let (local, process) = match line.as_bytes().first() {
            // lsof: `p` starts a process, `c` names it, `n` is one of its sockets.
            Some(b'p') => {
                lsof_process = None;
                continue;
            }
            Some(b'c') => {
                lsof_process = Some(line[1..].to_owned());
                continue;
            }
            Some(b'n') => (&line[1..], lsof_process.clone()),
            // ss: State Recv-Q Send-Q Local:Port Peer:Port [Process]
            _ => {
                let Some(local) = line.split_whitespace().nth(3) else {
                    continue;
                };
                let process = line
                    .split_once("users:((\"")
                    .and_then(|(_, rest)| rest.split_once('"'))
                    .map(|(name, _)| name.to_owned());
                (local, process)
            }
        };
        let Some((address, port)) = local.rsplit_once(':') else {
            continue;
        };
        let Ok(port) = port.parse::<u16>() else {
            continue;
        };
        if port < 1024 || !reachable_through_loopback(address) {
            continue;
        }
        match ports.iter_mut().find(|existing| existing.port == port) {
            Some(existing) => {
                if existing.process.is_none() {
                    existing.process = process;
                }
            }
            None => ports.push(DetectedPort { port, process }),
        }
    }
    ports.sort_by_key(|detected| (detected.process.is_none(), detected.port));
    ports
}

fn reachable_through_loopback(address: &str) -> bool {
    let address = address.split('%').next().unwrap_or(address);
    let address = address.trim_start_matches('[').trim_end_matches(']');
    matches!(address, "*" | "0.0.0.0" | "::" | "::1" | "::ffff:127.0.0.1")
        || address.starts_with("127.")
}

fn detect_ports(connection: &ConnectionSpec) -> Result<Vec<DetectedPort>, String> {
    let (_, target, config_path) = ssh_parts(connection)?;
    // Rides the host's control master: one round trip, no new handshake.
    let lease = acquire_control_master(connection)?.ok_or("port detection needs an SSH host")?;
    let mut command = ssh_base(config_path);
    command.arg("-T");
    lease.configure(&mut command, target, config_path)?;
    let output = command
        .arg(target)
        .arg(DETECT_COMMAND)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("failed to run ssh: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            "listening ports are unavailable on this host".into()
        } else {
            stderr
        });
    }
    Ok(parse_listening_ports(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn emitter(app: AppHandle) -> Notify {
    Arc::new(move |profile_id: &str| {
        let _ = app.emit(CHANGED_EVENT, profile_id);
    })
}

#[tauri::command]
pub fn ports_forward(
    app: AppHandle,
    forwards: State<'_, PortForwards>,
    connection: ConnectionSpec,
    remote_port: u16,
    local_port: u16,
) -> Result<(), String> {
    let (profile_id, target, config_path) = ssh_parts(&connection)?;
    let command = forward_command(target, config_path, remote_port, local_port);
    forwards
        .0
        .start(profile_id, remote_port, local_port, command, emitter(app))
}

#[tauri::command]
pub fn ports_stop(
    app: AppHandle,
    forwards: State<'_, PortForwards>,
    profile_id: String,
    remote_port: u16,
) {
    if forwards
        .0
        .stop(|forward| forward.profile_id == profile_id && forward.remote_port == remote_port)
    {
        emitter(app)(&profile_id);
    }
}

#[tauri::command]
pub fn ports_stop_host(app: AppHandle, forwards: State<'_, PortForwards>, profile_id: String) {
    if forwards.0.stop(|forward| forward.profile_id == profile_id) {
        emitter(app)(&profile_id);
    }
}

#[tauri::command]
pub fn ports_list(forwards: State<'_, PortForwards>) -> Vec<PortForward> {
    forwards.0.list()
}

#[tauri::command]
pub async fn ports_detect(connection: ConnectionSpec) -> Result<Vec<DetectedPort>, String> {
    tauri::async_runtime::spawn_blocking(move || detect_ports(&connection))
        .await
        .map_err(|error| format!("port detection task failed: {error}"))?
}

#[cfg(test)]
#[path = "ports/tests.rs"]
mod tests;
