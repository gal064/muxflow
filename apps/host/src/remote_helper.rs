use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const DEFAULT_REMOTE_PATH: &str = "$HOME/.local/bin/tmux-ide-host";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProbeReport {
    operating_system: String,
    architecture: String,
    tmux_version: String,
    git_version: String,
    installed: bool,
    helper_version: Option<String>,
    compatible: bool,
    digest: Option<String>,
    remote_path: String,
}

#[derive(Debug)]
struct Options {
    target: String,
    config: Option<PathBuf>,
    remote_path: String,
    artifact: Option<PathBuf>,
    digest: Option<String>,
    allow_upgrade: bool,
    expected_arch: Option<String>,
    control_socket: Option<PathBuf>,
    test_fail_after_shutdown: bool,
}

pub fn run_cli(arguments: Vec<String>) -> anyhow::Result<()> {
    let action = arguments
        .first()
        .map(String::as_str)
        .context("usage: tmux-ide-host helper <probe|install> TARGET [options]")?;
    let options = parse_options(&arguments[1..])?;
    let connection = SshControl::start(
        &options.target,
        options.config.as_deref(),
        options.control_socket.as_deref(),
    )?;
    match action {
        "probe" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&probe(&connection, &options.remote_path)?)?
            );
            Ok(())
        }
        "install" => install(&connection, &options),
        _ => bail!("unknown helper action {action}"),
    }
}

fn parse_options(arguments: &[String]) -> anyhow::Result<Options> {
    let target = arguments
        .first()
        .context("helper action requires an SSH target")?
        .clone();
    validate_target(&target)?;
    let mut options = Options {
        target,
        config: None,
        remote_path: DEFAULT_REMOTE_PATH.into(),
        artifact: None,
        digest: None,
        allow_upgrade: false,
        expected_arch: None,
        control_socket: None,
        test_fail_after_shutdown: false,
    };
    let mut index = 1;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--config" | "--remote-path" | "--artifact" | "--digest" | "--expected-arch"
            | "--control-socket" => {
                let flag = arguments[index].as_str();
                let value = arguments
                    .get(index + 1)
                    .with_context(|| format!("{flag} requires a value"))?
                    .clone();
                match flag {
                    "--config" => options.config = Some(value.into()),
                    "--remote-path" => options.remote_path = value,
                    "--artifact" => options.artifact = Some(value.into()),
                    "--digest" => options.digest = Some(value),
                    "--expected-arch" => options.expected_arch = Some(value),
                    "--control-socket" => options.control_socket = Some(value.into()),
                    _ => unreachable!(),
                }
                index += 2;
            }
            "--allow-upgrade" => {
                options.allow_upgrade = true;
                index += 1;
            }
            "--test-fail-after-shutdown" => {
                if std::env::var_os("ADE_PHASE1_TESTING").is_none() {
                    bail!("test fault injection requires ADE_PHASE1_TESTING");
                }
                options.test_fail_after_shutdown = true;
                index += 1;
            }
            other => bail!("unknown helper option {other}"),
        }
    }
    validate_remote_path(&options.remote_path)?;
    Ok(options)
}

fn probe(connection: &SshControl, remote_path: &str) -> anyhow::Result<ProbeReport> {
    let path = expand_remote_path(remote_path);
    let script = format!(
        "set -eu; os=$(uname -s); arch=$(uname -m); printf '%s\\n%s\\n' \"$os\" \"$arch\"; tmux -V; if git_version=$(git --version 2>/dev/null); then printf '%s\\n' \"$git_version\"; else printf 'unavailable\\n'; fi; if [ -x {path} ]; then printf 'installed\\n'; if version=$({path} version 2>/dev/null); then printf '%s\\n' \"$version\"; else printf '\\n'; fi; sha256sum {path} | cut -d' ' -f1; else printf 'absent\\n\\n\\n'; fi"
    );
    let output = connection.command(&script)?;
    let text = String::from_utf8(output)?;
    let mut lines = text.lines().map(ToOwned::to_owned);
    let operating_system = lines.next().context("remote probe omitted OS")?;
    let architecture = lines.next().context("remote probe omitted architecture")?;
    let tmux_version = lines.next().context("remote probe omitted tmux version")?;
    let git_version = lines.next().context("remote probe omitted Git version")?;
    let status = lines
        .next()
        .context("remote probe omitted install status")?;
    let version_line = lines.next().unwrap_or_default();
    let digest = lines.next().filter(|value| !value.is_empty());
    let helper_version = serde_json::from_str::<serde_json::Value>(&version_line)
        .ok()
        .and_then(|value| value.get("helperVersion")?.as_str().map(ToOwned::to_owned));
    let compatible = helper_version.as_deref() == Some(tmux_agent_protocol::HELPER_VERSION);
    Ok(ProbeReport {
        operating_system,
        architecture,
        tmux_version,
        git_version,
        installed: status == "installed",
        helper_version,
        compatible,
        digest,
        remote_path: remote_path.into(),
    })
}

