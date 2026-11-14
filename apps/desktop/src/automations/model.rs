//! What the helper says about a host's automations, and the plain-English
//! readings the panel draws from it. Port of `types.ts`, `api.ts`'s refusal
//! helpers, `cronText.ts`, and the pure half of `AutomationsPanel.tsx` and
//! `AutomationEditor.tsx`.

use chrono::{Datelike, NaiveDate};
pub use desktop_core::connection::automation::AutomationDefinition;
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct AutomationRun {
    pub number: u64,
    /// The host-local minute it was due, `YYYY-MM-DDTHH:MM`; empty for Run now.
    pub scheduled_for: String,
    pub started_at: u64,
    pub finished_at: u64,
    pub status: String,
    pub session_id: String,
    pub window_id: String,
    pub pane_id: String,
    pub server_identity: String,
    pub last_message: String,
    pub error: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct AutomationSummary {
    pub definition: AutomationDefinition,
    /// Why the entry does not run; empty when it is valid.
    pub definition_error: String,
    /// The next host-local minute it runs, or empty.
    pub next_run: String,
    /// Newest first.
    pub recent_runs: Vec<AutomationRun>,
    /// What is typed into a run's tab.
    pub command_line: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct AutomationListing {
    pub automations: Vec<AutomationSummary>,
    /// Why automations.json cannot be read; the last set that loaded still runs.
    pub file_error: String,
    /// Names the version of the file listed; saves are refused over a newer one.
    pub generation: String,
    pub state_dir: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct SchedulePreview {
    pub next_runs: Vec<String>,
    pub schedule_error: String,
}

/// The host refused a save because the file changed since it was listed.
pub fn is_stale_generation(error: &str) -> bool {
    error.contains("stale_generation")
}

/// The host's refusal, without its machine-readable code.
pub fn refusal_text(error: &str) -> String {
    let code = error.find(": ").filter(|end| {
        *end > 0
            && error[..*end]
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    });
    match code {
        Some(end) => error[end + 2..].to_owned(),
        None => error.to_owned(),
    }
}

pub fn status_text(status: &str) -> &str {
    match status {
        "launched" => "starting",
        "working" => "working",
        "needs_attention" => "needs attention",
        "done" => "done",
        "closed" => "tab closed",
        "failed" => "failed",
        "skipped" => "skipped",
        other => other,
    }
}

pub fn unfinished(run: &AutomationRun) -> bool {
    matches!(
        run.status.as_str(),
        "launched" | "working" | "needs_attention"
    )
}

/// How long an unfinished run has gone, at the half-minute it refreshes.
pub fn age(run: &AutomationRun, now: u64) -> String {
    let elapsed = now.saturating_sub(run.started_at);
    if elapsed < 60_000 {
        "<1m".into()
    } else {
        describe_duration(elapsed)
    }
}

/// What a run is doing, in one chip: `#12 done · 09:00`.
pub fn run_chip(run: &AutomationRun, now: u64, today: NaiveDate) -> String {
    let when = if unfinished(run) {
        age(run, now)
    } else if run.scheduled_for.is_empty() {
        "manual".into()
    } else {
        describe_minute(&run.scheduled_for, today)
    };
    format!("#{} {} · {when}", run.number, status_text(&run.status))
}

pub fn agent_text(summary: &AutomationSummary) -> &str {
    let definition = &summary.definition;
    if definition.agent == "custom" {
        &definition.command
    } else {
        &definition.agent
    }
}

/// The helper's unattended defaults, in each agent's auto-approval mode; docs/automations.md.
pub fn preset_command(agent: &str) -> Option<&'static str> {
    match agent {
        "claude" => Some("claude --permission-mode auto"),
        "codex" => Some("codex --approve-for-me"),
        _ => None,
    }
}

/// The command a run types, without the prompt the helper appends.
pub fn command_text(summary: &AutomationSummary) -> &str {
    let definition = &summary.definition;
    if definition.agent == "custom" {
        &definition.command
    } else {
        preset_command(&definition.agent).unwrap_or(&definition.agent)
    }
}

pub const AGENTS: [(&str, &str); 3] = [
    ("claude", "Claude Code"),
    ("codex", "Codex"),
    ("custom", "Custom command"),
];

/// A new automation's id: its name, as the file's id rules allow, made unique.
pub fn id_from_name(name: &str, taken: &[String]) -> String {
    let mut base = String::new();
    for character in name.to_lowercase().chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            base.push(character);
        } else if !base.ends_with('-') {
            base.push('-');
        }
    }
    let mut base: String = base.trim_matches('-').chars().take(56).collect();
    if base.is_empty() {
        base = "automation".into();
    }
    let mut id = base.clone();
    let mut index = 2;
    while taken.contains(&id) {
        id = format!("{base}-{index}");
        index += 1;
    }
    id
}

