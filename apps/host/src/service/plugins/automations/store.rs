//! The two files automations live in.
//!
//! `automations.json` holds the definitions. People and agents write it; the
//! helper only reads it, so nothing here can race an edit. `automations/
//! runs.json` holds run history and the errors found in the definitions, and
//! only the helper writes it.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::cron::{self, Schedule};

const DEFINITIONS_VERSION: u64 = 1;
const RUNS_VERSION: u32 = 1;
const RUNS_KEPT: usize = 100;
pub(super) const LAST_MESSAGE_MAX_BYTES: usize = 4096;
const PROMPT_MAX_BYTES: usize = 64 * 1024;
const NAME_MAX_BYTES: usize = 100;
const ID_MAX_BYTES: usize = 64;

#[derive(Clone, Debug)]
pub(super) struct Paths {
    pub definitions: PathBuf,
    pub runs: PathBuf,
    pub prompts: PathBuf,
}

impl Paths {
    pub(super) fn under(state_dir: &Path) -> Self {
        let own = state_dir.join("automations");
        Self {
            definitions: state_dir.join("automations.json"),
            runs: own.join("runs.json"),
            prompts: own.join("prompts"),
        }
    }

    pub(super) fn prompt(&self, id: &str, number: u32) -> PathBuf {
        self.prompts.join(format!("{id}-{number}.txt"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Agent {
    Claude,
    Codex,
    Custom,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Definition {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub schedule: Schedule,
    pub cwd: String,
    pub agent: Agent,
    pub command: String,
    pub prompt: String,
    pub keep_tabs: u32,
    pub max_runtime_minutes: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDefinition {
    id: String,
    name: String,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
    schedule: String,
    cwd: String,
    agent: String,
    #[serde(default)]
    command: String,
    prompt: String,
    #[serde(default)]
    keep_tabs: Option<u32>,
    #[serde(default)]
    max_runtime_minutes: Option<u32>,
}

fn enabled_by_default() -> bool {
    true
}

/// The definitions as last loaded, plus what was wrong with the file.
///
/// A file that cannot be read keeps the last set that loaded, so an agent
/// halfway through an edit never stops automations that were already running.
#[derive(Default)]
pub(super) struct Definitions {
    /// `None` until the first check; then the file's stamp, `None` if absent.
    stamp: Option<Option<Stamp>>,
    pub good: Vec<Definition>,
    /// Per-automation errors, keyed by id (or `#<index>` when there is none).
    pub invalid: BTreeMap<String, String>,
    pub file_error: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ns: i128,
}

/// What a reload found, for the caller deciding whether history may be pruned.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Reload {
    Unchanged,
    Loaded,
    Unreadable,
}

impl Definitions {
    /// Re-reads the file when its identity, size or mtime changed. A missing
    /// file is an empty set: the feature is idle. One that cannot be looked at
    /// is unreadable, not missing.
    pub(super) fn reload_if_changed(&mut self, path: &Path, this_year: i32) -> Reload {
        let stamp = match std::fs::metadata(path) {
            Ok(metadata) => Some(stamp(&metadata)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                self.file_error = format!("cannot read automations.json: {error}");
                return Reload::Unreadable;
            }
        };
        if self.stamp == Some(stamp) {
            return Reload::Unchanged;
        }
        self.stamp = Some(stamp);
        if stamp.is_none() {
            self.good.clear();
            self.invalid.clear();
            self.file_error.clear();
            return Reload::Loaded;
        }
        match std::fs::read(path)
            .map_err(|error| format!("cannot read automations.json: {error}"))
            .and_then(|bytes| parse_file(&bytes, this_year))
        {
            Ok((good, invalid)) => {
                self.good = good;
                self.invalid = invalid;
                self.file_error.clear();
                Reload::Loaded
            }
            Err(error) => {
                self.file_error = error;
                Reload::Unreadable
            }
        }
    }

    /// Every id the file mentions, valid or not, so history survives a typo.
    pub(super) fn known_ids(&self) -> BTreeSet<String> {
        self.good
            .iter()
            .map(|definition| definition.id.clone())
            .chain(self.invalid.keys().cloned())
            .collect()
    }
}

fn stamp(metadata: &std::fs::Metadata) -> Stamp {
    use std::os::unix::fs::MetadataExt as _;
    Stamp {
        dev: metadata.dev(),
        ino: metadata.ino(),
        size: metadata.size(),
        mtime_ns: i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec()),
    }
}

type Parsed = (Vec<Definition>, BTreeMap<String, String>);

/// The whole file, or why none of it can be trusted. Problems confined to one
/// automation leave the others running.
fn parse_file(bytes: &[u8], this_year: i32) -> Result<Parsed, String> {
    let file: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("automations.json is not valid JSON: {error}"))?;
    let version = file.get("version").and_then(serde_json::Value::as_u64);
    if version != Some(DEFINITIONS_VERSION) {
        return Err(format!(
            "automations.json needs \"version\": {DEFINITIONS_VERSION}"
        ));
    }
    let entries = match file.get("automations") {
        None => &[][..],
        Some(serde_json::Value::Array(entries)) => entries.as_slice(),
        Some(_) => return Err("automations.json: \"automations\" must be a list".into()),
    };
    let mut good: Vec<Definition> = Vec::new();
    let mut invalid = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let key = entry
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
            .map_or_else(|| format!("#{index}"), str::to_owned);
        if good.iter().any(|definition| definition.id == key) || invalid.contains_key(&key) {
            invalid.insert(key, "another automation already uses this id".into());
            continue;
        }
        match definition(entry, this_year) {
            Ok(definition) => good.push(definition),
            Err(error) => {
                invalid.insert(key, error);
            }
        }
    }
    // A duplicate makes both ambiguous; neither runs.
    good.retain(|definition| !invalid.contains_key(&definition.id));
    Ok((good, invalid))
}

fn definition(entry: &serde_json::Value, this_year: i32) -> Result<Definition, String> {
    let raw: RawDefinition =
        serde_json::from_value(entry.clone()).map_err(|error| error.to_string())?;
    if raw.id.is_empty()
        || raw.id.len() > ID_MAX_BYTES
        || !raw
            .id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(format!(
            "id must be 1 to {ID_MAX_BYTES} lowercase letters, digits or dashes"
        ));
    }
    if raw.name.trim().is_empty()
        || raw.name.len() > NAME_MAX_BYTES
        || raw.name.chars().any(char::is_control)
    {
        return Err(format!(
            "name must be 1 to {NAME_MAX_BYTES} bytes without control characters"
        ));
    }
    let schedule = cron::parse(&raw.schedule, this_year)?;
    if !(raw.cwd.starts_with('/') || raw.cwd == "~" || raw.cwd.starts_with("~/")) {
        return Err("cwd must be an absolute path or start with ~/".into());
    }
    let agent = match raw.agent.as_str() {
        "claude" => Agent::Claude,
        "codex" => Agent::Codex,
        "custom" => Agent::Custom,
        other => {
            return Err(format!(
                "agent must be claude, codex or custom, not {other:?}"
            ));
        }
    };
    if agent == Agent::Custom
        && (raw.command.trim().is_empty() || raw.command.chars().any(char::is_control))
    {
        return Err("a custom agent needs a one-line command".into());
    }
    if raw.prompt.trim().is_empty() || raw.prompt.len() > PROMPT_MAX_BYTES {
        return Err(format!("prompt must be 1 to {PROMPT_MAX_BYTES} bytes"));
    }
    let keep_tabs = raw.keep_tabs.unwrap_or(5);
    if !(1..=50).contains(&keep_tabs) {
        return Err("keepTabs must be 1 to 50".into());
    }
    let max_runtime_minutes = raw.max_runtime_minutes.unwrap_or(120);
    if !(5..=1440).contains(&max_runtime_minutes) {
        return Err("maxRuntimeMinutes must be 5 to 1440".into());
    }
    Ok(Definition {
        id: raw.id,
        name: raw.name,
        enabled: raw.enabled,
        schedule,
        cwd: raw.cwd,
        agent,
        command: if agent == Agent::Custom {
            raw.command.trim().to_owned()
        } else {
            String::new()
        },
        prompt: raw.prompt,
        keep_tabs,
        max_runtime_minutes,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Status {
    Launched,
    Working,
    NeedsAttention,
    Done,
    Closed,
    Failed,
    Skipped,
}

impl Status {
    pub(super) fn is_final(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Closed | Self::Failed | Self::Skipped
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Run {
    pub number: u32,
    /// The local minute this run was due, as `LocalMinute::key` spells it.
    pub scheduled_for: String,
    pub started_at: i64,
    #[serde(default)]
    pub finished_at: i64,
    pub status: Status,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub window_id: String,
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub server_identity: String,
    #[serde(default)]
    pub last_message: String,
    #[serde(default)]
    pub error: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AutomationRuns {
    #[serde(default)]
    pub definition_error: String,
    #[serde(default)]
    pub runs: Vec<Run>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RunsFile {
    pub version: u32,
    #[serde(default)]
    pub file_error: String,
    #[serde(default)]
    pub automations: BTreeMap<String, AutomationRuns>,
}

impl Default for RunsFile {
    fn default() -> Self {
        Self {
            version: RUNS_VERSION,
            file_error: String::new(),
            automations: BTreeMap::new(),
        }
    }
}

impl RunsFile {
    /// History is the helper's own; an unreadable file starts over rather than
    /// stopping automations.
    pub(super) fn load(path: &Path) -> Self {
        crate::service::snapshot::load_sidecar::<Self>(path, "automation runs")
            .ok()
            .filter(|file| file.version == RUNS_VERSION)
            .unwrap_or_default()
    }

    pub(super) fn save(&self, path: &Path) -> anyhow::Result<()> {
        crate::service::snapshot::save_sidecar(path, "automation runs", self)
    }

    pub(super) fn next_number(&self, id: &str) -> u32 {
        self.automations
            .get(id)
            .and_then(|history| history.runs.iter().map(|run| run.number).max())
            .unwrap_or(0)
            + 1
    }

    /// Copies the definition errors in, and forgets automations the file no
    /// longer mentions once none of their runs is unfinished. Called only
    /// after a successful load, so an unreadable file never erases history.
    /// Returns whether anything changed.
    pub(super) fn sync_definitions(&mut self, definitions: &Definitions) -> bool {
        let before = self.clone();
        let known = definitions.known_ids();
        self.automations.retain(|id, history| {
            known.contains(id) || history.runs.iter().any(|run| !run.status.is_final())
        });
        for id in known {
            let entry = self.automations.entry(id.clone()).or_default();
            entry.definition_error = definitions.invalid.get(&id).cloned().unwrap_or_default();
        }
        self.automations
            .retain(|_, history| !history.runs.is_empty() || !history.definition_error.is_empty());
        *self != before
    }

    /// Keeps the newest runs of each automation. Unfinished runs are never
    /// dropped: their outcome may land long after newer runs.
    pub(super) fn prune(&mut self) {
        for history in self.automations.values_mut() {
            let finished = history
                .runs
                .iter()
                .filter(|run| run.status.is_final())
                .count();
            let mut excess = finished.saturating_sub(RUNS_KEPT);
            history.runs.sort_by_key(|run| run.number);
            history.runs.retain(|run| {
                if excess > 0 && run.status.is_final() {
                    excess -= 1;
                    return false;
                }
                true
            });
        }
    }
}

/// The longest prefix of `text` within `max` bytes, cut on a char boundary.
pub(super) fn truncated(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const YEAR: i32 = 2026;

    fn write(path: &Path, value: serde_json::Value) {
        std::fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    }

    fn entry(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "name": format!("Automation {id}"),
            "schedule": "0 9 * * 1-5",
            "cwd": "~/dev",
            "agent": "claude",
            "prompt": "Check CI.",
        })
    }

    fn run(number: u32, status: Status) -> Run {
        Run {
            number,
            scheduled_for: format!("2026-10-13T09:{number:02}"),
            started_at: i64::from(number),
            finished_at: 0,
            status,
            session_id: String::new(),
            window_id: String::new(),
            pane_id: String::new(),
            server_identity: String::new(),
            last_message: String::new(),
            error: String::new(),
        }
    }

    #[test]
    fn a_valid_file_loads_with_defaults_and_ignores_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("automations.json");
        let mut custom = entry("custom");
        custom["agent"] = "custom".into();
        custom["command"] = "  cx --print  ".into();
        custom["futureField"] = true.into();
        write(
            &path,
            serde_json::json!({"version": 1, "automations": [entry("ci"), custom]}),
        );
        let mut definitions = Definitions::default();
        assert_eq!(definitions.reload_if_changed(&path, YEAR), Reload::Loaded);
        assert!(definitions.invalid.is_empty(), "{:?}", definitions.invalid);
        let ci = &definitions.good[0];
        assert!(ci.enabled);
        assert_eq!((ci.keep_tabs, ci.max_runtime_minutes), (5, 120));
        assert_eq!(ci.command, "", "a preset ignores command");
        assert_eq!(definitions.good[1].command, "cx --print");
        assert_eq!(
            definitions.reload_if_changed(&path, YEAR),
            Reload::Unchanged
        );
    }

    #[test]
    fn an_unreadable_file_keeps_the_last_good_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("automations.json");
        write(
            &path,
            serde_json::json!({"version": 1, "automations": [entry("ci")]}),
        );
        let mut definitions = Definitions::default();
        definitions.reload_if_changed(&path, YEAR);

        std::fs::write(&path, b"{\"version\": 1, \"automations\": [").unwrap();
        assert_eq!(
            definitions.reload_if_changed(&path, YEAR),
            Reload::Unreadable
        );
        assert!(
            definitions.file_error.contains("not valid JSON"),
            "{}",
            definitions.file_error
        );
        assert_eq!(definitions.good.len(), 1);

        write(&path, serde_json::json!({"version": 2, "automations": []}));
        assert_eq!(
            definitions.reload_if_changed(&path, YEAR),
            Reload::Unreadable
        );
        assert!(definitions.file_error.contains("version"));
        assert_eq!(definitions.good.len(), 1);

        std::fs::remove_file(&path).unwrap();
        assert_eq!(definitions.reload_if_changed(&path, YEAR), Reload::Loaded);
        assert!(definitions.good.is_empty() && definitions.file_error.is_empty());
    }

    #[test]
    fn one_bad_automation_does_not_stop_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("automations.json");
        let mut bad_schedule = entry("bad-schedule");
        bad_schedule["schedule"] = "61 * * * *".into();
        let mut no_command = entry("no-command");
        no_command["agent"] = "custom".into();
        let mut relative = entry("relative");
        relative["cwd"] = "dev".into();
        let mut too_many_tabs = entry("tabs");
        too_many_tabs["keepTabs"] = 51.into();
        let mut no_id = entry("x");
        no_id.as_object_mut().unwrap().remove("id");
        write(
            &path,
            serde_json::json!({"version": 1, "automations": [
                entry("ok"), bad_schedule, no_command, relative, too_many_tabs, no_id,
                entry("Upper"), entry("twice"), entry("twice"),
            ]}),
        );
        let mut definitions = Definitions::default();
        definitions.reload_if_changed(&path, YEAR);
        let good: Vec<_> = definitions.good.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(good, ["ok"]);
        let invalid: Vec<_> = definitions.invalid.keys().map(String::as_str).collect();
        assert_eq!(
            invalid,
            [
                "#5",
                "Upper",
                "bad-schedule",
                "no-command",
                "relative",
                "tabs",
                "twice"
            ]
        );
        assert!(definitions.invalid["#5"].contains("id"));
        assert!(definitions.invalid["twice"].contains("already uses"));
    }

    #[test]
    fn history_follows_the_file_but_survives_an_unreadable_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("automations.json");
        let mut broken = entry("broken");
        broken["agent"] = "nope".into();
        write(
            &path,
            serde_json::json!({"version": 1, "automations": [entry("ci"), broken]}),
        );
        let mut definitions = Definitions::default();
        definitions.reload_if_changed(&path, YEAR);
        let mut runs = RunsFile::default();
        runs.automations
            .entry("ci".into())
            .or_default()
            .runs
            .push(run(1, Status::Done));
        runs.automations
            .entry("gone".into())
            .or_default()
            .runs
            .push(run(1, Status::Done));
        runs.automations
            .entry("removed-mid-run".into())
            .or_default()
            .runs
            .push(run(1, Status::Working));
        assert!(runs.sync_definitions(&definitions));
        assert!(!runs.automations.contains_key("gone"));
        assert!(
            runs.automations.contains_key("removed-mid-run"),
            "a run still going keeps its history"
        );
        assert!(
            runs.automations["broken"]
                .definition_error
                .contains("agent")
        );
        assert_eq!(runs.next_number("ci"), 2);
        assert_eq!(runs.next_number("broken"), 1);
        assert!(
            !runs.sync_definitions(&definitions),
            "a second sync changes nothing"
        );
    }

    #[test]
    fn pruning_keeps_the_newest_finished_runs_and_every_unfinished_one() {
        let mut runs = RunsFile::default();
        let history = &mut runs.automations.entry("ci".into()).or_default().runs;
        history.push(run(1, Status::Working));
        for number in 2..=(RUNS_KEPT as u32 + 11) {
            history.push(run(number, Status::Done));
        }
        runs.prune();
        let kept = &runs.automations["ci"].runs;
        assert_eq!(kept.len(), RUNS_KEPT + 1);
        assert_eq!(kept[0].number, 1, "the unfinished run stays");
        assert_eq!(kept[1].number, 12);
    }

    #[test]
    fn runs_round_trip_through_the_private_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let mut runs = RunsFile::default();
        runs.automations
            .entry("ci".into())
            .or_default()
            .runs
            .push(run(3, Status::NeedsAttention));
        runs.save(&paths.runs).unwrap();
        assert_eq!(RunsFile::load(&paths.runs), runs);
        let text = std::fs::read_to_string(&paths.runs).unwrap();
        assert!(text.contains("\"needs_attention\"") && text.contains("\"scheduledFor\""));

        std::fs::write(&paths.runs, b"not json").unwrap();
        assert_eq!(RunsFile::load(&paths.runs), RunsFile::default());
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        assert_eq!(truncated("héllo", 2), "h");
        assert_eq!(truncated("hello", 10), "hello");
    }
}
