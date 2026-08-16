use std::fmt;
use std::{
    collections::HashMap,
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Condvar, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{BridgeStderr, apply_control_lane_options, ssh_base};
use crate::connection::{ConnectionSpec, validate_ssh_target};

#[derive(Clone)]
struct SshMaster {
    target: String,
    config_path: Option<String>,
    socket: PathBuf,
    coordination: Arc<MasterCoordination>,
}

#[derive(Default)]
struct MasterCoordination {
    state: Mutex<SshMasterState>,
    changed: Condvar,
}

struct SshMasterState {
    lifecycle: MasterLifecycle,
    generation: u64,
    revalidation_generation: u64,
    leases: usize,
    idle_generation: u64,
}

impl Default for SshMasterState {
    fn default() -> Self {
        Self {
            lifecycle: MasterLifecycle::Idle,
            generation: 0,
            revalidation_generation: 0,
            leases: 0,
            idle_generation: 0,
        }
    }
}

enum MasterLifecycle {
    Idle,
    Establishing,
    Ready {
        process: MasterProcess,
        needs_probe: bool,
    },
    Closing {
        in_flight: bool,
    },
}

enum MasterProcess {
    Owned(OwnedMaster),
    External,
}

struct OwnedMaster {
    child: Child,
    socket_identity: SocketIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

pub(in crate::connection) struct SshLease {
    master: SshMaster,
    route: LeaseRoute,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LeaseRoute {
    Multiplexed,
    Direct,
}

impl SshLease {
    pub(super) fn configure(
        &self,
        command: &mut Command,
        target: &str,
        config_path: Option<&str>,
    ) -> Result<(), String> {
        if self.master.target != target || self.master.config_path.as_deref() != config_path {
            return Err("SSH control-master lease does not match the requested profile".into());
        }
        if self.route == LeaseRoute::Direct {
            command.args(["-o", "ControlMaster=no", "-o", "ControlPath=none"]);
        } else {
            command.arg("-S").arg(&self.master.socket);
        }
        Ok(())
    }

    pub(in crate::connection) fn require_revalidation(&self) {
        let mut state = self.master.coordination.state.lock().unwrap();
        state.revalidation_generation = state.revalidation_generation.wrapping_add(1);
        if let MasterLifecycle::Ready { needs_probe, .. } = &mut state.lifecycle {
            *needs_probe = true;
        }
        self.master.coordination.changed.notify_all();
    }

    pub(in crate::connection) fn control_socket(&self) -> Option<&Path> {
        (self.route == LeaseRoute::Multiplexed).then_some(self.master.socket.as_path())
    }
}

impl Drop for SshLease {
    fn drop(&mut self) {
        release_control_master(&self.master, Duration::from_secs(60));
    }
}

#[derive(Default)]
struct SshMasterRegistry {
    masters: HashMap<PathBuf, SshMaster>,
    closing: bool,
}

static SSH_MASTERS: OnceLock<Mutex<SshMasterRegistry>> = OnceLock::new();

fn ssh_masters() -> &'static Mutex<SshMasterRegistry> {
    SSH_MASTERS.get_or_init(|| Mutex::new(SshMasterRegistry::default()))
}

pub(in crate::connection) fn acquire_control_master(
    connection: &ConnectionSpec,
) -> Result<Option<SshLease>, String> {
    acquire_control_master_cancellable(connection, || false)
}

pub(in crate::connection) fn acquire_control_master_cancellable(
    connection: &ConnectionSpec,
    cancelled: impl Fn() -> bool,
) -> Result<Option<SshLease>, String> {
    let ConnectionSpec::Ssh {
        profile_id,
        target,
        config_path,
    } = connection
    else {
        return Ok(None);
    };
    let socket = ssh_profile_control_socket(profile_id, target, config_path.as_deref())?;
    let lease = ensure_control_master_entry(target, config_path.as_deref(), &socket, &cancelled)?;
    Ok(Some(lease))
}

#[path = "control_master/socket.rs"]
mod socket;
pub(in crate::connection) use socket::ssh_profile_control_socket;
#[cfg(test)]
use socket::{control_socket_binds, ssh_profile_control_socket_in};
use socket::{validate_control_socket, validated_control_socket_identity};

pub(in crate::connection) fn acquire_control_master_for_socket(
    target: &str,
    config_path: Option<&str>,
    socket: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<SshLease, String> {
    ensure_control_master_entry(target, config_path, socket, cancelled)
}

fn master_entry(
    target: &str,
    config_path: Option<&str>,
    socket: &Path,
) -> Result<SshMaster, String> {
    let mut masters = ssh_masters().lock().unwrap();
    if masters.closing {
        return Err("SSH control masters are closing; retry connection setup".into());
    }
    Ok(masters
        .masters
        .entry(socket.to_owned())
        .or_insert_with(|| SshMaster {
            target: target.into(),
            config_path: config_path.map(ToOwned::to_owned),
            socket: socket.to_owned(),
            coordination: Arc::new(MasterCoordination::default()),
        })
        .clone())
}

fn ensure_control_master_entry(
    target: &str,
    config_path: Option<&str>,
    socket: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<SshLease, String> {
    crate::perf_log::record_remote_operation(
        crate::perf_log::RemoteOperation::ControlMasterEnsureAttempt,
    );
    let result = ensure_control_master_entry_inner(target, config_path, socket, cancelled);
    if result.is_err() {
        crate::perf_log::record_remote_operation(
            crate::perf_log::RemoteOperation::ControlMasterFailure,
        );
    }
    result
}

fn ensure_control_master_entry_inner(
    target: &str,
    config_path: Option<&str>,
    socket: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<SshLease, String> {
    validate_control_socket(socket)?;
    let master_entry = master_entry(target, config_path, socket)?;
    reserve_control_master(&master_entry)?;
    {
        let mut state = master_entry.coordination.state.lock().unwrap();
        match validated_control_socket_identity(socket) {
            Ok(Some(_)) if matches!(state.lifecycle, MasterLifecycle::Idle) => {
                state.lifecycle = MasterLifecycle::Ready {
                    process: MasterProcess::External,
                    needs_probe: true,
                };
            }
            Ok(_) => {}
            Err(error) => {
                drop(state);
                release_control_master(&master_entry, Duration::from_secs(60));
                return Err(error);
            }
        }
    }
    // Only contenders for this exact socket wait here. The process-global map
    // lock has already been released, so a slow ProxyJump/authentication flow
    // for one profile cannot serialize another profile's subprocesses.
    let outcome = match coordinate_master(&master_entry, target, config_path, socket, cancelled) {
        Ok(outcome) => outcome,
        Err(error) => {
            release_control_master(&master_entry, Duration::from_secs(60));
            return Err(error);
        }
    };
    if outcome.reused {
        crate::perf_log::record_remote_operation(
            crate::perf_log::RemoteOperation::ControlMasterReuseSuccess,
        );
    }
    Ok(SshLease {
        master: master_entry,
        route: if outcome.direct {
            LeaseRoute::Direct
        } else {
            LeaseRoute::Multiplexed
        },
    })
}

fn coordinate_master(
    master: &SshMaster,
    target: &str,
    config_path: Option<&str>,
    socket: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<CoordinationOutcome, String> {
    let control_cancelled = || cancelled() || master_is_closing(master);
    let outcome = coordinate_master_state(
        master,
        cancelled,
        |process, needs_probe| match process {
            MasterProcess::Owned(owned) => {
                let child_live = owned
                    .child
                    .try_wait()
                    .map_err(|error| error.to_string())?
                    .is_none();
                let current_identity = validated_control_socket_identity(socket)?;
                if current_identity.is_some() && current_identity != Some(owned.socket_identity) {
                    return Ok(MasterLiveness::ReclassifyExternal);
                }
                if !child_live || current_identity.is_none() {
                    return Ok(MasterLiveness::Replace);
                }
                if !needs_probe {
                    return Ok(MasterLiveness::Live);
                }
                match check_control_master(target, config_path, socket, &control_cancelled) {
                    Ok(true) => Ok(MasterLiveness::Live),
                    Ok(false) => Ok(MasterLiveness::Replace),
                    Err(ControlCommandError::Cancelled) => {
                        Err(ControlCommandError::Cancelled.to_string())
                    }
                    Err(_) => Ok(MasterLiveness::Replace),
                }
            }
            // A responsive socket can still belong to another ADE process,
            // whose local lease count cannot include this bridge. Reusing it
            // would let that owner tear it down underneath us. External
            // sockets are therefore preserved but always bypassed directly.
            MasterProcess::External => external_master_liveness(socket),
        },
        || establish_control_master(target, config_path, socket, &control_cancelled),
    )?;
    if let Some(generation) = outcome.owned_generation {
        spawn_master_reaper(master.clone(), generation);
    }
    Ok(outcome)
}

struct CoordinationOutcome {
    reused: bool,
    owned_generation: Option<u64>,
    direct: bool,
}

enum MasterLiveness {
    Live,
    Replace,
    ReclassifyExternal,
    Direct,
}

fn external_master_liveness(socket: &Path) -> Result<MasterLiveness, String> {
    if validated_control_socket_identity(socket)?.is_some() {
        Ok(MasterLiveness::Direct)
    } else {
        Ok(MasterLiveness::Replace)
    }
}

fn coordinate_master_state(
    master: &SshMaster,
    cancelled: &dyn Fn() -> bool,
    mut is_live: impl FnMut(&mut MasterProcess, bool) -> Result<MasterLiveness, String>,
    establish: impl FnOnce() -> Result<(MasterProcess, bool), String>,
) -> Result<CoordinationOutcome, String> {
    loop {
        if cancelled() {
            return Err("SSH control-master establishment cancelled".into());
        }
        let mut state = master.coordination.state.lock().unwrap();
        let (mut process, needs_probe, revalidation_generation) = match &state.lifecycle {
            MasterLifecycle::Closing { .. } => {
                return Err("SSH control masters are closing; retry connection setup".into());
            }
            MasterLifecycle::Establishing => {
                let (next, _) = master
                    .coordination
                    .changed
                    .wait_timeout(state, Duration::from_millis(25))
                    .unwrap();
                drop(next);
                continue;
            }
            MasterLifecycle::Idle => {
                state.lifecycle = MasterLifecycle::Establishing;
                drop(state);
                break;
            }
            MasterLifecycle::Ready { .. } => {
                let MasterLifecycle::Ready {
                    process,
                    needs_probe,
                } = std::mem::replace(&mut state.lifecycle, MasterLifecycle::Establishing)
                else {
                    unreachable!("ready lifecycle was just matched")
                };
                (process, needs_probe, state.revalidation_generation)
            }
        };
        master.coordination.changed.notify_all();
        drop(state);
        let live = is_live(&mut process, needs_probe);
        let mut state = master.coordination.state.lock().unwrap();
        if matches!(state.lifecycle, MasterLifecycle::Closing { .. }) {
            drop(state);
            dispose_process(master, process);
            finish_closing(master);
            return Err("SSH control masters are closing; retry connection setup".into());
        }
        let revalidation_requested = state.revalidation_generation != revalidation_generation;
        match live {
            Ok(MasterLiveness::Live) if revalidation_requested => {
                state.lifecycle = MasterLifecycle::Ready {
                    process,
                    needs_probe: true,
                };
                master.coordination.changed.notify_all();
                drop(state);
                continue;
            }
            Ok(MasterLiveness::Live) => {
                state.lifecycle = MasterLifecycle::Ready {
                    process,
                    needs_probe: false,
                };
                master.coordination.changed.notify_all();
                return Ok(CoordinationOutcome {
                    reused: true,
                    owned_generation: None,
                    direct: false,
                });
            }
            Ok(MasterLiveness::Replace) => {
                state.generation = state.generation.wrapping_add(1);
                drop(state);
                dispose_process(master, process);
                break;
            }
            Ok(MasterLiveness::ReclassifyExternal) => {
                state.generation = state.generation.wrapping_add(1);
                drop(state);
                dispose_process(master, process);
                let mut state = master.coordination.state.lock().unwrap();
                if matches!(state.lifecycle, MasterLifecycle::Closing { .. }) {
                    state.lifecycle = MasterLifecycle::Closing { in_flight: false };
                    master.coordination.changed.notify_all();
                    return Err("SSH control masters are closing; retry connection setup".into());
                }
                state.lifecycle = MasterLifecycle::Ready {
                    process: MasterProcess::External,
                    needs_probe: true,
                };
                master.coordination.changed.notify_all();
                drop(state);
                continue;
            }
            Ok(MasterLiveness::Direct) => {
                state.lifecycle = MasterLifecycle::Ready {
                    process,
                    needs_probe: true,
                };
                master.coordination.changed.notify_all();
                return Ok(CoordinationOutcome {
                    reused: false,
                    owned_generation: None,
                    direct: true,
                });
            }
            Err(error) => {
                state.lifecycle = MasterLifecycle::Ready {
                    process,
                    needs_probe: needs_probe || revalidation_requested,
                };
                master.coordination.changed.notify_all();
                return Err(error);
            }
        }
    }

    // Authentication and ProxyJump may take seconds. The per-socket state
    // records ownership of this attempt, but no mutex is held while OpenSSH is
    // running; same-socket contenders wait on the condition variable while
    // other sockets proceed independently.
    let established = establish();
    let mut state = master.coordination.state.lock().unwrap();
    if matches!(state.lifecycle, MasterLifecycle::Closing { .. }) {
        drop(state);
        if let Ok((process, _)) = established {
            dispose_process(master, process);
        }
        finish_closing(master);
        return Err("SSH control masters are closing; retry connection setup".into());
    }
    let (process, reused) = match established {
        Ok(value) => value,
        Err(error) => {
            state.lifecycle = MasterLifecycle::Idle;
            master.coordination.changed.notify_all();
            return Err(error);
        }
    };
    state.generation = state.generation.wrapping_add(1);
    let generation = state.generation;
    let owned_generation = matches!(process, MasterProcess::Owned(_)).then_some(generation);
    state.lifecycle = MasterLifecycle::Ready {
        process,
        needs_probe: false,
    };
    master.coordination.changed.notify_all();
    Ok(CoordinationOutcome {
        reused,
        owned_generation,
        direct: false,
    })
}

fn finish_in_flight(master: &SshMaster) {
    let mut state = master.coordination.state.lock().unwrap();
    match state.lifecycle {
        MasterLifecycle::Establishing => state.lifecycle = MasterLifecycle::Idle,
        MasterLifecycle::Closing { in_flight: true } => {
            state.lifecycle = MasterLifecycle::Closing { in_flight: false };
        }
        _ => {}
    }
    master.coordination.changed.notify_all();
}

fn finish_closing(master: &SshMaster) {
    let mut state = master.coordination.state.lock().unwrap();
    if matches!(
        state.lifecycle,
        MasterLifecycle::Closing { in_flight: true }
    ) {
        state.lifecycle = MasterLifecycle::Closing { in_flight: false };
    }
    master.coordination.changed.notify_all();
}

fn master_is_closing(master: &SshMaster) -> bool {
    matches!(
        master.coordination.state.lock().unwrap().lifecycle,
        MasterLifecycle::Closing { .. }
    )
}

fn reserve_control_master(master: &SshMaster) -> Result<(), String> {
    let mut state = master.coordination.state.lock().unwrap();
    if matches!(state.lifecycle, MasterLifecycle::Closing { .. }) {
        return Err("SSH control masters are closing; retry connection setup".into());
    }
    state.leases = state.leases.saturating_add(1);
    state.idle_generation = state.idle_generation.wrapping_add(1);
    master.coordination.changed.notify_all();
    Ok(())
}

fn release_control_master(master: &SshMaster, idle_timeout: Duration) {
    let idle_generation = {
        let mut state = master.coordination.state.lock().unwrap();
        state.leases = state.leases.saturating_sub(1);
        if state.leases != 0
            || !matches!(
                state.lifecycle,
                MasterLifecycle::Ready {
                    process: MasterProcess::Owned(_),
                    ..
                }
            )
        {
            return;
        }
        state.idle_generation = state.idle_generation.wrapping_add(1);
        let generation = state.idle_generation;
        master.coordination.changed.notify_all();
        generation
    };
    let master = master.clone();
    let _ = thread::Builder::new()
        .name("ssh-master-idle".into())
        .spawn(move || {
            let state = master.coordination.state.lock().unwrap();
            let (mut state, wait) = master
                .coordination
                .changed
                .wait_timeout_while(state, idle_timeout, |state| {
                    !matches!(state.lifecycle, MasterLifecycle::Closing { .. })
                        && state.leases == 0
                        && state.idle_generation == idle_generation
                })
                .unwrap();
            if !wait.timed_out()
                || matches!(state.lifecycle, MasterLifecycle::Closing { .. })
                || state.leases != 0
                || state.idle_generation != idle_generation
            {
                return;
            }
            let lifecycle = std::mem::replace(&mut state.lifecycle, MasterLifecycle::Establishing);
            let process = match lifecycle {
                MasterLifecycle::Ready {
                    process: process @ MasterProcess::Owned(_),
                    ..
                } => process,
                other => {
                    state.lifecycle = other;
                    return;
                }
            };
            state.generation = state.generation.wrapping_add(1);
            master.coordination.changed.notify_all();
            drop(state);
            dispose_process(&master, process);
            finish_in_flight(&master);
        });
}

fn establish_control_master(
    target: &str,
    config_path: Option<&str>,
    socket: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<(MasterProcess, bool), String> {
    let mut command = ssh_base(config_path);
    apply_control_lane_options(&mut command);
    let mut child = command
        .args([
            "-M",
            "-N",
            "-o",
            "ControlMaster=yes",
            "-o",
            "ControlPersist=60",
        ])
        .arg("-S")
        .arg(socket)
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let diagnostic = child.stderr.take().map(BridgeStderr::capture);
    loop {
        if cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("SSH control-master establishment cancelled".into());
        }
        match validated_control_socket_identity(socket) {
            Ok(Some(socket_identity)) => {
                // OpenSSH publishes the socket before the multiplex server is
                // necessarily ready to accept a client. Returning at inode
                // creation lets concurrent bridge launches miss the new
                // master and open redundant TCP connections. The master is
                // usable only after its own control command succeeds.
                match check_control_master(target, config_path, socket, cancelled) {
                    Ok(true) => {
                        crate::perf_log::record_remote_operation(
                            crate::perf_log::RemoteOperation::ControlMasterEstablishmentSuccess,
                        );
                        return Ok((
                            MasterProcess::Owned(OwnedMaster {
                                child,
                                socket_identity,
                            }),
                            false,
                        ));
                    }
                    Ok(false) | Err(ControlCommandError::TimedOut | ControlCommandError::Io(_)) => {
                    }
                    Err(ControlCommandError::Cancelled) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err("SSH control-master establishment cancelled".into());
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        };
        if let Some(status) = status {
            let detail = diagnostic
                .as_ref()
                .and_then(|value| value.diagnostic(Duration::from_millis(300)))
                .unwrap_or_else(|| status.to_string());
            return Err(format!("OpenSSH control master failed: {detail}"));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

const CONTROL_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, PartialEq, Eq)]
enum ControlCommandError {
    Cancelled,
    TimedOut,
    Io(String),
}

impl fmt::Display for ControlCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("SSH control operation cancelled"),
            Self::TimedOut => formatter.write_str("SSH control operation timed out"),
            Self::Io(error) => formatter.write_str(error),
        }
    }
}

fn check_control_master(
    target: &str,
    config_path: Option<&str>,
    socket: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<bool, ControlCommandError> {
    crate::perf_log::record_remote_operation(
        crate::perf_log::RemoteOperation::ControlMasterCheckAttempt,
    );
    run_control_command(
        control_master_check_command(target, config_path, socket),
        CONTROL_COMMAND_TIMEOUT,
        cancelled,
    )
}

fn control_master_check_command(target: &str, config_path: Option<&str>, socket: &Path) -> Command {
    let mut command = ssh_base(config_path);
    command
        .arg("-S")
        .arg(socket)
        .args(["-O", "check"])
        .arg(target);
    command
}

fn run_control_command(
    mut command: Command,
    timeout: Duration,
    cancelled: &dyn Fn() -> bool,
) -> Result<bool, ControlCommandError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| ControlCommandError::Io(error.to_string()))?;
    let deadline = Instant::now() + timeout;
    loop {
        if cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ControlCommandError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.success()),
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ControlCommandError::Io(error.to_string()));
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ControlCommandError::TimedOut);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn spawn_master_reaper(master: SshMaster, generation: u64) {
    let _ = thread::Builder::new()
        .name("ssh-master-reaper".into())
        .spawn(move || {
            loop {
                let state = master.coordination.state.lock().unwrap();
                if matches!(state.lifecycle, MasterLifecycle::Closing { .. })
                    || state.generation != generation
                {
                    return;
                }
                if matches!(state.lifecycle, MasterLifecycle::Establishing) {
                    let state = master.coordination.changed.wait(state).unwrap();
                    drop(state);
                    continue;
                }
                if !matches!(
                    state.lifecycle,
                    MasterLifecycle::Ready {
                        process: MasterProcess::Owned(_),
                        ..
                    }
                ) {
                    return;
                }
                let (state, wait) = master
                    .coordination
                    .changed
                    .wait_timeout(state, Duration::from_secs(1))
                    .unwrap();
                drop(state);
                if wait.timed_out() && reap_owned_master_if_finished(&master, generation) {
                    return;
                }
            }
        });
}

fn reap_owned_master_if_finished(master: &SshMaster, generation: u64) -> bool {
    let mut state = master.coordination.state.lock().unwrap();
    if matches!(state.lifecycle, MasterLifecycle::Closing { .. }) || state.generation != generation
    {
        return true;
    }
    let (finished, socket_identity) = match &mut state.lifecycle {
        MasterLifecycle::Ready {
            process: MasterProcess::Owned(owned),
            ..
        } => (
            owned.child.try_wait().is_ok_and(|status| status.is_some()),
            owned.socket_identity,
        ),
        _ => return true,
    };
    if finished {
        state.lifecycle = MasterLifecycle::Idle;
        state.generation = state.generation.wrapping_add(1);
        master.coordination.changed.notify_all();
        drop(state);
        remove_owned_control_socket(master, socket_identity);
    }
    finished
}

fn dispose_process(master: &SshMaster, process: MasterProcess) {
    let MasterProcess::Owned(mut owned) = process else {
        // A socket created by another ADE process is observed but never
        // adopted: app shutdown must not kill or unlink what it does not own.
        return;
    };
    let _ = owned.child.kill();
    let _ = owned.child.wait();
    remove_owned_control_socket(master, owned.socket_identity);
}

fn remove_owned_control_socket(master: &SshMaster, owned_identity: SocketIdentity) {
    if validated_control_socket_identity(&master.socket)
        .ok()
        .flatten()
        == Some(owned_identity)
    {
        let _ = fs::remove_file(&master.socket);
    }
}

fn close_master(master: &SshMaster) {
    let process = {
        let mut state = master.coordination.state.lock().unwrap();
        let in_flight = matches!(state.lifecycle, MasterLifecycle::Establishing);
        let lifecycle =
            std::mem::replace(&mut state.lifecycle, MasterLifecycle::Closing { in_flight });
        state.generation = state.generation.wrapping_add(1);
        master.coordination.changed.notify_all();
        let process = match lifecycle {
            MasterLifecycle::Ready { process, .. } => Some(process),
            MasterLifecycle::Idle | MasterLifecycle::Establishing => None,
            MasterLifecycle::Closing { .. } => return,
        };
        while matches!(
            state.lifecycle,
            MasterLifecycle::Closing { in_flight: true }
        ) {
            state = master.coordination.changed.wait(state).unwrap();
        }
        process
    };
    if let Some(process) = process {
        dispose_process(master, process);
    }
}

pub(crate) fn close_all_control_masters() {
    let masters: Vec<_> = {
        let mut registry = ssh_masters().lock().unwrap();
        registry.closing = true;
        registry.masters.drain().map(|(_, value)| value).collect()
    };
    for master in masters {
        close_master(&master);
    }
}

#[cfg(test)]
#[path = "control_master/tests.rs"]
mod tests;
