use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tmux_agent_protocol::v1;

use super::{fork_name, parse_common_hook, AgentAdapter, LaunchSpec, ParsedHook};

pub(crate) const HERMES_PLUGIN_NAME: &str = "muxflow-agent-status";
pub(crate) const HERMES_MARKER: &str = "# muxflow-managed owner=muxflow adapter=hermes";

const PLUGIN_MANIFEST: &str = r#"# muxflow-managed owner=muxflow adapter=hermes
name: muxflow-agent-status
version: "__MUXFLOW_MANAGED_VERSION__"
description: Reports Hermes session lifecycle state to muxflow.
author: muxflow
provides_hooks:
  - on_session_start
  - on_session_open
  - pre_llm_call
  - post_llm_call
  - pre_tool_call
  - post_tool_call
  - pre_approval_request
  - post_approval_response
  - on_human_input_request
  - on_human_input_resolved
  - agent_loop_stopped
  - on_session_end
  - on_session_finalize
  - on_session_reset
  - on_session_delete
"#;

const PLUGIN_INIT: &str = r#"# muxflow-managed owner=muxflow adapter=hermes
# Reports this Hermes session's lifecycle state to muxflow.
import json
import os
import subprocess

HELPER = "__MUXFLOW_HELPER__"
ARGS = [
    "hook",
    "ingest",
    "--adapter",
    "hermes",
    "--managed-owner",
    "__MUXFLOW_MANAGED_OWNER__",
    "--managed-version",
    "__MUXFLOW_MANAGED_VERSION__",
]
TIMEOUT_SECONDS = 5
_last_response = {}
_ended_turns = set()


