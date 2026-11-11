//! The sidebar's agents section: a flat list across every workspace, with one
//! control, the order it is in. Port of `agentsList.ts` and
//! `useRecentIdleClock.ts`'s deadline.
//!
//! `Workspace` follows the workspace list above it. `Status` is the visual
//! queue: blocked, working, recent, then older idle, each following its last
//! real lifecycle transition rather than every hook update. `Pinned` is the
//! same queue split in two: everything pinned, by tab or by workspace, above
//! everything that is not.

use std::cmp::Ordering;

use desktop_core::app_state::AgentSortMode;

use super::model::{AgentRecord, AttentionKind, DisplayState, compare_agents, display_state};

/// How long an acknowledged completed agent stays near the active work.
pub const RECENT_IDLE_WINDOW_MILLIS: u64 = 4 * 60 * 60 * 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PriorityBucket {
    Blocked,
    Working,
    Recent,
    Idle,
}

/// The four buckets the priority order draws, in the order they are worth your
/// attention. `Unknown` shares Idle's: a gap in reporting is not a fifth thing
/// an agent can be busy with.
pub const STATUS_GROUPS: [(PriorityBucket, &str, DisplayState); 4] = [
    (PriorityBucket::Blocked, "Blocked", DisplayState::Blocked),
    (PriorityBucket::Working, "Working", DisplayState::Working),
    (PriorityBucket::Recent, "Recent", DisplayState::Idle),
    (PriorityBucket::Idle, "Idle", DisplayState::Idle),
];

/// Where a row sits in the workspace list and in its workspace's tab strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLocation {
    pub workspace_order: usize,
    pub workspace_name: String,
    /// User-facing host identity, needed when workspace names collide.
    pub host_label: String,
    /// The host mark drawn before the name; empty draws none.
    pub host_letter: String,
    pub tab_index: Option<u32>,
    pub workspace_pinned: bool,
    pub tab_pinned: bool,
}

