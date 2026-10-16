//! What became of runs already started: their status, their final message,
//! whether one still blocks the next, and which old tabs to close.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use tmux_agent_protocol::v1;

use super::{
    launch::{AUTOMATION_TAG, RUN_TAG, Tmux, WORKSPACE_TAG},
    store::{LAST_MESSAGE_MAX_BYTES, Run, Status, truncated},
};

/// One pane as the observation fork reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Pane {
    pub window_id: String,
    pub pane_id: String,
    pub pane_pid: u32,
    pub automation_id: String,
    pub run_number: Option<u32>,
    /// The window is a lone pane in the automation workspace: one the user
    /// has split or moved elsewhere is theirs now.
    pub untouched: bool,
}

/// Every pane on the server, in one fork: none when no server runs, and `None`
/// when tmux could not be asked, so a failure never reads as every tab having
/// closed.
pub(super) fn observe(tmux: &Tmux) -> Option<Vec<Pane>> {
    let output = tmux()
        .ok()?
        .args([
            "list-panes",
            "-a",
            "-F",
            &format!(
                "#{{window_id}}\t#{{pane_id}}\t#{{pane_pid}}\t#{{{AUTOMATION_TAG}}}\t#{{{RUN_TAG}}}\t#{{&&:#{{{WORKSPACE_TAG}}},#{{==:#{{window_panes}},1}}}}"
            ),
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        let error = anyhow::anyhow!("{}", String::from_utf8_lossy(&output.stderr));
        return crate::service::tmux_config::is_no_server(&error).then(Vec::new);
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let fields: Vec<&str> = line.split('\t').collect();
                let [
                    window_id,
                    pane_id,
                    pane_pid,
                    automation_id,
                    run_number,
                    untouched,
                ] = fields.as_slice()
                else {
                    return None;
                };
                Some(Pane {
                    window_id: (*window_id).to_owned(),
                    pane_id: (*pane_id).to_owned(),
                    pane_pid: pane_pid.parse().unwrap_or_default(),
                    automation_id: (*automation_id).to_owned(),
                    run_number: run_number.parse().ok(),
                    untouched: *untouched == "1",
                })
            })
            .collect(),
    )
}

