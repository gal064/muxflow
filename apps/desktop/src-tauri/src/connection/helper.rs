use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{
    ConnectionSpec, ControlLane, acquire_control_master, ensure_control_master, host_helper_path,
    ssh_profile_control_socket, validate_ssh_target,
};

#[tauri::command]
pub fn probe_remote_helper(connection: ConnectionSpec) -> Result<serde_json::Value, String> {
    let _lease = acquire_control_master(&connection)?;
    let ConnectionSpec::Ssh {
        profile_id,
        target,
        config_path,
    } = connection
    else {
        return Err("remote helper probing requires an SSH profile".into());
    };
    run_remote_probe(&profile_id, &target, config_path.as_deref())
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
    let _lease = acquire_control_master(&connection)?;
    let ConnectionSpec::Ssh {
        profile_id,
        target,
        config_path,
    } = connection
    else {
        return Err("remote helper installation requires an SSH profile".into());
    };
    validate_ssh_target(&target)?;
    let probe = run_remote_probe(&profile_id, &target, config_path.as_deref())?;
    let remote_arch = probe
        .get("architecture")
        .and_then(serde_json::Value::as_str)
        .ok_or("remote helper probe omitted architecture")?;
    let artifact = helper_artifact_for_arch(remote_arch)?;
    let digest = sha256_file(&artifact)?;
    let control_socket = ssh_profile_control_socket(&profile_id, &target, config_path.as_deref())?;
    ensure_control_master(
        &target,
        config_path.as_deref(),
        &control_socket,
        ControlLane::Interactive,
    )?;
    let mut command = Command::new(host_helper_path()?);
    command
        .args(["helper", "install", &target, "--artifact"])
        .arg(&artifact)
        .args([
            "--digest",
            &digest,
            "--expected-arch",
            normalize_architecture(remote_arch),
        ]);
    command.arg("--control-socket").arg(&control_socket);
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
    ensure_control_master(
        target,
        config_path,
        &control_socket,
        ControlLane::Interactive,
    )?;
    let mut command = Command::new(host_helper_path()?);
    command.args(["helper", "probe", target]);
    command.arg("--control-socket").arg(&control_socket);
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
        let filename = format!("tmux-ide-host-linux-{architecture}");
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
