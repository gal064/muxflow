use std::path::Path;

use serde_json::Value;
use tmux_agent_protocol::v1;

use super::{AgentAdapter, LaunchSpec, MANAGED_OWNER, MANAGED_VERSION, ParsedHook, fork_name};

/// The ownership marker, first line of every installed file. Ownership is decided
/// by this line alone: a file at the same path without it belongs to the user.
#[cfg(test)]
const OPENCODE_MARKER: &str = "// muxflow-managed owner=muxflow adapter=opencode";
const OPENCODE_TUI_TEMPLATE: &str = include_str!("opencode_tui.ts");
const OPENCODE_SERVER_TEMPLATE: &str = include_str!("opencode_server.ts");

/// The plugin directory OpenCode discovers, and the two files it holds. OpenCode
/// has no hook setting and no auto-discovery of a bare terminal plugin: a
/// terminal part loads as the `./tui` part of a discovered plugin directory, and
/// a discovered directory needs a server part for the terminal to load it.
const PLUGIN_DIRECTORY: &str = ".config/opencode/plugins/muxflow-agent-status";
const TUI_FILE: &str = "tui.ts";
const SERVER_FILE: &str = "index.ts";

pub(crate) struct OpenCodeAdapter;

impl OpenCodeAdapter {
    fn render(&self, template: &str, helper_path: &Path) -> String {
        let helper =
            serde_json::to_string(&helper_path.to_string_lossy()).expect("a string always serializes");
        template
            .replace("__MUXFLOW_HELPER__", &helper)
            .replace("__MUXFLOW_MANAGED_OWNER__", MANAGED_OWNER)
            .replace("__MUXFLOW_MANAGED_VERSION__", &MANAGED_VERSION.to_string())
    }
}