/// The line the helper types into a run's tab.
pub fn command_preview(definition: &AutomationDefinition, state_dir: &str) -> String {
    let program = if definition.agent == "custom" {
        match definition.command.trim() {
            "" => "<command>".to_owned(),
            command => command.to_owned(),
        }
    } else {
        format!(
            "{} --",
            preset_command(&definition.agent).unwrap_or(&definition.agent)
        )
    };
    let id = if definition.id.is_empty() {
        "<id>"
    } else {
        &definition.id
    };
    format!("{program} \"$(cat '{state_dir}/automations/prompts/{id}-<n>.txt')\"")
}

/// Claude Code never saves trust for the home directory, so it asks on every run there.
pub fn is_home(cwd: &str) -> bool {
    let cwd = cwd.trim();
    cwd.starts_with('~') && cwd[1..].bytes().all(|byte| byte == b'/')
}

pub fn blank_definition() -> AutomationDefinition {
    AutomationDefinition {
        id: String::new(),
        name: String::new(),
        enabled: true,
        schedule: "0 9 * * 1-5".into(),
        cwd: "~/".into(),
        agent: "claude".into(),
        command: String::new(),
        prompt: String::new(),
        keep_tabs: 5,
        max_runtime_minutes: 120,
    }
}

/// A required field the editor still needs, in the order it asks for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    Name,
    Prompt,
    Cwd,
    Command,
}

impl Missing {
    pub fn text(self) -> &'static str {
        match self {
            Self::Name => "a name",
            Self::Prompt => "a prompt",
            Self::Cwd => "a working directory",
            Self::Command => "a command",
        }
    }
}

pub fn missing(draft: &AutomationDefinition) -> Option<Missing> {
    if draft.name.trim().is_empty() {
        Some(Missing::Name)
    } else if draft.prompt.trim().is_empty() {
        Some(Missing::Prompt)
    } else if draft.cwd.trim().is_empty() {
        Some(Missing::Cwd)
    } else if draft.agent == "custom" && draft.command.trim().is_empty() {
        Some(Missing::Command)
    } else {
        None
    }
}

/// The overlap limit as the detail view says it: whole hours, or a duration.
pub fn overlap_text(minutes: u32) -> String {
    if minutes.is_multiple_of(60) {
        format!("{}h", minutes / 60)
    } else {
        describe_duration(u64::from(minutes) * 60_000)
    }
}

const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const SHORT_DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const SHORT_MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn number(text: &str, max: u32) -> Option<u32> {
    if text.is_empty() || text.len() > 2 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|value| *value <= max)
}

fn clock(hour: u32, minute: u32) -> String {
    format!("{hour:02}:{minute:02}")
}

fn weekday(text: &str) -> Option<usize> {
    let alias = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"]
        .iter()
        .position(|day| *day == text.to_lowercase());
    alias.or_else(|| number(text, 7).map(|day| day as usize % 7))
}

/// `1-5`, `sat,sun` and single days, as words; `None` for anything else.
fn weekdays(field: &str) -> Option<String> {
    let lower = field.to_lowercase();
    if lower == "1-5" || lower == "mon-fri" {
        return Some("Weekdays".into());
    }
    if ["0,6", "6,0", "sat,sun", "sun,sat", "6,7"].contains(&lower.as_str()) {
        return Some("Weekends".into());
    }
    let names: Vec<String> = lower
        .split(',')
        .map(|day| weekday(day).map(|day| format!("{}s", DAY_NAMES[day])))
        .collect::<Option<_>>()?;
    Some(match names.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => return None,
    })
}

