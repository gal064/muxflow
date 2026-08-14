use std::sync::atomic::Ordering;

use serde::Deserialize;
use serde_json::{Value, json};
use tauri::State;
use tmux_agent_protocol::v1;

use super::{TerminalClients, get_client};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCommand {
    pub operation: String,
    #[serde(default)]
    pub adapter: String,
    #[serde(default)]
    pub adapter_id: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub native_session_id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub window_id: String,
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub active_root: String,
    #[serde(default)]
    pub root_token: String,
    #[serde(default)]
    pub expected_server_identity: String,
    #[serde(default)]
    pub expected_topology_generation: String,
    #[serde(default)]
    pub attention_generation: String,
    #[serde(default)]
    pub hook_management_target: String,
    #[serde(default)]
    pub confirmed: bool,
    #[serde(default)]
    pub confirmation_token: String,
    #[serde(default)]
    pub placement: String,
    #[serde(default)]
    pub connection_epoch: String,
}

/// Async so an agent launch or hook review never freezes the WebView's main
/// thread for the length of the host round trip.
#[tauri::command]
pub async fn agent_request(
    client_id: String,
    command: AgentCommand,
    clients: State<'_, TerminalClients>,
) -> Result<Value, String> {
    let (operation, action, hook_management) = operation_from_name(&command.operation)?;
    let client = get_client(&clients, &client_id)?;
    let requested_epoch = parse_u64("connectionEpoch", &command.connection_epoch)?;
    let current_epoch = client.terminal_epoch.load(Ordering::Acquire);
    if requested_epoch == 0 || requested_epoch != current_epoch {
        return Err("agent request belongs to a replaced connection epoch".into());
    }
    let host_profile_id = client.host_profile_id.lock().unwrap().clone();
    let requested_adapter = if command.adapter_id.is_empty() {
        command.adapter.as_str()
    } else {
        command.adapter_id.as_str()
    };
    let adapter_kind = legacy_adapter_kind(requested_adapter);
    let adapter_id = canonical_adapter_id(requested_adapter);
    let request = v1::Request {
        operation: operation.into(),
        agent: Some(v1::AgentRequest {
            action: action.into(),
            adapter: adapter_kind.into(),
            adapter_id,
            agent_id: command.agent_id,
            native_session_id: command.native_session_id,
            display_name: command.display_name,
            session_id: command.session_id,
            window_id: command.window_id,
            pane_id: command.pane_id,
            active_root: command.active_root,
            root_token: command.root_token,
            expected_server_identity: command.expected_server_identity,
            expected_topology_generation: parse_u64(
                "expectedTopologyGeneration",
                &command.expected_topology_generation,
            )?,
            attention_generation: parse_u64("attentionGeneration", &command.attention_generation)?,
            hook_management: hook_management.into(),
            hook_management_target: hook_action_from_name(&command.hook_management_target)?.into(),
            confirmed: command.confirmed,
            confirmation_token: command.confirmation_token,
            placement: placement_from_name(&command.placement)?.into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let response = tauri::async_runtime::spawn_blocking(move || client.request(request))
        .await
        .map_err(|error| format!("agent request task failed: {error}"))??;
    response
        .agent
        .as_ref()
        .map(|response| response_json(response, &host_profile_id, current_epoch))
        .ok_or_else(|| "host omitted agent response".into())
}

fn operation_from_name(
    value: &str,
) -> Result<(v1::Operation, v1::AgentActionKind, v1::HookManagementAction), String> {
    use v1::{AgentActionKind as Action, HookManagementAction as Hook, Operation};
    match value {
        "snapshot" => Ok((
            Operation::AgentSnapshot,
            Action::Unspecified,
            Hook::Unspecified,
        )),
        "launchWindow" => Ok((
            Operation::AgentAction,
            Action::LaunchWindow,
            Hook::Unspecified,
        )),
        "launchSplit" => Ok((
            Operation::AgentAction,
            Action::LaunchSplit,
            Hook::Unspecified,
        )),
        "resume" => Ok((Operation::AgentAction, Action::Resume, Hook::Unspecified)),
        "rename" => Ok((Operation::AgentAction, Action::Rename, Hook::Unspecified)),
        "markSeen" => Ok((
            Operation::AgentMarkSeen,
            Action::Unspecified,
            Hook::Unspecified,
        )),
        "hookReview" => Ok((
            Operation::AgentHookManagement,
            Action::Unspecified,
            Hook::Review,
        )),
        "hookInstall" => Ok((
            Operation::AgentHookManagement,
            Action::Unspecified,
            Hook::Install,
        )),
        "hookUninstall" => Ok((
            Operation::AgentHookManagement,
            Action::Unspecified,
            Hook::Uninstall,
        )),
        "hostNaming" => Ok((
            Operation::AgentHostNaming,
            Action::Unspecified,
            Hook::Unspecified,
        )),
        _ => Err(format!("unsupported agent operation {value}")),
    }
}

/// The protobuf enum is compatibility metadata only. Canonical registry IDs
/// remain opaque so a newer host adapter can flow through an older desktop.
fn legacy_adapter_kind(value: &str) -> v1::AgentAdapterKind {
    match value {
        "codex" => v1::AgentAdapterKind::Codex,
        "claude" | "claudeCode" | "claude-code" => v1::AgentAdapterKind::ClaudeCode,
        _ => v1::AgentAdapterKind::Unspecified,
    }
}

fn canonical_adapter_id(value: &str) -> String {
    match value {
        "claude" | "claudeCode" => "claude-code".into(),
        _ => value.into(),
    }
}

fn hook_action_from_name(value: &str) -> Result<v1::HookManagementAction, String> {
    match value {
        "" | "install" => Ok(v1::HookManagementAction::Install),
        "uninstall" => Ok(v1::HookManagementAction::Uninstall),
        _ => Err(format!("unsupported hook management target {value}")),
    }
}

fn placement_from_name(value: &str) -> Result<v1::AgentPlacementKind, String> {
    match value {
        "" => Ok(v1::AgentPlacementKind::Unspecified),
        "window" => Ok(v1::AgentPlacementKind::Window),
        "split" => Ok(v1::AgentPlacementKind::Split),
        _ => Err(format!("unsupported agent placement {value}")),
    }
}

fn parse_u64(label: &str, value: &str) -> Result<u64, String> {
    if value.is_empty() {
        Ok(0)
    } else {
        value
            .parse()
            .map_err(|_| format!("{label} must be a decimal u64 string"))
    }
}

pub(crate) fn response_json(
    value: &v1::AgentResponse,
    host_profile_id: &str,
    connection_epoch: u64,
) -> Value {
    json!({
        "snapshot": value.snapshot.as_ref().map(|snapshot| with_connection_epoch(snapshot_json(snapshot, host_profile_id), connection_epoch)),
        "agent": value.agent.as_ref().map(|agent| record_json(agent, host_profile_id)),
        "hookPlan": value.hook_plan.as_ref().map(hook_plan_json),
        "sessionId": value.session_id,
        "windowId": value.window_id,
        "paneId": value.pane_id,
        "acceptedGeneration": value.accepted_generation.to_string(),
        "connectionEpoch": connection_epoch.to_string(),
        "hostNaming": value.host_naming,
    })
}

pub(crate) fn event_json(value: &v1::AgentEvent, host_profile_id: &str) -> Value {
    json!({
        "agent": value.agent.as_ref().map(|agent| record_json(agent, host_profile_id)),
        "generation": value.generation.to_string(),
        "notify": value.notify,
        "reason": value.reason,
        "retiredAgentIds": value.retired_agent_ids,
    })
}

pub(crate) fn snapshot_json(value: &v1::AgentSnapshot, host_profile_id: &str) -> Value {
    json!({
        "generation": value.generation.to_string(),
        "agents": value.agents.iter().map(|agent| record_json(agent, host_profile_id)).collect::<Vec<_>>(),
        "authoritative": value.authoritative,
        "notificationWatermark": value.notification_watermark.to_string(),
        "acceptedGeneration": value.accepted_generation.to_string(),
        "adapters": value.adapters.iter().map(adapter_descriptor_json).collect::<Vec<_>>(),
    })
}

pub(crate) fn with_connection_epoch(mut value: Value, epoch: u64) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.insert("connectionEpoch".into(), epoch.to_string().into());
    }
    value
}

fn record_json(value: &v1::AgentRecord, host_profile_id: &str) -> Value {
    let route = value.route.as_ref();
    json!({
        "agentId": value.agent_id,
        "adapter": adapter_name(value.adapter),
        "adapterId": if value.adapter_id.is_empty() { adapter_name(value.adapter) } else { &value.adapter_id },
        "nativeSessionId": value.native_session_id,
        "displayName": value.display_name,
        "route": route.map(|route| json!({
            "hostProfileId": if route.host_profile_id.is_empty() { host_profile_id } else { &route.host_profile_id },
            "serverIdentity": route.server_identity,
            "sessionId": route.session_id,
            "sessionNameFallback": route.session_name_fallback,
            "windowId": route.window_id,
            "windowNameFallback": route.window_name_fallback,
            "paneId": route.pane_id,
            "paneIndexFallback": route.pane_index_fallback,
            "agentId": route.agent_id,
            "attentionGeneration": route.attention_generation.to_string(),
        })),
        "lifecycle": lifecycle_name(value.lifecycle),
        "authority": authority_name(value.authority),
        "stateGeneration": value.state_generation.to_string(),
        "attentionGeneration": value.attention_generation.to_string(),
        "attentionKind": value.attention_kind,
        "seenGeneration": value.seen_generation.to_string(),
        "updatedAtUnixMillis": value.updated_at_unix_millis.to_string(),
        "hookAuthorityExpiresAtUnixMillis": value.hook_authority_expires_at_unix_millis.to_string(),
        "detectedManually": value.detected_manually,
        "present": value.present,
    })
}

fn hook_plan_json(value: &v1::HookManagementPlan) -> Value {
    json!({ "adapter": adapter_name(value.adapter), "action": hook_action_name(value.action),
        "adapterId": if value.adapter_id.is_empty() { adapter_name(value.adapter) } else { &value.adapter_id },
        "configPath": value.config_path, "backupPath": value.backup_path,
        "managedVersion": value.managed_version, "summary": value.summary,
        "confirmationToken": value.confirmation_token, "alreadyCurrent": value.already_current,
        "proposedEvents": value.proposed_events, "proposedCommand": value.proposed_command,
        "ownershipMarker": value.ownership_marker, "trustGuidance": value.trust_guidance,
        "beforeHash": value.before_hash, "afterHash": value.after_hash,
        "createsConfig": value.creates_config, "removesConfig": value.removes_config,
        "beforePreview": value.before_preview, "afterPreview": value.after_preview,
        "diffPreview": value.diff_preview, "previewTruncated": value.preview_truncated })
}

fn adapter_descriptor_json(value: &v1::AgentAdapterDescriptor) -> Value {
    json!({ "adapter": adapter_name(value.adapter), "id": value.id,
        "displayName": value.display_name, "supportsLaunch": value.supports_launch,
        "supportsResume": value.supports_resume, "supportsHooks": value.supports_hooks,
        "supportsProcessDetection": value.supports_process_detection,
        "supportsScreenFallback": value.supports_screen_fallback,
        "hookConfigPath": value.hook_config_path, "hookEvents": value.hook_events,
        "hookWiring": hook_wiring_name(value.hook_wiring),
        "hookWiringDetail": value.hook_wiring_detail })
}

fn hook_wiring_name(value: i32) -> &'static str {
    match v1::AgentHookWiring::try_from(value).unwrap_or_default() {
        v1::AgentHookWiring::Wired => "wired",
        v1::AgentHookWiring::Partial => "partial",
        v1::AgentHookWiring::NotWired => "notWired",
        v1::AgentHookWiring::Absent => "absent",
        v1::AgentHookWiring::Unavailable => "unavailable",
        v1::AgentHookWiring::Unspecified => "unspecified",
    }
}

