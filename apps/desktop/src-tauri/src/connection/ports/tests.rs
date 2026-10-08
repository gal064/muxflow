use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use super::*;

fn quiet() -> Notify {
    Arc::new(|_: &str| {})
}

/// A stand-in for ssh: records its pid, writes `stderr` the way `ssh -v`
/// would — `\r\n` line endings included — and stays up like `-N` does.
fn fake_ssh(pid_file: &Path, stderr: &str) -> Command {
    let mut fake = Command::new("sh");
    fake.arg("-c").arg(format!(
        "echo $$ > '{}'; printf '%s\\r\\n' {stderr} >&2; exec sleep 30",
        pid_file.display()
    ));
    guarded(&fake)
}

/// A forward whose listener on `local_port` comes up.
fn fake_forward(pid_file: &Path, local_port: u16) -> Command {
    fake_ssh(
        pid_file,
        &format!("'debug1: {}'", listening_line(local_port)),
    )
}

fn read_pid(pid_file: &Path) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(pid) = fs::read_to_string(pid_file).map(|text| text.trim().parse::<i32>())
            && let Ok(pid) = pid
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "fake forward never started");
        thread::sleep(Duration::from_millis(10));
    }
}

fn process_ends(pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if unsafe { libc::kill(pid, 0) } != 0 {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

fn wait_for_state(registry: &Registry, remote_port: u16, state: ForwardState) -> PortForward {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(forward) = registry
            .list()
            .into_iter()
            .find(|forward| forward.remote_port == remote_port && forward.state == state)
        {
            return forward;
        }
        assert!(Instant::now() < deadline, "forward never reached {state:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_forward_is_active_once_listening_and_stopping_it_ends_ssh() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let registry = Arc::new(Registry::default());
    registry
        .start("devbox", 3000, 3000, fake_forward(&pid_file, 3000), quiet())
        .unwrap();
    wait_for_state(&registry, 3000, ForwardState::Active);
    let pid = read_pid(&pid_file);

    assert!(registry.stop(|forward| forward.remote_port == 3000));
    assert!(registry.list().is_empty());
    assert!(process_ends(pid), "ssh outlived its removed forward");
}

#[test]
fn ssh_ends_when_the_guard_holder_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    // The holder stands in for Muxflow: it owns the only write end of the
    // guard pipe, and SIGKILL gives it no chance to clean anything up.
    let mut holder = Command::new("sleep")
        .arg("30")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut wrapper = fake_forward(&pid_file, 3000)
        .stdin(Stdio::from(holder.stdout.take().unwrap()))
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = read_pid(&pid_file);

    holder.kill().unwrap();
    holder.wait().unwrap();
    assert!(
        process_ends(pid),
        "ssh outlived the process holding its guard"
    );
    wrapper.wait().unwrap();
}

#[test]
fn a_forward_that_exits_fails_with_ssh_s_own_words() {
    let registry = Arc::new(Registry::default());
    let mut fake = Command::new("sh");
    fake.arg("-c").arg(
        "echo 'OpenSSH_10.0p1, OpenSSL 3.5' >&2; \
         echo 'debug1: Connecting to devbox' >&2; \
         echo 'Timeout, server devbox not responding.' >&2; exit 255",
    );
    registry
        .start("devbox", 3000, 3000, guarded(&fake), quiet())
        .unwrap();

    let failed = wait_for_state(&registry, 3000, ForwardState::Failed);
    assert_eq!(
        failed.error.as_deref(),
        Some("Timeout, server devbox not responding.")
    );
}

#[test]
fn a_refused_bind_fails_the_forward_and_ends_ssh() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let registry = Arc::new(Registry::default());
    // What OpenSSH really prints, in its order: the listening line comes
    // before the bind, so it is the refusal that decides. Without
    // `ExitOnForwardFailure`, ssh then stays up.
    let fake = fake_ssh(
        &pid_file,
        &format!(
            "'debug1: {}' 'bind [127.0.0.1]:3000: Address already in use' \
             'Could not request local forwarding.'",
            listening_line(3000)
        ),
    );
    registry.start("devbox", 3000, 3000, fake, quiet()).unwrap();

    let failed = wait_for_state(&registry, 3000, ForwardState::Failed);
    assert_eq!(
        failed.error.as_deref(),
        Some("bind [127.0.0.1]:3000: Address already in use")
    );
    assert!(
        process_ends(read_pid(&pid_file)),
        "ssh outlived its refused forward"
    );
}

#[test]
fn forwards_from_the_host_s_ssh_config_neither_fail_nor_activate_ours() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let registry = Arc::new(Registry::default());
    // A `LocalForward 8080` in the config, already held by the control master.
    let fake = fake_ssh(
        &pid_file,
        &format!(
            "'bind [127.0.0.1]:8080: Address already in use' \
             'debug1: {}' 'debug1: {}'",
            listening_line(8080),
            listening_line(3000)
        ),
    );
    registry.start("devbox", 3000, 3000, fake, quiet()).unwrap();

    let active = wait_for_state(&registry, 3000, ForwardState::Active);
    assert_eq!(active.error, None);
    registry.stop(|_| true);
    assert!(process_ends(read_pid(&pid_file)));
}

