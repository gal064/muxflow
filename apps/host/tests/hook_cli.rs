use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

use prost::Message;
use tmux_agent_protocol::v1;

#[test]
fn cli_persists_an_exact_unsequenced_hook_envelope() {
    let runtime_root = if cfg!(target_os = "macos") {
        PathBuf::from("/private/tmp")
    } else {
        std::env::current_dir().unwrap().join("tmp")
    };
    let runtime = runtime_root.join(format!("phase6-hook-cli-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&runtime).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tmux-ide-host"))
        .args(["hook", "ingest", "--adapter", "codex"])
        .env("ADE_HOST_RUNTIME_DIR", &runtime)
        .env("TMUX_PANE", "%77")
        .env_remove("TMUX")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"hook_event_name":"Stop","generation":912,"prompt":"private","api_token":"secret"}"#)
        .unwrap();
    assert!(child.wait().unwrap().success());

    let mailbox: Vec<_> = fs::read_dir(&runtime)
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("hook-fallback-codex-77-")
        })
        .collect();
    assert_eq!(mailbox.len(), 1);
    let bytes = fs::read(mailbox[0].path()).unwrap();
    let event = v1::AgentHookEvent::decode(bytes.as_slice()).unwrap();
    assert_eq!(event.pane_id, "%77");
    assert!(event.origin_server_identity.is_empty());
    assert_eq!(event.source_generation, 0);
    assert!(!event.source_sequence_authoritative);
    let normalized = String::from_utf8(event.payload_json).unwrap();
    assert!(!normalized.contains("private"));
    assert!(!normalized.contains("secret"));
    fs::remove_dir_all(runtime).unwrap();
}

/// M13-E003, end to end: the hook and the daemon disagreed about which runtime
/// directory this machine has, and every event was filed where nobody read it.
///
/// The split is reproduced exactly as the field machine had it. The daemon is
/// started the way the desktop starts it over `ssh` — no `XDG_RUNTIME_DIR` — and
/// the hook is run the way an agent inside tmux runs it, with one set to a
/// directory that has no daemon in it. Only `HOME` is common to both, which is
/// the whole basis of the fix.
#[test]
fn a_hook_reaches_a_daemon_that_resolved_a_different_runtime_directory() {
    // Short, because the daemon's socket has to fit `sockaddr_un::sun_path`.
    let root = if cfg!(target_os = "macos") {
        PathBuf::from("/private/tmp")
    } else {
        PathBuf::from("/tmp")
    }
    .join(format!("ade-split-{}", uuid::Uuid::new_v4().simple()));
    let home = root.join("home");
    let daemon_runtime = root.join("daemon-runtime");
    let hook_xdg = root.join("hook-xdg");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&daemon_runtime).unwrap();
    fs::create_dir_all(&hook_xdg).unwrap();
    let socket = daemon_runtime.join("host.sock");

    // Detached from the harness's pipes and killed on any exit path: a daemon
    // that outlives a failing assertion holds the captured output open and
    // hangs `cargo test` instead of reporting the failure.
    let mut daemon = DaemonGuard(Some(
        Command::new(env!("CARGO_BIN_EXE_tmux-ide-host"))
            .args(["daemon"])
            .env("HOME", &home)
            .env("ADE_HOST_RUNTIME_DIR", &daemon_runtime)
            .env_remove("XDG_RUNTIME_DIR")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    ));
    let started = (0..200).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(25));
        socket.exists()
    });
    assert!(started, "the fixture daemon never bound its socket");

    let ingest = |mailbox_expected: bool| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_tmux-ide-host"))
            .args(["hook", "ingest", "--adapter", "codex"])
            .env("HOME", &home)
            // What a tmux pane inherits, and what the daemon never saw.
            .env("XDG_RUNTIME_DIR", &hook_xdg)
            .env("TMUX_PANE", "%91")
            .env_remove("ADE_HOST_RUNTIME_DIR")
            .env_remove("TMUX")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"hook_event_name":"Stop","session_id":"s"}"#)
            .unwrap();
        assert!(child.wait().unwrap().success());
        let stranded = mailbox_entries(&hook_xdg.join("tmux-agent-ide"));
        assert!(
            stranded.is_empty(),
            "the event was filed in the hook's own directory, where no daemon reads: {stranded:?}"
        );
        assert_eq!(
            mailbox_entries(&daemon_runtime).is_empty(),
            !mailbox_expected,
            "the daemon's directory holds the wrong number of undelivered events"
        );
    };

    ingest(false);

    // And with the daemon gone, the event waits in the directory that daemon
    // will come back to rather than in the one the hook happened to resolve.
    let stopped = Command::new(env!("CARGO_BIN_EXE_tmux-ide-host"))
        .args(["daemon-stop"])
        .arg("--socket")
        .arg(&socket)
        .env("HOME", &home)
        .env("ADE_HOST_RUNTIME_DIR", &daemon_runtime)
        .status()
        .unwrap();
    assert!(stopped.success());
    daemon.reap();
    ingest(true);

    fs::remove_dir_all(root).unwrap();
}