fn install(connection: &SshControl, options: &Options) -> anyhow::Result<()> {
    let artifact = options
        .artifact
        .as_deref()
        .context("install requires --artifact")?;
    let expected_digest = options
        .digest
        .as_deref()
        .context("install requires --digest")?;
    if !expected_digest.bytes().all(|byte| byte.is_ascii_hexdigit()) || expected_digest.len() != 64
    {
        bail!("expected digest must be 64 hexadecimal characters");
    }
    let expected_digest = expected_digest.to_ascii_lowercase();
    let actual_digest = sha256_file(artifact)?;
    if actual_digest != expected_digest {
        bail!("local helper digest mismatch; refusing upload");
    }
    let artifact_arch = elf_architecture(artifact)?;
    if let Some(expected_arch) = &options.expected_arch
        && normalize_arch(expected_arch) != artifact_arch
    {
        bail!("helper architecture {artifact_arch} does not match expected {expected_arch}");
    }
    let before = probe(connection, &options.remote_path)?;
    if before.operating_system != "Linux" {
        bail!(
            "remote helper supports Linux only, found {}",
            before.operating_system
        );
    }
    if !tmux_supported(&before.tmux_version) {
        bail!(
            "remote tmux 3.3 or newer is required, found {}",
            before.tmux_version
        );
    }
    if normalize_arch(&before.architecture) != artifact_arch {
        bail!(
            "helper architecture {artifact_arch} does not match remote {}",
            before.architecture
        );
    }
    let final_path = expand_remote_path(&options.remote_path);
    if before.installed && before.digest.as_deref() == Some(expected_digest.as_str()) {
        restart_remote_daemon(connection, &final_path, false)?;
        println!("helper-current: pass digest={expected_digest}");
        return Ok(());
    }
    if before.installed && !options.allow_upgrade {
        bail!("a different helper is installed; explicit --allow-upgrade is required");
    }

    let partial = format!("{final_path}.{}.partial", Uuid::new_v4());
    let parent = final_path
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .context("remote helper path has no parent")?;
    connection.command(&format!(
        "set -eu; install -d -m 0700 {parent}; umask 077; : > {partial}"
    ))?;
    if let Err(error) = connection.upload_independent(artifact, &partial) {
        let _ = connection.command(&format!("rm -f {partial}"));
        return Err(error);
    }
    let remote_digest =
        String::from_utf8(connection.command(&format!("sha256sum {partial} | cut -d' ' -f1"))?)?
            .trim()
            .to_owned();
    if remote_digest != expected_digest {
        let _ = connection.command(&format!("rm -f {partial}"));
        bail!("uploaded helper digest mismatch; existing helper was not replaced");
    }
    let metadata = String::from_utf8(
        connection.command(&format!("chmod 0700 {partial}; {partial} version"))?,
    )?;
    let value: serde_json::Value = serde_json::from_str(metadata.trim())
        .context("uploaded helper failed version verification")?;
    if value.get("architecture").and_then(|value| value.as_str()) != Some(artifact_arch) {
        let _ = connection.command(&format!("rm -f {partial}"));
        bail!("uploaded helper reported unexpected architecture");
    }
    let backup = format!("{final_path}.{}.previous", Uuid::new_v4());
    connection.command(&format!(
        "set -eu; chmod 0755 {partial}; if [ -e {final_path} ]; then cp -p {final_path} {backup}; fi; mv -f {partial} {final_path}"
    ))?;
    if let Err(error) =
        restart_remote_daemon(connection, &final_path, options.test_fail_after_shutdown)
    {
        let rollback = connection.command(&format!(
            "set -eu; runtime=${{XDG_RUNTIME_DIR:-/tmp/tmux-agent-ide-$(id -u)}}; install -d -m 0700 \"$runtime\"; metadata=\"$runtime/daemon.json\"; socket=\"$runtime/host.sock\"; log=\"$runtime/daemon-start.log\"; if [ -S \"$socket\" ]; then if ! {final_path} daemon-stop >/dev/null 2>&1; then pid=$(sed -n 's/.*\"pid\":\\([0-9][0-9]*\\).*/\\1/p' \"$metadata\"); recorded_start=$(sed -n 's/.*\"processStartTime\":\\([0-9][0-9]*\\).*/\\1/p' \"$metadata\"); recorded_exe=$(sed -n 's/.*\"executable\":\"\\([^\"]*\\)\".*/\\1/p' \"$metadata\"); [ -n \"$pid\" ] && [ -n \"$recorded_start\" ] && [ -n \"$recorded_exe\" ]; actual_start=$(awk '{{print $22}}' \"/proc/$pid/stat\"); actual_exe=$(readlink \"/proc/$pid/exe\"); actual_exe=${{actual_exe% (deleted)}}; [ \"$actual_start\" = \"$recorded_start\" ] && [ \"$actual_exe\" = \"$recorded_exe\" ]; kill -TERM \"$pid\"; for i in $(seq 1 100); do kill -0 \"$pid\" 2>/dev/null || break; sleep 0.05; done; ! kill -0 \"$pid\" 2>/dev/null; rm -f \"$socket\" \"$metadata\"; fi; fi; for i in $(seq 1 100); do [ ! -S \"$socket\" ] && break; sleep 0.05; done; [ ! -S \"$socket\" ]; if [ -e {backup} ]; then mv -f {backup} {final_path}; nohup {final_path} daemon </dev/null >\"$log\" 2>&1 & for i in $(seq 1 100); do [ -S \"$socket\" ] && break; sleep 0.05; done; {final_path} protocol-check >/dev/null || {{ cat \"$log\" >&2; false; }}; else rm -f {final_path}; fi"
        ));
        return match rollback {
            Ok(_) => Err(error).context(
                "new helper failed handshake; previous helper restored [rollback=restored]",
            ),
            Err(rollback_error) => Err(error).context(format!(
                "new helper failed handshake and rollback also failed [rollback=failed]: {rollback_error:#}"
            )),
        };
    }
    connection.command(&format!("rm -f {backup}"))?;
    println!(
        "helper-installed: pass digest={expected_digest} path={}",
        options.remote_path
    );
    Ok(())
}

