use std::{cell::Cell, path::PathBuf};

use super::*;
use crate::service::plugins::automations::{cron::weekday, test_support::TmuxFixture};

/// 2026-10-13 09:00:10 UTC, a Tuesday, ten seconds into a minute.
const START_SECS: i64 = 1_791_882_010;

/// A fixed UTC clock, so no test depends on the machine's time zone.
fn utc_minute(unix_secs: i64) -> Option<LocalMinute> {
    let days = unix_secs.div_euclid(86_400);
    let seconds = unix_secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
    Some(LocalMinute {
        year,
        month,
        day,
        hour: (seconds / 3600) as u8,
        minute: (seconds % 3600 / 60) as u8,
        weekday: weekday(year, month, day),
    })
}

#[test]
fn the_utc_test_clock_is_right() {
    assert_eq!(utc_minute(START_SECS).unwrap().key(), "2026-10-13T09:00");
    assert_eq!(utc_minute(START_SECS).unwrap().weekday, 2);
}

#[test]
fn the_cursor_fires_nothing_at_start_and_skips_long_gaps() {
    let mut cursor = Cursor::new(START_SECS);
    assert!(
        cursor.due(START_SECS + 20).is_empty(),
        "same minute as start"
    );
    cursor.finish(START_SECS + 20);
    assert_eq!(
        cursor.due(START_SECS + 55).collect::<Vec<_>>(),
        [START_SECS / 60 + 1]
    );
    cursor.finish(START_SECS + 55);
    assert!(
        cursor.due(START_SECS + 70).is_empty(),
        "minute already handled"
    );
    cursor.finish(START_SECS + 70);
    // Asleep for ten minutes: those minutes never fire.
    assert!(cursor.due(START_SECS + 670).is_empty());
    cursor.finish(START_SECS + 670);
    assert_eq!(
        cursor.due(START_SECS + 730).collect::<Vec<_>>(),
        [START_SECS / 60 + 12]
    );
    // A tick that itself took long is not a gap: the gap counts from its end.
    cursor.finish(START_SECS + 900);
    assert_eq!(cursor.due(START_SECS + 950).count(), 4, "minutes 13 to 16");
}

struct Harness {
    fixture: TmuxFixture,
    dir: tempfile::TempDir,
    clock: Cell<i64>,
}

impl Harness {
    fn new() -> Self {
        Self {
            fixture: TmuxFixture::new(),
            dir: tempfile::tempdir().unwrap(),
            clock: Cell::new(START_SECS * 1000),
        }
    }

    fn paths(&self) -> Paths {
        Paths::under(self.dir.path())
    }

    fn work_dir(&self) -> PathBuf {
        let work = self.dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        work
    }

    fn write(&self, automations: serde_json::Value) {
        std::fs::write(
            self.paths().definitions,
            serde_json::to_vec(&serde_json::json!({"version": 1, "automations": automations}))
                .unwrap(),
        )
        .unwrap();
    }

    fn tick(&self, state: &mut State, local: &dyn Fn(i64) -> Option<LocalMinute>) {
        let factory = self.fixture.factory();
        let identity =
            || crate::service::snapshot::server_identity_from_command(factory().unwrap());
        state.tick(&Env {
            tmux: &factory,
            now_ms: &|| self.clock.get(),
            local,
            server_identity: &identity,
            agents: &|_| Vec::new(),
            embedded_codex: &|| false,
            replies: &Vec::new,
        });
    }