impl AgentLocation {
    /// A workspace the list does not show: kept last instead of dropped.
    pub fn unlisted(agent: &AgentRecord) -> Self {
        Self {
            workspace_order: usize::MAX,
            workspace_name: agent.session_name.clone(),
            host_label: String::new(),
            host_letter: String::new(),
            tab_index: None,
            workspace_pinned: false,
            tab_pinned: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentListRow {
    pub agent: AgentRecord,
    pub state: DisplayState,
    /// The priority heading this row belongs under; Recent still draws Idle.
    pub bucket: PriorityBucket,
    pub location: AgentLocation,
    /// A row is only clickable when it resolves to an exact live pane.
    pub routable: bool,
    /// Whether this agent's tab is pinned. Workspace pins order workspaces.
    pub pinned: bool,
}

impl AgentListRow {
    /// Its own tab pin, or its workspace's: what the pinned ordering reads.
    pub fn is_pinned(&self) -> bool {
        self.pinned || self.location.workspace_pinned
    }
}

/// Blocked, or finished without its pane having been looked at: the rows the
/// jump shortcut goes to and the bell counts.
pub fn needs_attention(state: DisplayState) -> bool {
    matches!(state, DisplayState::Blocked | DisplayState::Done)
}

pub fn build_agent_rows<'a>(
    agents: impl IntoIterator<Item = &'a AgentRecord>,
    locate: impl Fn(&AgentRecord) -> AgentLocation,
    routable: impl Fn(&AgentRecord) -> bool,
    mode: AgentSortMode,
    now: u64,
) -> Vec<AgentListRow> {
    let mut rows: Vec<_> = agents
        .into_iter()
        .map(|agent| {
            let location = locate(agent);
            let state = display_state(agent);
            AgentListRow {
                state,
                bucket: priority_bucket(agent, state, now),
                routable: routable(agent),
                pinned: location.tab_pinned,
                location,
                agent: agent.clone(),
            }
        })
        .collect();
    rows.sort_by(match mode {
        AgentSortMode::Status => by_status,
        AgentSortMode::Pinned => by_pin,
        AgentSortMode::Workspace => by_workspace,
    });
    rows
}

fn by_pin(left: &AgentListRow, right: &AgentListRow) -> Ordering {
    right
        .is_pinned()
        .cmp(&left.is_pinned())
        .then_with(|| by_status(left, right))
}

/// Status is the primary queue. A tab pin leads only its own status; inside
/// peers only a real lifecycle transition changes recency, so repeated hooks
/// and route-only updates cannot reshuffle rows.
fn by_status(left: &AgentListRow, right: &AgentListRow) -> Ordering {
    rank(left.bucket)
        .cmp(&rank(right.bucket))
        .then(right.pinned.cmp(&left.pinned))
        .then(
            right
                .agent
                .lifecycle_changed_at
                .cmp(&left.agent.lifecycle_changed_at),
        )
        .then_with(|| left.agent.id.cmp(&right.agent.id))
}

fn by_workspace(left: &AgentListRow, right: &AgentListRow) -> Ordering {
    let (l, r) = (&left.location, &right.location);
    l.workspace_order
        .cmp(&r.workspace_order)
        .then_with(|| l.workspace_name.cmp(&r.workspace_name))
        // Keep a workspace contiguous even when same-named workspaces on two
        // hosts share a display order.
        .then_with(|| left.agent.host_profile_id.cmp(&right.agent.host_profile_id))
        .then_with(|| left.agent.server_identity.cmp(&right.agent.server_identity))
        .then_with(|| left.agent.session_id.cmp(&right.agent.session_id))
        // Inside one workspace a pinned tab's agents lead, as in its strip.
        .then(r.tab_pinned.cmp(&l.tab_pinned))
        .then(
            l.tab_index
                .unwrap_or(u32::MAX)
                .cmp(&r.tab_index.unwrap_or(u32::MAX)),
        )
        .then_with(|| left.agent.display_name.cmp(&right.agent.display_name))
        .then_with(|| left.agent.id.cmp(&right.agent.id))
}

fn rank(bucket: PriorityBucket) -> usize {
    STATUS_GROUPS
        .iter()
        .position(|(key, ..)| *key == bucket)
        .unwrap_or(usize::MAX)
}

fn priority_bucket(agent: &AgentRecord, state: DisplayState, now: u64) -> PriorityBucket {
    match state {
        DisplayState::Blocked => PriorityBucket::Blocked,
        DisplayState::Working => PriorityBucket::Working,
        // Completion attention changes only the row's dot, so acknowledging it
        // does not move the row out from under the pointer.
        DisplayState::Done => PriorityBucket::Recent,
        _ => match recent_agent_expiration(agent) {
            Some(expiration) if now < expiration => PriorityBucket::Recent,
            _ => PriorityBucket::Idle,
        },
    }
}

/// The one deadline at which an idle Recent row becomes old. Unread
/// completions stay Recent until acknowledgement supplies a clock.
pub fn recent_agent_expiration(agent: &AgentRecord) -> Option<u64> {
    if display_state(agent) != DisplayState::Idle {
        return None;
    }
    let since =
        if agent.attention_kind == Some(AttentionKind::Completed) && agent.attention_seen_at > 0 {
            agent.attention_seen_at
        } else {
            agent.lifecycle_changed_at
        };
    Some(since.saturating_add(RECENT_IDLE_WINDOW_MILLIS))
}

/// When the priority list next needs a redraw with no event at all: the
/// nearest Recent row turning Idle.
pub fn next_recent_expiration<'a>(
    agents: impl IntoIterator<Item = &'a AgentRecord>,
    now: u64,
) -> Option<u64> {
    agents
        .into_iter()
        .filter_map(recent_agent_expiration)
        .filter(|expiration| *expiration > now)
        .min()
}

pub struct WorkspaceGroup<'a> {
    pub key: String,
    pub workspace_name: String,
    pub host_label: String,
    pub pinned: bool,
    pub rows: Vec<&'a AgentListRow>,
}

/// Groups an already workspace-ordered list without re-sorting its rows. The
/// server identity is in the key because a reconnect can reuse a profile and
/// session id while referring to a different tmux server.
pub fn group_by_workspace(rows: &[AgentListRow]) -> Vec<WorkspaceGroup<'_>> {
    let mut groups: Vec<WorkspaceGroup<'_>> = Vec::new();
    for row in rows {
        let key = [
            row.agent.host_profile_id.as_str(),
            &row.agent.server_identity,
            &row.agent.session_id,
        ]
        .join("\0");
        match groups.iter_mut().find(|group| group.key == key) {
            Some(group) => group.rows.push(row),
            None => {
                let host_label = [&row.location.host_label, &row.agent.host_profile_id]
                    .into_iter()
                    .find(|label| !label.is_empty())
                    .cloned()
                    .unwrap_or_else(|| "unknown host".into());
                groups.push(WorkspaceGroup {
                    key,
                    workspace_name: row.location.workspace_name.clone(),
                    host_label,
                    pinned: row.location.workspace_pinned,
                    rows: vec![row],
                });
            }
        }
    }
    groups
}

