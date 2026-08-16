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

#[derive(Default)]
struct SshMasterState {
    closing: bool,
    establishing: bool,
    generation: u64,
    leases: usize,
    idle_generation: u64,
    needs_probe: bool,
    process: Option<MasterProcess>,
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
    direct: bool,
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
        if self.direct {
            command.args(["-o", "ControlMaster=no", "-o", "ControlPath=none"]);
        } else {
            command.arg("-S").arg(&self.master.socket);
        }
        Ok(())
    }

    pub(in crate::connection) fn require_revalidation(&self) {
        let mut state = self.master.coordination.state.lock().unwrap();
        if state.process.is_some() {
            state.needs_probe = true;
            self.master.coordination.changed.notify_all();
        }
    }

    pub(in crate::connection) fn control_socket(&self) -> Option<&Path> {
        (!self.direct).then_some(self.master.socket.as_path())
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
    let lease = ensure_control_master_entry(
        target,
        config_path.as_deref(),
        &socket,
        ControlLane::Interactive,
        &cancelled,
    )?;
    Ok(Some(lease))
}

// A Unix socket bind must fit `sockaddr_un::sun_path` including its terminator:
// 104 bytes on Darwin, 108 on Linux. OpenSSH binds a temporary sibling first,
// appending a dot and sixteen random characters before renaming it into place,
// so the published path needs that much extra headroom.
const CONTROL_SOCKET_PATH_BYTES: usize = if cfg!(target_os = "macos") { 104 } else { 108 };
const CONTROL_SOCKET_TEMPORARY_BYTES: usize = 17;

// Strict, because the bound counts the terminating NUL that must also fit.
fn control_socket_binds(socket: &Path) -> bool {
    socket.as_os_str().as_bytes().len() + CONTROL_SOCKET_TEMPORARY_BYTES < CONTROL_SOCKET_PATH_BYTES
}

// macOS gives every user a per-boot temporary directory roughly 49 bytes long,
// which leaves no room for the control socket. Fall back to the same short,
// uid-scoped root the helper already uses for its own runtime socket. The
// directory is still created 0700 and rejected unless this user owns it, so a
// pre-created path belonging to anyone else fails closed rather than downgrading.
const SHORT_CONTROL_ROOT: &str = "/tmp";

pub(in crate::connection) fn ssh_profile_control_socket(
    profile_id: &str,
    target: &str,
    config_path: Option<&str>,
) -> Result<PathBuf, String> {
    ssh_profile_control_socket_for_lane(profile_id, target, config_path, ControlLane::Interactive)
}

pub(super) fn ssh_profile_control_socket_for_lane(
    profile_id: &str,
    target: &str,
    config_path: Option<&str>,
    lane: ControlLane,
) -> Result<PathBuf, String> {
    let preferred = std::env::temp_dir();
    // A failure here is a real safety refusal, not a sizing problem, so it must
    // propagate instead of silently relocating the socket.
    let socket = ssh_profile_control_socket_in(&preferred, profile_id, target, config_path, lane)?;
    if control_socket_binds(&socket) {
        return Ok(socket);
    }
    if preferred != Path::new(SHORT_CONTROL_ROOT) {
        let short = ssh_profile_control_socket_in(
            Path::new(SHORT_CONTROL_ROOT),
            profile_id,
            target,
            config_path,
            lane,
        )?;
        if control_socket_binds(&short) {
            return Ok(short);
        }
    }
    Err(format!(
        "SSH control socket path does not fit this platform's {CONTROL_SOCKET_PATH_BYTES}-byte \
         limit: {}",
        socket.display()
    ))
}

fn ssh_profile_control_socket_in(
    temporary_root: &Path,
    profile_id: &str,
    target: &str,
    config_path: Option<&str>,
    lane: ControlLane,
) -> Result<PathBuf, String> {
    validate_ssh_target(target)?;
    let base = temporary_root.join(format!("tmux-agent-ide-{}", unsafe { libc::geteuid() }));
    ensure_private_directory(&base)?;
    let directory = base.join(process_socket_namespace());
    ensure_private_directory(&directory)?;
    let mut digest = Sha256::new();
    digest.update(profile_id.as_bytes());
    digest.update([0]);
    digest.update(match lane {
        ControlLane::Interactive => b"interactive".as_slice(),
        ControlLane::Bulk => b"bulk".as_slice(),
    });
    digest.update([0]);
    digest.update(target.as_bytes());
    digest.update([0]);
    if let Some(path) = config_path {
        digest.update(path.as_bytes());
    }
    let key = format!("{:x}", digest.finalize());
    Ok(directory.join(format!("profile-{}.sock", &key[..20])))
}

