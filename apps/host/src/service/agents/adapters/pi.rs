use std::path::Path;

use serde_json::Value;
use tmux_agent_protocol::v1;

use super::{AgentAdapter, LaunchSpec, MANAGED_OWNER, MANAGED_VERSION, ParsedHook};

/// The first line of the installed extension. Ownership is decided by this
/// line alone: a file at the same path without it belongs to the user.
#[cfg(test)]
const PI_EXTENSION_MARKER: &str = "// muxflow-managed owner=muxflow adapter=pi";
const PI_EXTENSION_TEMPLATE: &str = include_str!("pi_extension.ts");

pub(crate) struct PiAdapter;

impl AgentAdapter for PiAdapter {
    fn kind(&self) -> &'static str {
        "pi"
    }

    fn legacy_kind(&self) -> v1::AgentAdapterKind {
        v1::AgentAdapterKind::Pi
    }

    fn id(&self) -> &'static str {
        "pi"
    }

    fn display_name(&self) -> &'static str {
        "Pi"
    }

    fn executable(&self) -> &'static str {
        "pi"
    }

    /// pi has no hook configuration. It loads every file in this directory as
    /// an in-process extension, so the managed artifact is a whole file.
    fn hook_relative_path(&self) -> &'static str {
        ".pi/agent/extensions/muxflow-agent-status.ts"
    }

    /// The pi extension events the managed file listens to. The file sends each
    /// one under the lifecycle name ingest already understands, so ingest has
    /// no pi-specific path; see [`Self::parse_hook`].
    fn hook_events(&self) -> &'static [&'static str] {
        &[
            "session_start",
            "agent_start",
            "agent_end",
            "agent_settled",
            "ui_prompt_start",
            "ui_prompt_end",
            "session_shutdown",
        ]
    }

    fn hook_trust_guidance(&self) -> &'static str {
        "Pi runs every file in ~/.pi/agent/extensions with your permissions and does not ask first. This file only reports lifecycle state to muxflow."
    }

    /// pi sets its process title to `pi`, so a running pi normally reads as
    /// `pi`. Before it does, or where the title is not applied, the process
    /// is `node …/.bin/pi` or `node` with the package entry point.
    fn identifies_process(&self, command: &str) -> bool {
        let executable = command.split_ascii_whitespace().next().unwrap_or_default();
        executable.rsplit('/').next() == Some("pi")
            || executable.ends_with("/pi-coding-agent/dist/cli.js")
            || executable.ends_with("/pi-coding-agent/dist/bundle/cli.js")
    }

    fn launch(&self, native_session_id: Option<&str>) -> LaunchSpec {
        LaunchSpec {
            program: self.executable(),
            arguments: native_session_id
                .map(|id| vec!["--session".to_owned(), id.to_owned()])
                .unwrap_or_default(),
        }
    }

    /// `PermissionRequest` is an extension dialog open while a run is active,
    /// and `PostToolUse` is the last such dialog closing with the run still
    /// active. `Stop` is pi's `agent_settled`: no retry, compaction or queued
    /// continuation will follow.
    fn parse_hook(&self, payload: &Value) -> Result<ParsedHook, &'static str> {
        super::parse_common_hook(
            payload,
            &[
                ("SessionStart", v1::AgentLifecycleState::Idle),
                ("UserPromptSubmit", v1::AgentLifecycleState::Working),
                ("PermissionRequest", v1::AgentLifecycleState::Blocked),
                ("PostToolUse", v1::AgentLifecycleState::Working),
                ("Stop", v1::AgentLifecycleState::Idle),
                ("SessionEnd", v1::AgentLifecycleState::Idle),
            ],
        )
    }

    fn managed_file(&self, helper_path: &Path) -> Option<String> {
        let helper = serde_json::to_string(&helper_path.to_string_lossy())
            .expect("a string always serializes");
        Some(
            PI_EXTENSION_TEMPLATE
                .replace("__MUXFLOW_HELPER__", &helper)
                .replace("__MUXFLOW_MANAGED_OWNER__", MANAGED_OWNER)
                .replace("__MUXFLOW_MANAGED_VERSION__", &MANAGED_VERSION.to_string()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_events_map_to_lifecycle_states() {
        for (event, expected) in [
            ("SessionStart", v1::AgentLifecycleState::Idle),
            ("UserPromptSubmit", v1::AgentLifecycleState::Working),
            ("PermissionRequest", v1::AgentLifecycleState::Blocked),
            ("PostToolUse", v1::AgentLifecycleState::Working),
            ("Stop", v1::AgentLifecycleState::Idle),
            ("SessionEnd", v1::AgentLifecycleState::Idle),
            ("agent_start", v1::AgentLifecycleState::Unknown),
        ] {
            let parsed = PiAdapter
                .parse_hook(&serde_json::json!({
                    "hook_event_name": event,
                    "session_id": "pi-session",
                }))
                .unwrap();
            assert_eq!(parsed.lifecycle, expected, "{event}");
            assert_eq!(parsed.native_session_id, "pi-session");
        }
    }

    #[test]
    fn pi_process_detection_covers_the_launcher_and_npm_entry_points() {
        for command in [
            "pi",
            "/home/user/.local/bin/pi --session abc",
            "/home/user/.pi/agent/install/releases/1.3.0/node_modules/.bin/pi",
            "/usr/lib/node_modules/@earendil-works/pi-coding-agent/dist/cli.js",
            "/opt/node_modules/@mariozechner/pi-coding-agent/dist/bundle/cli.js",
        ] {
            assert!(PiAdapter.identifies_process(command), "{command}");
        }
        for command in ["pip", "pipx", "/usr/bin/pinentry", "/srv/pi-hole/cli.js"] {
            assert!(!PiAdapter.identifies_process(command), "{command}");
        }
        assert_eq!(
            super::super::detect_argv(&[
                "node".into(),
                "/home/user/.pi/agent/install/releases/1.3.0/node_modules/.bin/pi".into(),
            ])
            .map(|adapter| adapter.id()),
            Some("pi")
        );
    }

    #[test]
    fn pi_resumes_by_session() {
        assert!(PiAdapter.launch(None).arguments.is_empty());
        assert_eq!(
            PiAdapter.launch(Some("0193-abc")).arguments,
            ["--session", "0193-abc"]
        );
    }

    #[test]
    fn rendered_extension_starts_with_the_marker_and_names_the_helper() {
        let rendered = PiAdapter
            .managed_file(Path::new("/opt/muxflow's/muxflow-host"))
            .unwrap();
        assert!(rendered.starts_with(&format!("{PI_EXTENSION_MARKER}\n")));
        assert!(rendered.contains(r#"const HELPER = "/opt/muxflow's/muxflow-host";"#));
        assert!(rendered.contains(&format!(r#""--managed-version", "{MANAGED_VERSION}""#)));
        assert!(!rendered.contains("__MUXFLOW_"));
    }
}