#[test]
fn a_port_is_forwarded_once_and_a_failed_forward_can_be_retried() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(Registry::default());
    registry
        .start(
            "devbox",
            3000,
            3000,
            fake_forward(&dir.path().join("a"), 3000),
            quiet(),
        )
        .unwrap();
    assert_eq!(
        registry.start(
            "devbox",
            3000,
            4000,
            fake_forward(&dir.path().join("b"), 4000),
            quiet()
        ),
        Err("port 3000 is already forwarded".into())
    );
    assert_eq!(
        registry.start(
            "other",
            5000,
            3000,
            fake_forward(&dir.path().join("c"), 3000),
            quiet()
        ),
        Err("local port 3000 is already used by another forward".into())
    );
    assert_eq!(
        registry.start(
            "devbox",
            0,
            3000,
            fake_forward(&dir.path().join("d"), 3000),
            quiet()
        ),
        Err("ports must be between 1 and 65535".into())
    );

    let mut failing = Command::new("sh");
    failing.arg("-c").arg("exit 255");
    registry
        .start("devbox", 8080, 8080, guarded(&failing), quiet())
        .unwrap();
    wait_for_state(&registry, 8080, ForwardState::Failed);
    registry
        .start(
            "devbox",
            8080,
            8080,
            fake_forward(&dir.path().join("e"), 8080),
            quiet(),
        )
        .unwrap();
    wait_for_state(&registry, 8080, ForwardState::Active);
    assert_eq!(registry.list().len(), 2);
    registry.stop(|_| true);
}

#[test]
fn the_ssh_command_forwards_loopback_to_loopback_without_the_master() {
    let command = forward_command("devbox", Some("/home/me/.ssh/alt"), 3000, 8080);
    assert_eq!(command.get_program(), "sh");
    let args: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(args[0], "-c");
    assert_eq!(args[1], GUARD_SCRIPT);
    let ssh = &args[3..];
    assert_eq!(ssh[0], "ssh");
    for expected in [
        ["-F", "/home/me/.ssh/alt"],
        ["-o", "ControlPath=none"],
        ["-L", "127.0.0.1:8080:localhost:3000"],
    ] {
        assert!(
            ssh.windows(2).any(|pair| pair == expected),
            "missing {expected:?} in {ssh:?}"
        );
    }
    assert_eq!(ssh.last().map(String::as_str), Some("devbox"));
}

#[test]
fn detected_ports_are_unprivileged_loopback_reachable_and_named_first() {
    let output = "\
LISTEN 0 4096  127.0.0.53%lo:53      0.0.0.0:*
LISTEN 0 128         0.0.0.0:22      0.0.0.0:*
LISTEN 0 200       127.0.0.1:5432    0.0.0.0:*
LISTEN 0 511         0.0.0.0:3000    0.0.0.0:* users:((\"node\",pid=41,fd=20))
LISTEN 0 511            [::]:3000       [::]:* users:((\"node\",pid=41,fd=21))
LISTEN 0 511           [::1]:5173       [::]:* users:((\"node\",pid=52,fd=19))
LISTEN 0 2048 100.112.254.120:9119   0.0.0.0:* users:((\"hermes\",pid=7,fd=15))
LISTEN 0 10                *:8000          *:* users:((\"python3\",pid=9,fd=3))
not a listener line
";
    assert_eq!(
        parse_listening_ports(output),
        vec![
            DetectedPort {
                port: 3000,
                process: Some("node".into())
            },
            DetectedPort {
                port: 5173,
                process: Some("node".into())
            },
            DetectedPort {
                port: 8000,
                process: Some("python3".into())
            },
            DetectedPort {
                port: 5432,
                process: None
            },
        ]
    );
}

#[test]
fn detected_ports_on_a_mac_come_from_lsof() {
    // `lsof -nP -iTCP -sTCP:LISTEN -Fcn`, as macOS prints it.
    let output = "\
p41
cnode
f20
n*:3000
f21
n[::1]:5173
p7
cControlCe
f9
n*:5000
f10
n192.168.0.195:7000
p9
cpython3
f3
n127.0.0.1:8000
f4
n*:80
";
    assert_eq!(
        parse_listening_ports(output),
        vec![
            DetectedPort {
                port: 3000,
                process: Some("node".into())
            },
            DetectedPort {
                port: 5000,
                process: Some("ControlCe".into())
            },
            DetectedPort {
                port: 5173,
                process: Some("node".into())
            },
            DetectedPort {
                port: 8000,
                process: Some("python3".into())
            },
        ]
    );
}
