//! The sidebar's workspace rows, merged across every shown host. Port of
//! `features/workspaces/workspaceRows.ts` and `mergedWorkspaceRows.ts`.
//!
//! A row is derived, never stored: tmux owns the topology and the agents
//! feature owns the states ([`super::agents::AgentShell`]). This list is also
//! the workspace order Ctrl+1…9 and Quick Open read, so the pinned block is
//! applied here and not by the sidebar.

use std::{collections::HashMap, sync::LazyLock};

use regex::Regex;
use tmux_control::{Session, TmuxSnapshot};

use super::{agents::WorkspaceAgents, model::ordered_sessions};
use crate::connection_ui::{connection_reducer::ConnectionPhase, host_scope::HostScopeToken};

/// Everything one shown host contributes.
pub struct HostRowSource<'a> {
    pub host_profile_id: &'a str,
    pub letter: &'a str,
    pub label: &'a str,
    pub phase: ConnectionPhase,
    pub can_mutate: bool,
    pub scope: HostScopeToken,
    pub snapshot: &'a TmuxSnapshot,
    /// Only the active host has one.
    pub active_session_id: Option<&'a str>,
}

/// A workspace row that knows its host. Session ids repeat across hosts (`$0`
/// is on every tmux server), so every action a row starts is aimed at the
/// row's own host, whichever host is active.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceRow {
    pub key: String,
    pub host_profile_id: String,
    /// Empty when letters are hidden: one shown host has nothing to tell apart.
    pub letter: String,
    pub host_label: String,
    pub session: Session,
    pub active: bool,
    pub pinned: bool,
    /// The host's link phase; a row whose host is not connected is dimmed.
    pub phase: ConnectionPhase,
    pub can_mutate: bool,
    pub scope: HostScopeToken,
    /// The shell's own directory, abbreviated as a prompt would; for Quick Open.
    pub path: Option<String>,
    pub agents: WorkspaceAgents,
}

impl WorkspaceRow {
    /// Dimmed while its host's link is down: the row is the last thing the
    /// host said, and a change asked of it will be refused.
    pub fn offline(&self) -> bool {
        self.phase != ConnectionPhase::Connected
    }

    /// The row's whole accessible name: the badge and lines beside it are
    /// decorative.
    pub fn label(&self) -> String {
        let total = self.agents.agents.len() + self.agents.overflow;
        let mut parts = vec![self.session.name.clone()];
        if !self.letter.is_empty() {
            parts.push(format!("on {}", self.host_label));
        }
        if self.offline() {
            parts.push(format!("host {}", self.phase.wire_name()));
        }
        if self.pinned {
            parts.push("pinned".into());
        }
        if let Some(agent) = self.agents.agents.first() {
            parts.push(format!("{} · {}", agent.name, activity_word(agent.state)));
        }
        if total > 1 {
            parts.push(format!("{total} agents"));
        }
        if self.agents.unread > 0 {
            let plural = if self.agents.unread == 1 { "" } else { "s" };
            parts.push(format!("{} agent{plural} waiting", self.agents.unread));
        }
        parts.join(", ")
    }
}

/// The verb a row prints beside an agent's name.
pub fn activity_word(state: crate::agents::model::DisplayState) -> &'static str {
    match state {
        crate::agents::model::DisplayState::Done => "done, unread",
        state => state.word(),
    }
}