pub fn describe_schedule(schedule: &str) -> String {
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    let [
        minute_field,
        hour_field,
        day_field,
        month_field,
        weekday_field,
    ] = fields[..]
    else {
        return schedule.to_owned();
    };
    if month_field != "*" {
        return schedule.to_owned();
    }
    if fields.iter().all(|field| *field == "*") {
        return "Every minute".into();
    }
    let step = minute_field.strip_prefix("*/").filter(|step| {
        !step.is_empty() && step.len() <= 2 && step.bytes().all(|byte| byte.is_ascii_digit())
    });
    if let Some(step) = step
        && hour_field == "*"
        && day_field == "*"
        && weekday_field == "*"
    {
        return format!("Every {} minutes", step.parse::<u32>().unwrap_or_default());
    }
    let Some(minute) = number(minute_field, 59) else {
        return schedule.to_owned();
    };
    if hour_field == "*" && day_field == "*" && weekday_field == "*" {
        return if minute == 0 {
            "Every hour".into()
        } else {
            format!("Every hour at :{minute:02}")
        };
    }
    let Some(hour) = number(hour_field, 23) else {
        return schedule.to_owned();
    };
    let at = clock(hour, minute);
    if day_field == "*" && weekday_field == "*" {
        return format!("Daily {at}");
    }
    if day_field == "*" {
        return weekdays(weekday_field)
            .map_or_else(|| schedule.to_owned(), |days| format!("{days} {at}"));
    }
    match number(day_field, 31) {
        Some(day) if weekday_field == "*" => format!("Monthly on day {day}, {at}"),
        _ => schedule.to_owned(),
    }
}

/// When a host-local minute is, relative to `today`: the time alone today, a
/// weekday within the week, a date after that. Parsed without any time zone.
pub fn describe_minute(key: &str, today: NaiveDate) -> String {
    let parsed = (key.len() == 16 && key.is_ascii() && key.as_bytes()[10] == b'T')
        .then(|| {
            let date = NaiveDate::parse_from_str(&key[..10], "%Y-%m-%d").ok()?;
            let hour = number(&key[11..13], 23)?;
            let minute = number(&key[14..16], 59)?;
            (key.as_bytes()[13] == b':' && key[11..13].len() == 2)
                .then_some((date, clock(hour, minute)))
        })
        .flatten();
    let Some((date, time)) = parsed else {
        return key.to_owned();
    };
    let days = (date - today).num_days();
    let weekday = SHORT_DAYS[date.weekday().num_days_from_sunday() as usize];
    if days == 0 {
        time
    } else if days > 0 && days < 7 {
        format!("{weekday} {time}")
    } else {
        format!(
            "{weekday} {} {} {time}",
            date.day(),
            SHORT_MONTHS[date.month0() as usize]
        )
    }
}