/// Buckets an already status-ordered list without re-sorting it. An empty
/// bucket is absent, not an empty heading.
pub fn group_by_status(
    rows: &[AgentListRow],
) -> Vec<(&'static str, DisplayState, Vec<&AgentListRow>)> {
    STATUS_GROUPS
        .iter()
        .map(|(bucket, label, state)| {
            (
                *label,
                *state,
                rows.iter()
                    .filter(|row| row.bucket == *bucket)
                    .collect::<Vec<_>>(),
            )
        })
        .filter(|(.., rows)| !rows.is_empty())
        .collect()
}

/// Splits an already pin-ordered list under the two dividers.
pub fn group_by_pin(rows: &[AgentListRow]) -> Vec<(&'static str, Vec<&AgentListRow>)> {
    [("Pinned", true), ("Unpinned", false)]
        .into_iter()
        .map(|(label, pinned)| {
            (
                label,
                rows.iter()
                    .filter(|row| row.is_pinned() == pinned)
                    .collect::<Vec<_>>(),
            )
        })
        .filter(|(_, rows)| !rows.is_empty())
        .collect()
}

/// The row the jump shortcut and the bell go to: the loudest row that wants
/// attention and can be reached. Deliberately `compare_agents`, not the list's
/// order: pins organise peers, while this answers only which agent needs a
/// human most.
pub fn jump_target(rows: &[AgentListRow]) -> Option<&AgentListRow> {
    let mut sorted: Vec<_> = rows.iter().collect();
    sorted.sort_by(|left, right| compare_agents(&left.agent, &right.agent));
    sorted
        .into_iter()
        .find(|row| row.routable && needs_attention(row.state))
}

/// How many rows are waiting on a human: the number on the bell.
pub fn unread_count(rows: &[AgentListRow]) -> usize {
    rows.iter().filter(|row| needs_attention(row.state)).count()
}