fn restart_remote_daemon(
    connection: &SshControl,
    final_path: &str,
    test_fail_after_shutdown: bool,
) -> anyhow::Result<()> {
    let fault = if test_fail_after_shutdown {
        "false;"
    } else {
        ""
    };
    let script = format!(
        "set -eu; runtime=${{XDG_RUNTIME_DIR:-/tmp/tmux-agent-ide-$(id -u)}}; install -d -m 0700 \"$runtime\"; metadata=\"$runtime/daemon.json\"; socket=\"$runtime/host.sock\"; log=\"$runtime/daemon-start.log\"; if [ -S \"$socket\" ]; then if ! {final_path} daemon-stop >/dev/null 2>&1; then pid=$(sed -n 's/.*\"pid\":\\([0-9][0-9]*\\).*/\\1/p' \"$metadata\"); recorded_start=$(sed -n 's/.*\"processStartTime\":\\([0-9][0-9]*\\).*/\\1/p' \"$metadata\"); recorded_exe=$(sed -n 's/.*\"executable\":\"\\([^\"]*\\)\".*/\\1/p' \"$metadata\"); [ -n \"$pid\" ] && [ -n \"$recorded_start\" ] && [ -n \"$recorded_exe\" ]; actual_start=$(awk '{{print $22}}' \"/proc/$pid/stat\"); actual_exe=$(readlink \"/proc/$pid/exe\"); actual_exe=${{actual_exe% (deleted)}}; [ \"$actual_start\" = \"$recorded_start\" ] && [ \"$actual_exe\" = \"$recorded_exe\" ]; kill -TERM \"$pid\"; for i in $(seq 1 100); do kill -0 \"$pid\" 2>/dev/null || break; sleep 0.05; done; ! kill -0 \"$pid\" 2>/dev/null; rm -f \"$socket\" \"$metadata\"; fi; fi; for i in $(seq 1 100); do [ ! -S \"$socket\" ] && break; sleep 0.05; done; [ ! -S \"$socket\" ]; {fault} nohup {final_path} daemon </dev/null >\"$log\" 2>&1 & for i in $(seq 1 100); do [ -S \"$socket\" ] && break; sleep 0.05; done; [ -S \"$socket\" ]; {final_path} protocol-check >/dev/null || {{ cat \"$log\" >&2; false; }}"
    );
    connection
        .command(&script)
        .context("restart and verify upgraded remote daemon")?;
    Ok(())
}

struct SshControl {
    target: String,
    config: Option<PathBuf>,
    socket: PathBuf,
    owned_socket: bool,
}