/// One host's rows, pinned first, in that host's own order.
pub fn host_workspace_rows(
    source: &HostRowSource,
    show_letters: bool,
    agents: impl Fn(&str, &str) -> Option<WorkspaceAgents>,
) -> Vec<WorkspaceRow> {
    let home = infer_home(
        source
            .snapshot
            .panes
            .iter()
            .map(|pane| pane.current_path.as_str()),
    );
    let ordered =
        super::model::pinned_first(ordered_sessions(&source.snapshot.sessions), |s| s.pinned);
    ordered
        .into_iter()
        .map(|session| WorkspaceRow {
            key: format!("{}\0{}", source.host_profile_id, session.id),
            host_profile_id: source.host_profile_id.to_owned(),
            letter: if show_letters {
                source.letter.to_owned()
            } else {
                String::new()
            },
            host_label: source.label.to_owned(),
            session: session.clone(),
            active: source.active_session_id == Some(session.id.as_str()),
            pinned: session.pinned,
            phase: source.phase,
            can_mutate: source.can_mutate,
            scope: source.scope.clone(),
            path: abbreviate_home(session_path(source.snapshot, &session.id), home.as_deref()),
            agents: agents(source.host_profile_id, &session.id).unwrap_or_default(),
        })
        .collect()
}

/// Every shown host's rows as one list: every pinned row first, then the rest,
/// hosts in the order given and each host's own order inside.
pub fn merge_host_rows(per_host: Vec<Vec<WorkspaceRow>>) -> Vec<WorkspaceRow> {
    let (pinned, rest): (Vec<WorkspaceRow>, Vec<WorkspaceRow>) =
        per_host.into_iter().flatten().partition(|row| row.pinned);
    pinned.into_iter().chain(rest).collect()
}

/// The sidebar's pinned-only filter. The workspace on screen keeps its row, but
/// only on the active host: the same id on a peer is a different workspace.
pub fn pinned_only_rows(
    rows: &[WorkspaceRow],
    host: &str,
    session: Option<&str>,
) -> Vec<WorkspaceRow> {
    rows.iter()
        .filter(|row| {
            row.pinned || (row.host_profile_id == host && Some(row.session.id.as_str()) == session)
        })
        .cloned()
        .collect()
}

/// The active pane of the workspace's active window: the shell's own cwd.
pub fn session_path(snapshot: &TmuxSnapshot, session_id: &str) -> Option<String> {
    let windows: Vec<_> = snapshot
        .windows
        .iter()
        .filter(|window| window.session_id == session_id)
        .collect();
    let window = windows
        .iter()
        .find(|window| window.active)
        .or_else(|| windows.iter().min_by_key(|window| window.index))?;
    let panes: Vec<_> = snapshot
        .panes
        .iter()
        .filter(|pane| pane.window_id == window.id)
        .collect();
    let pane = panes
        .iter()
        .find(|pane| pane.active)
        .or_else(|| panes.iter().min_by_key(|pane| pane.index))?;
    (!pane.current_path.is_empty()).then(|| pane.current_path.clone())
}

static HOME_SHAPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(/home/[^/]+|/Users/[^/]+|/root)(?:/|$)").expect("a valid pattern")
});

/// The tmux user's home, inferred from where the panes are: the app cannot
/// ask the host without a protocol change. The home-shaped prefix most panes
/// share, ties to the smaller; nothing when no path looks like a home, since a
/// long path is a worse row than a short one but a wrong `~` is a lie.
pub fn infer_home<'a>(paths: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for path in paths {
        if let Some(prefix) = HOME_SHAPE.captures(path).and_then(|found| found.get(1)) {
            *counts.entry(prefix.as_str()).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by(|(left, left_count), (right, right_count)| {
            left_count.cmp(right_count).then_with(|| right.cmp(left))
        })
        .map(|(prefix, _)| prefix.to_owned())
}

