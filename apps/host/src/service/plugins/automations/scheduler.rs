//! The tick: reload the definitions, follow running runs, start the ones that
//! came due, and close old tabs.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use tmux_agent_protocol::v1;

use super::{
    cron::LocalMinute,
    launch::{self, Tmux},
    store::{Definition, Definitions, Paths, Reload, Run, RunsFile, Status},
    track::{self, Reply, Tab},
};

/// A gap this long between ticks means the host slept or the helper stalled.
/// The minutes in it are skipped: automations only run while the helper does.
const MAX_TICK_GAP_SECS: i64 = 120;
/// A run this young is left alone: its command may not have started yet, and
/// the shell still in the foreground would read as the command having exited.
const SETTLE_MS: i64 = 5_000;
const ORPHAN_PROMPT_SECS: u64 = 600;

/// Which minutes have started since the last tick.
pub(super) struct Cursor {
    last_minute: i64,
    last_tick_end: i64,
}

impl Cursor {
    /// Starts at the current minute, so nothing that came due before the
    /// helper started ever fires.
    pub(super) fn new(now_secs: i64) -> Self {
        Self {
            last_minute: now_secs.div_euclid(60),
            last_tick_end: now_secs,
        }
    }

    /// The Unix minutes (seconds / 60) that began since the previous tick.
    /// Unix minutes, not local ones: the hour daylight saving time repeats is
    /// two sets of Unix minutes with the same local keys, and the caller's
    /// per-key check fires it once.
    pub(super) fn due(&mut self, now_secs: i64) -> std::ops::Range<i64> {
        let current = now_secs.div_euclid(60);
        let previous = self.last_minute;
        self.last_minute = self.last_minute.max(current);
        if now_secs - self.last_tick_end > MAX_TICK_GAP_SECS {
            return 0..0;
        }
        previous + 1..current + 1
    }

    /// Measured from the end of a tick, so a slow tick is never mistaken for
    /// the host having slept.
    pub(super) fn finish(&mut self, now_secs: i64) {
        self.last_tick_end = now_secs;
    }
}

/// Everything a tick reaches outside its own state, so tests can supply it.
pub(super) struct Env<'a> {
    pub tmux: &'a Tmux,
    pub now_ms: &'a dyn Fn() -> i64,
    pub local: &'a dyn Fn(i64) -> Option<LocalMinute>,
    pub server_identity: &'a dyn Fn() -> String,
    pub agents: &'a dyn Fn(&str) -> Vec<v1::AgentRecord>,
    /// Whether a session created now should carry the embedded Codex marker:
    /// only when it would be what starts the tmux server.
    pub embedded_codex: &'a dyn Fn() -> bool,
    pub replies: &'a dyn Fn() -> Vec<Reply>,
}

pub(super) struct State {
    paths: Paths,
    definitions: Definitions,
    runs: RunsFile,
    cursor: Cursor,
}

impl State {
    pub(super) fn new(paths: Paths, now_ms: i64) -> Self {
        let runs = RunsFile::load(&paths.runs);
        remove_orphan_prompts(&paths, &runs);
        Self {
            paths,
            definitions: Definitions::default(),
            runs,
            cursor: Cursor::new(now_ms.div_euclid(1000)),
        }
    }

    pub(super) fn tick(&mut self, env: &Env) {
        let now_ms = (env.now_ms)();
        let now_secs = now_ms.div_euclid(1000);
        let this_year = (env.local)(now_secs).map_or(1970, |minute| minute.year);
        let mut dirty = false;

        if self
            .definitions
            .reload_if_changed(&self.paths.definitions, this_year)
            == Reload::Loaded
        {
            dirty |= self.runs.sync_definitions(&self.definitions);
        }
        if self.runs.file_error != self.definitions.file_error {
            self.runs.file_error = self.definitions.file_error.clone();
            dirty = true;
        }

        dirty |= self.follow_runs(env, now_ms);

        let mut due = Vec::new();
        for minute in self.cursor.due(now_secs) {
            let Some(local) = (env.local)(minute * 60) else {
                continue;
            };
            let key = local.key();
            for definition in &self.definitions.good {
                // Per local minute, so the hour daylight saving time repeats
                // runs once.
                let already = self
                    .runs
                    .automations
                    .get(&definition.id)
                    .is_some_and(|history| history.runs.iter().any(|run| run.scheduled_for == key));
                if definition.enabled && definition.schedule.matches(&local) && !already {
                    due.push((definition.clone(), key.clone()));
                }
            }
        }
        if !due.is_empty() {
            // Run numbers continue past the open tabs too, so a lost history
            // never repeats a number retention would then sort wrongly.
            let tabs = track::observe(env.tmux).unwrap_or_default();
            let mut started = BTreeSet::new();
            for (definition, key) in due {
                let highest_tab = tabs
                    .iter()
                    .filter(|pane| pane.automation_id == definition.id)
                    .filter_map(|pane| pane.run_number)
                    .max()
                    .unwrap_or(0);
                self.start_run(env, &definition, key, highest_tab, now_ms);
                started.insert(definition.id.clone());
            }
            self.close_old_tabs(env, &started, now_ms);
            dirty = true;
        }

        if dirty {
            self.runs.prune();
            let _ = self.runs.save(&self.paths.runs);
        }
        self.cursor.finish((env.now_ms)().div_euclid(1000));
    }