fn process_socket_namespace() -> &'static str {
    static NAMESPACE: OnceLock<String> = OnceLock::new();
    NAMESPACE.get_or_init(|| {
        let nonce = Uuid::new_v4().simple().to_string();
        format!("ssh-{}-{}", std::process::id(), &nonce[..8])
    })
}

fn ensure_private_directory(directory: &Path) -> Result<(), String> {
    match fs::DirBuilder::new().mode(0o700).create(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.to_string()),
    }
    let metadata = fs::symlink_metadata(directory).map_err(|error| error.to_string())?;
    if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(
            "SSH control directory must be an owned, private, non-symlink directory".into(),
        );
    }
    if metadata.mode() & 0o077 != 0 {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
        let secured = fs::symlink_metadata(directory).map_err(|error| error.to_string())?;
        if !secured.file_type().is_dir()
            || secured.uid() != unsafe { libc::geteuid() }
            || secured.mode() & 0o077 != 0
        {
            return Err(
                "SSH control directory must be an owned, private, non-symlink directory".into(),
            );
        }
    }
    Ok(())
}

fn validate_control_socket(socket: &Path) -> Result<bool, String> {
    validated_control_socket_identity(socket).map(|identity| identity.is_some())
}

fn validated_control_socket_identity(socket: &Path) -> Result<Option<SocketIdentity>, String> {
    let metadata = match fs::symlink_metadata(socket) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err("refusing an unowned or unsafe SSH control-socket path".into());
    }
    Ok(Some(SocketIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }))
}

/// Which multiplexed SSH connection a master serves.
///
/// The two lanes want opposite transport settings, and because these are
/// properties of the master rather than of a multiplexed client, the choice has
/// to be made here.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlLane {
    /// Keystrokes, control frames and terminal output.
    Interactive,
    /// File bodies and transfers.
    Bulk,
}

