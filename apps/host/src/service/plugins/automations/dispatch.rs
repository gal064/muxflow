//! `OPERATION_AUTOMATION`: what the desktop's Automations panel asks of the
//! helper. Blocking; the request dispatcher runs it off the reader loop.
//!
//! The panel shows `automations.json` as written, invalid entries included, so
//! they can be fixed there. Saving edits the file as JSON, keeping fields this
//! build does not know, and only over the version the panel last listed.

use std::path::Path;

use serde_json::{Map, Value};
use tmux_agent_protocol::v1;

use super::{
    clock, cron,
    launch::command_line,
    now_ms, state,
    store::{self, DEFINITIONS_VERSION, Run},
    subscribe, with_env,
};
use crate::service::tmux_actions::resolved_start_directory;

/// How many runs of each automation a listing carries.
const RECENT_RUNS: usize = 20;

pub(in crate::service) fn handle(
    request: v1::AutomationRequest,
    connection_id: u64,
) -> v1::Response {
    use v1::AutomationAction as Action;
    let outcome = match Action::try_from(request.action).unwrap_or_default() {
        Action::List => {
            subscribe(connection_id);
            list()
        }
        Action::Save => request
            .definition
            .ok_or_else(|| refusal("invalid_automation", "a save needs a definition"))
            .and_then(|definition| save(definition, &request.base_generation)),
        Action::Delete => delete(&request.id, &request.base_generation),
        Action::RunNow => run_now(&request.id),
        Action::PreviewSchedule => Ok(preview(&request.schedule)),
        Action::Unspecified => Err(refusal("invalid_automation", "unknown automation action")),
    };
    match outcome {
        Ok(response) => v1::Response {
            ok: true,
            automation: Some(response),
            ..Default::default()
        },
        Err(refusal) => v1::Response {
            ok: false,
            error_code: refusal.code.into(),
            display_message: refusal.message,
            ..Default::default()
        },
    }
}

/// Why a request was refused: a code the desktop can act on, and a sentence.
#[derive(Debug)]
struct Refusal {
    code: &'static str,
    message: String,
}

fn refusal(code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal {
        code,
        message: message.into(),
    }
}

fn this_year() -> i32 {
    clock::local_minute(now_ms().div_euclid(1000)).map_or(1970, |minute| minute.year)
}

/// The file's bytes, or none when it does not exist.
fn read(path: &Path) -> Result<Option<Vec<u8>>, Refusal> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(refusal(
            "file_unreadable",
            format!("cannot read automations.json: {error}"),
        )),
    }
}

/// Names one version of the file; a missing file is the empty name.
fn generation(bytes: Option<&[u8]>) -> String {
    bytes.map_or_else(String::new, |bytes| {
        blake3::hash(bytes).to_hex().to_string()
    })
}

/// The entries, or why the file cannot be read as a list of them.
fn entries(bytes: Option<&[u8]>) -> Result<Vec<Value>, String> {
    let Some(bytes) = bytes else {
        return Ok(Vec::new());
    };
    let file: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("automations.json is not valid JSON: {error}"))?;
    if file.get("version").and_then(Value::as_u64) != Some(DEFINITIONS_VERSION) {
        return Err(format!(
            "automations.json needs \"version\": {DEFINITIONS_VERSION}"
        ));
    }
    match file.get("automations") {
        None => Ok(Vec::new()),
        Some(Value::Array(entries)) => Ok(entries.clone()),
        Some(_) => Err("automations.json: \"automations\" must be a list".into()),
    }
}

