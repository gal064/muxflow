use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixListener,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// The incident this guards: a bridge whose client died kept draining a daemon
/// that never closed its side of the socket, and the process survived as an
/// orphan for days. After stdin EOF the bridge owes the daemon one idle window
/// and no more — even against a peer that stays silently connected forever.
#[test]
fn bridge_reaps_itself_when_the_daemon_never_closes_after_client_eof() {
    let runtime_root = if cfg!(target_os = "macos") {
        PathBuf::from("/private/tmp")
    } else {
        std::env::current_dir().unwrap().join("tmp")
    };
    let runtime = runtime_root.join(format!("bridge-cli-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&runtime).unwrap();
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
    let (mut daemon_side, _) = listener.accept().unwrap();

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
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "bridge must reap itself against a daemon that never closes"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "an idle-drain exit is a clean exit");
    fs::remove_dir_all(&runtime).unwrap();
}
