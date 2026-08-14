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