fn list() -> Result<v1::AutomationResponse, Refusal> {
    // A list is also a chance to pick up an edit the next tick would have, and
    // then the next tick will not see it change: other viewers hear it here.
    if state().reload(this_year()) {
        super::notify();
    }
    let state = state();
    let path = state.paths().definitions.clone();
    let bytes = read(&path)?;
    let mut response = v1::AutomationResponse {
        generation: generation(bytes.as_deref()),
        state_dir: path
            .parent()
            .map(|parent| parent.display().to_string())
            .unwrap_or_default(),
        ..Default::default()
    };
    // A file that cannot be read shows the last version that loaded, which
    // is what is still running.
    let entries = entries(bytes.as_deref()).unwrap_or_else(|error| {
        response.file_error = error;
        state.definitions().entries.clone()
    });
    let now_secs = now_ms().div_euclid(1000);
    let year = this_year();
    for entry in &entries {
        let shown = shown(entry);
        let mut summary = v1::AutomationSummary {
            recent_runs: recent_runs(state.runs(&shown.id)),
            ..Default::default()
        };
        match store::definition(entry, year) {
            Ok(definition) => {
                if definition.enabled {
                    summary.next_run = definition
                        .schedule
                        .next_runs(now_secs, 1, clock::local_minute)
                        .first()
                        .map(cron::LocalMinute::key)
                        .unwrap_or_default();
                }
                let prompt = state
                    .paths()
                    .prompts
                    .join(format!("{}-<n>.txt", definition.id));
                summary.command_line = command_line(&definition, &prompt).unwrap_or_default();
            }
            Err(error) => summary.definition_error = error,
        }
        // Two entries sharing an id: neither runs.
        if entries
            .iter()
            .filter(|other| other.get("id") == entry.get("id"))
            .count()
            > 1
        {
            summary.definition_error = "another automation already uses this id".into();
            summary.next_run.clear();
        }
        summary.definition = Some(shown);
        response.automations.push(summary);
    }
    Ok(response)
}

/// An entry as written, with the defaults loading would apply.
fn shown(entry: &Value) -> v1::AutomationDefinition {
    let text = |key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let number = |key: &str, default: u32| {
        entry
            .get(key)
            .and_then(Value::as_u64)
            .map_or(default, |value| u32::try_from(value).unwrap_or(u32::MAX))
    };
    v1::AutomationDefinition {
        id: text("id"),
        name: text("name"),
        enabled: entry
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        schedule: text("schedule"),
        cwd: text("cwd"),
        agent: text("agent"),
        command: text("command"),
        prompt: text("prompt"),
        keep_tabs: number("keepTabs", 5),
        max_runtime_minutes: number("maxRuntimeMinutes", 120),
    }
}

fn recent_runs(runs: &[Run]) -> Vec<v1::AutomationRun> {
    let mut runs: Vec<&Run> = runs.iter().collect();
    runs.sort_by_key(|run| std::cmp::Reverse(run.number));
    runs.into_iter()
        .take(RECENT_RUNS)
        .map(|run| v1::AutomationRun {
            number: run.number,
            scheduled_for: run.scheduled_for.clone(),
            started_at_unix_millis: run.started_at,
            finished_at_unix_millis: run.finished_at,
            status: serde_json::to_value(run.status)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            session_id: run.session_id.clone(),
            window_id: run.window_id.clone(),
            pane_id: run.pane_id.clone(),
            server_identity: run.server_identity.clone(),
            last_message: run.last_message.clone(),
            error: run.error.clone(),
        })
        .collect()
}

/// Writes the definition's fields into `entry`, leaving any others alone.
fn write_fields(entry: &mut Map<String, Value>, definition: &v1::AutomationDefinition) {
    entry.insert("id".into(), definition.id.clone().into());
    entry.insert("name".into(), definition.name.clone().into());
    entry.insert("enabled".into(), definition.enabled.into());
    entry.insert("schedule".into(), definition.schedule.trim().into());
    entry.insert("cwd".into(), definition.cwd.trim().into());
    entry.insert("agent".into(), definition.agent.clone().into());
    if definition.agent == "custom" {
        entry.insert("command".into(), definition.command.trim().into());
    } else {
        entry.remove("command");
    }
    entry.insert("prompt".into(), definition.prompt.clone().into());
    entry.insert("keepTabs".into(), definition.keep_tabs.into());
    entry.insert(
        "maxRuntimeMinutes".into(),
        definition.max_runtime_minutes.into(),
    );
}