def _send(payload):
    if not os.environ.get("TMUX_PANE"):
        return
    try:
        subprocess.run(
            [HELPER] + ARGS,
            input=json.dumps(payload, default=str),
            text=True,
            timeout=TIMEOUT_SECONDS,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
    except Exception:
        pass


def _report(event, **extra):
    payload = {"hook_event_name": event}
    payload.update({key: value for key, value in extra.items() if value is not None})
    _send(payload)


def _session_id(kwargs):
    return str(kwargs.get("session_id") or "")


def _turn_ended(session_id, turn_id):
    return (session_id, turn_id) in _ended_turns


def _session_start(**kwargs):
    session_id = _session_id(kwargs)
    _last_response.pop(session_id, None)
    _report("SessionStart", session_id=session_id, source=kwargs.get("reason"))


def _session_open(**kwargs):
    session_id = _session_id(kwargs)
    _report("SessionStart", session_id=session_id, source=kwargs.get("reason"))


def _user_prompt(**kwargs):
    _report("UserPromptSubmit", session_id=_session_id(kwargs))


def _llm_response(**kwargs):
    session_id = _session_id(kwargs)
    response = kwargs.get("assistant_response")
    if session_id and response:
        _last_response[session_id] = response


def _tool_call(**kwargs):
    session_id = _session_id(kwargs)
    if not _turn_ended(session_id, kwargs.get("turn_id")):
        _report("PostToolUse", session_id=session_id)


def _approval_request(**kwargs):
    session_id = _session_id(kwargs)
    if not _turn_ended(session_id, kwargs.get("turn_id")):
        _report("PermissionRequest", session_id=session_id)


def _approval_response(**kwargs):
    session_id = _session_id(kwargs)
    if not _turn_ended(session_id, kwargs.get("turn_id")):
        _report("PostToolUse", session_id=session_id)


def _loop_stopped(**kwargs):
    session_id = str(kwargs.get("session_id") or kwargs.get("session_key") or "")
    _report("Stop", session_id=session_id)


def _session_end(**kwargs):
    session_id = _session_id(kwargs)
    turn_id = kwargs.get("turn_id")
    _ended_turns.add((session_id, turn_id))
    event = "StopFailure" if kwargs.get("failed") or kwargs.get("interrupted") else "Stop"
    extra = {"session_id": session_id}
    response = _last_response.pop(session_id, None)
    if response:
        extra["last_assistant_message"] = response
    _report(event, **extra)


def _session_finalize(**kwargs):
    session_id = str(kwargs.get("old_session_id") or kwargs.get("session_id") or "")
    _report("SessionEnd", session_id=session_id)


def _session_reset(**kwargs):
    session_id = str(kwargs.get("old_session_id") or kwargs.get("session_id") or "")
    _report("SessionEnd", session_id=session_id)


def _session_delete(**kwargs):
    _report("SessionEnd", session_id=_session_id(kwargs))


def _safe(callback):
    def wrapped(**kwargs):
        try:
            callback(**kwargs)
        except Exception:
            pass
    return wrapped


def register(ctx):
    hooks = {
        "on_session_start": _session_start,
        "on_session_open": _session_open,
        "pre_llm_call": _user_prompt,
        "post_llm_call": _llm_response,
        "pre_tool_call": _tool_call,
        "post_tool_call": _tool_call,
        "pre_approval_request": _approval_request,
        "post_approval_response": _approval_response,
        "on_human_input_request": _approval_request,
        "on_human_input_resolved": _approval_response,
        "agent_loop_stopped": _loop_stopped,
        "on_session_end": _session_end,
        "on_session_finalize": _session_finalize,
        "on_session_reset": _session_reset,
        "on_session_delete": _session_delete,
    }
    for name, callback in hooks.items():
        ctx.register_hook(name, _safe(callback))
"#;

fn render(template: &str, helper_path: &Path) -> String {
    let helper = helper_path.to_string_lossy();
    template
        .replace("__MUXFLOW_HELPER__", &helper.replace('\\', "\\\\").replace('"', "\\\""))
        .replace("__MUXFLOW_MANAGED_OWNER__", super::MANAGED_OWNER)
        .replace(
            "__MUXFLOW_MANAGED_VERSION__",
            &super::MANAGED_VERSION.to_string(),
        )
}

#[derive(Debug, Default)]
pub(crate) struct HermesAdapter;

impl AgentAdapter for HermesAdapter {
    fn kind(&self) -> &'static str {
        "hermes"
    }

    fn legacy_kind(&self) -> v1::AgentAdapterKind {
        v1::AgentAdapterKind::Hermes
    }

    fn id(&self) -> &'static str {
        "hermes"
    }

    fn display_name(&self) -> &'static str {
        "Hermes"
    }

    fn executable(&self) -> &'static str {
        "hermes"
    }

    fn hook_relative_path(&self) -> &'static str {
        ".hermes/plugins/muxflow-agent-status"
    }

    fn hook_events(&self) -> &'static [&'static str] {
        &[
            "SessionStart",
            "UserPromptSubmit",
            "PermissionRequest",
            "PostToolUse",
            "Stop",
            "StopFailure",
            "SessionEnd",
        ]
    }

    fn hook_trust_guidance(&self) -> &'static str {
        "Muxflow installs a Hermes plugin directory and enables it with Hermes's own `hermes plugins enable` command. Hermes edits its own config; muxflow does not rewrite `~/.hermes/config.yaml`."
    }

    fn identifies_process(&self, command: &str) -> bool {
        let tokens: Vec<&str> = command.split_ascii_whitespace().collect();
        let first = tokens.first().copied().unwrap_or("");
        let base = first.rsplit('/').next().unwrap_or(first);
        let skipped = tokens
            .iter()
            .skip(1)
            .any(|token| *token == "gateway" || *token == "cron");

        if base == "hermes" || base == "hermes-agent" {
            return !skipped;
        }
        if base.starts_with("python") {
            let joined = tokens.join(" ");
            return !skipped
                && (joined.contains("hermes_cli/main.py")
                    || joined.contains("-m hermes_cli.main")
                    || joined.contains("hermes_cli.main"));
        }
        if base == "uv" {
            return !skipped && tokens.iter().any(|token| *token == "hermes");
        }
        false
    }

    fn launch(&self, native_session_id: Option<&str>) -> LaunchSpec {
        let mut arguments = vec!["chat".to_owned()];
        if let Some(id) = native_session_id {
            arguments.push("--resume".to_owned());
            arguments.push(id.to_owned());
        }
        LaunchSpec {
            program: self.executable(),
            arguments,
        }
    }

    fn branch(&self, _native_session_id: &str, source_title: &str) -> LaunchSpec {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let title = format!("{} {}", fork_name(source_title), stamp);
        LaunchSpec {
            program: self.executable(),
            arguments: vec![
                "chat".to_owned(),
                "--continue".to_owned(),
                title,
                "--create-if-missing".to_owned(),
            ],
        }
    }

    fn parse_hook(&self, payload: &Value) -> Result<ParsedHook, &'static str> {
        parse_common_hook(
            payload,
            &[
                ("SessionStart", v1::AgentLifecycleState::Idle),
                ("UserPromptSubmit", v1::AgentLifecycleState::Working),
                ("PermissionRequest", v1::AgentLifecycleState::Blocked),
                ("PostToolUse", v1::AgentLifecycleState::Working),
                ("Stop", v1::AgentLifecycleState::Idle),
                ("StopFailure", v1::AgentLifecycleState::Idle),
                ("SessionEnd", v1::AgentLifecycleState::Idle),
            ],
        )
    }

    fn managed_files(&self, helper_path: &Path) -> Option<Vec<(&'static str, String)>> {
        Some(vec![
            ("plugin.yaml", render(PLUGIN_MANIFEST, helper_path)),
            ("__init__.py", render(PLUGIN_INIT, helper_path)),
        ])
    }

    fn managed_activation(&self, install: bool) -> Option<(String, Vec<String>)> {
        Some((
            "hermes".to_owned(),
            vec![
                "plugins".to_owned(),
                if install { "enable" } else { "disable" }.to_owned(),
                HERMES_PLUGIN_NAME.to_owned(),
            ],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branch_title(adapter: &HermesAdapter, source: &str) -> String {
        adapter.branch("", source).arguments[2].clone()
    }

    #[test]
    fn hermes_events_map_to_lifecycle_states() {
        let adapter = HermesAdapter;
        let cases = [
            ("SessionStart", v1::AgentLifecycleState::Idle),
            ("UserPromptSubmit", v1::AgentLifecycleState::Working),
            ("PermissionRequest", v1::AgentLifecycleState::Blocked),
            ("PostToolUse", v1::AgentLifecycleState::Working),
            ("Stop", v1::AgentLifecycleState::Idle),
            ("StopFailure", v1::AgentLifecycleState::Idle),
            ("SessionEnd", v1::AgentLifecycleState::Idle),
        ];
        for (event, expected) in cases {
            let payload = serde_json::json!({
                "hook_event_name": event,
                "session_id": "s1",
            });
            assert_eq!(adapter.parse_hook(&payload).unwrap().lifecycle, expected);
        }
    }

    #[test]
    fn hermes_detects_manual_python_and_skips_gateway_cron() {
        let adapter = HermesAdapter;
        assert!(adapter.identifies_process("hermes"));
        assert!(adapter.identifies_process("hermes chat"));
        assert!(adapter.identifies_process("python3 -m hermes_cli.main chat"));
        assert!(adapter.identifies_process("python3 /opt/hermes_cli/main.py chat"));
        assert!(!adapter.identifies_process("hermes gateway"));
        assert!(!adapter.identifies_process("hermes cron"));
        assert!(!adapter.identifies_process("python3 -m hermes_cli.main gateway"));
    }

    #[test]
    fn hermes_branch_title_is_unique_and_prefixed() {
        let adapter = HermesAdapter;
        let first = branch_title(&adapter, "Fix tests");
        let second = branch_title(&adapter, "Fix tests");
        assert!(first.starts_with("Fix tests (fork) "));
        assert_ne!(first, second);
        assert_eq!(adapter.branch("s1", "Fix tests").arguments[0], "chat");
        assert_eq!(adapter.branch("s1", "Fix tests").arguments[1], "--continue");
        assert_eq!(
            adapter.branch("s1", "Fix tests").arguments[3],
            "--create-if-missing"
        );
    }

    #[test]
    fn hermes_plugin_files_are_marked_and_render_the_helper() {
        let adapter = HermesAdapter;
        let helper = Path::new("/tmp/muxflow-helper");
        let files = adapter.managed_files(helper).unwrap();
        assert_eq!(files.len(), 2);
        for (_, content) in &files {
            assert!(content.starts_with(HERMES_MARKER));
        }
        assert!(files[1].1.contains("/tmp/muxflow-helper"));
        assert!(files[0].1.contains("muxflow-agent-status"));
    }

    #[test]
    fn hermes_activation_uses_hermes_plugin_command() {
        let adapter = HermesAdapter;
        assert_eq!(
            adapter.managed_activation(true),
            Some(("hermes".to_owned(), vec!["plugins".to_owned(), "enable".to_owned(), "muxflow-agent-status".to_owned()]))
        );
        assert_eq!(
            adapter.managed_activation(false),
            Some(("hermes".to_owned(), vec!["plugins".to_owned(), "disable".to_owned(), "muxflow-agent-status".to_owned()]))
        );
    }
}
