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
    let mut child = Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
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

#[test]
fn cli_discards_large_tool_results_for_claude_and_codex() {
    let runtime_root = if cfg!(target_os = "macos") {
        PathBuf::from("/private/tmp")
    } else {
        std::env::current_dir().unwrap().join("tmp")
    };
    let private_image = "private-image-data".repeat(32 * 1024);

    for adapter in ["claude-code", "codex"] {
        let runtime = runtime_root.join(format!("large-hook-{adapter}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&runtime).unwrap();
        let payload = serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "session-1",
            "tool_name": "Read",
            "tool_response": {
                "content": [{
                    "type": "image",
                    "source": {"type": "base64", "data": private_image},
                }],
            },
        }))
        .unwrap();
        assert!(payload.len() > 256 * 1024);

        let mut child = Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
            .args(["hook", "ingest", "--adapter", adapter])
            .env("ADE_HOST_RUNTIME_DIR", &runtime)
            .env("TMUX_PANE", "%77")
            .env_remove("TMUX")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&payload).unwrap();
        assert!(
            child.wait().unwrap().success(),
            "{adapter} rejected a valid large vendor hook"
        );

        let mailbox = fs::read_dir(&runtime)
            .unwrap()
            .flatten()
            .find(|entry| entry.path().extension().is_some_and(|value| value == "pb"))
            .expect("large hook was not delivered to the fallback mailbox");
        let event =
            v1::AgentHookEvent::decode(fs::read(mailbox.path()).unwrap().as_slice()).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&event.payload_json).unwrap(),
            serde_json::json!({
                "hook_event_name": "PostToolUse",
                "session_id": "session-1",
            })
        );
        assert!(!String::from_utf8_lossy(&event.payload_json).contains("private-image-data"));
        fs::remove_dir_all(runtime).unwrap();
    }
}

/// Voice mode (docs/mobile/voice-mode-plan.md §4.7): a Codex `Stop` piped
/// through the CLI persists a `payload_json` that carries the final message
/// and still nothing else private.
#[test]
fn cli_forwards_the_last_assistant_message_on_stop() {
    let runtime_root = if cfg!(target_os = "macos") {
        PathBuf::from("/private/tmp")
    } else {
        std::env::current_dir().unwrap().join("tmp")
    };
    let runtime = runtime_root.join(format!("voice-hook-cli-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&runtime).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
        .args(["hook", "ingest", "--adapter", "codex"])
        .env("ADE_HOST_RUNTIME_DIR", &runtime)
        .env("TMUX_PANE", "%78")
        .env_remove("TMUX")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"hook_event_name":"Stop","session_id":"s","last_assistant_message":"Done: the tests pass.","prompt":"private","transcript_path":"/private/path"}"#)
        .unwrap();
    assert!(child.wait().unwrap().success());

    let mailbox = fs::read_dir(&runtime)
        .unwrap()
        .flatten()
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("hook-fallback-codex-78-")
        })
        .expect("the Stop was not filed");
    let event = v1::AgentHookEvent::decode(fs::read(mailbox.path()).unwrap().as_slice()).unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&event.payload_json).unwrap();
    assert_eq!(
        payload,
        serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "s",
            "last_assistant_message": "Done: the tests pass.",
        })
    );
    fs::remove_dir_all(runtime).unwrap();
}

/// The agent's configuration follows the user out of tmux; the hook must not
/// complain when it gets there.
///
/// One managed hook line runs on every prompt of every session that agent ever
/// starts, including a plain terminal with no tmux server in sight, where there
/// is legitimately no pane to file the event against. Exit zero, print nothing,
/// and write nothing — a mailbox entry here would be an event no daemon can
/// ever attribute to a pane. A malformed `TMUX_PANE` is the opposite case and
/// stays loud.
#[test]
fn a_hook_outside_tmux_is_silent_and_files_nothing() {
    let runtime_root = if cfg!(target_os = "macos") {
        PathBuf::from("/private/tmp")
    } else {
        std::env::current_dir().unwrap().join("tmp")
    };
    let runtime = runtime_root.join(format!("hook-no-tmux-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&runtime).unwrap();

    let ingest = |pane: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_muxflow-host"));
        command
            .args(["hook", "ingest", "--adapter", "claude-code"])
            .env("ADE_HOST_RUNTIME_DIR", &runtime)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(pane) = pane {
            command.env("TMUX_PANE", pane);
        }
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"hook_event_name":"UserPromptSubmit","session_id":"s"}"#)
            .unwrap();
        child.wait_with_output().unwrap()
    };

    let outside = ingest(None);
    assert!(
        outside.status.success(),
        "a session outside tmux must not report a hook error: {}",
        String::from_utf8_lossy(&outside.stderr)
    );
    assert!(outside.stdout.is_empty(), "the hook must print nothing");
    assert!(outside.stderr.is_empty(), "the hook must print nothing");
    assert_eq!(
        fs::read_dir(&runtime).unwrap().count(),
        0,
        "an event with no pane must not be filed for a pane"
    );

    let malformed = ingest(Some("pane-7"));
    assert!(
        !malformed.status.success(),
        "a present but malformed TMUX_PANE is a real misconfiguration and must still fail"
    );
    assert_eq!(fs::read_dir(&runtime).unwrap().count(), 0);
    fs::remove_dir_all(runtime).unwrap();
}