struct DaemonGuard(Option<std::process::Child>);

impl DaemonGuard {
    fn reap(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.wait();
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn mailbox_entries(directory: &std::path::Path) -> Vec<String> {
    fs::read_dir(directory)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with("hook-fallback-"))
                .collect()
        })
        .unwrap_or_default()
}

/// `hook status|install|uninstall` against an isolated fixture home.
///
/// The overrides are the whole point: they are how the installer is exercised
/// against a copy of a real machine's configuration without a test, a script,
/// or an impatient operator ever touching a real `~/.claude`.
#[test]
fn cli_reports_installs_and_reverses_wiring_against_an_isolated_home() {
    let home = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("phase13-hook-cli-{}", uuid::Uuid::new_v4()));
    let settings = home.join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let original = fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/phase13/fixtures/claude-settings-orca.json"
    ))
    .unwrap();
    fs::write(&settings, &original).unwrap();

    let run = |verb: &str| -> serde_json::Value {
        let output = Command::new(env!("CARGO_BIN_EXE_tmux-ide-host"))
            .args(["hook", verb, "--adapter", "claude-code"])
            .arg("--home")
            .arg(&home)
            .arg("--settings-path")
            .arg(&settings)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{verb}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };

    let before = run("status");
    assert_eq!(before["adapters"][0]["wiring"], "notWired");
    assert_eq!(
        before["adapters"].as_array().unwrap().len(),
        1,
        "--adapter scopes the report"
    );

    let applied = |report: &serde_json::Value, id: &str| -> serde_json::Value {
        report["applied"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["adapterId"] == id)
            .expect("every adapter is reported, acted on or not")
            .clone()
    };

    let installed = run("install");
    assert_eq!(applied(&installed, "claude-code")["changed"], true);
    assert_eq!(
        applied(&installed, "codex")["skipped"],
        "not selected",
        "an adapter the caller did not name is reported rather than silently dropped"
    );
    assert_eq!(installed["adapters"][0]["wiring"], "wired");
    let after_install = fs::read(&settings).unwrap();

    let repeated = run("install");
    assert_eq!(
        applied(&repeated, "claude-code")["changed"],
        false,
        "install must be idempotent"
    );
    assert_eq!(fs::read(&settings).unwrap(), after_install);
    assert_eq!(run("status")["adapters"][0]["wiring"], "wired");

    let removed = run("uninstall");
    assert_eq!(removed["adapters"][0]["wiring"], "notWired");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&settings).unwrap()).unwrap(),
        serde_json::from_slice::<serde_json::Value>(&original).unwrap(),
        "uninstall left the foreign configuration changed"
    );

    // The override names one adapter's file, so it cannot be used without one.
    let ambiguous = Command::new(env!("CARGO_BIN_EXE_tmux-ide-host"))
        .args(["hook", "status", "--settings-path", "/tmp/anything.json"])
        .output()
        .unwrap();
    assert!(!ambiguous.status.success());

    // Without `--adapter`, an install must not create configuration for an
    // agent that is not on this host — the exact thing the desktop refuses.
    let unscoped = Command::new(env!("CARGO_BIN_EXE_tmux-ide-host"))
        .args(["hook", "install", "--home"])
        .arg(&home)
        .env_remove("PATH")
        .output()
        .unwrap();
    assert!(
        unscoped.status.success(),
        "{}",
        String::from_utf8_lossy(&unscoped.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&unscoped.stdout).unwrap();
    let codex = report["applied"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["adapterId"] == "codex")
        .unwrap()
        .clone();
    assert_eq!(codex["skipped"], "agent is not installed here");
    assert!(
        !home.join(".codex").exists(),
        "an absent agent's configuration directory was created anyway"
    );

    // And an unscoped uninstall must actually remove. Install asks "would this
    // act here", which a wired adapter answers no to; sharing that predicate
    // with uninstall meant `hook uninstall` with no `--adapter` removed nothing
    // and reported that the agents were not installed.
    fs::write(&settings, &original).unwrap();
    let unscoped_verb = |verb: &str| -> serde_json::Value {
        let output = Command::new(env!("CARGO_BIN_EXE_tmux-ide-host"))
            .args(["hook", verb, "--home"])
            .arg(&home)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{verb}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };
    unscoped_verb("install");
    assert!(
        fs::read_to_string(&settings)
            .unwrap()
            .contains("tmux-agent-ide"),
        "the unscoped install did not wire the adapter that is present"
    );
    let removed = unscoped_verb("uninstall");
    let claude = removed["applied"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["adapterId"] == "claude-code")
        .unwrap()
        .clone();
    assert_eq!(
        claude["changed"], true,
        "unscoped uninstall did nothing: {removed}"
    );
    assert!(
        !fs::read_to_string(&settings)
            .unwrap()
            .contains("tmux-agent-ide"),
        "a managed entry survived an unscoped uninstall"
    );
    fs::remove_dir_all(home).unwrap();
}