/// A duration in milliseconds as `14m`, `1h 22m` or `40s`.
pub fn describe_duration(milliseconds: u64) -> String {
    let seconds = (milliseconds + 500) / 1000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = (seconds + 30) / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_schedules_people_write_and_anything_else_as_itself() {
        for (schedule, reading) in [
            ("* * * * *", "Every minute"),
            ("*/15 * * * *", "Every 15 minutes"),
            ("0 * * * *", "Every hour"),
            ("5 * * * *", "Every hour at :05"),
            ("0 2 * * *", "Daily 02:00"),
            ("0 9 * * 1-5", "Weekdays 09:00"),
            ("0 9 * * MON-FRI", "Weekdays 09:00"),
            ("0 10 * * sat,sun", "Weekends 10:00"),
            ("30 7 * * 1", "Mondays 07:30"),
            ("30 7 * * 1,3", "Mondays and Wednesdays 07:30"),
            ("0 6 1 * *", "Monthly on day 1, 06:00"),
        ] {
            assert_eq!(describe_schedule(schedule), reading, "{schedule}");
        }
        for unusual in ["0 9 * 1 *", "0,30 9 * * *", "0 9 1 * 1", "nonsense"] {
            assert_eq!(describe_schedule(unusual), unusual);
        }
    }

    #[test]
    fn says_when_a_host_minute_is_relative_to_today() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
        assert_eq!(describe_minute("2026-10-13T15:00", today), "15:00");
        assert_eq!(describe_minute("2026-10-14T09:00", today), "Wed 09:00");
        assert_eq!(
            describe_minute("2026-10-24T07:30", today),
            "Sat 24 Oct 07:30"
        );
        assert_eq!(describe_minute("garbage", today), "garbage");
    }

    #[test]
    fn reads_durations_at_the_precision_a_run_list_needs() {
        assert_eq!(describe_duration(40_000), "40s");
        assert_eq!(describe_duration(14 * 60_000), "14m");
        assert_eq!(describe_duration(82 * 60_000), "1h 22m");
        assert_eq!(overlap_text(120), "2h");
        assert_eq!(overlap_text(90), "1h 30m");
    }

    #[test]
    fn names_a_new_automations_id_after_it_uniquely() {
        assert_eq!(id_from_name("Fix CI!", &[]), "fix-ci");
        assert_eq!(
            id_from_name("Fix CI", &["fix-ci".into(), "fix-ci-2".into()]),
            "fix-ci-3"
        );
        assert_eq!(id_from_name("  ", &[]), "automation");
        assert_eq!(id_from_name(&"a".repeat(80), &[]).len(), 56);
    }

    #[test]
    fn previews_exactly_what_is_typed_for_presets_and_custom_commands() {
        let mut definition = blank_definition();
        definition.id = "ci".into();
        assert_eq!(
            command_preview(&definition, "~/.local/state/muxflow"),
            "claude --permission-mode auto -- \"$(cat '~/.local/state/muxflow/automations/prompts/ci-<n>.txt')\""
        );
        definition.agent = "custom".into();
        definition.id.clear();
        assert_eq!(
            command_preview(&definition, "/state"),
            "<command> \"$(cat '/state/automations/prompts/<id>-<n>.txt')\""
        );
        definition.command = " cx --print ".into();
        assert!(command_preview(&definition, "/state").starts_with("cx --print \""));
    }

    #[test]
    fn asks_for_what_is_missing_in_order() {
        let mut draft = blank_definition();
        assert_eq!(missing(&draft), Some(Missing::Name));
        draft.name = "Nightly".into();
        assert_eq!(missing(&draft), Some(Missing::Prompt));
        draft.prompt = "go".into();
        draft.cwd = " ".into();
        assert_eq!(
            missing(&draft).map(Missing::text),
            Some("a working directory")
        );
        draft.cwd = "~/dev".into();
        draft.agent = "custom".into();
        assert_eq!(missing(&draft), Some(Missing::Command));
        draft.command = "cx".into();
        assert_eq!(missing(&draft), None);
    }

    #[test]
    fn knows_the_home_folder_and_the_refusal_codes() {
        assert!(is_home("~"));
        assert!(is_home(" ~// "));
        assert!(!is_home("~/dev"));
        assert!(is_stale_generation("stale_generation: the file changed"));
        assert_eq!(refusal_text("invalid_definition: bad cron"), "bad cron");
        assert_eq!(refusal_text("Host said: no"), "Host said: no");
    }

    #[test]
    fn chips_say_what_a_run_is_doing() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
        let mut run = AutomationRun {
            number: 12,
            status: "done".into(),
            scheduled_for: "2026-10-13T09:00".into(),
            ..AutomationRun::default()
        };
        assert_eq!(run_chip(&run, 0, today), "#12 done · 09:00");
        run.scheduled_for.clear();
        assert_eq!(run_chip(&run, 0, today), "#12 done · manual");
        run.status = "working".into();
        run.started_at = 1_000;
        assert_eq!(run_chip(&run, 30_000, today), "#12 working · <1m");
        assert_eq!(
            run_chip(&run, 1_000 + 14 * 60_000, today),
            "#12 working · 14m"
        );
        assert_eq!(status_text("needs_attention"), "needs attention");
        assert_eq!(status_text("novel"), "novel");
    }

    #[test]
    fn reads_the_listing_the_helper_sends() {
        let listing: AutomationListing = serde_json::from_value(serde_json::json!({
            "automations": [{
                "definition": { "id": "ci", "name": "Fix CI", "enabled": true, "schedule": "0 9 * * 1-5",
                    "cwd": "~/dev", "agent": "codex", "command": "", "prompt": "go", "keepTabs": 5,
                    "maxRuntimeMinutes": 120 },
                "definitionError": "", "nextRun": "2026-10-14T09:00",
                "recentRuns": [{ "number": 7, "status": "done", "startedAt": 1, "finishedAt": 2,
                    "lastMessage": "All green." }],
                "commandLine": "codex --approve-for-me"
            }],
            "fileError": "", "generation": "g", "stateDir": "/state",
            "nextRuns": [], "scheduleError": ""
        }))
        .unwrap();
        let summary = &listing.automations[0];
        assert_eq!(
            (summary.definition.keep_tabs, command_text(summary)),
            (5, "codex --approve-for-me")
        );
        assert_eq!(summary.recent_runs[0].last_message, "All green.");
        assert_eq!(agent_text(summary), "codex");
    }
}