impl SshControl {
    fn start(
        target: &str,
        config: Option<&Path>,
        shared_socket: Option<&Path>,
    ) -> anyhow::Result<Self> {
        validate_target(target)?;
        // OpenSSH appends a temporary suffix while creating a control socket;
        // keep this path short enough for Linux's 108-byte AF_UNIX limit.
        let runtime = PathBuf::from(format!("/tmp/tmux-agent-ide-{}", unsafe {
            libc::geteuid()
        }))
        .join("ssh");
        crate::paths::prepare_runtime_dir(&runtime)?;
        let socket = shared_socket
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| runtime.join(format!("control-{}.sock", Uuid::new_v4())));
        let value = Self {
            target: target.into(),
            config: config.map(ToOwned::to_owned),
            socket,
            owned_socket: shared_socket.is_none(),
        };
        let check = value
            .base_command()
            .arg("-S")
            .arg(&value.socket)
            .args(["-O", "check"])
            .arg(&value.target)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if check.is_ok_and(|status| status.success()) {
            return Ok(value);
        }
        if value.socket.exists() {
            fs::remove_file(&value.socket).context("remove stale OpenSSH control socket")?;
        }
        let output = value
            .base_command()
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
            .arg(&value.socket)
            .arg(&value.target)
            .output()
            .context("start OpenSSH control master")?;
        if !output.status.success() {
            bail!(
                "OpenSSH control master failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(value)
    }

    fn command(&self, script: &str) -> anyhow::Result<Vec<u8>> {
        let output = self
            .base_command()
            .arg("-T")
            .arg("-S")
            .arg(&self.socket)
            .arg(&self.target)
            .arg(script)
            .output()?;
        if !output.status.success() {
            bail!(
                "remote command failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output.stdout)
    }

    fn upload_independent(&self, source: &Path, destination: &str) -> anyhow::Result<()> {
        let mut child = self
            .base_command()
            .args(["-T", "-o", "ControlMaster=no", "-o", "ControlPath=none"])
            .arg(&self.target)
            .arg(format!("cat > {destination}"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut input = child.stdin.take().context("SSH upload stdin unavailable")?;
        let mut file = File::open(source)?;
        std::io::copy(&mut file, &mut input)?;
        input.flush()?;
        drop(input);
        let output = child.wait_with_output()?;
        if !output.status.success() {
            bail!(
                "helper upload failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    fn base_command(&self) -> Command {
        let mut command = Command::new("ssh");
        if let Some(config) = &self.config {
            command.arg("-F").arg(config);
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
}

impl Drop for SshControl {
    fn drop(&mut self) {
        if !self.owned_socket {
            return;
        }
        let _ = self
            .base_command()
            .arg("-S")
            .arg(&self.socket)
            .args(["-O", "exit"])
            .arg(&self.target)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = fs::remove_file(&self.socket);
    }
}

fn validate_target(target: &str) -> anyhow::Result<()> {
    if target.is_empty()
        || target.starts_with('-')
        || target.chars().any(char::is_whitespace)
        || target.bytes().any(|byte| byte == 0)
    {
        bail!("invalid SSH target");
    }
    Ok(())
}

fn validate_remote_path(path: &str) -> anyhow::Result<()> {
    let suffix = path.strip_prefix("$HOME/");
    if suffix.is_none_or(|suffix| {
        suffix.is_empty()
            || suffix.contains("..")
            || !suffix.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-')
            })
    }) {
        bail!("remote helper path must be a simple path below $HOME");
    }
    Ok(())
}

fn expand_remote_path(path: &str) -> String {
    path.replacen("$HOME", "\"$HOME\"", 1)
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        digest.update(&buffer[..length]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn elf_architecture(path: &Path) -> anyhow::Result<&'static str> {
    let mut file = File::open(path)?;
    let mut header = [0_u8; 20];
    file.read_exact(&mut header)?;
    if &header[..4] != b"\x7fELF" || header[5] != 1 {
        bail!("helper artifact is not a little-endian ELF executable");
    }
    match u16::from_le_bytes([header[18], header[19]]) {
        62 => Ok("x86_64"),
        183 => Ok("aarch64"),
        machine => bail!("unsupported ELF machine {machine}"),
    }
}

fn normalize_arch(value: &str) -> &str {
    match value {
        "amd64" | "x86_64" => "x86_64",
        "arm64" | "aarch64" => "aarch64",
        other => other,
    }
}

fn tmux_supported(version: &str) -> bool {
    let Some(version) = version.strip_prefix("tmux ") else {
        return false;
    };
    let mut parts = version.split(['.', 'a', 'b', 'c', 'd']);
    let major = parts.next().and_then(|value| value.parse::<u32>().ok());
    let minor = parts.next().and_then(|value| value.parse::<u32>().ok());
    matches!((major, minor), (Some(major), Some(minor)) if major > 3 || (major == 3 && minor >= 3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_targets_and_paths() {
        assert!(validate_target("workbox").is_ok());
        assert!(validate_target("-oProxyCommand=bad").is_err());
        assert!(validate_remote_path("$HOME/.local/bin/tmux-ide-host").is_ok());
        assert!(validate_remote_path("$HOME/../victim").is_err());
    }

    #[test]
    fn enforces_minimum_tmux_version() {
        assert!(tmux_supported("tmux 3.3a"));
        assert!(tmux_supported("tmux 3.7"));
        assert!(!tmux_supported("tmux 3.2"));
    }
}
