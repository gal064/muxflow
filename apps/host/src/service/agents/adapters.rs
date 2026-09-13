use serde_json::Value;
use std::path::{Path, PathBuf};
use tmux_agent_protocol::v1;

const HOOK_AUTHORITY_MILLIS: i64 = 30_000;
pub(crate) const CODEX_APPROVAL_REVIEWER_FIELD: &str = "approval_reviewer";
pub(crate) const CODEX_APPROVAL_TURN_ID_FIELD: &str = "approval_turn_id";
/// Private, transient locator used only between a live Codex hook and daemon.
/// It is removed before an undelivered event enters the durable fallback
/// mailbox.
pub(crate) const CODEX_TRANSCRIPT_PATH_FIELD: &str = "codex_transcript_path";
/// Sanitized transcript-derived child edges carried only when a hook has to
/// enter the durable fallback mailbox.
pub(crate) const CODEX_CHILD_TRANSITIONS_FIELD: &str = "child_transitions";
pub(crate) const CODEX_SUBAGENT_ID_FIELD: &str = "agent_id";
pub(crate) const MAX_CODEX_SUBAGENT_ID_BYTES: usize = 1024;
pub(crate) const CLAUDE_HAS_RUNNING_SUBAGENT_FIELD: &str = "has_running_subagent";
/// The agent's final message, forwarded on `Stop` by both adapters for voice
/// mode (docs/mobile/voice-mode-plan.md §4.5). Consumed by ingest and handed
/// to the voice service; never stored in `AgentRecord` or `agents.json`.
pub(crate) const LAST_ASSISTANT_MESSAGE_FIELD: &str = "last_assistant_message";
/// Set when the hook cut the message at its 32 KiB bound.
pub(crate) const LAST_ASSISTANT_MESSAGE_TRUNCATED_FIELD: &str = "last_assistant_message_truncated";
pub(crate) const MANAGED_OWNER: &str = "muxflow";
/// Bumped whenever the managed event set or entry configuration changes. An
/// older install can otherwise look current while missing an event or carrying
/// vendor-invalid settings such as an excessive timeout.
pub(crate) const MANAGED_VERSION: u32 = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedHook {
    pub native_session_id: String,
    pub lifecycle: v1::AgentLifecycleState,
    pub authority_millis: i64,
    pub event_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchSpec {
    pub(crate) program: &'static str,
    pub(crate) arguments: Vec<String>,
}

pub(crate) trait AgentAdapter: Send + Sync {
    fn kind(&self) -> &'static str;
    fn legacy_kind(&self) -> v1::AgentAdapterKind;
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn executable(&self) -> &'static str;
    fn hook_relative_path(&self) -> &'static str;
    fn hook_events(&self) -> &'static [&'static str];
    /// Vendor-specific wall-clock allowance written into the managed entry.
    fn hook_timeout_seconds(&self, _event: &str) -> u64 {
        5
    }
    fn hook_trust_guidance(&self) -> &'static str;
    fn identifies_process(&self, command: &str) -> bool;
    fn launch(&self, native_session_id: Option<&str>) -> LaunchSpec;
    fn parse_hook(&self, payload: &Value) -> Result<ParsedHook, &'static str>;

    fn hook_path(&self, home: &Path) -> PathBuf {
        home.join(self.hook_relative_path())
    }

    fn hook_command(&self, helper_path: &Path) -> String {
        format!(
            "{} hook ingest --adapter {} --managed-owner {} --managed-version {}",
            shell_quote(helper_path),
            self.id(),
            MANAGED_OWNER,
            MANAGED_VERSION
        )
    }

    /// `observed` is this adapter's own wiring, not a list to search. Passing
    /// the list meant a linear lookup with a fallback for an adapter the probe
    /// said nothing about — a state its producer, which walks the same
    /// registry, cannot construct.
    fn descriptor(
        &self,
        home: &Path,
        observed: &super::hooks::AdapterWiring,
    ) -> v1::AgentAdapterDescriptor {
        v1::AgentAdapterDescriptor {
            hook_wiring: observed.state.into(),
            hook_wiring_detail: observed.detail.clone(),
            // The host decides, because the host is what observed the wiring.
            // The desktop had its own copy of this rule, in TypeScript, and it
            // had already drifted from the helper's.
            hook_setup_recommended: observed.state.invites_setup(),
            id: self.id().into(),
            display_name: self.display_name().into(),
            supports_launch: true,
            supports_resume: true,
            supports_hooks: true,
            supports_process_detection: true,
            hook_config_path: self.hook_path(home).to_string_lossy().into_owned(),
            hook_events: self
                .hook_events()
                .iter()
                .map(|event| (*event).into())
                .collect(),
        }
    }
}

