//! The host's agent JSON, read into [`super::model`], and the requests that go
//! back. Port of `api.ts` and `generation.ts`; transport is `desktop-core`'s
//! `agent_request`, reached through the connection backend.

use desktop_core::connection::agent::AgentCommand;
use serde_json::Value;

use super::model::{
    AdapterDescriptor, AgentRecord, AgentSnapshot, AttentionKind, EventScope, Generation,
    HookWiring, Lifecycle, Placement, WireEvent,
};

/// What every agent request is checked against on the host.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RequestScope {
    pub client_id: String,
    pub host_profile_id: String,
    pub server_identity: String,
    pub topology_generation: u64,
    pub connection_epoch: u64,
}

impl RequestScope {
    /// The host a consent answer belongs to, stable while it stays connected.
    /// The topology generation is left out: it moves on every pane change, and
    /// an install racing an unrelated split must not fail for nothing.
    pub fn identity(&self) -> String {
        format!(
            "{}\0{}\0{}",
            self.host_profile_id, self.client_id, self.connection_epoch
        )
    }

    /// Any change asks the host for a fresh snapshot.
    pub fn request_key(&self) -> String {
        format!(
            "{}\0{}\0{}\0{}\0{}",
            self.host_profile_id,
            self.client_id,
            self.connection_epoch,
            self.server_identity,
            self.topology_generation
        )
    }

    pub fn event_scope(&self) -> EventScope {
        EventScope {
            host_profile_id: self.host_profile_id.clone(),
            server_identity: self.server_identity.clone(),
            connection_epoch: self.connection_epoch,
        }
    }

    /// `command` with the fields every request carries.
    pub fn command(&self, operation: &str, command: AgentCommand) -> AgentCommand {
        AgentCommand {
            operation: operation.to_owned(),
            expected_server_identity: self.server_identity.clone(),
            expected_topology_generation: self.topology_generation.to_string(),
            connection_epoch: self.connection_epoch.to_string(),
            ..command
        }
    }
}

/// Canonical unsigned decimal, as a string or a JSON number; the full u64 range.
pub fn generation(value: Option<&Value>, label: &str) -> Result<Generation, String> {
    match value {
        Some(Value::String(text))
            if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) =>
        {
            text.parse()
                .map_err(|_| format!("{label} is outside the u64 range."))
        }
        Some(Value::Number(number)) => number
            .as_u64()
            .ok_or_else(|| format!("{label} must be an unsigned decimal string.")),
        _ => Err(format!("{label} must be an unsigned decimal string.")),
    }
}

fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn strings(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The host's epoch, as a string or a number, matches this bridge's.
fn same_epoch(scope_epoch: u64, value: Option<&Value>) -> bool {
    match value {
        Some(Value::String(text)) => *text == scope_epoch.to_string(),
        Some(Value::Number(number)) => number.as_u64() == Some(scope_epoch),
        _ => false,
    }
}

pub fn map_record(scope: &EventScope, value: &Value) -> Result<AgentRecord, String> {
    let route = value.get("route").filter(|route| route.is_object());
    let agent_id = string(value, "agentId");
    let Some(route) = route.filter(|route| {
        !agent_id.is_empty()
            && string(route, "agentId") == agent_id
            && string(route, "hostProfileId") == scope.host_profile_id
            && string(route, "serverIdentity") == scope.server_identity
    }) else {
        return Err("Host returned a malformed agent route.".into());
    };
    let attention_generation = generation(
        value.get("attentionGeneration"),
        "agent attention generation",
    )?;
    if generation(route.get("attentionGeneration"), "agent route generation")?
        != attention_generation
    {
        return Err("Agent route generation conflicts with its record.".into());
    }
    let adapter_id = string(value, "adapterId");
    if adapter_id.is_empty() {
        return Err("Agent adapter identity is missing.".into());
    }
    let display_name = match string(value, "displayName") {
        name if name.is_empty() => format!("{adapter_id} agent"),
        name => name,
    };
    Ok(AgentRecord {
        id: agent_id,
        native_session_id: string(value, "nativeSessionId"),
        display_name,
        host_profile_id: scope.host_profile_id.clone(),
        server_identity: scope.server_identity.clone(),
        session_id: string(route, "sessionId"),
        session_name: string(route, "sessionNameFallback"),
        window_id: string(route, "windowId"),
        window_name: string(route, "windowNameFallback"),
        pane_id: string(route, "paneId"),
        pane_title: string(route, "paneTitleFallback"),
        // A value this build does not know is `unknown`, the product's word for
        // "nothing here can say", never a thrown snapshot that blanks the list.
        lifecycle: match value.get("lifecycle").and_then(Value::as_str) {
            Some("working") => Lifecycle::Working,
            Some("blocked") => Lifecycle::Blocked,
            Some("idle") => Lifecycle::Idle,
            _ => Lifecycle::Unknown,
        },
        lifecycle_generation: generation(value.get("stateGeneration"), "agent state generation")?,
        attention_generation,
        attention_kind: match value.get("attentionKind").and_then(Value::as_str) {
            Some("blocked") => Some(AttentionKind::Blocked),
            Some("completed") => Some(AttentionKind::Completed),
            _ => None,
        },
        seen_generation: generation(value.get("seenGeneration"), "agent seen generation")?,
        updated_at: generation(value.get("updatedAtUnixMillis"), "agent update time")?,
        lifecycle_changed_at: generation(
            value.get("lifecycleChangedAtUnixMillis"),
            "agent lifecycle change time",
        )?,
        attention_seen_at: generation(
            value.get("attentionSeenAtUnixMillis"),
            "agent attention seen time",
        )?,
        detected_manually: flag(value, "detectedManually"),
        present: flag(value, "present"),
        adapter_id,
    })
}

fn map_adapter(value: &Value) -> Result<AdapterDescriptor, String> {
    let id = string(value, "id");
    if id.is_empty() {
        return Err("Agent adapter identity is missing.".into());
    }
    Ok(AdapterDescriptor {
        display_name: match string(value, "displayName") {
            name if name.is_empty() => id.clone(),
            name => name,
        },
        supports_launch: flag(value, "supportsLaunch"),
        supports_resume: flag(value, "supportsResume"),
        supports_hooks: flag(value, "supportsHooks"),
        supports_process_detection: flag(value, "supportsProcessDetection"),
        hook_config_path: string(value, "hookConfigPath"),
        hook_events: strings(value, "hookEvents"),
        // A wiring state this build does not know has told us nothing; guessing
        // "not wired" would prompt to install over a configuration that may be
        // correct already.
        hook_wiring: match value.get("hookWiring").and_then(Value::as_str) {
            Some("wired") => HookWiring::Wired,
            Some("partial") => HookWiring::Partial,
            Some("notWired") => HookWiring::NotWired,
            Some("absent") => HookWiring::Absent,
            Some("unavailable") => HookWiring::Unavailable,
            _ => HookWiring::Unspecified,
        },
        hook_wiring_detail: string(value, "hookWiringDetail"),
        hook_setup_recommended: flag(value, "hookSetupRecommended"),
        id,
    })
}

pub fn map_snapshot(scope: &EventScope, value: &Value) -> Result<AgentSnapshot, String> {
    let agents = value.get("agents").and_then(Value::as_array);
    let (true, Some(agents)) = (flag(value, "authoritative"), agents) else {
        return Err("Host returned a non-authoritative agent snapshot.".into());
    };
    if !same_epoch(scope.connection_epoch, value.get("connectionEpoch")) {
        return Err("Host returned an agent snapshot for a replaced connection epoch.".into());
    }
    let revision = generation(value.get("generation"), "agent snapshot generation")?;
    Ok(AgentSnapshot {
        host_profile_id: scope.host_profile_id.clone(),
        server_identity: scope.server_identity.clone(),
        connection_epoch: scope.connection_epoch,
        revision,
        accepted_generation: generation(
            value.get("acceptedGeneration"),
            "agent snapshot accepted generation",
        )?,
        notification_watermark: match value.get("notificationWatermark") {
            Some(watermark) => generation(Some(watermark), "agent notification watermark")?,
            None => revision,
        },
        agents: agents
            .iter()
            .map(|agent| map_record(scope, agent))
            .collect::<Result<_, _>>()?,
        adapters: value
            .get("adapters")
            .and_then(Value::as_array)
            .map(|adapters| adapters.iter().map(map_adapter).collect::<Result<_, _>>())
            .transpose()?
            .unwrap_or_default(),
    })
}

/// A pushed snapshot or event from the bridge. `None` is a payload for a
/// replaced bridge epoch, or an event with nothing in it.
pub fn map_pushed(
    scope: &EventScope,
    label: &str,
    payload: &Value,
) -> Result<Option<WireEvent>, String> {
    if !same_epoch(scope.connection_epoch, payload.get("connectionEpoch")) {
        return Ok(None);
    }
    if label == "snapshot" {
        return Ok(Some(WireEvent::Snapshot {
            snapshot: map_snapshot(scope, payload)?,
            replayed: true,
        }));
    }
    let sequence = generation(payload.get("generation"), "agent event generation")?;
    let retired = strings(payload, "retiredAgentIds");
    let replayed = payload.get("notify").and_then(Value::as_bool) == Some(false);
    // An event may carry retirements and no record: that is how the host says
    // an agent's process is gone.
    match payload.get("agent").filter(|agent| !agent.is_null()) {
        None if retired.is_empty() => Ok(None),
        None => Ok(Some(WireEvent::Retired {
            scope: scope.clone(),
            sequence,
            retired,
            replayed,
        })),
        Some(agent) => Ok(Some(WireEvent::Upsert {
            scope: scope.clone(),
            sequence,
            record: Box::new(map_record(scope, agent)?),
            retired,
            replayed,
        })),
    }
}

/// The host's answer to `markSeen`: the record and the sequence it spent.
pub fn map_seen(scope: &EventScope, response: &Value) -> Result<(AgentRecord, Generation), String> {
    match (response.get("agent"), response.get("acceptedGeneration")) {
        (Some(agent), Some(accepted)) if !agent.is_null() => Ok((
            map_record(scope, agent)?,
            generation(Some(accepted), "agent response accepted generation")?,
        )),
        _ => Err("Host omitted the accepted seen mutation.".into()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookAction {
    Install,
    Uninstall,
}

impl HookAction {
    pub fn word(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Uninstall => "uninstall",
        }
    }
}

/// One proposed change to an agent's configuration, as the host computed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookChange {
    pub path: String,
    pub summary: String,
    pub owner: String,
    pub command: String,
    pub events: Vec<String>,
    pub creates_config: bool,
    pub removes_config: bool,
    pub diff_preview: String,
    /// Whole-file changes send the two sides instead of a diff.
    pub before_preview: String,
    pub after_preview: String,
    pub before_hash: String,
    pub after_hash: String,
    pub preview_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookReview {
    pub adapter_id: String,
    pub adapter_display_name: String,
    pub action: HookAction,
    /// The host-issued confirmation token.
    pub revision: String,
    pub already_installed: bool,
    pub changes: Vec<HookChange>,
    pub managed_label: String,
    pub trust_guidance: Option<String>,
    pub backup_path: Option<String>,
}

pub fn map_hook_review(response: &Value, action: HookAction) -> Result<HookReview, String> {
    let plan = response.get("hookPlan").filter(|plan| plan.is_object());
    let Some(plan) = plan.filter(|plan| {
        !string(plan, "confirmationToken").is_empty() && !string(plan, "configPath").is_empty()
    }) else {
        return Err("Host omitted the reviewable hook plan.".into());
    };
    let adapter_id = string(plan, "adapterId");
    if adapter_id.is_empty() {
        return Err("Agent adapter identity is missing.".into());
    }
    let optional = |key| Some(string(plan, key)).filter(|text| !text.is_empty());
    Ok(HookReview {
        adapter_display_name: adapter_id.clone(),
        adapter_id,
        action,
        revision: string(plan, "confirmationToken"),
        already_installed: flag(plan, "alreadyCurrent"),
        changes: vec![HookChange {
            path: string(plan, "configPath"),
            summary: string(plan, "summary"),
            owner: string(plan, "ownershipMarker"),
            command: string(plan, "proposedCommand"),
            events: strings(plan, "proposedEvents"),
            creates_config: flag(plan, "createsConfig"),
            removes_config: flag(plan, "removesConfig"),
            diff_preview: string(plan, "diffPreview"),
            before_preview: string(plan, "beforePreview"),
            after_preview: string(plan, "afterPreview"),
            before_hash: string(plan, "beforeHash"),
            after_hash: string(plan, "afterHash"),
            preview_truncated: flag(plan, "previewTruncated"),
        }],
        managed_label: string(plan, "managedVersion"),
        trust_guidance: optional("trustGuidance"),
        backup_path: optional("backupPath"),
    })
}

/// What the host did with the recommended tmux window naming. Anything else
/// is `unavailable`, this side's answer to a value the host did not give.
pub fn map_host_naming(response: &Value) -> &'static str {
    const KNOWN: [&str; 6] = [
        "applied",
        "alreadyCurrent",
        "userConfigured",
        "removed",
        "nothingToRemove",
        "noServer",
    ];
    let given = response.get("hostNaming").and_then(Value::as_str);
    KNOWN
        .into_iter()
        .find(|known| Some(*known) == given)
        .unwrap_or("unavailable")
}

/// Where a launch or resume opens, in the active pane's context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchRequest {
    pub adapter_id: String,
    pub placement: Placement,
    pub session_id: String,
    pub window_id: String,
    pub pane_id: String,
    pub active_root: String,
    pub root_token: String,
}

pub fn launch_command(scope: &RequestScope, request: &LaunchRequest) -> AgentCommand {
    let operation = match request.placement {
        Placement::Window => "launchWindow",
        Placement::Split => "launchSplit",
    };
    scope.command(
        operation,
        AgentCommand {
            adapter_id: request.adapter_id.clone(),
            session_id: request.session_id.clone(),
            window_id: request.window_id.clone(),
            pane_id: request.pane_id.clone(),
            active_root: request.active_root.clone(),
            root_token: request.root_token.clone(),
            ..AgentCommand::default()
        },
    )
}

pub fn resume_command(
    scope: &RequestScope,
    agent: &AgentRecord,
    request: &LaunchRequest,
) -> AgentCommand {
    scope.command(
        "resume",
        AgentCommand {
            agent_id: agent.id.clone(),
            native_session_id: agent.native_session_id.clone(),
            adapter_id: request.adapter_id.clone(),
            placement: request.placement.word().to_owned(),
            session_id: request.session_id.clone(),
            window_id: request.window_id.clone(),
            pane_id: match request.placement {
                Placement::Split => request.pane_id.clone(),
                Placement::Window => String::new(),
            },
            active_root: request.active_root.clone(),
            root_token: request.root_token.clone(),
            ..AgentCommand::default()
        },
    )
}

/// A branch opens beside its agent, with no root: the host works it out.
pub fn branch_command(scope: &RequestScope, agent: &AgentRecord) -> AgentCommand {
    scope.command(
        "branch",
        AgentCommand {
            agent_id: agent.id.clone(),
            native_session_id: agent.native_session_id.clone(),
            adapter_id: agent.adapter_id.clone(),
            session_id: agent.session_id.clone(),
            pane_id: agent.pane_id.clone(),
            ..AgentCommand::default()
        },
    )
}

/// Where a branch opened, or why the reply cannot say.
pub fn map_branch(response: &Value) -> Result<(String, String), String> {
    match (string(response, "sessionId"), string(response, "paneId")) {
        (session, pane) if !session.is_empty() && !pane.is_empty() => Ok((session, pane)),
        _ => Err("The host did not say where the branch opened.".into()),
    }
}

pub fn mark_seen_command(
    scope: &RequestScope,
    agent_id: &str,
    attention: Generation,
) -> AgentCommand {
    scope.command(
        "markSeen",
        AgentCommand {
            agent_id: agent_id.to_owned(),
            attention_generation: attention.to_string(),
            ..AgentCommand::default()
        },
    )
}

pub fn hook_review_command(
    scope: &RequestScope,
    adapter_id: &str,
    action: HookAction,
) -> AgentCommand {
    scope.command(
        "hookReview",
        AgentCommand {
            adapter_id: adapter_id.to_owned(),
            hook_management_target: action.word().to_owned(),
            ..AgentCommand::default()
        },
    )
}

pub fn hook_apply_command(scope: &RequestScope, review: &HookReview) -> AgentCommand {
    let operation = match review.action {
        HookAction::Install => "hookInstall",
        HookAction::Uninstall => "hookUninstall",
    };
    scope.command(
        operation,
        AgentCommand {
            adapter_id: review.adapter_id.clone(),
            confirmed: true,
            confirmation_token: review.revision.clone(),
            ..AgentCommand::default()
        },
    )
}

pub fn host_naming_command(scope: &RequestScope, action: HookAction) -> AgentCommand {
    let operation = match action {
        HookAction::Install => "hostNaming",
        HookAction::Uninstall => "hostNamingRemove",
    };
    scope.command(operation, AgentCommand::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::model::fixtures::agent;
    use serde_json::json;

    fn scope() -> RequestScope {
        RequestScope {
            client_id: "client".into(),
            host_profile_id: "local".into(),
            server_identity: "server-a".into(),
            topology_generation: 9,
            connection_epoch: 4,
        }
    }

    fn wire_record() -> Value {
        json!({
            "agentId": "agent-1", "adapterId": "codex", "nativeSessionId": "native",
            "displayName": "Review bot",
            "route": {
                "hostProfileId": "local", "serverIdentity": "server-a", "sessionId": "$1",
                "sessionNameFallback": "work", "windowId": "@1", "windowNameFallback": "agent",
                "paneId": "%1", "paneTitleFallback": "✳ Plan rollout", "agentId": "agent-1",
                "attentionGeneration": "4"
            },
            "lifecycle": "blocked", "stateGeneration": "5", "attentionGeneration": "4",
            "attentionKind": "blocked", "seenGeneration": "2",
            "updatedAtUnixMillis": "100", "lifecycleChangedAtUnixMillis": "90",
            "attentionSeenAtUnixMillis": "80", "detectedManually": true, "present": true
        })
    }

    fn snapshot_json(agents: Vec<Value>, adapters: Value) -> Value {
        json!({
            "generation": "6", "acceptedGeneration": "6", "agents": agents,
            "authoritative": true, "notificationWatermark": "4", "connectionEpoch": "4",
            "adapters": adapters
        })
    }

    #[test]
    fn maps_the_json_snapshot_into_the_adapter_neutral_record() {
        let snapshot = map_snapshot(
            &scope().event_scope(),
            &snapshot_json(
                vec![wire_record()],
                json!([{ "id": "future", "displayName": "Future", "supportsLaunch": true,
                    "supportsResume": true, "supportsHooks": false,
                    "supportsProcessDetection": true, "hookConfigPath": "", "hookEvents": [] }]),
            ),
        )
        .unwrap();
        assert_eq!(
            (
                snapshot.revision,
                snapshot.accepted_generation,
                snapshot.notification_watermark
            ),
            (6, 6, 4)
        );
        let adapter = &snapshot.adapters[0];
        assert_eq!(adapter.display_name, "Future");
        assert_eq!(adapter.placements(), [Placement::Window, Placement::Split]);
        assert!(adapter.supports_process_detection);
        assert_eq!(adapter.hook_wiring, HookWiring::Unspecified);
        let record = &snapshot.agents[0];
        assert_eq!(record.adapter_id, "codex");
        assert_eq!(record.lifecycle, Lifecycle::Blocked);
        assert_eq!(record.attention_generation, 4);
        assert_eq!(record.attention_kind, Some(AttentionKind::Blocked));
        assert_eq!(
            (record.lifecycle_changed_at, record.attention_seen_at),
            (90, 80)
        );
        assert_eq!(record.pane_title, "✳ Plan rollout");
        assert!(record.detected_manually);
    }

    #[test]
    fn stamps_every_command_with_the_scope_it_was_asked_in() {
        let command = scope().command("snapshot", AgentCommand::default());
        assert_eq!(command.operation, "snapshot");
        assert_eq!(command.expected_server_identity, "server-a");
        assert_eq!(command.expected_topology_generation, "9");
        assert_eq!(command.connection_epoch, "4");
    }

    #[test]
    fn degrades_an_unrecognised_lifecycle_to_unknown_instead_of_blanking_the_list() {
        let mut strange = wire_record();
        strange["agentId"] = json!("agent-2");
        strange["route"]["agentId"] = json!("agent-2");
        strange["lifecycle"] = json!("deliberating");
        strange["attentionKind"] = json!("pondering");
        let snapshot = map_snapshot(
            &scope().event_scope(),
            &snapshot_json(vec![wire_record(), strange], json!([])),
        )
        .unwrap();
        assert_eq!(snapshot.agents.len(), 2);
        assert_eq!(snapshot.agents[0].lifecycle, Lifecycle::Blocked);
        assert_eq!(snapshot.agents[1].lifecycle, Lifecycle::Unknown);
        assert_eq!(snapshot.agents[1].attention_kind, None);
    }

    #[test]
    fn passes_an_opaque_adapter_id_through_launch_and_binds_seen_to_an_exact_generation() {
        let request = LaunchRequest {
            adapter_id: "future-agent".into(),
            placement: Placement::Split,
            session_id: "$1".into(),
            window_id: "@1".into(),
            pane_id: "%1".into(),
            active_root: "/repo".into(),
            root_token: "root-token".into(),
        };
        let launch = launch_command(&scope(), &request);
        assert_eq!(
            (
                launch.operation.as_str(),
                launch.adapter_id.as_str(),
                launch.active_root.as_str(),
                launch.root_token.as_str()
            ),
            ("launchSplit", "future-agent", "/repo", "root-token")
        );
        let seen = mark_seen_command(&scope(), "agent-1", 4);
        assert_eq!(
            (
                seen.operation.as_str(),
                seen.agent_id.as_str(),
                seen.attention_generation.as_str()
            ),
            ("markSeen", "agent-1", "4")
        );
        let mut accepted = wire_record();
        accepted["seenGeneration"] = json!("4");
        accepted["attentionSeenAtUnixMillis"] = json!("120");
        let (record, sequence) = map_seen(
            &scope().event_scope(),
            &json!({ "agent": accepted, "acceptedGeneration": "7" }),
        )
        .unwrap();
        assert_eq!(
            (record.seen_generation, record.attention_seen_at, sequence),
            (4, 120, 7)
        );
        assert!(map_seen(&scope().event_scope(), &json!({})).is_err());
    }

    #[test]
    fn sends_resume_placement_with_the_complete_active_root_context() {
        let request = LaunchRequest {
            adapter_id: "codex".into(),
            placement: Placement::Window,
            session_id: "$1".into(),
            window_id: "@1".into(),
            pane_id: "%1".into(),
            active_root: "/repo".into(),
            root_token: "root-token".into(),
        };
        let command = resume_command(
            &scope(),
            &AgentRecord {
                native_session_id: "native-1".into(),
                ..agent()
            },
            &request,
        );
        assert_eq!(command.operation, "resume");
        assert_eq!(
            (
                command.agent_id.as_str(),
                command.native_session_id.as_str()
            ),
            ("agent-1", "native-1")
        );
        assert_eq!(
            (
                command.placement.as_str(),
                command.session_id.as_str(),
                command.window_id.as_str()
            ),
            ("window", "$1", "@1")
        );
        assert_eq!(command.pane_id, "");
        assert_eq!(
            (command.active_root.as_str(), command.root_token.as_str()),
            ("/repo", "root-token")
        );
    }

    #[test]
    fn sends_a_branch_the_agents_own_workspace_and_pane_and_no_root() {
        let record = AgentRecord {
            session_id: "$2".into(),
            pane_id: "%4".into(),
            ..agent()
        };
        let command = branch_command(&scope(), &record);
        assert_eq!(command.operation, "branch");
        assert_eq!(
            (
                command.agent_id.as_str(),
                command.native_session_id.as_str(),
                command.adapter_id.as_str()
            ),
            ("agent-1", "native-1", "codex")
        );
        assert_eq!(
            (command.session_id.as_str(), command.pane_id.as_str()),
            ("$2", "%4")
        );
        assert_eq!(command.active_root, "");
        assert_eq!(
            map_branch(&json!({ "sessionId": "$7", "windowId": "@8", "paneId": "%9" })),
            Ok(("$7".into(), "%9".into()))
        );
        assert!(
            map_branch(&json!({}))
                .unwrap_err()
                .contains("did not say where")
        );
    }

    #[test]
    fn requires_a_host_issued_confirmation_token_before_hook_install_or_uninstall() {
        let response = json!({ "hookPlan": {
            "adapterId": "claude-code", "action": "review", "configPath": "/config",
            "backupPath": "/backup", "managedVersion": "muxflow/v1", "summary": "one managed hook",
            "confirmationToken": "confirm", "alreadyCurrent": false, "ownershipMarker": "Claude adapter",
            "proposedCommand": "/usr/bin/host hook ingest", "proposedEvents": ["Stop"],
            "trustGuidance": "Review trust", "beforeHash": "old", "afterHash": "new",
            "createsConfig": false, "removesConfig": true,
            "diffPreview": "--- before\n-redacted\n+++ after\n+managed", "previewTruncated": false,
            "beforePreview": "{\"hooks\":{}}", "afterPreview": ""
        }});
        let review = map_hook_review(&response, HookAction::Uninstall).unwrap();
        assert_eq!(
            (review.action, review.revision.as_str()),
            (HookAction::Uninstall, "confirm")
        );
        assert_eq!(review.trust_guidance.as_deref(), Some("Review trust"));
        let change = &review.changes[0];
        assert_eq!(
            (
                change.path.as_str(),
                change.owner.as_str(),
                change.command.as_str()
            ),
            ("/config", "Claude adapter", "/usr/bin/host hook ingest")
        );
        assert_eq!(change.events, ["Stop"]);
        assert!(change.removes_config);
        assert!(change.diff_preview.contains("--- before"));
        assert_eq!(
            (
                change.before_hash.as_str(),
                change.after_hash.as_str(),
                change.before_preview.as_str(),
                change.after_preview.as_str()
            ),
            ("old", "new", "{\"hooks\":{}}", "")
        );
        let apply = hook_apply_command(&scope(), &review);
        assert_eq!(
            (
                apply.operation.as_str(),
                apply.confirmed,
                apply.confirmation_token.as_str()
            ),
            ("hookUninstall", true, "confirm")
        );
        assert!(
            map_hook_review(
                &json!({ "hookPlan": { "configPath": "/config" } }),
                HookAction::Install
            )
            .is_err()
        );
    }

    #[test]
    fn publishes_manual_to_native_retirements_in_the_same_ordered_upsert() {
        let event = map_pushed(
            &scope().event_scope(),
            "agent",
            &json!({ "agent": wire_record(), "generation": "18446744073709551615",
                "connectionEpoch": "4", "retiredAgentIds": ["manual-1"] }),
        )
        .unwrap();
        let Some(WireEvent::Upsert {
            sequence,
            retired,
            replayed,
            ..
        }) = event
        else {
            panic!("expected an upsert");
        };
        assert_eq!(
            (sequence, retired, replayed),
            (u64::MAX, vec!["manual-1".to_owned()], false)
        );
    }

    #[test]
    fn reads_a_retirement_with_no_record_and_a_replay_flag() {
        let event = map_pushed(
            &scope().event_scope(),
            "agent",
            &json!({ "generation": "8", "connectionEpoch": "4", "retiredAgentIds": ["gone"], "notify": false }),
        )
        .unwrap();
        assert!(matches!(
            event,
            Some(WireEvent::Retired {
                sequence: 8,
                replayed: true,
                ..
            })
        ));
        let empty = map_pushed(
            &scope().event_scope(),
            "agent",
            &json!({ "generation": "8", "connectionEpoch": "4" }),
        );
        assert_eq!(empty, Ok(None));
    }

    #[test]
    fn drops_agent_service_payloads_from_a_replaced_bridge_epoch() {
        let event = map_pushed(
            &scope().event_scope(),
            "agent",
            &json!({ "agent": wire_record(), "generation": "7", "connectionEpoch": "3" }),
        );
        assert_eq!(event, Ok(None));
    }

    #[test]
    fn retains_an_unmapped_wire_record_with_empty_canonical_route_ids() {
        let mut unmapped = wire_record();
        for key in [
            "sessionId",
            "sessionNameFallback",
            "windowId",
            "windowNameFallback",
            "paneId",
        ] {
            unmapped["route"][key] = json!("");
        }
        let snapshot = map_snapshot(
            &scope().event_scope(),
            &snapshot_json(vec![unmapped], json!([])),
        )
        .unwrap();
        let record = &snapshot.agents[0];
        assert_eq!(
            (
                record.pane_id.as_str(),
                record.session_id.as_str(),
                record.window_id.as_str()
            ),
            ("", "", "")
        );
    }

    #[test]
    fn reads_generations_losslessly_and_refuses_anything_else() {
        assert_eq!(
            generation(Some(&json!("18446744073709551615")), "g"),
            Ok(u64::MAX)
        );
        assert_eq!(generation(Some(&json!("0007")), "g"), Ok(7));
        assert_eq!(generation(Some(&json!(12)), "g"), Ok(12));
        for bad in [
            json!("18446744073709551616"),
            json!("-1"),
            json!(""),
            json!("1e3"),
            json!(-1),
            json!(null),
        ] {
            assert!(generation(Some(&bad), "g").is_err(), "{bad}");
        }
        assert!(generation(None, "g").is_err());
    }

    #[test]
    fn refuses_a_record_routed_to_another_host_or_server() {
        let mut elsewhere = wire_record();
        elsewhere["route"]["serverIdentity"] = json!("server-b");
        assert!(map_record(&scope().event_scope(), &elsewhere).is_err());
        let mut conflicting = wire_record();
        conflicting["route"]["attentionGeneration"] = json!("5");
        assert!(map_record(&scope().event_scope(), &conflicting).is_err());
    }

    #[test]
    fn reads_only_the_host_naming_outcomes_it_knows() {
        assert_eq!(
            map_host_naming(&json!({ "hostNaming": "applied" })),
            "applied"
        );
        assert_eq!(
            map_host_naming(&json!({ "hostNaming": "whatever" })),
            "unavailable"
        );
        assert_eq!(map_host_naming(&json!({})), "unavailable");
    }
}
