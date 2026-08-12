use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};

use prost::Message;
use tmux_agent_protocol::v1;

#[test]
fn cli_persists_an_exact_unsequenced_hook_envelope() {
    let runtime = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("phase6-hook-cli-{}", uuid::Uuid::new_v4()));
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

    let bytes = fs::read(runtime.join("hook-fallback-codex-77.pb")).unwrap();
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
