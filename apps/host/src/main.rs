#[cfg(debug_assertions)]
use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    time::Instant,
};

#[cfg(debug_assertions)]
use anyhow::Context;
use anyhow::bail;
#[cfg(debug_assertions)]
use uuid::Uuid;

mod bridge;
mod daemon;
mod diagnostics;
mod hook;
mod paths;
#[cfg(debug_assertions)]
mod phase1_client;
mod remote_helper;
mod service;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("daemon") => {
            daemon::run(argument_path("--socket").unwrap_or_else(paths::default_socket_path)).await
        }
        Some("daemon-stop") => {
            daemon::stop(argument_path("--socket").unwrap_or_else(paths::default_socket_path)).await
        }
        Some("protocol-check") => {
            daemon::check(argument_path("--socket").unwrap_or_else(paths::default_socket_path))
                .await
        }
        Some("bridge") => {
            if std::env::args().nth(2).as_deref() != Some("--stdio") {
                bail!("usage: tmux-ide-host bridge --stdio [--socket PATH]");
            }
            bridge::run(
                argument_path("--socket").unwrap_or_else(paths::default_socket_path),
                !std::env::args().any(|argument| argument == "--no-start"),
            )
            .await
        }
        Some("helper") => remote_helper::run_cli(std::env::args().skip(2).collect()),
        Some("hook") => match std::env::args().nth(2).as_deref() {
            Some("ingest") => hook::run(std::env::args().skip(3).collect()).await,
            Some(verb @ ("status" | "install" | "uninstall")) => {
                hook::manage(verb, std::env::args().skip(3).collect())
            }
            _ => bail!(
                "usage: tmux-ide-host hook <ingest --adapter ID|status|install|uninstall> [--adapter ID] [--home PATH] [--settings-path PATH]"
            ),
        },
        Some("version") => {
            println!(
                "{}",
                serde_json::json!({
                    "helperVersion": tmux_agent_protocol::HELPER_VERSION,
                    "protocolMajor": tmux_agent_protocol::PROTOCOL_MAJOR,
                    "protocolMinor": tmux_agent_protocol::PROTOCOL_MINOR,
                    "os": std::env::consts::OS,
                    "architecture": std::env::consts::ARCH,
                })
            );
            Ok(())
        }
        Some("doctor") => diagnostics::run_doctor(std::env::args().skip(2)),
        Some("support-bundle") => diagnostics::write_support_bundle(std::env::args().skip(2)),
        Some("hooks-status") => {
            let managed = service::agents::HookManager::system_default()?.managed_adapters()?;
            println!(
                "{}",
                serde_json::json!({
                    "managedHooksPresent": !managed.is_empty(),
                    "managedAdapters": managed,
                })
            );
            Ok(())
        }
        // The same code path the desktop drives after the one-time host
        // prompt, reachable without one — which is how it is tested against
        // real tmux servers on isolated sockets rather than a developer's own.
        Some("host-naming") => {
            let outcome = service::apply_recommended_tmux_naming()?;
            println!("{}", serde_json::json!({ "outcome": outcome.label() }));
            Ok(())
        }
        Some("discover") => {
            let snapshot = if let Some(name) = std::env::var_os("ADE_TMUX_SOCKET_NAME") {
                tmux_control::discover_with_socket_name(&name.to_string_lossy())?
            } else {
                tmux_control::discover()?
            };
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
            Ok(())
        }
        #[cfg(debug_assertions)]
        Some("phase0-lanes") => {
            let report = phase0_core::run_lane_probe(512 * 1024 * 1024, 256 * 1024).await;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        #[cfg(debug_assertions)]
        Some("phase0-ssh") => {
            let target = std::env::args()
                .nth(2)
                .context("phase0-ssh requires an SSH target")?;
            phase0_ssh(&target)
        }
        #[cfg(debug_assertions)]
        Some("phase1-client") => phase1_client::run(std::env::args().skip(2).collect()),
        _ => bail!(
            "usage: tmux-ide-host <daemon|daemon-stop|protocol-check|bridge --stdio|hook <ingest|status|install|uninstall>|hooks-status|host-naming|helper|version|doctor [--json]|support-bundle --output PATH|discover>"
        ),
    }
}

fn argument_path(flag: &str) -> Option<std::path::PathBuf> {
    let mut arguments = std::env::args_os();
    while let Some(argument) = arguments.next() {
        if argument == flag {
            return arguments.next().map(Into::into);
        }
    }
    None
}

