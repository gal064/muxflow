use std::{
    collections::HashMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Mutex, OnceLock},
};

use sha2::{Digest, Sha256};

use super::{ConnectionSpec, validate_ssh_target};

#[derive(Clone)]
struct SshMaster {
    target: String,
    config_path: Option<String>,
    socket: PathBuf,
    leases: usize,
}

pub(super) struct SshLease {
    socket: PathBuf,
}

impl Drop for SshLease {
    fn drop(&mut self) {
        release_control_master(&self.socket);
    }
}

static SSH_MASTERS: OnceLock<Mutex<HashMap<PathBuf, SshMaster>>> = OnceLock::new();

fn ssh_masters() -> &'static Mutex<HashMap<PathBuf, SshMaster>> {
    SSH_MASTERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn spawn_bridge(connection: &ConnectionSpec, _client_id: &str) -> Result<Child, String> {
    connection.validate()?;
    let mut command = match connection {
        ConnectionSpec::Local => {
            let mut command = Command::new(host_helper_path()?);
            command.args(["bridge", "--stdio"]);
            command
        }
        ConnectionSpec::Ssh {
            profile_id,
            target,
            config_path,
        } => {
            let socket = ssh_profile_control_socket(profile_id, target, config_path.as_deref())?;
            ensure_control_master(target, config_path.as_deref(), &socket)?;
            let mut command = ssh_base(config_path.as_deref());
            command
                .arg("-T")
                .arg("-S")
                .arg(socket)
                .arg(target)
                .arg("$HOME/.local/bin/tmux-ide-host bridge --stdio");
            command
        }
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("failed to start host bridge: {error}"))
}

pub(crate) fn spawn_bulk_bridge(connection: &ConnectionSpec) -> Result<Child, String> {
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
            command
                .arg("-T")
                .args(["-o", "ControlMaster=no", "-o", "ControlPath=none"])
                .arg(target)
                .arg("$HOME/.local/bin/tmux-ide-host bridge --stdio");
            command
        }
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("failed to start independent bulk bridge: {error}"))
}

pub(super) fn acquire_control_master(
    connection: &ConnectionSpec,
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
    ensure_control_master(target, config_path.as_deref(), &socket)?;
    let mut masters = ssh_masters().lock().unwrap();
    let master = masters
        .get_mut(&socket)
        .ok_or("SSH control master registry lost the acquired profile")?;
    master.leases += 1;
    Ok(Some(SshLease { socket }))
}

pub(super) fn host_helper_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("ADE_HOST_HELPER_PATH") {
        return Ok(path.into());
    }
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    if let Some(parent) = current.parent() {
        let sibling = parent.join("tmux-ide-host");
        if sibling.is_file() {
            return Ok(sibling);
        }
    }
    #[cfg(debug_assertions)]
    {
        for profile in ["debug", "release"] {
            let candidate = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../target")
                .join(profile)
                .join("tmux-ide-host");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(
        "tmux-ide-host helper is not installed beside the desktop; build or install the sidecar"
            .into(),
    )
}

pub(super) fn ssh_profile_control_socket(
    profile_id: &str,
    target: &str,
    config_path: Option<&str>,
) -> Result<PathBuf, String> {
    validate_ssh_target(target)?;
    let directory = std::env::temp_dir()
        .join(format!("tmux-agent-ide-{}", unsafe { libc::geteuid() }))
        .join("ssh");
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    let mut digest = Sha256::new();
    digest.update(profile_id.as_bytes());
    digest.update([0]);
    digest.update(target.as_bytes());
    digest.update([0]);
    if let Some(path) = config_path {
        digest.update(path.as_bytes());
    }
    let key = format!("{:x}", digest.finalize());
    Ok(directory.join(format!("profile-{}.sock", &key[..20])))
}

pub(super) fn ensure_control_master(
    target: &str,
    config_path: Option<&str>,
    socket: &Path,
) -> Result<(), String> {
    let mut masters = ssh_masters().lock().unwrap();
    let check = ssh_base(config_path)
        .arg("-S")
        .arg(socket)
        .args(["-O", "check"])
        .arg(target)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if check.is_ok_and(|status| status.success()) {
        masters
            .entry(socket.to_owned())
            .or_insert_with(|| SshMaster {
                target: target.into(),
                config_path: config_path.map(ToOwned::to_owned),
                socket: socket.to_owned(),
                leases: 0,
            });
        return Ok(());
    }
    if socket.exists() {
        fs::remove_file(socket).map_err(|error| error.to_string())?;
    }
    let output = ssh_base(config_path)
        .args([
            "-M",
            "-N",
            "-f",
            "-o",
            "ControlMaster=yes",
            "-o",
            "ControlPersist=60",
        ])
        .arg("-S")
        .arg(socket)
        .arg(target)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        masters.insert(
            socket.to_owned(),
            SshMaster {
                target: target.into(),
                config_path: config_path.map(ToOwned::to_owned),
                socket: socket.to_owned(),
                leases: 0,
            },
        );
        Ok(())
    } else {
        Err(format!(
            "OpenSSH control master failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn release_control_master(socket: &Path) {
    let master = {
        let mut masters = ssh_masters().lock().unwrap();
        let Some(master) = masters.get_mut(socket) else {
            return;
        };
        master.leases = master.leases.saturating_sub(1);
        if master.leases != 0 {
            return;
        }
        masters.remove(socket)
    };
    if let Some(master) = master {
        let _ = ssh_base(master.config_path.as_deref())
            .arg("-S")
            .arg(&master.socket)
            .args(["-O", "exit"])
            .arg(&master.target)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = fs::remove_file(master.socket);
    }
}

pub(crate) fn close_all_control_masters() {
    let masters: Vec<_> = ssh_masters()
        .lock()
        .unwrap()
        .drain()
        .map(|(_, value)| value)
        .collect();
    for master in masters {
        let _ = ssh_base(master.config_path.as_deref())
            .arg("-S")
            .arg(&master.socket)
            .args(["-O", "exit"])
            .arg(&master.target)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = fs::remove_file(master.socket);
    }
}

fn ssh_base(config_path: Option<&str>) -> Command {
    let mut command = Command::new("ssh");
    if let Some(path) = config_path {
        command.arg("-F").arg(path);
    }
    command.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ServerAliveInterval=1",
        "-o",
        "ServerAliveCountMax=2",
    ]);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_socket_identity_is_profile_scoped() {
        let first = ssh_profile_control_socket("profile-a", "same-host", None).unwrap();
        let second = ssh_profile_control_socket("profile-b", "same-host", None).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            first,
            ssh_profile_control_socket("profile-a", "same-host", None).unwrap()
        );
    }
}
