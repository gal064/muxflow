use std::{
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const DEFAULT_REMOTE_PATH: &str = "$HOME/.local/bin/muxflow-host";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProbeReport {
    operating_system: String,
    architecture: String,
    tmux_version: String,
    git_version: String,
    installed: bool,
    helper_version: Option<String>,
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
        .context("usage: muxflow-host helper <probe|install> TARGET [options]")?;
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
        "set -eu; os=$(uname -s); arch=$(uname -m); printf '%s\\n%s\\n' \"$os\" \"$arch\"; {}; if git_version=$(git --version 2>/dev/null); then printf '%s\\n' \"$git_version\"; else printf 'unavailable\\n'; fi; if [ -x {path} ]; then printf 'installed\\n'; if version=$({path} version 2>/dev/null); then printf '%s\\n' \"$version\"; else printf '\\n'; fi; {}; else printf 'absent\\n\\n\\n'; fi",
        remote_tmux_version_command(),
        remote_sha256(&path),
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
    Ok(ProbeReport {
        operating_system,
        architecture,
        tmux_version,
        git_version,
        installed: status == "installed",
        helper_version,
        digest,
        remote_path: remote_path.into(),
    })
}

/// Prints a remote file's SHA-256. GNU coreutils has `sha256sum`; macOS before
/// 14 has only `shasum`. Both print `digest  path`.
fn remote_sha256(path: &str) -> String {
    format!(
        "{{ if command -v sha256sum >/dev/null 2>&1; then sha256sum {path}; else shasum -a 256 {path}; fi; }} | cut -d' ' -f1"
    )
}

/// Finds tmux in the non-interactive SSH environment without sourcing a shell
/// profile. Kept in step with `tmux-control`'s Linux and macOS candidates: this
/// probe runs before a helper is necessarily installed, so it cannot delegate
/// yet. macOS `sshd` runs commands with no Homebrew or MacPorts on PATH.
fn remote_tmux_version_command() -> &'static str {
    r#"tmux_bin=''; if [ "${MUXFLOW_TMUX_PATH+x}" = x ]; then case "$MUXFLOW_TMUX_PATH" in /*) ;; *) echo 'MUXFLOW_TMUX_PATH must be absolute' >&2; exit 1;; esac; [ -f "$MUXFLOW_TMUX_PATH" ] && [ -x "$MUXFLOW_TMUX_PATH" ] || { echo 'MUXFLOW_TMUX_PATH is not executable' >&2; exit 1; }; tmux_bin=$MUXFLOW_TMUX_PATH; elif resolved=$(command -v tmux 2>/dev/null) && [ "${resolved#/}" != "$resolved" ] && [ -f "$resolved" ] && [ -x "$resolved" ]; then tmux_bin=$resolved; else for candidate in /usr/local/bin/tmux /usr/bin/tmux /bin/tmux /home/linuxbrew/.linuxbrew/bin/tmux /run/current-system/sw/bin/tmux /nix/var/nix/profiles/default/bin/tmux /usr/pkg/bin/tmux /snap/bin/tmux /opt/homebrew/bin/tmux /opt/local/bin/tmux /opt/pkg/bin/tmux "$HOME/.local/bin/tmux" "$HOME/.nix-profile/bin/tmux" "$HOME/.linuxbrew/bin/tmux"; do if [ -f "$candidate" ] && [ -x "$candidate" ]; then tmux_bin=$candidate; break; fi; done; fi; [ -n "$tmux_bin" ] || { echo 'tmux executable was not found' >&2; exit 127; }; "$tmux_bin" -V"#
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
    let (artifact_os, artifact_arch) = executable_target(artifact)?;
    if let Some(expected_arch) = &options.expected_arch
        && normalize_arch(expected_arch) != artifact_arch
    {
        bail!("helper architecture {artifact_arch} does not match expected {expected_arch}");
    }
    let before = probe(connection, &options.remote_path)?;
    if before.operating_system != artifact_os {
        bail!(
            "helper is built for {artifact_os}, but the host runs {}",
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
    let staged = (|| {
        connection.command(&format!(
            "set -eu; install -d -m 0700 {parent}; umask 077; : > {partial}"
        ))?;
        connection.upload_independent(artifact, &partial)?;
        let remote_digest = String::from_utf8(connection.command(&remote_sha256(&partial))?)?
            .trim()
            .to_owned();
        if remote_digest != expected_digest {
            bail!("uploaded helper digest mismatch; existing helper was not replaced");
        }
        let metadata = String::from_utf8(
            connection.command(&format!("chmod 0700 {partial}; {partial} version"))?,
        )?;
        let value: serde_json::Value = serde_json::from_str(metadata.trim())
            .context("uploaded helper failed version verification")?;
        if value.get("architecture").and_then(|value| value.as_str()) != Some(artifact_arch) {
            bail!("uploaded helper reported unexpected architecture");
        }
        // Serialize publication and rollback while each upload keeps its private path.
        let install_lock = RemoteInstallLock::acquire(connection, &final_path)?;
        let backup = format!("{final_path}.{}.previous", Uuid::new_v4());
        connection.command(&format!(
            "set -eu; chmod 0755 {partial}; if [ -e {final_path} ]; then cp -p {final_path} {backup}; fi; mv -f {partial} {final_path}"
        ))?;
        Ok((install_lock, backup))
    })();
    let (_install_lock, backup) = cleanup_remote_partial_on_error(staged, || {
        let _ = connection.command(&format!("rm -f {partial}"));
    })?;
    if let Err(error) =
        restart_remote_daemon(connection, &final_path, options.test_fail_after_shutdown)
    {
        let rollback = connection.command(&format!(
            "set -eu; {} if [ -e {backup} ]; then mv -f {backup} {final_path}; nohup {final_path} daemon </dev/null >\"$log\" 2>&1 & for i in $(seq 1 100); do [ -S \"$socket\" ] && break; sleep 0.05; done; {final_path} protocol-check >/dev/null || {{ cat \"$log\" >&2; false; }}; else rm -f {final_path}; fi",
            stop_remote_daemon(&final_path),
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

/// Runs cleanup for every error while an upload is still owned by its private
/// staging path. Once the caller returns success, the partial has been moved to
/// its final path and must no longer be removed by this transaction.
fn cleanup_remote_partial_on_error<T>(
    result: anyhow::Result<T>,
    cleanup: impl FnOnce(),
) -> anyhow::Result<T> {
    if result.is_err() {
        cleanup();
    }
    result
}

struct RemoteInstallLock<'a> {
    connection: &'a SshControl,
    path: String,
}

impl<'a> RemoteInstallLock<'a> {
    fn acquire(connection: &'a SshControl, final_path: &str) -> anyhow::Result<Self> {
        let path = format!("{final_path}.install.lock");
        connection
            .command(&format!(
                "set -eu; for i in $(seq 1 300); do if mkdir {path} 2>/dev/null; then exit 0; fi; sleep 0.1; done; echo 'another helper install still owns the remote lock' >&2; exit 1"
            ))
            .context("acquire remote helper install lock")?;
        Ok(Self { connection, path })
    }
}

impl Drop for RemoteInstallLock<'_> {
    fn drop(&mut self) {
        let _ = self.connection.command(&format!("rmdir {}", self.path));
    }
}

/// Sets `$socket` and `$log` and stops whatever daemon holds the socket.
/// `daemon-stop --force` asks it to shut down and, if it cannot, sends SIGTERM
/// only after proving the recorded PID is still that daemon — the same check
/// on Linux and macOS, so this script needs no `/proc`.
fn stop_remote_daemon(final_path: &str) -> String {
    format!(
        "runtime=/tmp/muxflow-$(id -u); install -d -m 0700 \"$runtime\"; socket=\"$runtime/host.sock\"; log=\"$runtime/daemon-start.log\"; if [ -S \"$socket\" ]; then {final_path} daemon-stop --force >/dev/null; fi; for i in $(seq 1 100); do [ ! -S \"$socket\" ] && break; sleep 0.05; done; [ ! -S \"$socket\" ];"
    )
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
        "set -eu; {} {fault} nohup {final_path} daemon </dev/null >\"$log\" 2>&1 & for i in $(seq 1 100); do [ -S \"$socket\" ] && break; sleep 0.05; done; [ -S \"$socket\" ]; {final_path} protocol-check >/dev/null || {{ cat \"$log\" >&2; false; }}",
        stop_remote_daemon(final_path),
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
    borrowed_identity: Option<SocketIdentity>,
    owned_master: Option<OwnedControlMaster>,
}

#[derive(Debug)]
struct OwnedControlMaster {
    child: Child,
    socket_identity: SocketIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

impl SshControl {
    fn start(
        target: &str,
        config: Option<&Path>,
        shared_socket: Option<&Path>,
    ) -> anyhow::Result<Self> {
        validate_target(target)?;
        if let Some(socket) = shared_socket {
            let borrowed_identity = safe_socket_identity(socket)?.with_context(|| {
                format!(
                    "borrowed OpenSSH control socket is missing: {}",
                    socket.display()
                )
            })?;
            let borrowed = Self {
                target: target.into(),
                config: config.map(ToOwned::to_owned),
                socket: socket.to_owned(),
                borrowed_identity: Some(borrowed_identity),
                owned_master: None,
            };
            if borrowed.check_control_master()? {
                return Ok(borrowed);
            }
            bail!(
                "borrowed OpenSSH control socket is not responsive; its owner must revalidate it"
            );
        }

        // OpenSSH appends a temporary suffix while creating a control socket;
        // keep this path short enough for Linux's 108-byte AF_UNIX limit.
        let runtime =
            PathBuf::from(format!("/tmp/muxflow-{}", unsafe { libc::geteuid() })).join("ssh");
        crate::paths::prepare_runtime_dir(&runtime)?;
        let socket = runtime.join(format!("control-{}.sock", Uuid::new_v4()));
        if safe_socket_identity(&socket)?.is_some() {
            bail!("refusing an existing private OpenSSH control-socket path");
        }
        let mut value = Self {
            target: target.into(),
            config: config.map(ToOwned::to_owned),
            socket,
            borrowed_identity: None,
            owned_master: None,
        };
        // No `ControlPersist`, for the reason the desktop's control-master lane
        // documents: it makes OpenSSH daemonize once the socket exists, so this
        // child exits while the real master keeps running reparented to init.
        // Everything below owns `child` as if it were the master — the loop
        // reads an exit as "died before creating its socket", and `Drop` kills
        // it and unlinks the socket it believes it owns. Under `ControlPersist`
        // both are wrong: establishment becomes a race between the socket
        // appearing and the child exiting, and a shutdown can unlink the socket
        // of a master that is still serving. Staying in the foreground makes
        // this type's ownership real.
        let mut child = value
            .base_command()
            .args(["-M", "-N", "-o", "ControlMaster=yes"])
            .arg("-S")
            .arg(&value.socket)
            .arg(&value.target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start OpenSSH control master")?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match safe_socket_identity(&value.socket) {
                Ok(Some(socket_identity)) => {
                    value.owned_master = Some(OwnedControlMaster {
                        child,
                        socket_identity,
                    });
                    return Ok(value);
                }
                Ok(None) => {}
                Err(error) => {
                    terminate_child_bounded(child, None);
                    return Err(error);
                }
            }
            match child.try_wait() {
                Ok(Some(_)) => {
                    bail!("OpenSSH control master exited before creating its socket");
                }
                Ok(None) => {}
                Err(error) => {
                    terminate_child_bounded(child, None);
                    return Err(error.into());
                }
            }
            if Instant::now() >= deadline {
                terminate_child_bounded(child, None);
                bail!("OpenSSH control master timed out before creating its socket");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn check_control_master(&self) -> anyhow::Result<bool> {
        self.validate_control_socket()?;
        let mut command = self.multiplexed_command();
        command
            .args(["-O", "check"])
            .arg(&self.target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        run_control_command(command, Duration::from_secs(2))
    }

    fn command(&self, script: &str) -> anyhow::Result<Vec<u8>> {
        self.validate_control_socket()?;
        let output = self
            .multiplexed_command()
            .arg("-T")
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
        let mut command = self.base_command();
        command
            .args(["-T", "-o", "ControlMaster=no", "-o", "ControlPath=none"])
            .arg(&self.target)
            .arg(format!("cat > {destination}"));
        upload_helper_file(command, source)
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
            // Match the desktop transport: a brief stall during the separate
            // upload must not kill SSH before it can finish the helper update.
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
        ]);
        command
    }

    fn multiplexed_command(&self) -> Command {
        let mut command = self.base_command();
        // `-S` alone still inherits ControlMaster=auto from user config. If the
        // borrowed owner disappears in the narrow gap after validation, that
        // would create an untracked master at its path. A mux client never
        // needs to become a master, and `ProxyCommand=false` makes the missing-
        // socket fallback fail closed instead of opening a direct connection.
        command
            .args(["-o", "ControlMaster=no", "-o", "ProxyCommand=false"])
            .arg("-S")
            .arg(&self.socket);
        command
    }

    fn validate_control_socket(&self) -> anyhow::Result<()> {
        let expected_identity = self.borrowed_identity.or_else(|| {
            self.owned_master
                .as_ref()
                .map(|owned| owned.socket_identity)
        });
        if let Some(identity) = expected_identity
            && safe_socket_identity(&self.socket)? != Some(identity)
        {
            bail!("OpenSSH control socket disappeared or was replaced");
        }
        Ok(())
    }
}

impl Drop for SshControl {
    fn drop(&mut self) {
        if let Some(owned) = self.owned_master.take() {
            terminate_child_bounded(owned.child, Some((&self.socket, owned.socket_identity)));
        }
    }
}

/// Close upload stdin and collect SSH's explanation even when copying fails.
/// Returning directly from `copy` loses stderr and leaves the child unreaped.
fn upload_helper_file(mut command: Command, source: &Path) -> anyhow::Result<()> {
    let mut file = File::open(source).context("read helper artifact for upload")?;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("start helper upload")?;
    let mut input = child.stdin.take().context("SSH upload stdin unavailable")?;
    let copied = std::io::copy(&mut file, &mut input).and_then(|_| input.flush());
    drop(input);
    let output = child.wait_with_output().context("wait for helper upload")?;
    let detail = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        bail!(
            "helper upload failed ({}): {}",
            output.status,
            detail.trim()
        );
    }
    copied.with_context(|| {
        let detail = detail.trim();
        if detail.is_empty() {
            "helper upload failed while copying the artifact".to_owned()
        } else {
            format!("helper upload failed while copying the artifact: {detail}")
        }
    })?;
    Ok(())
}

fn run_control_command(mut command: Command, timeout: Duration) -> anyhow::Result<bool> {
    let mut child = command.spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.success()),
            Ok(None) => {}
            Err(error) => {
                terminate_child_bounded(child, None);
                return Err(error.into());
            }
        }
        if Instant::now() >= deadline {
            terminate_child_bounded(child, None);
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn terminate_child_bounded(mut child: Child, owned_socket: Option<(&Path, SocketIdentity)>) {
    let _ = child.kill();
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => {
                if let Some((socket, identity)) = owned_socket {
                    remove_socket_if_identity(socket, identity);
                }
                return;
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let socket = owned_socket.map(|(path, identity)| (path.to_owned(), identity));
                let _ = thread::Builder::new()
                    .name("ssh-helper-master-reaper".into())
                    .spawn(move || {
                        let _ = child.wait();
                        if let Some((socket, identity)) = socket {
                            remove_socket_if_identity(&socket, identity);
                        }
                    });
                return;
            }
        }
    }
}

fn safe_socket_identity(socket: &Path) -> anyhow::Result<Option<SocketIdentity>> {
    let metadata = match fs::symlink_metadata(socket) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        bail!("refusing an unowned or unsafe OpenSSH control socket");
    }
    Ok(Some(SocketIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }))
}

fn remove_socket_if_identity(socket: &Path, owned_identity: SocketIdentity) {
    if safe_socket_identity(socket).ok().flatten() == Some(owned_identity) {
        let _ = fs::remove_file(socket);
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

/// The `uname -s` and architecture a helper executable runs on, read from its
/// header: a little-endian ELF for Linux or a thin 64-bit Mach-O for macOS.
fn executable_target(path: &Path) -> anyhow::Result<(&'static str, &'static str)> {
    let mut file = File::open(path)?;
    let mut header = [0_u8; 20];
    file.read_exact(&mut header)?;
    if &header[..4] == b"\x7fELF" && header[5] == 1 {
        return match u16::from_le_bytes([header[18], header[19]]) {
            62 => Ok(("Linux", "x86_64")),
            183 => Ok(("Linux", "aarch64")),
            machine => bail!("unsupported ELF machine {machine}"),
        };
    }
    if header[..4] == 0xfeed_facf_u32.to_le_bytes() {
        return match u32::from_le_bytes([header[4], header[5], header[6], header[7]]) {
            0x0100_0007 => Ok(("Darwin", "x86_64")),
            0x0100_000c => Ok(("Darwin", "aarch64")),
            cpu => bail!("unsupported Mach-O CPU type {cpu:#x}"),
        };
    }
    bail!("helper artifact is neither a little-endian ELF nor a 64-bit Mach-O executable")
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
    use std::{
        cell::Cell,
        os::unix::{fs::PermissionsExt, net::UnixListener},
        sync::mpsc,
    };

    #[test]
    fn upload_preserves_ssh_error_when_the_child_stops_reading() {
        let temporary = tempfile::tempdir().unwrap();
        let artifact = temporary.path().join("helper");
        fs::write(&artifact, vec![0_u8; 1024 * 1024]).unwrap();
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "exec 0<&-; echo 'Connection to fixture timed out' >&2; exit 255",
        ]);
        let error = upload_helper_file(command, &artifact).unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("Connection to fixture timed out"),
            "{message}"
        );
        assert!(message.contains("255"), "{message}");
    }

    #[test]
    fn upload_checks_the_child_result_after_all_bytes_are_copied() {
        let temporary = tempfile::tempdir().unwrap();
        let artifact = temporary.path().join("helper");
        fs::write(&artifact, b"artifact").unwrap();
        let mut command = Command::new("sh");
        command.args(["-c", "cat >/dev/null; echo 'remote disk full' >&2; exit 1"]);
        let error = upload_helper_file(command, &artifact).unwrap_err();
        assert!(error.to_string().contains("remote disk full"));
    }

    #[test]
    fn upload_delivers_the_complete_artifact_before_success() {
        let temporary = tempfile::tempdir().unwrap();
        let artifact = temporary.path().join("helper");
        let received = temporary.path().join("received");
        let bytes: Vec<_> = (0..=255).cycle().take(256 * 1024).collect();
        fs::write(&artifact, &bytes).unwrap();
        let mut command = Command::new("sh");
        command
            .args(["-c", "cat > \"$1\"", "upload-test"])
            .arg(&received);
        upload_helper_file(command, &artifact).unwrap();
        assert_eq!(fs::read(received).unwrap(), bytes);
    }

    #[test]
    fn remote_partial_cleanup_runs_only_when_staging_fails() {
        let cleaned = Cell::new(false);
        let failure = cleanup_remote_partial_on_error::<()>(
            Err(anyhow::anyhow!(
                "uploaded helper failed version verification"
            )),
            || cleaned.set(true),
        );
        assert!(failure.is_err());
        assert!(cleaned.get(), "a failed staging transaction must clean up");

        cleaned.set(false);
        let success = cleanup_remote_partial_on_error(Ok(7), || cleaned.set(true));
        assert_eq!(success.unwrap(), 7);
        assert!(
            !cleaned.get(),
            "a published helper must not be removed as a partial"
        );
    }

    #[test]
    fn rejects_unsafe_targets_and_paths() {
        assert!(validate_target("workbox").is_ok());
        assert!(validate_target("-oProxyCommand=bad").is_err());
        assert!(validate_remote_path("$HOME/.local/bin/muxflow-host").is_ok());
        assert!(validate_remote_path("$HOME/../victim").is_err());
    }

    #[test]
    fn enforces_minimum_tmux_version() {
        assert!(tmux_supported("tmux 3.3a"));
        assert!(tmux_supported("tmux 3.7"));
        assert!(!tmux_supported("tmux 3.2"));
    }

    #[test]
    fn remote_probe_honors_an_absolute_tmux_override_without_a_login_shell() {
        let temporary = tempfile::tempdir().unwrap();
        let tmux = temporary.path().join("custom-tmux");
        fs::write(&tmux, b"#!/bin/sh\nprintf 'tmux 9.9\\n'\n").unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", remote_tmux_version_command()])
            .env("PATH", "/usr/bin:/bin")
            .env("MUXFLOW_TMUX_PATH", &tmux)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"tmux 9.9\n");
    }

    #[test]
    fn remote_probe_rejects_an_explicitly_empty_tmux_override() {
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", remote_tmux_version_command()])
            .env("PATH", "/usr/bin:/bin")
            .env("MUXFLOW_TMUX_PATH", "")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("must be absolute"));
    }

    #[test]
    fn remote_probe_fallback_order_matches_the_runtime_resolver() {
        let script = remote_tmux_version_command();
        let fixed = script.find("/usr/local/bin/tmux").unwrap();
        let home = script.find("$HOME/.local/bin/tmux").unwrap();
        assert!(
            fixed < home,
            "fixed-prefix candidates must precede home candidates"
        );
        // macOS sshd runs commands without Homebrew or MacPorts on PATH.
        let homebrew = script.find("/opt/homebrew/bin/tmux").unwrap();
        assert!(homebrew < home);
        assert!(script.contains("/opt/local/bin/tmux"));
    }

    #[test]
    fn executable_target_reads_elf_and_mach_o_headers() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, header: &[u8]| {
            let path = dir.path().join(name);
            let mut bytes = header.to_vec();
            bytes.resize(64, 0);
            fs::write(&path, bytes).unwrap();
            path
        };
        let elf = |machine: u16| {
            let mut header = [0_u8; 20];
            header[..4].copy_from_slice(b"\x7fELF");
            header[5] = 1;
            header[18..20].copy_from_slice(&machine.to_le_bytes());
            header
        };
        let mach_o = |cpu: u32| {
            let mut header = [0_u8; 8];
            header[..4].copy_from_slice(&0xfeed_facf_u32.to_le_bytes());
            header[4..8].copy_from_slice(&cpu.to_le_bytes());
            header
        };
        for (name, header, expected) in [
            ("linux-x86_64", elf(62).to_vec(), ("Linux", "x86_64")),
            ("linux-aarch64", elf(183).to_vec(), ("Linux", "aarch64")),
            (
                "macos-aarch64",
                mach_o(0x0100_000c).to_vec(),
                ("Darwin", "aarch64"),
            ),
            (
                "macos-x86_64",
                mach_o(0x0100_0007).to_vec(),
                ("Darwin", "x86_64"),
            ),
        ] {
            assert_eq!(executable_target(&write(name, &header)).unwrap(), expected);
        }
        // A universal binary, a 32-bit ARM Mach-O and junk are all refused.
        for header in [
            0xcafe_babe_u32.to_be_bytes().to_vec(),
            mach_o(12).to_vec(),
            b"#!/bin/sh\n".to_vec(),
        ] {
            assert!(executable_target(&write("refused", &header)).is_err());
        }
    }

    #[test]
    fn remote_digest_falls_back_to_shasum() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("helper");
        fs::write(&file, b"helper bytes").unwrap();
        let expected = sha256_file(&file).unwrap();
        let script = remote_sha256(&file.display().to_string());
        let output = Command::new("sh").args(["-c", &script]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
        assert!(script.contains("shasum -a 256"));
    }

    #[test]
    fn nonresponsive_borrowed_socket_returns_promptly_without_mutation() {
        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("borrowed.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let original_identity = safe_socket_identity(&socket).unwrap().unwrap();
        let (accepted_sender, accepted_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            accepted_sender.send(()).unwrap();
            let _ = release_receiver.recv();
        });

        let started = Instant::now();
        let error = SshControl::start(
            "nonresponsive-host",
            Some(Path::new("/dev/null")),
            Some(&socket),
        )
        .err()
        .expect("a nonresponsive borrowed socket must be refused");

        assert!(error.to_string().contains("not responsive"), "{error:#}");
        assert!(started.elapsed() < Duration::from_secs(3));
        accepted_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            safe_socket_identity(&socket).unwrap(),
            Some(original_identity),
            "borrowed sockets must never be unlinked or replaced"
        );
        release_sender.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn owned_teardown_preserves_a_replacement_inode() {
        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("owned.sock");
        let original_listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let original_identity = safe_socket_identity(&socket).unwrap().unwrap();
        let child = Command::new("sh").args(["-c", "sleep 5"]).spawn().unwrap();
        let control = SshControl {
            target: "unused-host".into(),
            config: None,
            socket: socket.clone(),
            borrowed_identity: None,
            owned_master: Some(OwnedControlMaster {
                child,
                socket_identity: original_identity,
            }),
        };
        // Unlink but keep the original listener bound until the replacement
        // exists: the bound socket pins its inode, so a filesystem that reuses
        // freed inode numbers (ext4, unlike tmpfs) cannot hand it out again.
        fs::remove_file(&socket).unwrap();
        let replacement_listener = UnixListener::bind(&socket).unwrap();
        drop(original_listener);
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let replacement_identity = safe_socket_identity(&socket).unwrap().unwrap();

        drop(control);

        assert_eq!(
            safe_socket_identity(&socket).unwrap(),
            Some(replacement_identity)
        );
        drop(replacement_listener);
    }

    #[test]
    fn mux_clients_override_auto_master_config_and_fail_if_the_socket_disappears() {
        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("borrowed-auto.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let borrowed_identity = safe_socket_identity(&socket).unwrap().unwrap();
        let config = temporary.path().join("ssh-config");
        fs::write(
            &config,
            "Host *\n  ControlMaster auto\n  ControlPersist 60\n  ProxyCommand ignored-proxy\n",
        )
        .unwrap();
        let control = SshControl {
            target: "configured-host".into(),
            config: Some(config),
            socket: socket.clone(),
            borrowed_identity: Some(borrowed_identity),
            owned_master: None,
        };
        let arguments: Vec<_> = control
            .multiplexed_command()
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["ControlMaster=no", "-o"])
        );
        assert!(arguments.iter().any(|value| value == "ProxyCommand=false"));
        let effective = control
            .multiplexed_command()
            .arg("-G")
            .arg(&control.target)
            .output()
            .unwrap();
        assert!(effective.status.success());
        let effective = String::from_utf8(effective.stdout).unwrap();
        assert!(effective.lines().any(|line| line == "controlmaster false"));
        assert!(effective.lines().any(|line| line == "proxycommand false"));

        drop(listener);
        fs::remove_file(&socket).unwrap();
        let error = control.command("true").unwrap_err();
        assert!(error.to_string().contains("disappeared or was replaced"));
        assert!(
            !socket.exists(),
            "a mux client must not recreate its socket"
        );
    }
}