fn save(
    definition: v1::AutomationDefinition,
    base_generation: &str,
) -> Result<v1::AutomationResponse, Refusal> {
    edit(base_generation, |entries| {
        let at = only_entry(entries, &definition.id)?;
        let mut entry = at
            .and_then(|at| entries[at].as_object().cloned())
            .unwrap_or_default();
        write_fields(&mut entry, &definition);
        let entry = Value::Object(entry);
        store::definition(&entry, this_year())
            .map_err(|error| refusal("invalid_automation", error))?;
        resolved_start_directory(definition.cwd.trim())
            .map_err(|error| refusal("invalid_automation", format!("{error:#}")))?;
        match at {
            Some(at) => entries[at] = entry,
            None => entries.push(entry),
        }
        Ok(())
    })
}

fn delete(id: &str, base_generation: &str) -> Result<v1::AutomationResponse, Refusal> {
    edit(base_generation, |entries| {
        let at = only_entry(entries, id)?
            .ok_or_else(|| refusal("not_found", format!("no automation {id:?}")))?;
        entries.remove(at);
        Ok(())
    })
}

/// Where the entry with `id` is, refusing an id the file uses twice: the
/// panel cannot tell which of the two an edit means.
fn only_entry(entries: &[Value], id: &str) -> Result<Option<usize>, Refusal> {
    let mut found = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.get("id").and_then(Value::as_str) == Some(id))
        .map(|(at, _)| at);
    let at = found.next();
    if found.next().is_some() {
        return Err(refusal(
            "duplicate_id",
            format!(
                "automations.json has more than one automation with id {id:?}; fix the file on the host"
            ),
        ));
    }
    Ok(at)
}

/// Applies `change` to the file's entries, if the file is still the version
/// the caller listed, and answers with the new listing.
fn edit(
    base_generation: &str,
    change: impl FnOnce(&mut Vec<Value>) -> Result<(), Refusal>,
) -> Result<v1::AutomationResponse, Refusal> {
    {
        let mut state = state();
        let path = state.paths().definitions.clone();
        edit_file(&path, base_generation, change)?;
        state.reload(this_year());
    }
    super::notify();
    list()
}

fn edit_file(
    path: &Path,
    base_generation: &str,
    change: impl FnOnce(&mut Vec<Value>) -> Result<(), Refusal>,
) -> Result<(), Refusal> {
    let bytes = read(path)?;
    if generation(bytes.as_deref()) != base_generation {
        return Err(refusal(
            "stale_generation",
            "automations.json changed since it was listed",
        ));
    }
    let mut list = entries(bytes.as_deref()).map_err(|error| {
        refusal(
            "file_unreadable",
            format!("fix automations.json by hand first: {error}"),
        )
    })?;
    // The whole file, so fields beside the list survive too.
    let mut file: Value = match &bytes {
        None => serde_json::json!({ "version": DEFINITIONS_VERSION }),
        Some(bytes) => serde_json::from_slice(bytes)
            .map_err(|error| refusal("file_unreadable", error.to_string()))?,
    };
    change(&mut list)?;
    file["automations"] = Value::Array(list);
    let mut text = serde_json::to_vec_pretty(&file)
        .map_err(|error| refusal("write_failed", error.to_string()))?;
    text.push(b'\n');
    crate::service::snapshot::write_private_atomic(path, "automations", &text)
        .map_err(|error| refusal("write_failed", format!("{error:#}")))
}

fn run_now(id: &str) -> Result<v1::AutomationResponse, Refusal> {
    // A run that failed to start is still recorded, so viewers hear of it.
    let started = {
        let mut state = state();
        with_env(|env| state.run_now(env, id))
    };
    super::notify();
    started.map_err(|error| refusal("run_failed", error))?;
    list()
}

