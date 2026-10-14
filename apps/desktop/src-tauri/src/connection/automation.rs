//! The Automations panel's one command (docs/automations.md): list, save,
//! delete, run now and schedule preview, against one host's helper.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::State;
use tmux_agent_protocol::v1;

use super::{TerminalClients, get_client};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct AutomationDefinition {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub schedule: String,
    pub cwd: String,
    pub agent: String,
    pub command: String,
    pub prompt: String,
    pub keep_tabs: u32,
    pub max_runtime_minutes: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AutomationCommand {
    pub action: String,
    pub definition: Option<AutomationDefinition>,
    pub id: String,
    pub base_generation: String,
    pub schedule: String,
}

/// Async so a run started now, which waits on the new tab's shell, never
/// freezes the WebView's main thread.
#[tauri::command]
pub async fn automation_request(
    client_id: String,
    command: AutomationCommand,
    clients: State<'_, TerminalClients>,
) -> Result<Value, String> {
    let request = request_from(command)?;
    let client = get_client(&clients, &client_id)?;
    let response = tauri::async_runtime::spawn_blocking(move || client.request(request))
        .await
        .map_err(|error| format!("automation request task failed: {error}"))??;
    response
        .automation
        .as_ref()
        .map(response_json)
        .ok_or_else(|| "host omitted automation response".into())
}

fn request_from(command: AutomationCommand) -> Result<v1::Request, String> {
    use v1::AutomationAction as Action;
    let action = match command.action.as_str() {
        "list" => Action::List,
        "save" => Action::Save,
        "delete" => Action::Delete,
        "runNow" => Action::RunNow,
        "previewSchedule" => Action::PreviewSchedule,
        other => return Err(format!("unknown automation action {other:?}")),
    };
    Ok(v1::Request {
        operation: v1::Operation::Automation.into(),
        automation: Some(v1::AutomationRequest {
            action: action.into(),
            definition: command
                .definition
                .map(|definition| v1::AutomationDefinition {
                    id: definition.id,
                    name: definition.name,
                    enabled: definition.enabled,
                    schedule: definition.schedule,
                    cwd: definition.cwd,
                    agent: definition.agent,
                    command: definition.command,
                    prompt: definition.prompt,
                    keep_tabs: definition.keep_tabs,
                    max_runtime_minutes: definition.max_runtime_minutes,
                }),
            id: command.id,
            base_generation: command.base_generation,
            schedule: command.schedule,
        }),
        ..Default::default()
    })
}

fn definition_json(definition: &v1::AutomationDefinition) -> Value {
    json!(AutomationDefinition {
        id: definition.id.clone(),
        name: definition.name.clone(),
        enabled: definition.enabled,
        schedule: definition.schedule.clone(),
        cwd: definition.cwd.clone(),
        agent: definition.agent.clone(),
        command: definition.command.clone(),
        prompt: definition.prompt.clone(),
        keep_tabs: definition.keep_tabs,
        max_runtime_minutes: definition.max_runtime_minutes,
    })
}

fn run_json(run: &v1::AutomationRun) -> Value {
    json!({
        "number": run.number,
        "scheduledFor": run.scheduled_for,
        "startedAt": run.started_at_unix_millis,
        "finishedAt": run.finished_at_unix_millis,
        "status": run.status,
        "sessionId": run.session_id,
        "windowId": run.window_id,
        "paneId": run.pane_id,
        "serverIdentity": run.server_identity,
        "lastMessage": run.last_message,
        "error": run.error,
    })
}

fn response_json(response: &v1::AutomationResponse) -> Value {
    json!({
        "automations": response.automations.iter().map(|summary| json!({
            "definition": summary.definition.as_ref().map(definition_json),
            "definitionError": summary.definition_error,
            "nextRun": summary.next_run,
            "recentRuns": summary.recent_runs.iter().map(run_json).collect::<Vec<_>>(),
            "commandLine": summary.command_line,
        })).collect::<Vec<_>>(),
        "fileError": response.file_error,
        "generation": response.generation,
        "stateDir": response.state_dir,
        "nextRuns": response.next_runs,
        "scheduleError": response.schedule_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_save_command_becomes_an_automation_request() {
        let command: AutomationCommand = serde_json::from_value(json!({
            "action": "save",
            "baseGeneration": "abc",
            "definition": {
                "id": "ci", "name": "Fix CI", "enabled": true, "schedule": "0 9 * * 1-5",
                "cwd": "~/dev", "agent": "codex", "prompt": "go", "keepTabs": 5,
                "maxRuntimeMinutes": 120,
            },
        }))
        .unwrap();
        let request = request_from(command).unwrap();
        assert_eq!(request.operation, i32::from(v1::Operation::Automation));
        let automation = request.automation.unwrap();
        assert_eq!(automation.action, i32::from(v1::AutomationAction::Save));
        assert_eq!(automation.base_generation, "abc");
        let definition = automation.definition.unwrap();
        assert_eq!(
            (definition.agent.as_str(), definition.keep_tabs),
            ("codex", 5)
        );
        assert!(
            request_from(AutomationCommand {
                action: "explode".into(),
                ..Default::default()
            })
            .is_err()
        );
    }

    #[test]
    fn a_listing_reads_back_in_camel_case() {
        let value = response_json(&v1::AutomationResponse {
            automations: vec![v1::AutomationSummary {
                definition: Some(v1::AutomationDefinition {
                    id: "ci".into(),
                    keep_tabs: 3,
                    ..Default::default()
                }),
                next_run: "2026-10-14T09:00".into(),
                recent_runs: vec![v1::AutomationRun {
                    number: 7,
                    status: "done".into(),
                    last_message: "All green.".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            generation: "g".into(),
            ..Default::default()
        });
        let summary = &value["automations"][0];
        assert_eq!(summary["definition"]["keepTabs"], 3);
        assert_eq!(summary["nextRun"], "2026-10-14T09:00");
        assert_eq!(summary["recentRuns"][0]["lastMessage"], "All green.");
        assert_eq!(value["generation"], "g");
    }
}