pub fn abbreviate_home(path: Option<String>, home: Option<&str>) -> Option<String> {
    let path = path?;
    let Some(home) = home.filter(|home| *home != "/") else {
        return Some(path);
    };
    match path.strip_prefix(home) {
        Some("") => Some("~".into()),
        Some(rest) if rest.starts_with('/') => Some(format!("~{rest}")),
        _ => Some(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspaces::{
        agents::RowAgent,
        model::fixtures::{pane, session, window},
    };

    fn snapshot() -> TmuxSnapshot {
        let mut panes = vec![
            pane("%1", "@1", "$1", (0, 0, 80, 24)),
            pane("%2", "@2", "$1", (0, 0, 80, 24)),
            pane("%9", "@5", "$2", (0, 0, 80, 24)),
        ];
        for (pane, path) in panes.iter_mut().zip([
            "/home/operator/dev/muxflow",
            "/home/operator/dev/muxflow",
            "/home/operator/dev/project-e2e",
        ]) {
            pane.current_path = path.into();
            pane.active = true;
        }
        TmuxSnapshot {
            sessions: vec![session("$2", "project-e2e", 1), session("$1", "muxflow", 0)],
            windows: vec![
                window("@1", "$1", 1, "claude", false),
                window("@2", "$1", 2, "zsh", true),
                window("@5", "$2", 1, "shell", true),
            ],
            panes,
        }
    }

    fn scope(host: &str) -> HostScopeToken {
        HostScopeToken {
            host_profile_id: host.into(),
            connection_key: host.into(),
            connection_epoch: 1,
            server_identity: Some("server".into()),
            generation: 1,
        }
    }

    fn source<'a>(
        host: &'a str,
        snapshot: &'a TmuxSnapshot,
        active: Option<&'a str>,
    ) -> HostRowSource<'a> {
        HostRowSource {
            host_profile_id: host,
            letter: if host == "local" { "L" } else { "P" },
            label: host,
            phase: ConnectionPhase::Connected,
            can_mutate: true,
            scope: scope(host),
            snapshot,
            active_session_id: active,
        }
    }

    fn pinned(mut snapshot: TmuxSnapshot, ids: &[&str]) -> TmuxSnapshot {
        for session in &mut snapshot.sessions {
            session.pinned = ids.contains(&session.id.as_str());
        }
        snapshot
    }

    fn none(_: &str, _: &str) -> Option<WorkspaceAgents> {
        None
    }

    fn ids(rows: &[WorkspaceRow]) -> Vec<String> {
        rows.iter().map(|row| row.session.id.clone()).collect()
    }

    #[test]
    fn orders_by_the_workspace_order_tmux_reports() {
        let snapshot = snapshot();
        let rows = host_workspace_rows(&source("local", &snapshot, Some("$1")), false, none);
        let names: Vec<&str> = rows.iter().map(|row| row.session.name.as_str()).collect();
        assert_eq!(names, ["muxflow", "project-e2e"]);
        assert!(rows[0].active && !rows[1].active);
        assert_eq!(rows[0].path.as_deref(), Some("~/dev/muxflow"));
        assert_eq!(rows[1].path.as_deref(), Some("~/dev/project-e2e"));
        assert_eq!(rows[0].letter, "");
    }

    #[test]
    fn moves_pinned_workspaces_to_the_top_in_their_own_order() {
        let rows = |pins: &[&str]| {
            let snapshot = pinned(snapshot(), pins);
            ids(&host_workspace_rows(
                &source("local", &snapshot, Some("$1")),
                false,
                none,
            ))
        };
        assert_eq!(rows(&["$2"]), ["$2", "$1"]);
        assert_eq!(rows(&["$1", "$2"]), ["$1", "$2"]);
        assert_eq!(rows(&["$99"]), ["$1", "$2"]);
    }

    #[test]
    fn narrows_to_pinned_keeping_the_selected_one() {
        let snapshot = pinned(snapshot(), &["$2"]);
        let rows = host_workspace_rows(&source("local", &snapshot, Some("$1")), false, none);
        assert_eq!(
            ids(&pinned_only_rows(&rows, "local", Some("$1"))),
            ["$2", "$1"]
        );
        assert_eq!(ids(&pinned_only_rows(&rows, "local", Some("$2"))), ["$2"]);
        assert_eq!(ids(&pinned_only_rows(&rows, "peer", Some("$1"))), ["$2"]);
        let plain = snapshot_without_pins();
        let rows = host_workspace_rows(&source("local", &plain, None), false, none);
        assert!(pinned_only_rows(&rows, "local", None).is_empty());
    }

    fn snapshot_without_pins() -> TmuxSnapshot {
        snapshot()
    }

    #[test]
    fn merges_every_pinned_row_first_and_keys_hosts_apart() {
        let local = pinned(snapshot(), &["$2"]);
        let peer = pinned(snapshot(), &["$1"]);
        let merged = merge_host_rows(vec![
            host_workspace_rows(&source("local", &local, Some("$1")), true, none),
            host_workspace_rows(&source("peer", &peer, None), true, none),
        ]);
        let keys: Vec<String> = merged
            .iter()
            .map(|row| row.key.replace('\0', ":"))
            .collect();
        assert_eq!(keys, ["local:$2", "peer:$1", "local:$1", "peer:$2"]);
        assert_eq!(merged[1].letter, "P");
        assert!(!merged[1].active, "only the active host has an active row");
    }

    #[test]
    fn takes_the_path_from_the_active_pane_of_the_active_window() {
        assert_eq!(
            session_path(&snapshot(), "$1").as_deref(),
            Some("/home/operator/dev/muxflow")
        );
        assert_eq!(session_path(&snapshot(), "$nope"), None);
    }

    #[test]
    fn shortens_paths_only_at_a_path_boundary() {
        let home = Some("/home/operator");
        let short = |path: &str| abbreviate_home(Some(path.into()), home);
        assert_eq!(short("/home/operator/dev/x").as_deref(), Some("~/dev/x"));
        assert_eq!(short("/home/operator").as_deref(), Some("~"));
        assert_eq!(
            short("/home/operator-extra/x").as_deref(),
            Some("/home/operator-extra/x")
        );
        assert_eq!(short("/srv/app").as_deref(), Some("/srv/app"));
        assert_eq!(abbreviate_home(None, home), None);
    }

    #[test]
    fn infers_home_from_the_panes_or_says_nothing() {
        assert_eq!(
            infer_home(["/home/operator/dev/a", "/home/operator/dev/b", "/srv/x"]).as_deref(),
            Some("/home/operator")
        );
        assert_eq!(
            infer_home(["/Users/operator/dev/a", "/Users/operator"]).as_deref(),
            Some("/Users/operator")
        );
        assert_eq!(infer_home(["/root/x"]).as_deref(), Some("/root"));
        assert_eq!(infer_home(["/srv/app", "/var/lib", ""]), None);
        assert_eq!(infer_home([]), None);
        assert_eq!(
            infer_home(["/home/operator/a", "/home/operator/b", "/home/deploy/c"]).as_deref(),
            Some("/home/operator")
        );
        assert_eq!(
            infer_home(["/home/zed/a", "/home/operator/b"]).as_deref(),
            Some("/home/operator")
        );
    }

    #[test]
    fn labels_a_row_with_everything_its_marks_say() {
        let snapshot = pinned(snapshot(), &["$1"]);
        let mut row = host_workspace_rows(
            &source("local", &snapshot, Some("$1")),
            true,
            |_, session| {
                (session == "$1").then(|| WorkspaceAgents {
                    agents: vec![RowAgent {
                        id: "a".into(),
                        adapter_id: "codex".into(),
                        name: "codex".into(),
                        state: crate::agents::model::DisplayState::Blocked,
                    }],
                    overflow: 2,
                    unread: 1,
                    working: true,
                    state: Some(crate::agents::model::DisplayState::Blocked),
                })
            },
        )
        .remove(0);
        assert_eq!(
            row.label(),
            "muxflow, on local, pinned, codex · blocked, 3 agents, 1 agent waiting"
        );
        row.phase = ConnectionPhase::Reconnecting;
        row.letter.clear();
        row.pinned = false;
        row.agents = WorkspaceAgents::default();
        assert_eq!(row.label(), "muxflow, host reconnecting");
    }
}
