use serde_json::Value;
use std::path::{Path, PathBuf};
use tmux_agent_protocol::v1;

const HOOK_AUTHORITY_MILLIS: i64 = 30_000;
pub(crate) const MANAGED_OWNER: &str = "muxflow";
/// Bumped whenever the managed *event set* changes, not only the command
/// string: an install from an older version covers fewer events, and reporting
/// it as current would leave a transition that can never arrive.
pub(crate) const MANAGED_VERSION: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalEffect {
    None,
    Pending,
    ResolveMatching,
    ResolveAll,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedHook {
    pub native_session_id: String,
    pub lifecycle: v1::AgentLifecycleState,
    pub authority_millis: i64,
    pub event_name: String,
    pub approval_key: String,
    pub approval_effect: ApprovalEffect,
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
            adapter: self.legacy_kind().into(),
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

    /// Measured against a real `~/.codex/hooks.json` and checked against the
    /// Codex CLI 0.148 hook contract: Codex fires `SessionStart`,
    /// `SessionEnd`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`,
    /// `PostToolUse`, `Stop`, `SubagentStart` and `SubagentStop`.
    ///
    /// Two events Claude Code has are absent from that surface and are
    /// therefore gaps rather than omissions: there is no `StopFailure`, so a
    /// turn that ends in failure is indistinguishable from one that succeeds,
    /// and there is no `Notification`, so `PermissionRequest` is the only
    /// evidence of a blocked Codex agent. `SubagentStart` is deliberately not
    /// taken: it says nothing `PreToolUse` has not already said, and every hook
    /// costs a daemon connection.
    fn hook_events(&self) -> &'static [&'static str] {
        &[
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PermissionRequest",
            "PostToolUse",
            "SubagentStop",
            "Stop",
            "SessionEnd",
        ]
    }

    fn hook_trust_guidance(&self) -> &'static str {
        "Codex will review the exact hook hash; approve it in Codex. This installer never edits or bypasses hook trust state."
    }

    fn hook_timeout_seconds(&self, event: &str) -> u64 {
        if event == "SessionEnd" { 3 } else { 5 }
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
        let approval_effect = match event {
            "PermissionRequest" => ApprovalEffect::Pending,
            "PostToolUse" => ApprovalEffect::ResolveMatching,
            "UserPromptSubmit" | "Stop" | "SessionEnd" | "SessionStart" => {
                ApprovalEffect::ResolveAll
            }
            _ => ApprovalEffect::None,
        };
        parse_common_hook(
            payload,
            &[
                ("PermissionRequest", v1::AgentLifecycleState::Blocked),
                ("UserPromptSubmit", v1::AgentLifecycleState::Working),
                ("PreToolUse", v1::AgentLifecycleState::Working),
                ("PostToolUse", v1::AgentLifecycleState::Working),
                // A subagent finishing says the parent is still mid-turn.
                ("SubagentStop", v1::AgentLifecycleState::Working),
                ("Stop", v1::AgentLifecycleState::Idle),
                ("SessionEnd", v1::AgentLifecycleState::Idle),
                ("SessionStart", v1::AgentLifecycleState::Idle),
            ],
            approval_effect,
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
            return Ok(parsed(payload, lifecycle, ApprovalEffect::None));
        }
        parse_common_hook(
            payload,
            &[
                ("PermissionRequest", v1::AgentLifecycleState::Blocked),
                ("UserPromptSubmit", v1::AgentLifecycleState::Working),
                ("PreToolUse", v1::AgentLifecycleState::Working),
                ("PostToolUse", v1::AgentLifecycleState::Working),
                ("SubagentStop", v1::AgentLifecycleState::Working),
                // A `Stop` is a finished turn, unconditionally — including one
                // that leaves background tasks or session crons running behind
                // it. Background work is not foreground attention, and reading
                // it as `Working` left every such pane working forever; worse,
                // it withheld the terminal flag `ingest` sets on an idle Stop,
                // which is exactly what stops Claude Code's routine idle
                // notification (~60s after any idle turn) from reading as
                // Blocked.
                ("Stop", v1::AgentLifecycleState::Idle),
                // A failed turn is over. It is the case most worth surfacing
                // and the one that used to leave the row working forever.
                ("StopFailure", v1::AgentLifecycleState::Idle),
                ("SessionStart", v1::AgentLifecycleState::Idle),
            ],
            ApprovalEffect::None,
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
    approval_effect: ApprovalEffect,
) -> Result<ParsedHook, &'static str> {
    let event = hook_event_name(payload)?;
    let lifecycle = mappings
        .iter()
        .find_map(|(candidate, lifecycle)| (*candidate == event).then_some(*lifecycle))
        .unwrap_or(v1::AgentLifecycleState::Unknown);
    Ok(parsed(payload, lifecycle, approval_effect))
}

fn parsed(
    payload: &Value,
    lifecycle: v1::AgentLifecycleState,
    approval_effect: ApprovalEffect,
) -> ParsedHook {
    let event_name = hook_event_name(payload).unwrap_or_default();
    ParsedHook {
        native_session_id: payload
            .get("session_id")
            .or_else(|| payload.get("sessionId"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        lifecycle,
        authority_millis: HOOK_AUTHORITY_MILLIS,
        event_name: event_name.to_owned(),
        approval_key: payload
            .get("approval_key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        approval_effect,
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
        assert_eq!(codex.hook_events().len(), 8);
        assert_eq!(codex.descriptor(home, &observed[0].1).id, "codex");
        assert_eq!(
            codex.hook_command(Path::new("/opt/muxflow-host")),
            "'/opt/muxflow-host' hook ingest --adapter codex --managed-owner muxflow --managed-version 4"
        );
        let claude = adapter(v1::AgentAdapterKind::ClaudeCode).unwrap();
        assert!(claude.hook_events().contains(&"Notification"));
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

    /// Background work outlives the turn that started it; the turn is still
    /// over. Reading these payloads as `Working` is what pinned a pane to
    /// "working" for the rest of the session.
    #[test]
    fn claude_stop_with_live_background_work_still_ends_the_turn() {
        let claude = adapter(v1::AgentAdapterKind::ClaudeCode).unwrap();
        for field in ["background_tasks", "session_crons"] {
            let payload = serde_json::json!({
                "hook_event_name": "Stop",
                "session_id": "claude-session",
                field: [{"command": "sleep 300", "status": "running"}]
            });
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