/// The one agent row the active pane represents. A duplicate is only the short
/// interval where process detection and a native hook both describe a pane:
/// the native identity wins, then the freshest, then the immutable id.
pub fn selected_agent_id_for_pane<'a>(
    rows: &'a [AgentListRow],
    active_pane_id: Option<&str>,
    active_host_profile_id: Option<&str>,
) -> Option<&'a str> {
    let (pane, host) = (active_pane_id?, active_host_profile_id?);
    let native = |row: &AgentListRow| {
        !row.agent.detected_manually && !row.agent.native_session_id.trim().is_empty()
    };
    rows.iter()
        .filter(|row| {
            row.routable && row.agent.host_profile_id == host && row.agent.pane_id == pane
        })
        .min_by(|left, right| {
            native(right)
                .cmp(&native(left))
                .then(right.agent.updated_at.cmp(&left.agent.updated_at))
                .then_with(|| left.agent.id.cmp(&right.agent.id))
        })
        .map(|row| row.agent.id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::model::Lifecycle;
    use crate::agents::model::fixtures::agent;

    fn rec(id: &str, f: impl FnOnce(&mut AgentRecord)) -> AgentRecord {
        let mut record = AgentRecord {
            id: id.into(),
            ..agent()
        };
        f(&mut record);
        record
    }

    fn locate(record: &AgentRecord) -> AgentLocation {
        let at = |order, name: &str, tab| AgentLocation {
            workspace_order: order,
            workspace_name: name.into(),
            tab_index: Some(tab),
            ..AgentLocation::unlisted(record)
        };
        match record.session_id.as_str() {
            "$1" => at(0, "muxflow", 1),
            "$2" => at(1, "project-e2e", 2),
            _ => AgentLocation::unlisted(record),
        }
    }

    fn working() -> AgentRecord {
        rec("working", |a| {
            a.display_name = "claude".into();
            a.updated_at = 30;
        })
    }
    fn blocked() -> AgentRecord {
        rec("blocked", |a| {
            a.display_name = "codex".into();
            a.lifecycle = Lifecycle::Blocked;
            a.updated_at = 20;
        })
    }
    fn done() -> AgentRecord {
        rec("done", |a| {
            a.display_name = "claude two".into();
            a.lifecycle = Lifecycle::Idle;
            a.attention_kind = Some(AttentionKind::Completed);
            a.attention_generation = 4;
            a.seen_generation = 2;
            a.session_id = "$2".into();
            a.updated_at = 10;
        })
    }
    fn idle() -> AgentRecord {
        rec("idle", |a| {
            a.display_name = "claude three".into();
            a.lifecycle = Lifecycle::Idle;
            a.session_id = "$2".into();
            a.updated_at = 5;
        })
    }
    fn four() -> Vec<AgentRecord> {
        vec![idle(), working(), done(), blocked()]
    }

    // The TS tests build at `Date.now()`, long after every fixture changed.
    const NOW: u64 = 10 * RECENT_IDLE_WINDOW_MILLIS;

    fn build(agents: &[AgentRecord], mode: AgentSortMode) -> Vec<AgentListRow> {
        build_agent_rows(agents, locate, |_| true, mode, NOW)
    }

    fn ids(rows: &[AgentListRow]) -> Vec<&str> {
        rows.iter().map(|row| row.agent.id.as_str()).collect()
    }

    fn ids_of<'a>(rows: &[&'a AgentListRow]) -> Vec<&'a str> {
        rows.iter().map(|row| row.agent.id.as_str()).collect()
    }

    #[test]
    fn ranks_the_visual_queue_blocked_working_recent_completion_idle() {
        assert_eq!(
            ids(&build(&four(), AgentSortMode::Status)),
            ["blocked", "working", "done", "idle"]
        );
    }

    #[test]
    fn follows_the_workspace_list_then_tab_order_in_workspace_mode() {
        let rows = build(
            &[done(), idle(), blocked(), working()],
            AgentSortMode::Workspace,
        );
        let names: Vec<_> = rows
            .iter()
            .map(|row| row.location.workspace_name.as_str())
            .collect();
        assert_eq!(names, ["muxflow", "muxflow", "project-e2e", "project-e2e"]);
        let tail: Vec<_> = rows[2..]
            .iter()
            .map(|row| row.agent.display_name.as_str())
            .collect();
        assert_eq!(tail, ["claude three", "claude two"]);
    }

    #[test]
    fn sends_the_jump_to_the_loudest_reachable_agent_that_actually_wants_something() {
        let mut rows = build(&four(), AgentSortMode::Workspace);
        assert_eq!(jump_target(&rows).unwrap().agent.id, "blocked");
        assert_eq!(
            jump_target(&build(&four(), AgentSortMode::Status))
                .unwrap()
                .agent
                .id,
            "blocked"
        );
        for row in &mut rows {
            if row.agent.id == "blocked" {
                row.routable = false;
            }
        }
        assert_eq!(jump_target(&rows).unwrap().agent.id, "done");
        assert!(jump_target(&build(&[idle(), working()], AgentSortMode::Status)).is_none());
    }

    #[test]
    fn puts_blocked_ahead_of_done_unread_whatever_the_timestamps_say() {
        let stale = rec("blocked", |a| {
            a.lifecycle = Lifecycle::Blocked;
            a.updated_at = 1;
        });
        let fresh = rec("done", |a| {
            a.lifecycle = Lifecycle::Idle;
            a.attention_kind = Some(AttentionKind::Completed);
            a.attention_generation = 4;
            a.seen_generation = 2;
            a.session_id = "$2".into();
            a.updated_at = 99;
        });
        for mode in [AgentSortMode::Status, AgentSortMode::Workspace] {
            let rows = build(&[fresh.clone(), stale.clone()], mode);
            assert_eq!(jump_target(&rows).unwrap().agent.id, "blocked");
        }
    }

    #[test]
    fn counts_only_the_rows_waiting_on_a_human_as_unread() {
        assert_eq!(unread_count(&build(&four(), AgentSortMode::Status)), 2);
        assert!(!needs_attention(DisplayState::Working));
        assert!(needs_attention(DisplayState::Done));
        assert!(needs_attention(DisplayState::Blocked));
    }

    #[test]
    fn buckets_the_priority_order_without_a_separate_done_group() {
        let unknown = rec("unknown", |a| a.lifecycle = Lifecycle::Unknown);
        let mut agents = four();
        agents.push(unknown);
        let rows = build(&agents, AgentSortMode::Status);
        let groups = group_by_status(&rows);
        let shape: Vec<_> = groups
            .iter()
            .map(|(label, _, rows)| (*label, ids_of(rows)))
            .collect();
        assert_eq!(
            shape,
            [
                ("Blocked", vec!["blocked"]),
                ("Working", vec!["working"]),
                ("Recent", vec!["done"]),
                ("Idle", vec!["idle", "unknown"]),
            ]
        );
        let states: Vec<_> = groups.iter().map(|(_, state, _)| *state).collect();
        assert_eq!(
            states,
            [
                DisplayState::Blocked,
                DisplayState::Working,
                DisplayState::Idle,
                DisplayState::Idle
            ]
        );
        let only = build(&[working()], AgentSortMode::Status);
        assert_eq!(group_by_status(&only).len(), 1);
        assert!(group_by_status(&[]).is_empty());
        assert_eq!(
            groups.iter().map(|(.., rows)| rows.len()).sum::<usize>(),
            rows.len()
        );
    }

    #[test]
    fn orders_every_status_by_its_last_lifecycle_change_not_its_latest_hook_update() {
        let first = rec("first", |a| {
            a.display_name = "z".into();
            a.updated_at = 10;
            a.lifecycle_changed_at = 300;
        });
        let second = rec("second", |a| {
            a.display_name = "a".into();
            a.updated_at = 999;
            a.lifecycle_changed_at = 200;
        });
        assert_eq!(
            ids(&build(
                &[second.clone(), first.clone()],
                AgentSortMode::Status
            )),
            ["first", "second"]
        );
        let (mut first, mut second) = (first, second);
        first.updated_at = 2_000;
        second.updated_at = 3_000;
        assert_eq!(
            ids(&build(&[first, second], AgentSortMode::Status)),
            ["first", "second"]
        );
    }

    #[test]
    fn uses_an_immutable_tie_break_when_lifecycle_change_times_match() {
        let alpha = rec("alpha", |a| {
            a.display_name = "z".into();
            a.session_id = "$2".into();
            a.lifecycle_changed_at = 300;
        });
        let omega = rec("omega", |a| {
            a.display_name = "a".into();
            a.lifecycle_changed_at = 300;
        });
        assert_eq!(
            ids(&build(
                &[omega.clone(), alpha.clone()],
                AgentSortMode::Status
            )),
            ["alpha", "omega"]
        );
        let omega = AgentRecord {
            display_name: "zz".into(),
            session_id: "$2".into(),
            ..omega
        };
        let alpha = AgentRecord {
            display_name: "aa".into(),
            session_id: "$1".into(),
            ..alpha
        };
        assert_eq!(
            ids(&build(&[omega, alpha], AgentSortMode::Status)),
            ["alpha", "omega"]
        );
    }

    #[test]
    fn splits_recent_from_idle_at_four_hours_and_keeps_both_newest_change_first() {
        let now = 10 * RECENT_IDLE_WINDOW_MILLIS;
        let hour = 60 * 60 * 1_000;
        let idle_at = |id: &str, changed: u64| {
            rec(id, |a| {
                a.lifecycle = Lifecycle::Idle;
                a.lifecycle_changed_at = changed;
            })
        };
        let agents = [
            idle_at("old-idle", now - 8 * hour),
            idle_at("new-recent", now - hour),
            idle_at("new-idle", now - 7 * hour),
            idle_at("old-recent", now - 2 * hour),
            idle_at("boundary", now - RECENT_IDLE_WINDOW_MILLIS),
            rec("unknown", |a| {
                a.lifecycle = Lifecycle::Unknown;
                a.lifecycle_changed_at = now - 1;
            }),
        ];
        let rows = build_agent_rows(&agents, locate, |_| true, AgentSortMode::Status, now);
        let shape: Vec<_> = group_by_status(&rows)
            .iter()
            .map(|(label, _, rows)| (*label, ids_of(rows)))
            .collect();
        assert_eq!(
            shape,
            [
                ("Recent", vec!["new-recent", "old-recent"]),
                ("Idle", vec!["unknown", "boundary", "new-idle", "old-idle"]),
            ]
        );
    }

    #[test]
    fn keeps_a_completion_in_recent_when_acknowledgement_clears_its_dot() {
        let now = 100_000_000;
        let completion = now - 60 * 60 * 1_000;
        let completed = rec("completed", |a| {
            a.lifecycle = Lifecycle::Idle;
            a.attention_kind = Some(AttentionKind::Completed);
            a.attention_generation = 4;
            a.seen_generation = 1;
            a.lifecycle_changed_at = completion;
        });
        let neighbor = rec("neighbor", |a| {
            a.lifecycle = Lifecycle::Idle;
            a.lifecycle_changed_at = completion - 1_000;
        });
        let acknowledged = AgentRecord {
            seen_generation: 4,
            attention_seen_at: now,
            ..completed.clone()
        };
        let before = build_agent_rows(
            &[neighbor.clone(), completed],
            locate,
            |_| true,
            AgentSortMode::Status,
            now,
        );
        let after = build_agent_rows(
            &[neighbor, acknowledged],
            locate,
            |_| true,
            AgentSortMode::Status,
            now,
        );
        let find = |rows: &[AgentListRow]| {
            rows.iter()
                .find(|row| row.agent.id == "completed")
                .cloned()
                .unwrap()
        };
        assert_eq!(
            (find(&before).bucket, find(&after).bucket),
            (PriorityBucket::Recent, PriorityBucket::Recent)
        );
        assert_eq!(
            (find(&before).state, find(&after).state),
            (DisplayState::Done, DisplayState::Idle)
        );
        assert_eq!(ids(&before), ids(&after));
    }

    #[test]
    fn keeps_unread_completions_recent_indefinitely_and_starts_four_hours_when_read() {
        let now = 10 * RECENT_IDLE_WINDOW_MILLIS;
        let long_ago = rec("a", |a| {
            a.lifecycle = Lifecycle::Idle;
            a.attention_kind = Some(AttentionKind::Completed);
            a.attention_generation = 4;
            a.seen_generation = 1;
            a.lifecycle_changed_at = now - 8 * RECENT_IDLE_WINDOW_MILLIS;
        });
        let bucket = |record: &AgentRecord, at| {
            build_agent_rows([record], locate, |_| true, AgentSortMode::Status, at)[0].bucket
        };
        assert_eq!(bucket(&long_ago, now), PriorityBucket::Recent);
        let read = AgentRecord {
            seen_generation: 4,
            attention_seen_at: now,
            ..long_ago
        };
        assert_eq!(bucket(&read, now), PriorityBucket::Recent);
        assert_eq!(
            bucket(&read, now + RECENT_IDLE_WINDOW_MILLIS),
            PriorityBucket::Idle
        );
    }

    #[test]
    fn schedules_the_redraw_at_the_nearest_recent_expiration_only() {
        let now = 10 * RECENT_IDLE_WINDOW_MILLIS;
        let hour = 60 * 60 * 1_000;
        let later = rec("later", |a| {
            a.lifecycle = Lifecycle::Idle;
            a.lifecycle_changed_at = now - hour;
        });
        let sooner = rec("sooner", |a| {
            a.lifecycle = Lifecycle::Idle;
            a.lifecycle_changed_at = now - 3 * hour;
        });
        assert_eq!(
            next_recent_expiration([&later, &sooner], now),
            Some(now + hour)
        );
        assert_eq!(
            next_recent_expiration([&later, &sooner], now + hour),
            Some(now + 3 * hour)
        );
        assert_eq!(
            next_recent_expiration([&later, &sooner], now + 3 * hour),
            None
        );
        assert_eq!(next_recent_expiration([&working()], now), None);
    }

    #[test]
    fn keeps_agents_whose_workspace_is_not_in_the_list_last_instead_of_dropping_them() {
        let orphan = rec("orphan", |a| {
            a.session_id = "$9".into();
            a.session_name = "gone".into();
            a.display_name = "zed".into();
        });
        let rows = build_agent_rows(
            &[orphan, working()],
            locate,
            |_| false,
            AgentSortMode::Workspace,
            NOW,
        );
        assert_eq!(ids(&rows), ["working", "orphan"]);
        assert!(rows.iter().all(|row| !row.routable));
    }

    #[test]
    fn groups_by_host_server_and_workspace_without_changing_display_order() {
        let on = |id: &str, host: &str, server: &str| {
            rec(id, |a| {
                a.host_profile_id = host.into();
                a.server_identity = server.into();
                a.session_name = "api".into();
            })
        };
        let agents = [
            on("a", "local", "one"),
            on("c", "local", "one"),
            on("b", "remote", "two"),
        ];
        let rows = build_agent_rows(
            &agents,
            |record| AgentLocation {
                workspace_order: usize::from(record.host_profile_id != "local"),
                workspace_name: "api".into(),
                host_label: if record.host_profile_id == "local" {
                    "This Mac"
                } else {
                    "build-box"
                }
                .into(),
                ..AgentLocation::unlisted(record)
            },
            |_| true,
            AgentSortMode::Workspace,
            NOW,
        );
        let groups = group_by_workspace(&rows);
        let shape: Vec<_> = groups
            .iter()
            .map(|group| (group.workspace_name.as_str(), group.host_label.as_str()))
            .collect();
        assert_eq!(shape, [("api", "This Mac"), ("api", "build-box")]);
        let flat: Vec<_> = groups
            .iter()
            .flat_map(|group| ids_of(&group.rows))
            .collect();
        assert_eq!(flat, ids(&rows));
    }

    fn place(record: &AgentRecord) -> AgentLocation {
        let mut location = match record.session_id.as_str() {
            "$pin" => AgentLocation {
                workspace_order: 0,
                workspace_name: "pinned-ws".into(),
                tab_index: Some(3),
                workspace_pinned: true,
                ..AgentLocation::unlisted(record)
            },
            "$plain" => AgentLocation {
                workspace_order: 1,
                workspace_name: "plain-ws".into(),
                tab_index: Some(1),
                ..AgentLocation::unlisted(record)
            },
            _ => AgentLocation {
                workspace_order: 2,
                ..AgentLocation::unlisted(record)
            },
        };
        if record.window_id == "@pinned" {
            location.tab_index = Some(9);
            location.tab_pinned = true;
        }
        location
    }

    fn pin_case() -> Vec<AgentRecord> {
        vec![
            rec("blocked-away", |a| {
                a.display_name = "c".into();
                a.session_id = "$plain".into();
                a.lifecycle = Lifecycle::Blocked;
                a.updated_at = 99;
            }),
            rec("idle-here", |a| {
                a.display_name = "a".into();
                a.session_id = "$pin".into();
                a.lifecycle = Lifecycle::Idle;
                a.updated_at = 1;
                a.lifecycle_changed_at = 200;
            }),
            rec("pinned-tab", |a| {
                a.display_name = "b".into();
                a.session_id = "$pin".into();
                a.window_id = "@pinned".into();
                a.lifecycle = Lifecycle::Idle;
                a.updated_at = 2;
                a.lifecycle_changed_at = 100;
            }),
        ]
    }

    fn placed(agents: &[AgentRecord], mode: AgentSortMode) -> Vec<AgentListRow> {
        build_agent_rows(agents, place, |_| true, mode, NOW)
    }

    #[test]
    fn keeps_status_ahead_of_pins_in_the_priority_ordering() {
        let rows = placed(&pin_case(), AgentSortMode::Status);
        assert_eq!(ids(&rows), ["blocked-away", "pinned-tab", "idle-here"]);
        let pins: Vec<_> = rows.iter().map(|row| row.pinned).collect();
        assert_eq!(pins, [false, true, false]);
    }

    #[test]
    fn puts_a_pinned_workspaces_agents_first_in_the_workspace_ordering_too() {
        assert_eq!(
            ids(&placed(&pin_case(), AgentSortMode::Workspace)),
            ["pinned-tab", "idle-here", "blocked-away"]
        );
    }

    #[test]
    fn puts_pinned_agents_before_unpinned_peers_inside_each_status() {
        let mut agents = pin_case();
        agents.push(rec("stray", |a| {
            a.display_name = "d".into();
            a.session_id = "$plain".into();
            a.window_id = "@pinned".into();
            a.lifecycle = Lifecycle::Blocked;
            a.updated_at = 50;
        }));
        assert_eq!(
            ids(&placed(&agents, AgentSortMode::Status)),
            ["stray", "blocked-away", "pinned-tab", "idle-here"]
        );
    }

    #[test]
    fn orders_lifecycle_recency_separately_inside_pinned_and_unpinned_peers() {
        let at = |id: &str, changed: u64, window: &str| {
            rec(id, |a| {
                a.lifecycle_changed_at = changed;
                a.window_id = window.into();
            })
        };
        let rows = placed(
            &[
                at("pinned-old", 200, "@pinned"),
                at("loose-new", 400, "@loose-new"),
                at("pinned-new", 300, "@pinned"),
                at("loose-old", 100, "@loose-old"),
            ],
            AgentSortMode::Status,
        );
        assert_eq!(
            ids(&rows),
            ["pinned-new", "pinned-old", "loose-new", "loose-old"]
        );
    }

    #[test]
    fn leaves_both_orderings_exactly_as_they_were_when_nothing_is_pinned() {
        assert_eq!(
            ids(&build(&four(), AgentSortMode::Status)),
            ["blocked", "working", "done", "idle"]
        );
        assert!(
            build(&four(), AgentSortMode::Workspace)
                .iter()
                .all(|row| !row.pinned)
        );
        let rows = build(&four(), AgentSortMode::Pinned);
        let groups = group_by_pin(&rows);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, "Unpinned");
        assert_eq!(ids_of(&groups[0].1), ["blocked", "working", "done", "idle"]);
    }

    #[test]
    fn puts_everything_pinned_by_tab_or_by_workspace_above_everything_else() {
        let mut agents = pin_case();
        agents.push(rec("stray", |a| {
            a.display_name = "d".into();
            a.session_id = "$plain".into();
            a.window_id = "@pinned".into();
            a.lifecycle = Lifecycle::Idle;
            a.updated_at = 50;
            a.lifecycle_changed_at = 50;
        }));
        agents.push(rec("working-away", |a| {
            a.display_name = "e".into();
            a.session_id = "$plain".into();
            a.updated_at = 60;
            a.lifecycle_changed_at = 60;
        }));
        let rows = placed(&agents, AgentSortMode::Pinned);
        assert_eq!(
            ids(&rows),
            [
                "pinned-tab",
                "stray",
                "idle-here",
                "blocked-away",
                "working-away"
            ]
        );
        let groups: Vec<_> = group_by_pin(&rows)
            .into_iter()
            .map(|(label, rows)| (label, ids_of(&rows)))
            .collect();
        assert_eq!(
            groups,
            [
                ("Pinned", vec!["pinned-tab", "stray", "idle-here"]),
                ("Unpinned", vec!["blocked-away", "working-away"]),
            ]
        );
    }

    #[test]
    fn omits_an_empty_pinned_half_rather_than_drawing_an_empty_heading() {
        let agents = &pin_case()[1..];
        let rows = placed(agents, AgentSortMode::Pinned);
        let labels: Vec<_> = group_by_pin(&rows)
            .into_iter()
            .map(|(label, _)| label)
            .collect();
        assert_eq!(labels, ["Pinned"]);
        assert!(group_by_pin(&[]).is_empty());
    }

    #[test]
    fn the_bell_and_jump_ignore_pins() {
        let pinned_done = rec("pinned-done", |a| {
            a.session_id = "$pin".into();
            a.window_id = "@pinned".into();
            a.lifecycle = Lifecycle::Idle;
            a.attention_kind = Some(AttentionKind::Completed);
            a.attention_generation = 4;
            a.seen_generation = 1;
            a.updated_at = 1;
        });
        let rows = placed(&[pinned_done, pin_case().remove(0)], AgentSortMode::Status);
        assert_eq!(rows[0].agent.id, "blocked-away");
        assert_eq!(jump_target(&rows).unwrap().agent.id, "blocked-away");
    }

    #[test]
    fn selects_the_routable_agent_whose_exact_pane_is_active_including_idle_agents() {
        let active_idle = rec("active-idle", |a| {
            a.pane_id = "%7".into();
            a.lifecycle = Lifecycle::Idle;
        });
        let elsewhere = rec("elsewhere", |a| a.pane_id = "%8".into());
        let rows = build(&[elsewhere, active_idle.clone()], AgentSortMode::Status);
        assert_eq!(
            selected_agent_id_for_pane(&rows, Some("%7"), Some("local")),
            Some("active-idle")
        );
        assert_eq!(
            selected_agent_id_for_pane(&rows, Some("%missing"), Some("local")),
            None
        );
        assert_eq!(selected_agent_id_for_pane(&rows, None, Some("local")), None);
        let unroutable = build_agent_rows(
            [&active_idle],
            locate,
            |_| false,
            AgentSortMode::Status,
            NOW,
        );
        assert_eq!(
            selected_agent_id_for_pane(&unroutable, Some("%7"), Some("local")),
            None
        );
        let peer = rec("peer-seven", |a| {
            a.host_profile_id = "peer".into();
            a.pane_id = "%7".into();
            a.updated_at = 999;
        });
        let rows = build(&[peer.clone(), active_idle], AgentSortMode::Status);
        assert_eq!(
            selected_agent_id_for_pane(&rows, Some("%7"), Some("local")),
            Some("active-idle")
        );
        let rows = build(&[peer], AgentSortMode::Status);
        assert_eq!(
            selected_agent_id_for_pane(&rows, Some("%7"), Some("local")),
            None
        );
    }

    #[test]
    fn resolves_a_defensive_duplicate_to_native_then_newest_then_stable_id() {
        let at = |id: &str, manual: bool, native: &str, updated: u64| {
            rec(id, |a| {
                a.pane_id = "%7".into();
                a.detected_manually = manual;
                a.native_session_id = native.into();
                a.updated_at = updated;
            })
        };
        let manual = at("manual", true, "", 999);
        let old = at("native-old", false, "session-old", 10);
        let new_b = at("native-b", false, "session-b", 20);
        let new_a = at("native-a", false, "session-a", 20);
        let pick = |agents: &[AgentRecord]| {
            let rows = build(agents, AgentSortMode::Status);
            selected_agent_id_for_pane(&rows, Some("%7"), Some("local")).map(str::to_owned)
        };
        assert_eq!(pick(&[manual, old.clone()]).as_deref(), Some("native-old"));
        assert_eq!(pick(&[old, new_b.clone()]).as_deref(), Some("native-b"));
        assert_eq!(pick(&[new_b, new_a]).as_deref(), Some("native-a"));
    }
}