fn preview(schedule: &str) -> v1::AutomationResponse {
    let now_secs = now_ms().div_euclid(1000);
    match cron::parse(schedule, this_year()) {
        Ok(schedule) => v1::AutomationResponse {
            next_runs: schedule
                .next_runs(now_secs, 3, clock::local_minute)
                .iter()
                .map(cron::LocalMinute::key)
                .collect(),
            ..Default::default()
        },
        Err(error) => v1::AutomationResponse {
            schedule_error: error,
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Value {
        serde_json::json!({
            "id": "ci",
            "name": "Fix CI",
            "schedule": "0 9 * * 1-5",
            "cwd": "/tmp",
            "agent": "custom",
            "command": "cx --print",
            "prompt": "hello",
            "owner": "kept",
        })
    }

    #[test]
    fn an_entry_shows_as_written_with_defaults() {
        let shown = shown(&entry());
        assert_eq!(shown.id, "ci");
        assert!(shown.enabled);
        assert_eq!(shown.keep_tabs, 5);
        assert_eq!(shown.max_runtime_minutes, 120);
        assert_eq!(shown.command, "cx --print");
    }

    #[test]
    fn writing_fields_keeps_unknown_ones_and_drops_a_preset_command() {
        let mut object = entry().as_object().unwrap().clone();
        let mut definition = shown(&entry());
        definition.agent = "claude".into();
        definition.keep_tabs = 3;
        write_fields(&mut object, &definition);
        assert_eq!(object["owner"], "kept");
        assert_eq!(object["keepTabs"], 3);
        assert!(!object.contains_key("command"));
        assert!(store::definition(&Value::Object(object), 2026).is_ok());
    }

    #[test]
    fn the_file_reads_as_entries_or_an_error() {
        assert!(entries(None).unwrap().is_empty());
        assert_eq!(
            entries(Some(br#"{"version":1,"automations":[{"id":"a"}]}"#))
                .unwrap()
                .len(),
            1
        );
        assert!(entries(Some(b"{")).unwrap_err().contains("not valid JSON"));
        assert!(
            entries(Some(br#"{"version":2}"#))
                .unwrap_err()
                .contains("version")
        );
        assert_eq!(generation(None), "");
        assert_ne!(generation(Some(b"a")), generation(Some(b"b")));
    }

    #[test]
    fn edits_apply_only_over_the_version_listed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("automations.json");
        let add = |entries: &mut Vec<Value>| {
            entries.push(entry());
            Ok(())
        };
        // A missing file is the empty generation, and is created.
        assert_eq!(
            edit_file(&path, "stale", add).unwrap_err().code,
            "stale_generation"
        );
        edit_file(&path, "", add).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let written: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(written["version"], 1);
        assert_eq!(written["automations"][0]["owner"], "kept");
        // Someone else edits; the old generation is refused.
        let listed = generation(Some(&bytes));
        std::fs::write(&path, br#"{"version":1,"automations":[],"note":"theirs"}"#).unwrap();
        assert_eq!(
            edit_file(&path, &listed, add).unwrap_err().code,
            "stale_generation"
        );
        let current = generation(Some(&std::fs::read(&path).unwrap()));
        edit_file(&path, &current, add).unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["note"], "theirs", "fields beside the list survive");
        // A file that is not a list of automations is never overwritten.
        std::fs::write(&path, b"{").unwrap();
        assert_eq!(
            edit_file(&path, &generation(Some(b"{")), add)
                .unwrap_err()
                .code,
            "file_unreadable"
        );
    }

    #[test]
    fn an_id_used_twice_is_refused_rather_than_guessed() {
        let entries = vec![entry(), entry()];
        assert_eq!(only_entry(&entries, "ci").unwrap_err().code, "duplicate_id");
        assert_eq!(only_entry(&entries[..1], "ci").unwrap(), Some(0));
        assert_eq!(only_entry(&entries, "other").unwrap(), None);
    }

    #[test]
    fn a_schedule_preview_lists_three_runs_or_says_why_not() {
        assert_eq!(preview("*/5 * * * *").next_runs.len(), 3);
        let broken = preview("61 * * * *");
        assert!(broken.next_runs.is_empty());
        assert!(
            broken.schedule_error.contains("minute"),
            "{}",
            broken.schedule_error
        );
    }
}