pub(super) struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn kind(&self) -> &'static str {
        "codex"
    }

    fn legacy_kind(&self) -> v1::AgentAdapterKind {
        v1::AgentAdapterKind::Codex
    }

    fn id(&self) -> &'static str {
        "codex"
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn executable(&self) -> &'static str {
        "codex"
    }

    fn hook_relative_path(&self) -> &'static str {
        ".codex/hooks.json"
    }

    /// Measured against a real `~/.codex/hooks.json` (Codex CLI 0.128 through
    /// 0.154.0): Codex fires `SessionStart`, `UserPromptSubmit`, `PreToolUse`,
    /// `PermissionRequest`, `PostToolUse`, `Stop`, `Interrupt`, `SessionEnd`,
    /// `SubagentStart` and `SubagentStop`.
    ///
    /// Two events Claude Code has are absent from that surface and are
    /// therefore gaps rather than omissions: there is no `StopFailure`, so a
    /// turn that ends in failure is indistinguishable from one that succeeds,
    /// and there is no `Notification`. `UserPromptSubmit` seeds the turn's
    /// reviewer cache before work starts; `PermissionRequest`
    /// revalidates a cache miss. Only an explicitly user-reviewed request and
    /// `PreToolUse(request_user_input)` are treated as blocked; an unresolved
    /// reviewer stays Working because transient transcript races are common in
    /// auto-review. Codex's parent `Stop` does not
    /// report its live children, so `SubagentStart` and `SubagentStop` provide
    /// the opaque IDs needed to keep an unwaited parent working until its last
    /// child finishes.
    fn hook_events(&self) -> &'static [&'static str] {
        &[
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PermissionRequest",
            "PostToolUse",
            "SubagentStart",
            "SubagentStop",
            "Stop",
            "Interrupt",
            "SessionEnd",
        ]
    }

    fn hook_timeout_seconds(&self, event: &str) -> u64 {
        match event {
            // Codex clamps terminal lifecycle hooks to three seconds and
            // warns whenever their configured timeout is higher.
            "Interrupt" | "SessionEnd" => 3,
            _ => 5,
        }
    }

    fn hook_trust_guidance(&self) -> &'static str {
        "Codex will review the exact hook hash; approve it in Codex. This installer never edits or bypasses hook trust state."
    }

    fn identifies_process(&self, command: &str) -> bool {
        command_basename(command) == "codex"
    }

    fn launch(&self, native_session_id: Option<&str>) -> LaunchSpec {
        LaunchSpec {
            program: self.executable(),
            arguments: native_session_id
                .map(|id| vec!["resume".to_owned(), id.to_owned()])
                .unwrap_or_default(),
        }
    }

    fn parse_hook(&self, payload: &Value) -> Result<ParsedHook, &'static str> {
        let event = hook_event_name(payload)?;
        if matches!(event, "SubagentStart" | "SubagentStop") {
            let child_id = payload
                .get(CODEX_SUBAGENT_ID_FIELD)
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or("Codex subagent hook missing agent ID")?;
            if child_id.len() > MAX_CODEX_SUBAGENT_ID_BYTES {
                return Err("Codex subagent hook agent ID exceeds its bound");
            }
        }
        let pre_tool_lifecycle =
            if payload.get("tool_name").and_then(Value::as_str) == Some("request_user_input") {
                v1::AgentLifecycleState::Blocked
            } else {
                v1::AgentLifecycleState::Working
            };
        parse_common_hook(
            payload,
            &[
                // Ingest may promote this to Working when the request's turn
                // matches the positive cache or its own reviewer is auto-review.
                ("PermissionRequest", v1::AgentLifecycleState::Blocked),
                ("UserPromptSubmit", v1::AgentLifecycleState::Working),
                ("PreToolUse", pre_tool_lifecycle),
                ("PostToolUse", v1::AgentLifecycleState::Working),
                ("SubagentStart", v1::AgentLifecycleState::Working),
                // A subagent finishing says the parent is still mid-turn.
                ("SubagentStop", v1::AgentLifecycleState::Working),
                ("Stop", v1::AgentLifecycleState::Idle),
                ("Interrupt", v1::AgentLifecycleState::Idle),
                ("SessionEnd", v1::AgentLifecycleState::Idle),
                ("SessionStart", v1::AgentLifecycleState::Idle),
            ],
        )
    }
}

