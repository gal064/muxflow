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
    expected_version: Option<String>,
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
        expected_version: None,
        control_socket: None,
        test_fail_after_shutdown: false,
    };
    let mut index = 1;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--config" | "--remote-path" | "--artifact" | "--digest" | "--expected-arch"
            | "--expected-version" | "--control-socket" => {
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
                    "--expected-version" => options.expected_version = Some(value),
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
        "set -eu; os=$(uname -s); arch=$(uname -m); printf '%s\\n%s\\n' \"$os\" \"$arch\"; {}; if git_version=$(git --version 2>/dev/null); then printf '%s\\n' \"$git_version\"; else printf 'unavailable\\n'; fi; if [ -x {path} ]; then printf 'installed\\n'; if version=$({path} version 2>/dev/null); then printf '%s\\n' \"$version\"; else printf '\\n'; fi; sha256sum {path} | cut -d' ' -f1; else printf 'absent\\n\\n\\n'; fi",
        remote_tmux_version_command(),
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

/// Finds tmux in the non-interactive SSH environment without sourcing a shell
/// profile. Kept in step with `tmux-control`'s Linux candidates: this probe runs
/// before a helper is necessarily installed, so it cannot delegate yet.
fn remote_tmux_version_command() -> &'static str {
    r#"tmux_bin=''; if [ "${MUXFLOW_TMUX_PATH+x}" = x ]; then case "$MUXFLOW_TMUX_PATH" in /*) ;; *) echo 'MUXFLOW_TMUX_PATH must be absolute' >&2; exit 1;; esac; [ -f "$MUXFLOW_TMUX_PATH" ] && [ -x "$MUXFLOW_TMUX_PATH" ] || { echo 'MUXFLOW_TMUX_PATH is not executable' >&2; exit 1; }; tmux_bin=$MUXFLOW_TMUX_PATH; elif resolved=$(command -v tmux 2>/dev/null) && [ "${resolved#/}" != "$resolved" ] && [ -f "$resolved" ] && [ -x "$resolved" ]; then tmux_bin=$resolved; else for candidate in /usr/local/bin/tmux /usr/bin/tmux /bin/tmux /home/linuxbrew/.linuxbrew/bin/tmux /run/current-system/sw/bin/tmux /nix/var/nix/profiles/default/bin/tmux /usr/pkg/bin/tmux /snap/bin/tmux "$HOME/.local/bin/tmux" "$HOME/.nix-profile/bin/tmux" "$HOME/.linuxbrew/bin/tmux"; do if [ -f "$candidate" ] && [ -x "$candidate" ]; then tmux_bin=$candidate; break; fi; done; fi; [ -n "$tmux_bin" ] || { echo 'tmux executable was not found' >&2; exit 127; }; "$tmux_bin" -V"#
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
    if before.installed
        && helper_is_newer(
            before.helper_version.as_deref(),
            options.expected_version.as_deref(),
        )
    {
        bail!(
            "remote helper {} is newer than this app expects {}; refusing downgrade",
            before.helper_version.as_deref().unwrap_or("unknown"),
            options.expected_version.as_deref().unwrap_or("unknown"),
        );
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
        let remote_digest = String::from_utf8(
            connection.command(&format!("sha256sum {partial} | cut -d' ' -f1"))?,
        )?
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
        // Uploads happen outside the critical section: they are private UUID paths
        // and cannot affect the installed helper. Serialize the fresh direction
        // check and replacement so two desktops cannot both approve an old helper,
        // then let the older one overwrite what the newer one just installed.
        let install_lock = RemoteInstallLock::acquire(connection, &final_path)?;
        let current = probe(connection, &options.remote_path)?;
        if current.installed
            && helper_is_newer(
                current.helper_version.as_deref(),
                options.expected_version.as_deref(),
            )
        {
            bail!(
                "remote helper {} is newer than this app expects {}; refusing downgrade",
                current.helper_version.as_deref().unwrap_or("unknown"),
                options.expected_version.as_deref().unwrap_or("unknown"),
            );
        }
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
            "set -eu; runtime=/tmp/muxflow-$(id -u); install -d -m 0700 \"$runtime\"; metadata=\"$runtime/daemon.json\"; socket=\"$runtime/host.sock\"; log=\"$runtime/daemon-start.log\"; if [ -S \"$socket\" ]; then if ! {final_path} daemon-stop >/dev/null 2>&1; then pid=$(sed -n 's/.*\"pid\":\\([0-9][0-9]*\\).*/\\1/p' \"$metadata\"); recorded_start=$(sed -n 's/.*\"processStartTime\":\\([0-9][0-9]*\\).*/\\1/p' \"$metadata\"); recorded_exe=$(sed -n 's/.*\"executable\":\"\\([^\"]*\\)\".*/\\1/p' \"$metadata\"); [ -n \"$pid\" ] && [ -n \"$recorded_start\" ] && [ -n \"$recorded_exe\" ]; actual_start=$(awk '{{print $22}}' \"/proc/$pid/stat\"); actual_exe=$(readlink \"/proc/$pid/exe\"); actual_exe=${{actual_exe% (deleted)}}; [ \"$actual_start\" = \"$recorded_start\" ] && [ \"$actual_exe\" = \"$recorded_exe\" ]; kill -TERM \"$pid\"; for i in $(seq 1 100); do kill -0 \"$pid\" 2>/dev/null || break; sleep 0.05; done; ! kill -0 \"$pid\" 2>/dev/null; rm -f \"$socket\" \"$metadata\"; fi; fi; for i in $(seq 1 100); do [ ! -S \"$socket\" ] && break; sleep 0.05; done; [ ! -S \"$socket\" ]; if [ -e {backup} ]; then mv -f {backup} {final_path}; nohup {final_path} daemon </dev/null >\"$log\" 2>&1 & for i in $(seq 1 100); do [ -S \"$socket\" ] && break; sleep 0.05; done; {final_path} protocol-check >/dev/null || {{ cat \"$log\" >&2; false; }}; else rm -f {final_path}; fi"
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
        "set -eu; runtime=/tmp/muxflow-$(id -u); install -d -m 0700 \"$runtime\"; metadata=\"$runtime/daemon.json\"; socket=\"$runtime/host.sock\"; log=\"$runtime/daemon-start.log\"; if [ -S \"$socket\" ]; then if ! {final_path} daemon-stop >/dev/null 2>&1; then pid=$(sed -n 's/.*\"pid\":\\([0-9][0-9]*\\).*/\\1/p' \"$metadata\"); recorded_start=$(sed -n 's/.*\"processStartTime\":\\([0-9][0-9]*\\).*/\\1/p' \"$metadata\"); recorded_exe=$(sed -n 's/.*\"executable\":\"\\([^\"]*\\)\".*/\\1/p' \"$metadata\"); [ -n \"$pid\" ] && [ -n \"$recorded_start\" ] && [ -n \"$recorded_exe\" ]; actual_start=$(awk '{{print $22}}' \"/proc/$pid/stat\"); actual_exe=$(readlink \"/proc/$pid/exe\"); actual_exe=${{actual_exe% (deleted)}}; [ \"$actual_start\" = \"$recorded_start\" ] && [ \"$actual_exe\" = \"$recorded_exe\" ]; kill -TERM \"$pid\"; for i in $(seq 1 100); do kill -0 \"$pid\" 2>/dev/null || break; sleep 0.05; done; ! kill -0 \"$pid\" 2>/dev/null; rm -f \"$socket\" \"$metadata\"; fi; fi; for i in $(seq 1 100); do [ ! -S \"$socket\" ] && break; sleep 0.05; done; [ ! -S \"$socket\" ]; {fault} nohup {final_path} daemon </dev/null >\"$log\" 2>&1 & for i in $(seq 1 100); do [ -S \"$socket\" ] && break; sleep 0.05; done; [ -S \"$socket\" ]; {final_path} protocol-check >/dev/null || {{ cat \"$log\" >&2; false; }}"
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

fn release_ordinal(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let ordinal = (next()?, next()?, next()?);
    parts.next().is_none().then_some(ordinal)
}

fn helper_is_newer(installed: Option<&str>, expected: Option<&str>) -> bool {
    installed
        .and_then(release_ordinal)
        .zip(expected.and_then(release_ordinal))
        .is_some_and(|(installed, expected)| installed > expected)
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
    }

    #[test]
    fn refuses_only_a_parseable_newer_helper_version() {
        assert!(helper_is_newer(Some("2.1.0"), Some("2.0.9")));
        assert!(!helper_is_newer(Some("2.0.9"), Some("2.1.0")));
        assert!(!helper_is_newer(Some("development"), Some("2.1.0")));
        assert!(!helper_is_newer(Some("2.1.0"), None));
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
        drop(original_listener);
        fs::remove_file(&socket).unwrap();
        let replacement_listener = UnixListener::bind(&socket).unwrap();
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