/// The development override is authoritative even when the daemon and hook
/// inherit conflicting XDG runtime environments.
#[test]
fn a_hook_reaches_the_overridden_daemon_across_different_shell_environments() {
    // Short, because the daemon's socket has to fit `sockaddr_un::sun_path`.
    let root = if cfg!(target_os = "macos") {
        PathBuf::from("/private/tmp")
    } else {
        PathBuf::from("/tmp")
    }
    .join(format!("ade-split-{}", uuid::Uuid::new_v4().simple()));
    let home = root.join("home");
    let hook_xdg = root.join("hx");
    let daemon_runtime = root.join("dr");
    fs::create_dir_all(&hook_xdg).unwrap();
    fs::create_dir_all(&daemon_runtime).unwrap();
    let socket = daemon_runtime.join("host.sock");

    // Detached from the harness's pipes and killed on any exit path: a daemon
    // that outlives a failing assertion holds the captured output open and
    // hangs `cargo test` instead of reporting the failure.
    let mut daemon = DaemonGuard(Some(
        Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
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
        let mut child = Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
            .args(["hook", "ingest", "--adapter", "codex"])
            .env("HOME", &home)
            // What a tmux pane inherits, and what the daemon never saw.
            .env("XDG_RUNTIME_DIR", &hook_xdg)
            .env("ADE_HOST_RUNTIME_DIR", &daemon_runtime)
            .env("TMUX_PANE", "%91")
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
        assert_eq!(
            mailbox_entries(&daemon_runtime).is_empty(),
            !mailbox_expected,
            "the daemon's directory holds the wrong number of undelivered events"
        );
    };

    ingest(false);

    // With the daemon gone, the event waits in the same isolated state root.
    let stopped = Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
        .args(["daemon-stop"])
        .arg("--socket")
        .arg(&socket)
        .env("HOME", &home)
        .env("ADE_HOST_RUNTIME_DIR", &daemon_runtime)
        .env_remove("XDG_RUNTIME_DIR")
        .status()
        .unwrap();
    assert!(stopped.success());
    daemon.reap();
    ingest(true);

    // And it is read when that daemon comes back. Delivering new events
    // correctly is only half of it: an event written while the daemon was down
    // is worth nothing if the daemon that returns does not look where the hook
    // was told to leave it.
    let mut restarted = DaemonGuard(Some(
        Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
            .args(["daemon"])
            .env("HOME", &home)
            .env("ADE_HOST_RUNTIME_DIR", &daemon_runtime)
            .env_remove("XDG_RUNTIME_DIR")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    ));
    let drained = (0..200).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(25));
        socket.exists() && mailbox_entries(&daemon_runtime).is_empty()
    });
    assert!(
        drained,
        "the restarted daemon left events waiting in its own directory: {:?}",
        mailbox_entries(&daemon_runtime)
    );
    assert!(
        fs::read_to_string(daemon_runtime.join("agents.json"))
            .unwrap_or_default()
            .contains("\"source_event_ids\":[\""),
        "the replayed event was not recorded as hook-sourced"
    );
    restarted.reap_after_stop(&socket, &home, &daemon_runtime);

    fs::remove_dir_all(root).unwrap();
}

struct DaemonGuard(Option<std::process::Child>);

impl DaemonGuard {
    fn reap(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.wait();
        }
    }

    fn reap_after_stop(&mut self, socket: &std::path::Path, home: &PathBuf, runtime: &PathBuf) {
        let _ = Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
            .args(["daemon-stop"])
            .arg("--socket")
            .arg(socket)
            .env("HOME", home)
            .env("ADE_HOST_RUNTIME_DIR", runtime)
            .env_remove("XDG_RUNTIME_DIR")
            .status();
        self.reap();
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
        "/../../tests/integration/agent-status/fixtures/claude-settings-existing-hooks.json"
    ))
    .unwrap();
    fs::write(&settings, &original).unwrap();

    let run = |verb: &str| -> serde_json::Value {
        let output = Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
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

    // Nothing writes the operator's own configuration without being told to.
    // This command is the one installer with no interface to ask through, and
    // it used to have no notion of consent at all.
    let unconfirmed = Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
        .args(["hook", "install"])
        .output()
        .unwrap();
    assert!(!unconfirmed.status.success());
    let refusal = String::from_utf8_lossy(&unconfirmed.stderr).into_owned();
    assert!(refusal.contains("--yes"), "{refusal}");
    assert!(
        refusal.contains("your own agent configuration"),
        "{refusal}"
    );
    // The gate asks where the write lands, not how the command was spelled: a
    // redirection that resolves back to the operator's own files is not one.
    let laundered = Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
        .args(["hook", "install", "--home"])
        .arg(std::env::var("HOME").unwrap())
        .output()
        .unwrap();
    assert!(
        !laundered.status.success(),
        "--home $HOME wrote the operator's real configuration unconfirmed"
    );
    // And `status` still answers freely: it reads, it does not write.
    assert!(
        Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
            .args(["hook", "status"])
            .output()
            .unwrap()
            .status
            .success()
    );

    // The override names one adapter's file, so it cannot be used without one.
    let ambiguous = Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
        .args(["hook", "status", "--settings-path", "/tmp/anything.json"])
        .output()
        .unwrap();
    assert!(!ambiguous.status.success());

    // Without `--adapter`, an install must not create configuration for an
    // agent that is not on this host — the exact thing the desktop refuses.
    let unscoped = Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
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
        let output = Command::new(env!("CARGO_BIN_EXE_muxflow-host"))
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
        fs::read_to_string(&settings).unwrap().contains("muxflow"),
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
        !fs::read_to_string(&settings).unwrap().contains("muxflow"),
        "a managed entry survived an unscoped uninstall"
    );
    fs::remove_dir_all(home).unwrap();
}