impl AgentAdapter for OpenCodeAdapter {
    fn kind(&self) -> &'static str {
        "opencode"
    }

    fn legacy_kind(&self) -> v1::AgentAdapterKind {
        v1::AgentAdapterKind::Opencode
    }

    fn id(&self) -> &'static str {
        "opencode"
    }

    fn display_name(&self) -> &'static str {
        "OpenCode"
    }

    fn executable(&self) -> &'static str {
        "opencode"
    }

    /// The discovered plugin directory. The managed artifact is this directory
    /// with [`TUI_FILE`] and [`SERVER_FILE`] in it, so a bare config file is
    /// never written and the plugin is owned whole.
    fn hook_relative_path(&self) -> &'static str {
        PLUGIN_DIRECTORY
    }

    /// The lifecycle names the managed plugin sends. It reports under the names
    /// ingest already understands, so ingest has no OpenCode-specific path; see
    /// [`Self::parse_hook`]. The raw OpenCode events (session.status, session.idle,
    /// session.error, session.created, session.deleted, permission.updated,
    /// permission.replied) are reduced to these inside the plugin.
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
        "OpenCode runs this plugin with your permissions and does not ask first. It reports only lifecycle state to muxflow; prompts, tool input and tool output never leave OpenCode."
    }

    /// OpenCode is a single native binary whose process title is `opencode`; the
    /// terminal and the shared `serve` process share that name.
    fn identifies_process(&self, command: &str) -> bool {
        let executable = command.split_ascii_whitespace().next().unwrap_or_default();
        executable.rsplit('/').next() == Some("opencode")
    }

    fn launch(&self, native_session_id: Option<&str>) -> LaunchSpec {
        LaunchSpec {
            program: self.executable(),
            arguments: native_session_id
                .map(|id| vec!["--session".to_owned(), id.to_owned()])
                .unwrap_or_default(),
        }
    }

    /// `--fork` continues a copy of the conversation under a new session id, the
    /// same flag `opencode run` exposes; `--title` names the copy so it is
    /// distinguishable from the original in the session list.
    fn branch(&self, native_session_id: &str, source_title: &str) -> LaunchSpec {
        let source_name = crate::service::topology::agent_title_suffix(source_title)
            .unwrap_or(source_title);
        LaunchSpec {
            program: self.executable(),
            arguments: vec![
                "--session".to_owned(),
                native_session_id.to_owned(),
                "--fork".to_owned(),
                "--title".to_owned(),
                fork_name(source_name),
            ],
        }
    }

    /// The plugin reports under the generic lifecycle names, so this matches the
    /// pi adapter's table exactly: a permission request blocks, a reply resumes a
    /// running turn, and the two Stop names end it.
    fn parse_hook(&self, payload: &Value) -> Result<ParsedHook, &'static str> {
        super::parse_common_hook(
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
            (TUI_FILE, self.render(OPENCODE_TUI_TEMPLATE, helper_path)),
            (SERVER_FILE, self.render(OPENCODE_SERVER_TEMPLATE, helper_path)),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_events_map_to_lifecycle_states() {
        for (event, expected) in [
            ("SessionStart", v1::AgentLifecycleState::Idle),
            ("UserPromptSubmit", v1::AgentLifecycleState::Working),
            ("PermissionRequest", v1::AgentLifecycleState::Blocked),
            ("PostToolUse", v1::AgentLifecycleState::Working),
            ("Stop", v1::AgentLifecycleState::Idle),
            ("StopFailure", v1::AgentLifecycleState::Idle),
            ("SessionEnd", v1::AgentLifecycleState::Idle),
            ("session.status", v1::AgentLifecycleState::Unknown),
        ] {
            let parsed = OpenCodeAdapter
                .parse_hook(&serde_json::json!({
                    "hook_event_name": event,
                    "session_id": "opencode-session",
                }))
                .unwrap();
            assert_eq!(parsed.lifecycle, expected, "{event}");
            assert_eq!(parsed.native_session_id, "opencode-session");
        }
    }

    #[test]
    fn opencode_process_detection_is_the_native_binary() {
        for command in ["opencode", "/usr/bin/opencode serve --service", "/a/b/opencode --session ses_1"] {
            assert!(OpenCodeAdapter.identifies_process(command), "{command}");
        }
        for command in ["opencoded", "/usr/bin/open", "oc"] {
            assert!(!OpenCodeAdapter.identifies_process(command), "{command}");
        }
    }

    #[test]
    fn opencode_resumes_by_session_and_forks_a_copy() {
        assert!(OpenCodeAdapter.launch(None).arguments.is_empty());
        assert_eq!(OpenCodeAdapter.launch(Some("ses_1")).arguments, ["--session", "ses_1"]);
        assert_eq!(
            OpenCodeAdapter.branch("ses_1", "✳ Fix tests").arguments,
            ["--session", "ses_1", "--fork", "--title", "Fix tests (fork)"]
        );
    }

    #[test]
    fn rendered_plugin_starts_with_the_marker_and_names_the_helper() {
        let files = OpenCodeAdapter
            .managed_files(Path::new("/opt/muxflow's/muxflow-host"))
            .unwrap();
        assert_eq!(files.len(), 2);
        let tui = &files
            .iter()
            .find(|(name, _)| *name == TUI_FILE)
            .unwrap()
            .1;
        let server = &files
            .iter()
            .find(|(name, _)| *name == SERVER_FILE)
            .unwrap()
            .1;
        assert!(tui.starts_with(&format!("{OPENCODE_MARKER}\n")));
        assert!(server.starts_with(&format!("{OPENCODE_MARKER}\n")));
        assert!(tui.contains(r#"const HELPER = "/opt/muxflow's/muxflow-host";"#));
        assert!(tui.contains(r#""--adapter", "opencode","#));
        assert!(tui.contains(&format!(r#""--managed-version", "{MANAGED_VERSION}""#)));
        assert!(server.contains("id: \"muxflow.agent-status\""));
        assert!(!tui.contains("__MUXFLOW_"));
        assert!(!server.contains("__MUXFLOW_"));
    }
}