/// Whether the pane's shell is its terminal's foreground job again, which is
/// what a finished command looks like. Asked of the process table rather than
/// read from tmux's command name, which says `bash` while a bash script runs.
/// `None` when it cannot be told.
pub(super) fn shell_in_foreground(pane_pid: u32) -> Option<bool> {
    if pane_pid == 0 {
        return None;
    }
    let output = std::process::Command::new("ps")
        .args(["-o", "pgid=,tpgid=", "-p", &pane_pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut fields = text.split_whitespace().map(str::parse::<i64>);
    match (fields.next(), fields.next()) {
        (Some(Ok(group)), Some(Ok(foreground))) => Some(group == foreground),
        _ => None,
    }
}

/// Whether an agent is at work or waiting on someone.
pub(super) fn agent_busy(agent: &v1::AgentRecord) -> bool {
    let lifecycle = v1::AgentLifecycleState::try_from(agent.lifecycle).unwrap_or_default();
    matches!(
        lifecycle,
        v1::AgentLifecycleState::Working | v1::AgentLifecycleState::Blocked
    ) || agent.attention_kind == "blocked"
}

/// A run's status from what its pane shows now.
///
/// Agent hooks decide when the pane has an agent with a known lifecycle.
/// Without one, the process table does: the shell back in the foreground means
/// the command exited. A pane that is gone, or a tmux server that is not the
/// one the run started on, means the tab closed.
pub(super) fn next_status(
    run: &Run,
    pane: Option<&Pane>,
    agent: Option<&v1::AgentRecord>,
    same_server: bool,
    shell_in_foreground: impl FnOnce(u32) -> Option<bool>,
) -> Status {
    if run.status.is_final() {
        return run.status;
    }
    let Some(pane) = pane.filter(|_| same_server) else {
        return Status::Closed;
    };
    if let Some(agent) = agent {
        let lifecycle = v1::AgentLifecycleState::try_from(agent.lifecycle).unwrap_or_default();
        if agent.attention_kind == "blocked" || lifecycle == v1::AgentLifecycleState::Blocked {
            return Status::NeedsAttention;
        }
        match lifecycle {
            v1::AgentLifecycleState::Working => return Status::Working,
            v1::AgentLifecycleState::Idle
                if agent.attention_kind == "completed"
                    && agent.lifecycle_changed_at_unix_millis >= run.started_at =>
            {
                return Status::Done;
            }
            v1::AgentLifecycleState::Idle => return run.status,
            _ => {}
        }
    }
    match shell_in_foreground(pane.pane_pid) {
        Some(true) => Status::Done,
        Some(false) => Status::Working,
        None => run.status,
    }
}

/// Whether a run still holds off the next scheduled one and keeps its tab.
/// `max_runtime_minutes` bounds both, so a status that never clears, such as
/// an agent without hooks or a question nobody answers, cannot block runs or
/// keep tabs forever.
pub(super) fn is_active(run: &Run, now_ms: i64, max_runtime_minutes: u32) -> bool {
    matches!(
        run.status,
        Status::Launched | Status::Working | Status::NeedsAttention
    ) && now_ms - run.started_at < i64::from(max_runtime_minutes) * 60_000
}

/// One automation tab as retention sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Tab {
    pub window_id: String,
    pub run_number: u32,
    /// An agent in it is at work or waiting: someone may have picked the
    /// conversation up again.
    pub busy: bool,
}

/// The tabs of one automation to close: everything beyond the newest `keep`
/// whose run is no longer active and where no agent is busy. A tab with no run
/// on record is an old one.
pub(super) fn windows_to_close(
    runs: &[Run],
    tabs: &[Tab],
    keep: u32,
    now_ms: i64,
    max_runtime_minutes: u32,
) -> Vec<String> {
    let mut tabs = tabs.to_vec();
    tabs.sort_by_key(|tab| std::cmp::Reverse(tab.run_number));
    let runs: BTreeMap<u32, &Run> = runs.iter().map(|run| (run.number, run)).collect();
    tabs.into_iter()
        .skip(keep as usize)
        .filter(|tab| {
            !tab.busy
                && runs
                    .get(&tab.run_number)
                    .is_none_or(|run| !is_active(run, now_ms, max_runtime_minutes))
        })
        .map(|tab| tab.window_id)
        .collect()
}

const REPLY_QUEUE_MAX: usize = 64;

/// Final messages from `Stop` hooks, waiting for the next tick to file them.
static REPLIES: Mutex<VecDeque<Reply>> = Mutex::new(VecDeque::new());

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Reply {
    pub server_identity: String,
    pub pane_id: String,
    pub text: String,
}

/// Called under ingest's ordering lock: record and return, and never panic on
/// a poisoned lock, which would take hook ingest down with it.
pub(super) fn queue_reply(reply: &crate::service::voice::AgentReply) {
    let mut queue = REPLIES.lock().unwrap_or_else(PoisonError::into_inner);
    if queue.len() == REPLY_QUEUE_MAX {
        queue.pop_front();
    }
    queue.push_back(Reply {
        server_identity: reply.server_identity.clone(),
        pane_id: reply.pane_id.clone(),
        text: truncated(&reply.text, LAST_MESSAGE_MAX_BYTES),
    });
}

pub(super) fn take_replies() -> Vec<Reply> {
    REPLIES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .drain(..)
        .collect()
}

/// Files each reply on the run whose pane it came from. Pane ids are never
/// reused within one tmux server, so the pane and server name the run.
pub(super) fn file_replies<'a>(replies: &[Reply], runs: impl Iterator<Item = &'a mut Run>) -> bool {
    let mut changed = false;
    let mut runs: Vec<&mut Run> = runs.collect();
    for reply in replies {
        if let Some(run) = runs.iter_mut().find(|run| {
            run.pane_id == reply.pane_id
                && run.server_identity == reply.server_identity
                && !run.pane_id.is_empty()
        }) && run.last_message != reply.text
        {
            run.last_message = reply.text.clone();
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(number: u32, status: Status) -> Run {
        Run {
            number,
            scheduled_for: String::new(),
            started_at: 1_000,
            finished_at: 0,
            status,
            session_id: "$1".into(),
            window_id: format!("@{number}"),
            pane_id: format!("%{number}"),
            server_identity: "tmux:a".into(),
            last_message: String::new(),
            error: String::new(),
        }
    }

    fn pane() -> Pane {
        Pane {
            window_id: "@1".into(),
            pane_id: "%1".into(),
            pane_pid: 42,
            automation_id: "ci".into(),
            run_number: Some(1),
            untouched: true,
        }
    }

    fn agent(
        lifecycle: v1::AgentLifecycleState,
        attention: &str,
        changed_at: i64,
    ) -> v1::AgentRecord {
        v1::AgentRecord {
            lifecycle: lifecycle.into(),
            attention_kind: attention.into(),
            lifecycle_changed_at_unix_millis: changed_at,
            ..Default::default()
        }
    }

    #[test]
    fn status_follows_the_agent_then_the_foreground_job() {
        use v1::AgentLifecycleState::{Blocked, Idle, Unknown, Working};
        let launched = run(1, Status::Launched);
        let busy = |_| Some(false);
        let cases = [
            (Some(agent(Working, "", 2_000)), Status::Working),
            (
                Some(agent(Blocked, "blocked", 2_000)),
                Status::NeedsAttention,
            ),
            (Some(agent(Idle, "blocked", 2_000)), Status::NeedsAttention),
            (Some(agent(Idle, "completed", 2_000)), Status::Done),
            // A completion from before this run started is not this run's.
            (Some(agent(Idle, "completed", 500)), Status::Launched),
            (Some(agent(Idle, "", 2_000)), Status::Launched),
            // Without a lifecycle the process table decides.
            (Some(agent(Unknown, "", 2_000)), Status::Working),
            (None, Status::Working),
        ];
        for (record, expected) in cases {
            assert_eq!(
                next_status(&launched, Some(&pane()), record.as_ref(), true, busy),
                expected,
                "{record:?}"
            );
        }
        let shell_back = |pid| {
            assert_eq!(pid, 42);
            Some(true)
        };
        assert_eq!(
            next_status(&launched, Some(&pane()), None, true, shell_back),
            Status::Done
        );
        assert_eq!(
            next_status(&launched, Some(&pane()), None, true, |_| None),
            Status::Launched,
            "unknown leaves the status alone"
        );
        assert_eq!(
            next_status(&launched, None, None, true, busy),
            Status::Closed
        );
        assert_eq!(
            next_status(&launched, Some(&pane()), None, false, busy),
            Status::Closed
        );
        for done in [
            Status::Done,
            Status::Failed,
            Status::Skipped,
            Status::Closed,
        ] {
            assert_eq!(
                next_status(&run(1, done), None, None, false, busy),
                done,
                "final stays"
            );
        }
    }

    #[test]
    fn the_process_table_tells_a_running_job_from_an_idle_shell() {
        use std::os::unix::process::CommandExt as _;
        // A process in a group of its own is never a terminal's foreground
        // job, whether or not the tests run in one.
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .process_group(0)
            .spawn()
            .unwrap();
        assert_eq!(shell_in_foreground(child.id()), Some(false));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(shell_in_foreground(0), None);
    }

    #[test]
    fn a_run_blocks_the_next_until_it_finishes_or_runs_too_long() {
        let minutes = 60_000;
        for (status, age, expected) in [
            (Status::Launched, 0, true),
            (Status::Working, 119 * minutes, true),
            (Status::NeedsAttention, minutes, true),
            (Status::Working, 120 * minutes, false),
            (Status::Done, 0, false),
            (Status::Closed, 0, false),
            (Status::Skipped, 0, false),
        ] {
            let mut candidate = run(1, status);
            candidate.started_at = 0;
            assert_eq!(
                is_active(&candidate, age, 120),
                expected,
                "{status:?} at {age}"
            );
        }
    }

    #[test]
    fn retention_closes_old_idle_tabs_only() {
        let mut overdue = run(3, Status::Working);
        overdue.started_at = 2_000 - 120 * 60_000;
        let runs = [
            run(1, Status::Done),
            run(2, Status::Working),
            overdue,
            run(4, Status::Done),
            run(5, Status::Launched),
            run(6, Status::Done),
        ];
        let tab = |number: u32, busy: bool| Tab {
            window_id: format!("@{number}"),
            run_number: number,
            busy,
        };
        let tabs = [
            tab(1, false),
            tab(2, false),
            tab(3, false),
            tab(4, false),
            tab(5, false),
            tab(6, true),
            tab(9, false),
        ];
        // Keep the newest two, 9 and 6. Of the rest, 5 and 2 are still going,
        // 3 has run past its limit, and 4 and 1 finished.
        assert_eq!(
            windows_to_close(&runs, &tabs, 2, 2_000, 120),
            ["@4", "@3", "@1"]
        );
        // 6 finished, but an agent in it is busy again.
        assert_eq!(
            windows_to_close(&runs, &tabs, 0, 2_000, 120),
            ["@9", "@4", "@3", "@1"],
            "9 has no record"
        );
        assert!(windows_to_close(&runs, &tabs, 7, 2_000, 120).is_empty());
    }

    #[test]
    fn replies_land_on_the_run_in_their_pane() {
        let mut runs = [run(1, Status::Done), run(2, Status::Working)];
        let replies = [
            Reply {
                server_identity: "tmux:a".into(),
                pane_id: "%2".into(),
                text: "All green.".into(),
            },
            Reply {
                server_identity: "tmux:other".into(),
                pane_id: "%1".into(),
                text: "not ours".into(),
            },
        ];
        assert!(file_replies(&replies, runs.iter_mut()));
        assert_eq!(runs[1].last_message, "All green.");
        assert_eq!(runs[0].last_message, "");
        assert!(
            !file_replies(&replies, runs.iter_mut()),
            "the same reply changes nothing"
        );
    }

    #[test]
    fn the_reply_queue_is_bounded() {
        take_replies();
        for index in 0..REPLY_QUEUE_MAX + 10 {
            queue_reply(&crate::service::voice::AgentReply {
                server_identity: "tmux:a".into(),
                pane_id: format!("%{index}"),
                text: "x".repeat(LAST_MESSAGE_MAX_BYTES + 10),
                truncated: false,
                state_generation: 0,
                occurred_at_unix_millis: 0,
            });
        }
        let replies = take_replies();
        assert_eq!(replies.len(), REPLY_QUEUE_MAX);
        assert_eq!(replies[0].pane_id, "%10", "the oldest were dropped");
        assert_eq!(replies[0].text.len(), LAST_MESSAGE_MAX_BYTES);
    }
}