pub(super) struct ClaudeCodeAdapter;

impl AgentAdapter for ClaudeCodeAdapter {
    fn kind(&self) -> &'static str {
        "claude-code"
    }

    fn legacy_kind(&self) -> v1::AgentAdapterKind {
        v1::AgentAdapterKind::ClaudeCode
    }

    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn executable(&self) -> &'static str {
        "claude"
    }

    fn hook_relative_path(&self) -> &'static str {
        ".claude/settings.json"
    }

    /// Measured against a real `~/.claude/settings.json`: `StopFailure` and
    /// `SubagentStop` are live events this app previously ignored, and a turn
    /// that ended in failure therefore left the agent showing "working"
    /// forever, since the `Stop` that would have ended it never fires.
    fn hook_events(&self) -> &'static [&'static str] {
        &[
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PermissionRequest",
            "PostToolUse",
            "SubagentStop",
            "Stop",
            "StopFailure",
            "Notification",
        ]
    }

    fn hook_trust_guidance(&self) -> &'static str {
        "Review the listed Claude Code hook events and exact command before confirming installation."
    }

    fn identifies_process(&self, command: &str) -> bool {
        matches!(command_basename(command), "claude" | "claude-code")
    }

    fn launch(&self, native_session_id: Option<&str>) -> LaunchSpec {
        LaunchSpec {
            program: self.executable(),
            arguments: native_session_id
                .map(|id| vec!["--resume".to_owned(), id.to_owned()])
                .unwrap_or_default(),
        }
    }

    fn parse_hook(&self, payload: &Value) -> Result<ParsedHook, &'static str> {
        let event = hook_event_name(payload)?;
        if event == "Notification" {
            let notification = payload
                .get("notification_type")
                .or_else(|| payload.get("notificationType"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let lifecycle = if matches!(
                notification,
                "permission_prompt" | "idle_prompt" | "elicitation_dialog"
            ) {
                v1::AgentLifecycleState::Blocked
            } else {
                v1::AgentLifecycleState::Unknown
            };
            return Ok(parsed(payload, lifecycle));
        }
        let stop_lifecycle = if payload
            .get(CLAUDE_HAS_RUNNING_SUBAGENT_FIELD)
            .and_then(Value::as_bool)
            == Some(true)
        {
            v1::AgentLifecycleState::Working
        } else {
            v1::AgentLifecycleState::Idle
        };
        parse_common_hook(
            payload,
            &[
                ("PermissionRequest", v1::AgentLifecycleState::Blocked),
                ("UserPromptSubmit", v1::AgentLifecycleState::Working),
                ("PreToolUse", v1::AgentLifecycleState::Working),
                ("PostToolUse", v1::AgentLifecycleState::Working),
                ("SubagentStop", v1::AgentLifecycleState::Working),
                // Claude's parent emits `Stop` while background subagents are
                // still running. The hook CLI reduces the raw task list to one
                // privacy-safe boolean. Only this kind of background work
                // extends the user's turn; commands and session crons do not.
                ("Stop", stop_lifecycle),
                // A failed turn is over. It is the case most worth surfacing
                // and the one that used to leave the row working forever.
                ("StopFailure", v1::AgentLifecycleState::Idle),
                ("SessionStart", v1::AgentLifecycleState::Idle),
            ],
        )
    }
}