    /// Updates every unfinished run from its pane, and files final messages.
    /// No tmux fork happens when nothing is unfinished.
    fn follow_runs(&mut self, env: &Env, now_ms: i64) -> bool {
        let mut changed = false;
        let unfinished = self
            .runs
            .automations
            .values()
            .flat_map(|history| &history.runs)
            .any(|run| !run.status.is_final());
        let observed = if unfinished {
            track::observe(env.tmux).map(|panes| (panes, (env.server_identity)()))
        } else {
            None
        };
        // Panes without a server identity mean tmux answered one question and
        // not the other; nothing can be told about the runs this time.
        if let Some((panes, identity)) =
            observed.filter(|(panes, identity)| panes.is_empty() || identity != "tmux:none")
        {
            let agents = (env.agents)(&identity);
            for (id, history) in &mut self.runs.automations {
                for run in &mut history.runs {
                    if run.status.is_final() || now_ms - run.started_at < SETTLE_MS {
                        continue;
                    }
                    // Launched while tmux would not say which server it was.
                    if run.server_identity == "tmux:none" {
                        run.server_identity.clone_from(&identity);
                    }
                    let pane = panes.iter().find(|pane| pane.pane_id == run.pane_id);
                    let agent = agents
                        .iter()
                        .filter(|agent| {
                            agent
                                .route
                                .as_ref()
                                .is_some_and(|route| route.pane_id == run.pane_id)
                        })
                        .max_by_key(|agent| agent.updated_at_unix_millis);
                    let next = track::next_status(
                        run,
                        pane,
                        agent,
                        run.server_identity == identity,
                        track::shell_in_foreground,
                    );
                    if next != run.status {
                        run.status = next;
                        if next.is_final() {
                            run.finished_at = now_ms;
                            // The command read it long ago.
                            let _ = std::fs::remove_file(self.paths.prompt(id, run.number));
                        }
                        changed = true;
                    }
                }
            }
        }
        let replies = (env.replies)();
        if !replies.is_empty() {
            changed |= track::file_replies(&replies, self.all_runs_mut());
        }
        changed
    }

    fn all_runs_mut(&mut self) -> impl Iterator<Item = &mut Run> {
        self.runs
            .automations
            .values_mut()
            .flat_map(|history| history.runs.iter_mut())
    }

