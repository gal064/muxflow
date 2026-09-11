use std::{
    fs,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};
use tmux_agent_protocol::{
    HELPER_VERSION, HOST_CAPABILITIES, PROTOCOL_MAJOR, envelope, read_frame_sync,
    v1::{self, envelope::Payload},
    write_frame_sync,
};

/// A short root, not the default tempdir: the sockets bound below must stay
/// under the platform's 104/108-byte limit, and macOS puts the default tempdir
/// 50+ bytes deep under /var/folders.
fn temporary_runtime() -> PathBuf {
    let root = if cfg!(target_os = "macos") {
        PathBuf::from("/private/tmp")
    } else {
        std::env::current_dir().unwrap().join("tmp")
    };
    let runtime = root.join(format!("bridge-cli-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&runtime).unwrap();
    runtime
}

/// Waits for a bridge to leave on its own, killing it and failing with
/// `whose_failure` rather than hanging the suite when it does not.
fn wait_for_exit(child: &mut Child, within: Duration, whose_failure: &str) -> ExitStatus {
    let deadline = Instant::now() + within;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{whose_failure}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Answer the bridge's private compatibility probe, then return the separate
/// stream whose first frame still belongs to the app-side client.
fn accept_compatible_bridge(listener: &UnixListener) -> UnixStream {
    let (mut probe, _) = listener.accept().unwrap();
    let _ = read_frame_sync(&mut probe).unwrap().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
        .arg("version")
        .output()
        .unwrap();
    assert!(output.status.success());
    let version: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let mut hello = envelope(
        1,
        0,
        Payload::ServerHello(v1::ServerHello {
            helper_version: HELPER_VERSION.into(),
            helper_build_digest: version["helperBuildDigest"].as_str().unwrap().into(),
            capabilities: HOST_CAPABILITIES,
            ..Default::default()
        }),
    );
    hello.protocol_major = PROTOCOL_MAJOR;
    write_frame_sync(&mut probe, &hello).unwrap();
    drop(probe);
    listener.accept().unwrap().0
}

/// The incident this guards: a bridge whose client died kept draining a daemon
/// that never closed its side of the socket, and the process survived as an
/// orphan for days. After stdin EOF the bridge owes the daemon one idle window
/// and no more — even against a peer that stays silently connected forever.
#[test]
fn bridge_reaps_itself_when_the_daemon_never_closes_after_client_eof() {
    let runtime = temporary_runtime();
    let socket = runtime.join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
        .args(["bridge", "--stdio", "--no-start", "--socket"])
        .arg(&socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut child_stdin = child.stdin.take().unwrap();
    let mut child_stdout = child.stdout.take().unwrap();
    let mut daemon_side = accept_compatible_bridge(&listener);

    // Both pump directions work before the client leaves.
    child_stdin.write_all(b"request-bytes").unwrap();
    child_stdin.flush().unwrap();
    let mut upload = [0_u8; 13];
    daemon_side.read_exact(&mut upload).unwrap();
    assert_eq!(&upload, b"request-bytes");
    daemon_side.write_all(b"response-bytes").unwrap();
    let mut download = [0_u8; 14];
    child_stdout.read_exact(&mut download).unwrap();
    assert_eq!(&download, b"response-bytes");

    // The client dies; the daemon flushes one late frame and then — the bug
    // under guard — holds the socket open without ever closing it.
    drop(child_stdin);
    daemon_side.write_all(b"late-frame").unwrap();
    let mut late = [0_u8; 10];
    child_stdout.read_exact(&mut late).unwrap();
    assert_eq!(&late, b"late-frame", "post-EOF drain must not lose bytes");

    // One idle window (10s) plus margin: the bridge must exit on its own.
    let status = wait_for_exit(
        &mut child,
        Duration::from_secs(15),
        "bridge must reap itself against a daemon that never closes",
    );
    assert!(status.success(), "an idle-drain exit is a clean exit");
    fs::remove_dir_all(&runtime).unwrap();
}

/// The incident this guards: a laptop slept, the daemon hung up, `bridge::run`
/// returned — and the process stayed alive on the remote host anyway. Its stdin
/// is an SSH session whose client silently vanished, so it never EOFs; the
/// blocking read parked on that stdin cannot be cancelled, and dropping the
/// tokio runtime on the way out of `main` waits for it forever. A finished pump
/// has to end the process, not just the future.
#[test]
fn bridge_exits_when_the_daemon_hangs_up_while_stdin_never_eofs() {
    let runtime = temporary_runtime();
    let socket = runtime.join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
        .args(["bridge", "--stdio", "--no-start", "--socket"])
        .arg(&socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Held for the whole test: this is the stdin that never delivers EOF.
    let mut child_stdin = child.stdin.take().unwrap();
    let mut daemon_side = accept_compatible_bridge(&listener);
    // A bridge that stopped forwarding would otherwise hang the whole suite
    // here rather than fail this test.
    daemon_side
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    // One round trip, so the upload pump is provably parked on the next
    // blocking read of stdin before the daemon goes away. Without it the
    // hang is a race the test would lose more often than win.
    child_stdin.write_all(b"request-bytes").unwrap();
    child_stdin.flush().unwrap();
    let mut upload = [0_u8; 13];
    daemon_side.read_exact(&mut upload).unwrap();

    // The daemon closes its side while the client is still attached.
    drop(daemon_side);

    let status = wait_for_exit(
        &mut child,
        Duration::from_secs(5),
        "bridge must exit once the daemon hangs up, even with stdin still open",
    );
    assert!(status.success(), "a daemon hangup is a clean exit");

    // The exit line is written beside the daemon's logs, not to stderr: in the
    // orphan case the SSH session carrying stderr is the thing already gone.
    let diagnostic = fs::read_to_string(runtime.join("bridge.log")).unwrap_or_default();
    assert!(
        diagnostic.contains(r#""event":"bridgeExit""#),
        "an exit nothing recorded is the incident all over again: {diagnostic}"
    );
    assert!(
        diagnostic.contains(r#""reason":"daemon-eof""#),
        "the exit line has to name why the bridge left: {diagnostic}"
    );

    drop(child_stdin);
    fs::remove_dir_all(&runtime).unwrap();
}