pub(crate) fn adapter(kind: v1::AgentAdapterKind) -> Option<&'static dyn AgentAdapter> {
    static CODEX: CodexAdapter = CodexAdapter;
    static CLAUDE: ClaudeCodeAdapter = ClaudeCodeAdapter;
    [&CODEX as &dyn AgentAdapter, &CLAUDE as &dyn AgentAdapter]
        .into_iter()
        .find(|adapter| adapter.legacy_kind() == kind)
}

pub(crate) fn all() -> impl Iterator<Item = &'static dyn AgentAdapter> {
    [
        v1::AgentAdapterKind::Codex,
        v1::AgentAdapterKind::ClaudeCode,
    ]
    .into_iter()
    .filter_map(adapter)
}

/// Each adapter renders its own observation, as
/// [`super::hooks::HookManager::wiring`] already paired them.
pub(crate) fn descriptors(
    home: &Path,
    wiring: &[super::hooks::ObservedAdapter],
) -> Vec<v1::AgentAdapterDescriptor> {
    wiring
        .iter()
        .map(|(adapter, observed)| adapter.descriptor(home, observed))
        .collect()
}

pub(crate) fn by_id(id: &str) -> Option<&'static dyn AgentAdapter> {
    all().find(|adapter| adapter.kind() == id)
}

pub(super) fn detect(command: &str) -> Option<&'static dyn AgentAdapter> {
    all().find(|candidate| candidate.identifies_process(command))
}

pub(super) fn detect_argv(argv: &[String]) -> Option<&'static dyn AgentAdapter> {
    let executable = argv.first()?;
    if let Some(adapter) = detect(executable) {
        return Some(adapter);
    }
    if !matches!(
        command_basename(executable),
        "node" | "nodejs" | "bun" | "npx"
    ) {
        return None;
    }
    let launched = argv.get(1)?;
    all().find(|candidate| {
        candidate.identifies_process(launched)
            || (candidate.id() == "claude-code" && launched.ends_with("/claude-code/cli.js"))
    })
}

fn command_basename(command: &str) -> &str {
    command
        .split_ascii_whitespace()
        .next()
        .unwrap_or_default()
        .rsplit('/')
        .next()
        .unwrap_or_default()
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn parse_common_hook(
    payload: &Value,
    mappings: &[(&str, v1::AgentLifecycleState)],
) -> Result<ParsedHook, &'static str> {
    let event = hook_event_name(payload)?;
    let lifecycle = mappings
        .iter()
        .find_map(|(candidate, lifecycle)| (*candidate == event).then_some(*lifecycle))
        .unwrap_or(v1::AgentLifecycleState::Unknown);
    Ok(parsed(payload, lifecycle))
}

