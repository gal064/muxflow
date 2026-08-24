use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tmux_agent_protocol::HELPER_VERSION;

use super::{
    ConnectionSpec, SshLease, acquire_control_master, acquire_control_master_for_socket,
    host_helper_path, ssh_profile_control_socket, validate_ssh_target,
};

#[tauri::command]
pub fn probe_remote_helper(connection: ConnectionSpec) -> Result<serde_json::Value, String> {
    let ConnectionSpec::Ssh {
        profile_id,
        target,
        config_path,
    } = connection
    else {
        return Err("remote helper probing requires an SSH profile".into());
    };
    let mut probe = run_remote_probe(&profile_id, &target, config_path.as_deref())?;
    settle_compatibility(&mut probe);
    Ok(probe)
}

/// The single source of truth for "is the host running the helper this desktop
/// ships": the bytes match, or they do not.
///
/// The helper version string used to decide this, and it silently lied. A
/// release that changed the helper's behaviour without bumping `HELPER_VERSION`
/// left the probe reporting `compatible` while the bridge refused the same
/// helper at the handshake, so the upgrade button the user needed was hidden
/// exactly when it was required. A digest cannot drift from what it describes,
/// and the packaged Linux helpers are built reproducibly for this reason: the
/// same source produces the same bytes, so an equal digest is a real match and
/// not a coincidence.
///
/// An unreadable artifact leaves the probe's own answer alone: no packaged
/// artifact means there is nothing to install, and claiming a mismatch would
/// offer an upgrade that cannot run.
fn settle_compatibility(probe: &mut serde_json::Value) {
    let Some(architecture) = probe
        .get("architecture")
        .and_then(serde_json::Value::as_str)
    else {
        return;
    };
    let Ok(expected) = helper_artifact_for_arch(architecture).and_then(|path| sha256_file(&path))
    else {
        return;
    };
    let matches = probe
        .get("digest")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|installed| installed == expected);
    // A mismatch says the helper is not ours; it does not say ours is newer.
    // Pushing on that alone lets an older desktop overwrite a helper a newer one
    // installed, and with two desktops on one host each would keep reinstalling
    // over the other. Direction decides who moves: if the host already runs a
    // newer helper, this app is the stale side and must be updated instead.
    let app_outdated = probe
        .get("helperVersion")
        .and_then(serde_json::Value::as_str)
        .and_then(|installed| {
            Some((
                release_ordinal(installed)?,
                release_ordinal(HELPER_VERSION)?,
            ))
        })
        .is_some_and(|(installed, ours)| installed > ours);
    if let Some(object) = probe.as_object_mut() {
        object.insert("compatible".into(), serde_json::Value::Bool(matches));
        object.insert("appOutdated".into(), serde_json::Value::Bool(app_outdated));
        object.insert(
            "expectedHelperVersion".into(),
            serde_json::Value::String(HELPER_VERSION.into()),
        );
    }
}

/// `major.minor.patch` as one comparable number, or `None` when it is not that
/// shape — an unreadable version orders against nothing, so the caller keeps its
/// existing answer rather than guessing a direction from a string it cannot parse.
fn release_ordinal(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let ordinal = (next()?, next()?, next()?);
    parts.next().is_none().then_some(ordinal)
}

#[tauri::command]
pub fn install_remote_helper(
    connection: ConnectionSpec,
    allow_upgrade: bool,
) -> HelperInstallReport {
    helper_install_report(install_remote_helper_inner(connection, allow_upgrade))
}