pub(in crate::connection) fn acquire_control_master_for_socket(
    target: &str,
    config_path: Option<&str>,
    socket: &Path,
    lane: ControlLane,
) -> Result<SshLease, String> {
    ensure_control_master_entry(target, config_path, socket, lane, &|| false)
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
    lane: ControlLane,
    cancelled: &dyn Fn() -> bool,
) -> Result<SshLease, String> {
    crate::perf_log::record_remote_operation("controlMasterEnsure");
    validate_control_socket(socket)?;
    let master_entry = master_entry(target, config_path, socket)?;
    reserve_control_master(&master_entry)?;
    {
        let mut state = master_entry.coordination.state.lock().unwrap();
        match validated_control_socket_identity(socket) {
            Ok(Some(_)) if state.process.is_none() => {
                state.process = Some(MasterProcess::External);
                state.needs_probe = true;
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
    let outcome =
        match coordinate_master(&master_entry, target, config_path, socket, lane, cancelled) {
            Ok(outcome) => outcome,
            Err(error) => {
                release_control_master(&master_entry, Duration::from_secs(60));
                return Err(error);
            }
        };
    if outcome.reused {
        crate::perf_log::record_remote_operation("controlMasterReuse");
    }
    Ok(SshLease {
        master: master_entry,
        direct: outcome.direct,
    })
}

fn coordinate_master(
    master: &SshMaster,
    target: &str,
    config_path: Option<&str>,
    socket: &Path,
    lane: ControlLane,
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
        || establish_control_master(target, config_path, socket, lane, &control_cancelled),
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
        if state.closing {
            return Err("SSH control masters are closing; retry connection setup".into());
        }
        if state.establishing {
            let (next, _) = master
                .coordination
                .changed
                .wait_timeout(state, Duration::from_millis(25))
                .unwrap();
            drop(next);
            continue;
        }
        if let Some(mut process) = state.process.take() {
            let needs_probe = state.needs_probe;
            state.establishing = true;
            master.coordination.changed.notify_all();
            drop(state);
            let live = is_live(&mut process, needs_probe);
            let mut state = master.coordination.state.lock().unwrap();
            if state.closing {
                drop(state);
                dispose_process(master, process);
                finish_coordination(master);
                return Err("SSH control masters are closing; retry connection setup".into());
            }
            match live {
                Ok(MasterLiveness::Live) => {
                    state.process = Some(process);
                    state.needs_probe = false;
                    state.establishing = false;
                    master.coordination.changed.notify_all();
                    return Ok(CoordinationOutcome {
                        reused: true,
                        owned_generation: None,
                        direct: false,
                    });
                }
                Ok(MasterLiveness::Replace) => {
                    state.needs_probe = false;
                    state.generation = state.generation.wrapping_add(1);
                    drop(state);
                    dispose_process(master, process);
                    break;
                }
                Ok(MasterLiveness::ReclassifyExternal) => {
                    state.needs_probe = true;
                    state.generation = state.generation.wrapping_add(1);
                    drop(state);
                    dispose_process(master, process);
                    let mut state = master.coordination.state.lock().unwrap();
                    state.process = Some(MasterProcess::External);
                    state.establishing = false;
                    master.coordination.changed.notify_all();
                    drop(state);
                    continue;
                }
                Ok(MasterLiveness::Direct) => {
                    state.process = Some(process);
                    state.needs_probe = true;
                    state.establishing = false;
                    master.coordination.changed.notify_all();
                    return Ok(CoordinationOutcome {
                        reused: false,
                        owned_generation: None,
                        direct: true,
                    });
                }
                Err(error) => {
                    state.process = Some(process);
                    state.needs_probe = needs_probe;
                    state.establishing = false;
                    master.coordination.changed.notify_all();
                    return Err(error);
                }
            }
        }
        state.establishing = true;
        drop(state);
        break;
    }

    // Authentication and ProxyJump may take seconds. The per-socket state
    // records ownership of this attempt, but no mutex is held while OpenSSH is
    // running; same-socket contenders wait on the condition variable while
    // other sockets proceed independently.
    let established = establish();
    let mut state = master.coordination.state.lock().unwrap();
    if state.closing {
        drop(state);
        if let Ok((process, _)) = established {
            dispose_process(master, process);
        }
        let mut state = master.coordination.state.lock().unwrap();
        state.establishing = false;
        master.coordination.changed.notify_all();
        return Err("SSH control masters are closing; retry connection setup".into());
    }
    state.establishing = false;
    master.coordination.changed.notify_all();
    let (process, reused) = established?;
    state.generation = state.generation.wrapping_add(1);
    let generation = state.generation;
    let owned_generation = matches!(process, MasterProcess::Owned(_)).then_some(generation);
    state.process = Some(process);
    master.coordination.changed.notify_all();
    Ok(CoordinationOutcome {
        reused,
        owned_generation,
        direct: false,
    })
}

fn finish_coordination(master: &SshMaster) {
    let mut state = master.coordination.state.lock().unwrap();
    state.establishing = false;
    master.coordination.changed.notify_all();
}

fn master_is_closing(master: &SshMaster) -> bool {
    master.coordination.state.lock().unwrap().closing
}

fn reserve_control_master(master: &SshMaster) -> Result<(), String> {
    let mut state = master.coordination.state.lock().unwrap();
    if state.closing {
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
        if state.leases != 0 || !matches!(state.process, Some(MasterProcess::Owned(_))) {
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
                    !state.closing && state.leases == 0 && state.idle_generation == idle_generation
                })
                .unwrap();
            if !wait.timed_out()
                || state.closing
                || state.leases != 0
                || state.idle_generation != idle_generation
            {
                return;
            }
            let Some(process @ MasterProcess::Owned(_)) = state.process.take() else {
                return;
            };
            state.establishing = true;
            state.generation = state.generation.wrapping_add(1);
            master.coordination.changed.notify_all();
            drop(state);
            dispose_process(&master, process);
            finish_coordination(&master);
        });
}

fn establish_control_master(
    target: &str,
    config_path: Option<&str>,
    socket: &Path,
    lane: ControlLane,
    cancelled: &dyn Fn() -> bool,
) -> Result<(MasterProcess, bool), String> {
    let mut command = ssh_base(config_path);
    crate::perf_log::record_remote_operation("controlMasterEstablishment");
    if lane == ControlLane::Interactive {
        apply_control_lane_options(&mut command);
    }
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
                return Ok((
                    MasterProcess::Owned(OwnedMaster {
                        child,
                        socket_identity,
                    }),
                    false,
                ));
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
    crate::perf_log::record_remote_operation("controlMasterCheck");
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
                if state.closing || state.generation != generation {
                    return;
                }
                if state.establishing {
                    let state = master.coordination.changed.wait(state).unwrap();
                    drop(state);
                    continue;
                }
                if !matches!(state.process, Some(MasterProcess::Owned(_))) {
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
    if state.closing || state.generation != generation {
        return true;
    }
    let (finished, socket_identity) = match state.process.as_mut() {
        Some(MasterProcess::Owned(owned)) => (
            owned.child.try_wait().is_ok_and(|status| status.is_some()),
            owned.socket_identity,
        ),
        _ => return true,
    };
    if finished {
        state.process = None;
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
        state.closing = true;
        master.coordination.changed.notify_all();
        while state.establishing {
            state = master.coordination.changed.wait(state).unwrap();
        }
        state.generation = state.generation.wrapping_add(1);
        state.process.take()
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