    /// Starts one run, or records why it did not start.
    fn start_run(
        &mut self,
        env: &Env,
        definition: &Definition,
        scheduled_for: String,
        highest_tab: u32,
        now_ms: i64,
    ) {
        let number = self.runs.next_number(&definition.id).max(highest_tab + 1);
        let history = self
            .runs
            .automations
            .entry(definition.id.clone())
            .or_default();
        let mut run = Run {
            number,
            scheduled_for,
            started_at: now_ms,
            finished_at: 0,
            status: Status::Launched,
            session_id: String::new(),
            window_id: String::new(),
            pane_id: String::new(),
            server_identity: String::new(),
            last_message: String::new(),
            error: String::new(),
        };
        if let Some(active) = history
            .runs
            .iter()
            .filter(|run| track::is_active(run, now_ms, definition.max_runtime_minutes))
            .max_by_key(|run| run.number)
        {
            run.status = Status::Skipped;
            run.finished_at = now_ms;
            run.error = format!("#{} was still working", active.number);
            // One record stands for a row of skips behind the same run, so a
            // long run does not use up numbers or crowd the history.
            match history.runs.iter_mut().max_by_key(|run| run.number) {
                Some(latest) if latest.status == Status::Skipped => {
                    run.number = latest.number;
                    *latest = run;
                }
                _ => history.runs.push(run),
            }
            return;
        }
        let prompt_file = self.paths.prompt(&definition.id, number);
        let embedded_codex = (env.embedded_codex)();
        let launched = write_prompt(&self.paths.prompts, &prompt_file, &definition.prompt)
            .and_then(|()| {
                launch::launch(env.tmux, definition, number, &prompt_file, embedded_codex)
            });
        match launched {
            Ok(launched) => {
                // From when the command was typed, so a slow tick never cuts
                // into the settle time.
                run.started_at = (env.now_ms)();
                run.session_id = launched.session_id;
                run.window_id = launched.window_id;
                run.pane_id = launched.pane_id;
                run.server_identity = (env.server_identity)();
            }
            Err(error) => {
                run.status = Status::Failed;
                run.finished_at = now_ms;
                run.error = format!("{error:#}");
                let _ = std::fs::remove_file(&prompt_file);
            }
        }
        history.runs.push(run);
    }

    /// Closes each started automation's tabs beyond its newest `keepTabs`.
    fn close_old_tabs(&mut self, env: &Env, started: &BTreeSet<String>, now_ms: i64) {
        let Some(panes) = track::observe(env.tmux) else {
            return;
        };
        // Without the server's identity no agent can be told busy.
        let identity = (env.server_identity)();
        if identity == "tmux:none" {
            return;
        }
        let agents = (env.agents)(&identity);
        let busy = |pane_id: &str| {
            agents.iter().any(|agent| {
                agent
                    .route
                    .as_ref()
                    .is_some_and(|route| route.pane_id == pane_id)
                    && track::agent_busy(agent)
            })
        };
        for id in started {
            let (Some(definition), Some(history)) = (
                self.definitions
                    .good
                    .iter()
                    .find(|definition| &definition.id == id),
                self.runs.automations.get(id),
            ) else {
                continue;
            };
            let mut tabs: BTreeMap<String, Tab> = BTreeMap::new();
            for pane in panes
                .iter()
                .filter(|pane| &pane.automation_id == id && pane.untouched)
            {
                let Some(run_number) = pane.run_number else {
                    continue;
                };
                let tab = tabs.entry(pane.window_id.clone()).or_insert(Tab {
                    window_id: pane.window_id.clone(),
                    run_number,
                    busy: false,
                });
                tab.busy |= busy(&pane.pane_id);
            }
            let tabs: Vec<Tab> = tabs.into_values().collect();
            for window_id in track::windows_to_close(
                &history.runs,
                &tabs,
                definition.keep_tabs,
                now_ms,
                definition.max_runtime_minutes,
            ) {
                if let Ok(mut kill) = (env.tmux)() {
                    let _ = kill.args(["kill-window", "-t", &window_id]).output();
                }
            }
        }
    }
}

fn write_prompt(directory: &Path, file: &Path, prompt: &str) -> anyhow::Result<()> {
    use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _};
    crate::paths::prepare_runtime_dir(directory)?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(file)?;
    output.write_all(prompt.as_bytes())?;
    Ok(())
}

/// Prompt files are kept only while their run is unfinished; the rest are
/// leftovers. A recent one may belong to a run a helper being replaced is
/// still starting, so only files older than ten minutes go.
fn remove_orphan_prompts(paths: &Paths, runs: &RunsFile) {
    let Ok(entries) = std::fs::read_dir(&paths.prompts) else {
        return;
    };
    let wanted: BTreeSet<_> = runs
        .automations
        .iter()
        .flat_map(|(id, history)| {
            history
                .runs
                .iter()
                .filter(|run| !run.status.is_final())
                .map(move |run| paths.prompt(id, run.number))
        })
        .collect();
    let old = |entry: &std::fs::DirEntry| {
        entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| {
                modified
                    .elapsed()
                    .is_ok_and(|age| age.as_secs() > ORPHAN_PROMPT_SECS)
            })
    };
    for entry in entries.flatten() {
        if !wanted.contains(&entry.path()) && old(&entry) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests;