fn helper_install_report(result: Result<String, String>) -> HelperInstallReport {
    match result {
        Ok(message) => HelperInstallReport {
            ok: true,
            message,
            rollback: HelperRollback::NotNeeded,
        },
        Err(message) => HelperInstallReport {
            ok: false,
            rollback: if message.contains("[rollback=restored]") {
                HelperRollback::Restored
            } else if message.contains("[rollback=failed]") {
                HelperRollback::Failed
            } else {
                HelperRollback::NotNeeded
            },
            message: message
                .replace(" [rollback=restored]", "")
                .replace(" [rollback=failed]", ""),
        },
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HelperInstallReport {
    ok: bool,
    message: String,
    rollback: HelperRollback,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HelperRollback {
    NotNeeded,
    Restored,
    Failed,
}

fn install_remote_helper_inner(
    connection: ConnectionSpec,
    allow_upgrade: bool,
) -> Result<String, String> {
    let lease = acquire_control_master(&connection)?
        .ok_or("remote helper installation requires an SSH profile")?;
    let ConnectionSpec::Ssh {
        profile_id: _,
        target,
        config_path,
    } = connection
    else {
        return Err("remote helper installation requires an SSH profile".into());
    };
    validate_ssh_target(&target)?;
    let mut probe = run_remote_probe_with_lease(&target, config_path.as_deref(), &lease)?;
    settle_compatibility(&mut probe);
    if probe
        .get("appOutdated")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Err(format!(
            "remote helper {} is newer than this app expects {}; refusing downgrade",
            probe
                .get("helperVersion")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown"),
            HELPER_VERSION,
        ));
    }
    let remote_arch = probe
        .get("architecture")
        .and_then(serde_json::Value::as_str)
        .ok_or("remote helper probe omitted architecture")?;
    let artifact = helper_artifact_for_arch(remote_arch)?;
    let digest = sha256_file(&artifact)?;
    let mut command = Command::new(host_helper_path()?);
    command
        .args(["helper", "install", &target, "--artifact"])
        .arg(&artifact)
        .args([
            "--digest",
            &digest,
            "--expected-arch",
            normalize_architecture(remote_arch),
            "--expected-version",
            HELPER_VERSION,
        ]);
    if let Some(control_socket) = lease.control_socket() {
        command.arg("--control-socket").arg(control_socket);
    }
    if let Some(path) = config_path {
        command.args(["--config", &path]);
    }
    if allow_upgrade {
        command.arg("--allow-upgrade");
    }
    let output = command.output().map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn run_remote_probe(
    profile_id: &str,
    target: &str,
    config_path: Option<&str>,
) -> Result<serde_json::Value, String> {
    validate_ssh_target(target)?;
    let control_socket = ssh_profile_control_socket(profile_id, target, config_path)?;
    let lease = acquire_control_master_for_socket(target, config_path, &control_socket, &|| false)?;
    run_remote_probe_with_lease(target, config_path, &lease)
}

fn run_remote_probe_with_lease(
    target: &str,
    config_path: Option<&str>,
    lease: &SshLease,
) -> Result<serde_json::Value, String> {
    let mut command = Command::new(host_helper_path()?);
    command.args(["helper", "probe", target]);
    if let Some(control_socket) = lease.control_socket() {
        command.arg("--control-socket").arg(control_socket);
    }
    if let Some(path) = config_path {
        command.args(["--config", path]);
    }
    let output = command.output().map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())
}

fn helper_artifact_for_arch(architecture: &str) -> Result<PathBuf, String> {
    let architecture = normalize_architecture(architecture);
    let variable = match architecture {
        "x86_64" => "ADE_HOST_HELPER_X86_64_PATH",
        "aarch64" => "ADE_HOST_HELPER_AARCH64_PATH",
        other => return Err(format!("unsupported remote helper architecture {other}")),
    };
    if let Some(path) = std::env::var_os(variable) {
        return Ok(path.into());
    }
    // A native desktop helper is reusable only when the desktop itself is a
    // Linux ELF. A same-architecture macOS helper is Mach-O and must never be
    // uploaded to a Linux host.
    if cfg!(target_os = "linux") && architecture == normalize_architecture(std::env::consts::ARCH) {
        return host_helper_path();
    }
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    if let Some(parent) = current.parent() {
        let filename = format!("muxflow-host-linux-{architecture}");
        for packaged in [
            parent.join(&filename),
            parent.join("../Resources").join(&filename),
        ] {
            if packaged.is_file() {
                return Ok(packaged);
            }
        }
    }
    Err(format!(
        "no packaged {architecture} remote helper artifact is available"
    ))
}

fn normalize_architecture(value: &str) -> &str {
    match value {
        "amd64" | "x86_64" => "x86_64",
        "arm64" | "aarch64" => "aarch64",
        other => other,
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let length = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if length == 0 {
            break;
        }
        hasher.update(&buffer[..length]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_install_result_exposes_typed_rollback_without_ui_message_parsing() {
        assert_eq!(
            helper_install_report(Err("handshake failed [rollback=restored]".into())),
            HelperInstallReport {
                ok: false,
                message: "handshake failed".into(),
                rollback: HelperRollback::Restored,
            }
        );
        assert_eq!(
            helper_install_report(Err("replacement failed [rollback=failed]: disk".into()))
                .rollback,
            HelperRollback::Failed
        );
        assert_eq!(
            helper_install_report(Ok("installed".into())),
            HelperInstallReport {
                ok: true,
                message: "installed".into(),
                rollback: HelperRollback::NotNeeded,
            }
        );
    }
}