fn parsed(payload: &Value, lifecycle: v1::AgentLifecycleState) -> ParsedHook {
    ParsedHook {
        native_session_id: payload
            .get("session_id")
            .or_else(|| payload.get("sessionId"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        lifecycle,
        authority_millis: HOOK_AUTHORITY_MILLIS,
        event_name: hook_event_name(payload).unwrap_or_default().to_owned(),
    }
}

fn hook_event_name(payload: &Value) -> Result<&str, &'static str> {
    let object = payload
        .as_object()
        .ok_or("hook payload must be a JSON object")?;
    object
        .get("hook_event_name")
        .or_else(|| object.get("hookEventName"))
        .or_else(|| object.get("event"))
        .and_then(Value::as_str)
        .filter(|event| !event.is_empty())
        .ok_or("hook payload omitted its event name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_detection_uses_exact_executable_basename() {
        assert_eq!(
            detect("/usr/local/bin/codex").unwrap().display_name(),
            "Codex"
        );
        assert_eq!(
            detect("claude --resume abc").unwrap().display_name(),
            "Claude Code"
        );
        assert!(detect("my-codex-wrapper").is_none());
        assert!(detect_argv(&["vim".into(), "codex".into()]).is_none());
        assert!(detect_argv(&["bash".into(), "/usr/bin/codex".into()]).is_none());
    }

    #[test]
    fn adapters_normalize_blocked_and_completion_events() {
        let codex = adapter(v1::AgentAdapterKind::Codex).unwrap();
        let blocked = codex
            .parse_hook(
                &serde_json::from_slice(include_bytes!(
                    "../../../../../tests/integration/agents/fixtures/codex-permission.json"
                ))
                .unwrap(),
            )
            .unwrap();
        assert_eq!(blocked.lifecycle, v1::AgentLifecycleState::Blocked);
        for event_name in ["Interrupt", "SessionEnd"] {
            assert_eq!(
                codex
                    .parse_hook(&serde_json::json!({"hook_event_name": event_name}))
                    .unwrap()
                    .lifecycle,
                v1::AgentLifecycleState::Idle
            );
        }
        let claude = adapter(v1::AgentAdapterKind::ClaudeCode).unwrap();
        let idle = claude
            .parse_hook(
                &serde_json::from_slice(include_bytes!(
                    "../../../../../tests/integration/agents/fixtures/claude-stop.json"
                ))
                .unwrap(),
            )
            .unwrap();
        assert_eq!(idle.lifecycle, v1::AgentLifecycleState::Idle);
        assert_eq!(idle.native_session_id, "claude-session-3");
    }

    #[test]
    fn codex_permission_requires_ingest_to_match_the_turn_cache() {
        let codex = adapter(v1::AgentAdapterKind::Codex).unwrap();
        for reviewer in [Some("auto_review"), Some("user"), None] {
            let mut payload = serde_json::json!({
                "hook_event_name": "PermissionRequest",
                "session_id": "codex-session",
            });
            payload[CODEX_APPROVAL_TURN_ID_FIELD] = "turn-1".into();
            if let Some(reviewer) = reviewer {
                payload[CODEX_APPROVAL_REVIEWER_FIELD] = reviewer.into();
            }
            assert_eq!(
                codex.parse_hook(&payload).unwrap().lifecycle,
                v1::AgentLifecycleState::Blocked
            );
        }
    }

    #[test]
    fn codex_question_is_the_only_pre_tool_event_that_blocks() {
        let codex = adapter(v1::AgentAdapterKind::Codex).unwrap();
        for (tool_name, expected) in [
            (
                Some(serde_json::json!("request_user_input")),
                v1::AgentLifecycleState::Blocked,
            ),
            (
                Some(serde_json::json!("Bash")),
                v1::AgentLifecycleState::Working,
            ),
            (None, v1::AgentLifecycleState::Working),
            (
                Some(serde_json::json!({})),
                v1::AgentLifecycleState::Working,
            ),
        ] {
            let mut payload = serde_json::json!({"hook_event_name": "PreToolUse"});
            if let Some(tool_name) = tool_name {
                payload["tool_name"] = tool_name;
            }
            assert_eq!(codex.parse_hook(&payload).unwrap().lifecycle, expected);
        }
    }

    #[test]
    fn codex_subagent_events_require_their_stable_id() {
        let codex = adapter(v1::AgentAdapterKind::Codex).unwrap();
        for event_name in ["SubagentStart", "SubagentStop"] {
            assert!(
                codex
                    .parse_hook(&serde_json::json!({"hook_event_name": event_name}))
                    .is_err()
            );
            assert_eq!(
                codex
                    .parse_hook(&serde_json::json!({
                        "hook_event_name": event_name,
                        "agent_id": "agent-1",
                    }))
                    .unwrap()
                    .lifecycle,
                v1::AgentLifecycleState::Working
            );
            assert!(
                codex
                    .parse_hook(&serde_json::json!({
                        "hook_event_name": event_name,
                        "agent_id": "x".repeat(MAX_CODEX_SUBAGENT_ID_BYTES + 1),
                    }))
                    .is_err()
            );
        }
    }

    #[test]
    fn registry_owns_descriptors_manifests_paths_and_commands() {
        let home = Path::new("/fixture/home");
        let observed: Vec<super::super::hooks::ObservedAdapter> = all()
            .zip([v1::AgentHookWiring::Wired, v1::AgentHookWiring::NotWired])
            .map(|(adapter, state)| {
                (
                    adapter,
                    super::super::hooks::AdapterWiring {
                        adapter_id: adapter.id(),
                        config_path: adapter.hook_path(home),
                        state,
                        detail: String::new(),
                    },
                )
            })
            .collect();
        let descriptors = descriptors(home, &observed);
        assert_eq!(descriptors.len(), 2);
        let codex = adapter(v1::AgentAdapterKind::Codex).unwrap();
        assert_eq!(codex.hook_path(home), home.join(".codex/hooks.json"));
        assert_eq!(codex.hook_events().len(), 10);
        assert_eq!(codex.descriptor(home, &observed[0].1).id, "codex");
        assert_eq!(codex.hook_timeout_seconds("Interrupt"), 3);
        assert_eq!(codex.hook_timeout_seconds("SessionEnd"), 3);
        assert_eq!(codex.hook_timeout_seconds("Stop"), 5);
        assert_eq!(
            codex.hook_command(Path::new("/opt/muxflow-host")),
            "'/opt/muxflow-host' hook ingest --adapter codex --managed-owner muxflow --managed-version 6"
        );
        let claude = adapter(v1::AgentAdapterKind::ClaudeCode).unwrap();
        assert!(claude.hook_events().contains(&"Notification"));
        assert!(
            claude
                .hook_events()
                .iter()
                .all(|event| claude.hook_timeout_seconds(event) == 5)
        );
        assert!(claude.descriptor(home, &observed[1].1).supports_resume);
        // Each descriptor carries its own adapter's observation and no other's,
        // and the host is what decides whether an install would act on it.
        assert_eq!(
            descriptors
                .iter()
                .map(|descriptor| (
                    descriptor.id.as_str(),
                    descriptor.hook_wiring,
                    descriptor.hook_setup_recommended
                ))
                .collect::<Vec<_>>(),
            [
                ("codex", v1::AgentHookWiring::Wired as i32, false),
                ("claude-code", v1::AgentHookWiring::NotWired as i32, true),
            ]
        );
    }

    /// Only a running subagent extends the user's turn. Ordinary background
    /// work and session crons still end at `Stop`.
    #[test]
    fn claude_stop_distinguishes_a_running_subagent_from_other_background_work() {
        let claude = adapter(v1::AgentAdapterKind::ClaudeCode).unwrap();
        let running_subagent = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "claude-session",
            CLAUDE_HAS_RUNNING_SUBAGENT_FIELD: true
        });
        assert_eq!(
            claude.parse_hook(&running_subagent).unwrap().lifecycle,
            v1::AgentLifecycleState::Working
        );
        for payload in [
            serde_json::json!({
                "hook_event_name": "Stop",
                "session_id": "claude-session",
                CLAUDE_HAS_RUNNING_SUBAGENT_FIELD: false
            }),
            serde_json::json!({
                "hook_event_name": "Stop",
                "session_id": "claude-session"
            }),
        ] {
            assert_eq!(
                claude.parse_hook(&payload).unwrap().lifecycle,
                v1::AgentLifecycleState::Idle
            );
        }
    }

    #[test]
    fn claude_idle_notification_is_blocked_before_ingest_applies_its_guard() {
        let claude = adapter(v1::AgentAdapterKind::ClaudeCode).unwrap();
        let payload = serde_json::json!({
            "hook_event_name": "Notification",
            "session_id": "claude-session",
            "notification_type": "idle_prompt"
        });
        assert_eq!(
            claude.parse_hook(&payload).unwrap().lifecycle,
            v1::AgentLifecycleState::Blocked
        );
    }
}