fn adapter_name(value: i32) -> &'static str {
    match v1::AgentAdapterKind::try_from(value).unwrap_or_default() {
        v1::AgentAdapterKind::Codex => "codex",
        v1::AgentAdapterKind::ClaudeCode => "claudeCode",
        _ => "unspecified",
    }
}

fn lifecycle_name(value: i32) -> &'static str {
    match v1::AgentLifecycleState::try_from(value).unwrap_or_default() {
        v1::AgentLifecycleState::Working => "working",
        v1::AgentLifecycleState::Blocked => "blocked",
        v1::AgentLifecycleState::Idle => "idle",
        v1::AgentLifecycleState::Unknown | v1::AgentLifecycleState::Unspecified => "unknown",
    }
}

fn authority_name(value: i32) -> &'static str {
    match v1::AgentAuthority::try_from(value).unwrap_or_default() {
        v1::AgentAuthority::Screen => "screen",
        v1::AgentAuthority::Process => "process",
        v1::AgentAuthority::Hook => "hook",
        v1::AgentAuthority::Unspecified => "unspecified",
    }
}

fn hook_action_name(value: i32) -> &'static str {
    match v1::HookManagementAction::try_from(value).unwrap_or_default() {
        v1::HookManagementAction::Review => "review",
        v1::HookManagementAction::Install => "install",
        v1::HookManagementAction::Uninstall => "uninstall",
        v1::HookManagementAction::Unspecified => "unspecified",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u64_values_are_lossless_json_strings_and_profile_is_enriched() {
        let value = v1::AgentRecord {
            agent_id: "codex:1".into(),
            state_generation: (1_u64 << 53) + 1,
            attention_kind: "completed".into(),
            route: Some(v1::AgentRoute {
                agent_id: "codex:1".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let json = record_json(&value, "profile-1");
        assert_eq!(json["stateGeneration"], ((1_u64 << 53) + 1).to_string());
        assert_eq!(json["attentionKind"], "completed");
        assert_eq!(json["route"]["hostProfileId"], "profile-1");
        let sideband = with_connection_epoch(json, (1_u64 << 53) + 3);
        assert_eq!(sideband["connectionEpoch"], ((1_u64 << 53) + 3).to_string());
    }

    #[test]
    fn canonical_adapter_ids_are_opaque_and_legacy_enum_mapping_is_optional() {
        assert_eq!(canonical_adapter_id("future-agent"), "future-agent");
        assert_eq!(
            legacy_adapter_kind("future-agent"),
            v1::AgentAdapterKind::Unspecified
        );
        assert_eq!(canonical_adapter_id("claudeCode"), "claude-code");
        assert_eq!(
            legacy_adapter_kind("claudeCode"),
            v1::AgentAdapterKind::ClaudeCode
        );
    }
}