#[cfg(debug_assertions)]
fn phase0_ssh(target: &str) -> anyhow::Result<()> {
    if target.is_empty() || target.starts_with('-') || target.contains(char::is_whitespace) {
        bail!("invalid SSH target");
    }
    // OpenSSH appends a temporary suffix while creating a control socket, and a
    // macOS per-user temporary directory leaves no room for it under the
    // 104-byte AF_UNIX limit. Use the same short, private, uid-scoped runtime
    // root the helper's own SSH control sockets already use.
    let runtime = std::path::PathBuf::from(format!("/tmp/tmux-agent-ide-{}", unsafe {
        libc::geteuid()
    }))
    .join("ssh");
    paths::prepare_runtime_dir(&runtime)?;
    let control_socket = runtime.join(format!("ade-phase0-{}.sock", Uuid::new_v4()));
    let result = phase0_ssh_inner(target, &control_socket);
    let _ = ssh_command()
        .arg("-S")
        .arg(&control_socket)
        .args(["-O", "exit", target])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = std::fs::remove_file(&control_socket);
    result
}

#[cfg(debug_assertions)]
fn phase0_ssh_inner(target: &str, control_socket: &std::path::Path) -> anyhow::Result<()> {
    start_control_master(target, control_socket)?;
    let version = ssh_output(target, control_socket, &["tmux", "-V"])?;
    println!("remote-tmux: {}", String::from_utf8_lossy(&version).trim());

    let mut control = ssh_command()
        .arg("-T")
        .arg("-S")
        .arg(control_socket)
        .arg(target)
        .arg("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start persistent SSH control lane")?;
    let mut control_input = control
        .stdin
        .take()
        .context("SSH control stdin unavailable")?;
    let mut control_output = BufReader::new(
        control
            .stdout
            .take()
            .context("SSH control stdout unavailable")?,
    );
    control_round_trip(&mut control_input, &mut control_output, "warmup")?;

    const BULK_BYTES: u64 = 64 * 1024 * 1024;
    let mut bulk = ssh_command()
        .arg("-T")
        .args(["-o", "ControlMaster=no", "-o", "ControlPath=none"])
        .arg(target)
        .args(["head", "-c", &BULK_BYTES.to_string(), "/dev/zero"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start SSH bulk lane")?;

    let mut max_control_micros = 0_u128;
    for index in 0..32 {
        let expected = format!("control-{index}");
        let start = Instant::now();
        control_round_trip(&mut control_input, &mut control_output, &expected)?;
        max_control_micros = max_control_micros.max(start.elapsed().as_micros());
    }
    drop(control_input);
    if !control.wait()?.success() {
        bail!("persistent SSH control lane failed");
    }
    let bulk_status = bulk.wait()?;
    if !bulk_status.success() {
        bail!("SSH bulk lane failed with {bulk_status}");
    }
    println!("bulk-bytes: {BULK_BYTES}");
    println!("max-control-latency-micros: {max_control_micros}");

    let exit = ssh_command()
        .arg("-S")
        .arg(control_socket)
        .args(["-O", "exit", target])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()?;
    if !exit.success() {
        bail!("failed to force SSH control-master disconnect");
    }
    start_control_master(target, control_socket)?;
    ssh_output(target, control_socket, &["true"])?;
    println!("forced-reconnect: pass");
    Ok(())
}

#[cfg(debug_assertions)]
fn control_round_trip(
    input: &mut impl Write,
    output: &mut impl BufRead,
    expected: &str,
) -> anyhow::Result<()> {
    writeln!(input, "{expected}")?;
    input.flush()?;
    let mut received = String::new();
    output.read_line(&mut received)?;
    if received.trim_end() != expected {
        bail!("persistent SSH control lane returned unexpected data");
    }
    Ok(())
}

#[cfg(debug_assertions)]
fn start_control_master(target: &str, control_socket: &std::path::Path) -> anyhow::Result<()> {
    let output = ssh_command()
        .args(["-M", "-N", "-f"])
        .args(["-o", "ControlMaster=yes", "-o", "ControlPersist=60"])
        .arg("-S")
        .arg(control_socket)
        .arg(target)
        .output()
        .context("failed to launch OpenSSH control master")?;
    if !output.status.success() {
        bail!(
            "OpenSSH control master failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(debug_assertions)]
fn ssh_output(
    target: &str,
    control_socket: &std::path::Path,
    remote_command: &[&str],
) -> anyhow::Result<Vec<u8>> {
    let output = ssh_command()
        .arg("-T")
        .arg("-S")
        .arg(control_socket)
        .arg(target)
        .args(remote_command)
        .output()?;
    if !output.status.success() {
        bail!(
            "SSH command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

#[cfg(debug_assertions)]
fn ssh_command() -> Command {
    let mut command = Command::new("ssh");
    if let Some(config_path) = std::env::var_os("ADE_PHASE0_SSH_CONFIG") {
        command.arg("-F").arg(config_path);
    }
    command
}