    /// Moves the fake clock, and lets the real shells in the fixture catch up
    /// with what the previous tick typed.
    fn advance(&self, seconds: i64) {
        self.clock.set(self.clock.get() + seconds * 1000);
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    fn runs(&self, id: &str) -> Vec<Run> {
        RunsFile::load(&self.paths().runs)
            .automations
            .get(id)
            .map(|history| history.runs.clone())
            .unwrap_or_default()
    }

    fn windows(&self) -> Vec<String> {
        let output = self
            .fixture
            .run(&["list-windows", "-a", "-F", "#{window_name}"]);
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn automation(id: &str, cwd: &std::path::Path, command: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "name": id,
        "schedule": "* * * * *",
        "cwd": cwd,
        "agent": "custom",
        "command": command,
        "prompt": "hello",
        "keepTabs": 2,
        "maxRuntimeMinutes": 5,
    })
}

#[test]
fn due_runs_start_finish_and_old_tabs_close() {
    let harness = Harness::new();
    let work = harness.work_dir();
    harness.write(serde_json::json!([automation(
        "quick",
        &work,
        "printf '%s'"
    )]));
    let mut state = State::new(harness.paths(), harness.clock.get());

    harness.tick(&mut state, &utc_minute);
    assert!(harness.runs("quick").is_empty(), "nothing fires at start");

    for _ in 0..3 {
        harness.advance(60);
        harness.tick(&mut state, &utc_minute);
    }
    let runs = harness.runs("quick");
    let numbers: Vec<u32> = runs.iter().map(|run| run.number).collect();
    assert_eq!(numbers, [1, 2, 3]);
    assert_eq!(runs[0].scheduled_for, "2026-10-13T09:01");
    assert_eq!(
        runs[0].status,
        Status::Done,
        "printf exited, the shell is back"
    );
    // Two tabs kept; the first run's tab is gone, and so is the prompt file
    // of every run that finished.
    assert_eq!(harness.windows(), ["quick #2", "quick #3"]);
    assert!(!harness.paths().prompt("quick", 1).exists());
    assert!(!harness.paths().prompt("quick", 2).exists());
    assert!(harness.paths().prompt("quick", 3).exists());

    // Closing a tab by hand closes its run.
    harness
        .fixture
        .run(&["kill-window", "-t", &runs[2].window_id]);
    harness.advance(10);
    harness.tick(&mut state, &utc_minute);
    assert_eq!(harness.runs("quick")[2].status, Status::Closed);
}

#[test]
fn a_run_still_working_skips_the_next_until_its_limit() {
    let harness = Harness::new();
    let work = harness.work_dir();
    // A script run by the same shell as the tab's: tmux names both `sh`, so
    // only the process table tells it is still going.
    let script = work.join("slow.sh");
    std::fs::write(&script, "sleep 600\ntrue\n").unwrap();
    harness.write(serde_json::json!([automation(
        "slow",
        &work,
        &format!("sh {}", script.display())
    )]));
    let mut state = State::new(harness.paths(), harness.clock.get());
    harness.tick(&mut state, &utc_minute);
    for _ in 0..7 {
        harness.advance(60);
        harness.tick(&mut state, &utc_minute);
    }
    let runs = harness.runs("slow");
    let summary: Vec<(u32, Status)> = runs.iter().map(|run| (run.number, run.status)).collect();
    // Run 1 starts at minute 1 and blocks minutes 2 to 5, which share one
    // skipped record; at minute 6 it has used its five minutes and no longer
    // blocks, so run 3 starts.
    assert_eq!(
        summary,
        [
            (1, Status::Working),
            (2, Status::Skipped),
            (3, Status::Working),
            (4, Status::Skipped),
        ]
    );
    assert_eq!(runs[1].error, "#1 was still working");
    assert_eq!(runs[1].scheduled_for, "2026-10-13T09:05");
    assert_eq!(runs[3].error, "#3 was still working");
}

#[test]
fn a_repeated_local_minute_fires_once_and_failures_are_recorded() {
    let harness = Harness::new();
    harness.write(serde_json::json!([automation(
        "missing",
        std::path::Path::new("/definitely/not/here"),
        "true"
    )]));
    let mut state = State::new(harness.paths(), harness.clock.get());
    // Daylight saving time ends: every minute reads as the same local minute.
    let repeated = |_| utc_minute(START_SECS + 60);
    harness.tick(&mut state, &repeated);
    for _ in 0..3 {
        harness.advance(60);
        harness.tick(&mut state, &repeated);
    }
    let runs = harness.runs("missing");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, Status::Failed);
    assert!(
        runs[0].error.contains("does not exist"),
        "{}",
        runs[0].error
    );
}

#[test]
fn a_broken_file_keeps_the_last_set_and_reports_why() {
    let harness = Harness::new();
    let work = harness.work_dir();
    let mut paused = automation("paused", &work, "true");
    paused["enabled"] = false.into();
    harness.write(serde_json::json!([
        automation("quick", &work, "true"),
        paused
    ]));
    let mut state = State::new(harness.paths(), harness.clock.get());
    harness.tick(&mut state, &utc_minute);

    std::fs::write(harness.paths().definitions, b"{\"version\": 1,").unwrap();
    harness.advance(60);
    harness.tick(&mut state, &utc_minute);
    let file = RunsFile::load(&harness.paths().runs);
    assert!(
        file.file_error.contains("not valid JSON"),
        "{}",
        file.file_error
    );
    assert_eq!(
        harness.runs("quick").len(),
        1,
        "the last good set still runs"
    );
    assert!(
        harness.runs("paused").is_empty(),
        "a disabled automation never runs"
    );

    harness.write(serde_json::json!([automation("quick", &work, "true")]));
    harness.advance(60);
    harness.tick(&mut state, &utc_minute);
    assert_eq!(RunsFile::load(&harness.paths().runs).file_error, "");
}

#[test]
fn run_numbers_continue_past_open_tabs_when_history_is_lost() {
    let harness = Harness::new();
    let work = harness.work_dir();
    let quick = automation("quick", &work, "true");
    harness.write(serde_json::json!([quick.clone()]));
    let mut state = State::new(harness.paths(), harness.clock.get());
    harness.tick(&mut state, &utc_minute);
    for _ in 0..2 {
        harness.advance(60);
        harness.tick(&mut state, &utc_minute);
    }
    // Both runs finish; then it is removed and added again: its history
    // goes, its tabs stay.
    harness.advance(10);
    harness.tick(&mut state, &utc_minute);
    harness.write(serde_json::json!([]));
    harness.advance(10);
    harness.tick(&mut state, &utc_minute);
    assert!(harness.runs("quick").is_empty());
    harness.write(serde_json::json!([quick]));
    harness.advance(60);
    harness.tick(&mut state, &utc_minute);
    let numbers: Vec<u32> = harness.runs("quick").iter().map(|run| run.number).collect();
    assert_eq!(numbers, [3]);
    assert_eq!(harness.windows(), ["quick #2", "quick #3"]);
}
