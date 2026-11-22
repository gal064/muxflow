//! What the assistant is shown, and the prompt that frames it. Port of
//! `features/ai/context.ts`.
//!
//! The snapshot is taken from the tabs at the moment the question is sent, so
//! the model sees what the person can see. Each section is named for the tab
//! it came from, so an answer can point at a tab the user can click.

use std::rc::Rc;
use std::sync::Arc;

use desktop_core::connection::ports::{DetectedPort, PortForward};
use tmux_control::TmuxSnapshot;

use crate::automations::model::{AutomationListing, describe_minute, describe_schedule};
use crate::git::model::StatusSnapshot;

/// What the AI tab offers the model, one source per panel tab it maps onto.
/// "Session" is the tmux view the whole shell is built on; the rest are the
/// tabs by their own names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiSource {
    Session,
    Files,
    Git,
    Ports,
    Automations,
}

pub const AI_SOURCES: [AiSource; 5] = [
    AiSource::Session,
    AiSource::Files,
    AiSource::Git,
    AiSource::Ports,
    AiSource::Automations,
];

impl AiSource {
    pub fn id(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Files => "files",
            Self::Git => "git",
            Self::Ports => "ports",
            Self::Automations => "automations",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Session => "Session",
            Self::Files => "Files",
            Self::Git => "Git",
            Self::Ports => "Ports",
            Self::Automations => "Automations",
        }
    }
}

/// One coding agent as the hooks reported it, reduced to what a prompt needs.
pub struct AgentRow {
    pub display_name: String,
    pub adapter_id: String,
    pub session_name: String,
    pub window_name: String,
    pub window_id: String,
    /// Already in words: "working", "blocked, waiting for input".
    pub lifecycle: &'static str,
}

/// One open document tab.
pub struct OpenTab {
    pub kind: String,
    pub title: String,
    pub resource: String,
}

/// Everything the tabs are showing, as the panel holds it at the moment of
/// asking. Owned, so the panel can read every tab on the main thread and let
/// the builders run whenever the call is assembled.
pub struct ContextInput {
    pub host_label: String,
    pub transport: &'static str,
    pub connected: bool,
    /// The live tmux view of the host on screen.
    pub snapshot: Arc<TmuxSnapshot>,
    pub active_session_id: Option<String>,
    pub active_window_id: Option<String>,
    pub active_pane_id: Option<String>,
    /// The host's agents, as the hooks reported them.
    pub agents: Vec<AgentRow>,
    /// The document tabs open in the current workspace.
    pub open_tabs: Vec<OpenTab>,
    pub active_root_path: Option<String>,
    pub git_status: Option<Rc<StatusSnapshot>>,
    pub git_error: Option<String>,
    /// The host's own port forwards.
    pub port_forwards: Vec<PortForward>,
    /// Filled in only when the source was on and the host answered `ss`.
    pub detected_ports: Option<Vec<DetectedPort>>,
    /// Filled in only when the source was on and the helper answered.
    pub automations: Option<AutomationListing>,
    pub automations_error: Option<String>,
}

/// The lifecycle word the model reads for each agent state.
pub fn lifecycle_words(lifecycle: crate::agents::model::Lifecycle) -> &'static str {
    use crate::agents::model::Lifecycle;
    match lifecycle {
        Lifecycle::Working => "working",
        Lifecycle::Blocked => "blocked, waiting for input",
        Lifecycle::Idle => "idle",
        Lifecycle::Unknown => "of unknown state",
    }
}

/// What each section may hold at most: a snapshot, not an archive.
const MAX_PANES: usize = 24;
const MAX_TABS: usize = 30;
const MAX_ENTRIES: usize = 60;
const MAX_PORTS: usize = 30;
const MAX_AUTOMATIONS: usize = 20;
const MAX_AGENTS: usize = 20;

fn session_section(input: &ContextInput) -> String {
    let mut lines = vec![format!(
        "Host: {} ({}, {}).",
        input.host_label,
        input.transport,
        if input.connected {
            "connected"
        } else {
            "disconnected"
        }
    )];
    if input.snapshot.sessions.is_empty() {
        lines.push("No tmux sessions.".into());
    } else {
        lines.push(format!(
            "Workspaces (tmux sessions): {}.",
            input
                .snapshot
                .sessions
                .iter()
                .map(|session| format!(
                    "{} ({} window{}{})",
                    session.name,
                    session.window_count,
                    if session.window_count == 1 { "" } else { "s" },
                    if session.attached_clients > 0 {
                        format!(", {} attached", session.attached_clients)
                    } else {
                        String::new()
                    }
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let window = input
        .snapshot
        .windows
        .iter()
        .find(|candidate| Some(candidate.id.as_str()) == input.active_window_id.as_deref());
    let session = input
        .snapshot
        .sessions
        .iter()
        .find(|candidate| Some(candidate.id.as_str()) == input.active_session_id.as_deref())
        .or_else(|| {
            window.and_then(|window| {
                input
                    .snapshot
                    .sessions
                    .iter()
                    .find(|candidate| candidate.id == window.session_id)
            })
        });
    if let (Some(window), Some(session)) = (window, session) {
        lines.push(format!(
            "On screen: window \"{}\" of workspace \"{}\".",
            window.name, session.name
        ));
    }
    if let Some(window) = window {
        let panes: Vec<_> = input
            .snapshot
            .panes
            .iter()
            .filter(|pane| pane.window_id == window.id)
            .take(MAX_PANES)
            .collect();
        if !panes.is_empty() {
            lines.push(format!(
                "Panes in this window: {}.",
                panes
                    .iter()
                    .map(|pane| format!(
                        "#{}{} in {}{}",
                        pane.index,
                        if input.active_pane_id.as_deref() == Some(pane.id.as_str()) || pane.active
                        {
                            " (active)"
                        } else {
                            ""
                        },
                        if pane.current_path.is_empty() {
                            "?"
                        } else {
                            &pane.current_path
                        },
                        if pane.current_command.is_empty() {
                            String::new()
                        } else {
                            format!(" running {}", pane.current_command)
                        }
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
    }
    let agents: Vec<&AgentRow> = input.agents.iter().take(MAX_AGENTS).collect();
    if !agents.is_empty() {
        lines.push(format!(
            "Coding agents: {}.",
            agents
                .iter()
                .map(|agent| format!(
                    "{} in {}/{} — {}",
                    if agent.display_name.is_empty() {
                        &agent.adapter_id
                    } else {
                        &agent.display_name
                    },
                    agent.session_name,
                    agent.window_name,
                    agent.lifecycle
                ))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    } else {
        lines.push("No coding agents reported by the host.".into());
    }
    lines.join("\n")
}

fn files_section(input: &ContextInput) -> String {
    let mut lines = vec![input.active_root_path.as_deref().map_or_else(
        || {
            "The Files tab has no root right now — no pane is open, or the host has not answered."
                .to_owned()
        },
        |root| format!("Explorer root: {root}"),
    )];
    if input.open_tabs.is_empty() {
        lines.push("No documents are open.".into());
    } else {
        lines.push(format!(
            "Open documents ({}): {}.",
            input.open_tabs.len(),
            input
                .open_tabs
                .iter()
                .take(MAX_TABS)
                .map(|tab| format!("{} [{}]", tab.title, tab.kind))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    lines.join("\n")
}

/// The porcelain letter a change kind prints as, the same one the Git panel's
/// row colours by.
fn kind_letter(kind: crate::git::model::ChangeKind) -> char {
    use crate::git::model::ChangeKind;
    match kind {
        ChangeKind::None => ' ',
        ChangeKind::Modified => 'M',
        ChangeKind::Added => 'A',
        ChangeKind::Deleted => 'D',
        ChangeKind::Renamed => 'R',
        ChangeKind::Copied => 'C',
        ChangeKind::TypeChanged => 'T',
        ChangeKind::Unmerged => 'U',
        ChangeKind::Untracked => '?',
        ChangeKind::Ignored => 'I',
    }
}

fn git_section(input: &ContextInput) -> String {
    if let Some(error) = &input.git_error {
        return format!("The Git tab reports an error: {error}");
    }
    let Some(status) = input.git_status.as_deref() else {
        return "The Git tab has no repository — the active pane's directory is not a worktree, or nothing has loaded yet.".into();
    };
    let head = if status.repository.initial {
        "no commits yet".to_owned()
    } else if status.repository.detached_head {
        "detached HEAD".to_owned()
    } else {
        format!(
            "branch {}",
            status.repository.head_name.as_deref().unwrap_or("?")
        )
    };
    let mut lines = vec![format!(
        "Repository {}, {}.",
        status.repository.worktree_root, head
    )];
    if let Some(error) = &status.error {
        lines.push(format!("The host reported: {error}"));
    }
    if status.oversized {
        lines.push(format!(
            "The status is oversized: only part of {} entries is listed.",
            if status.total_entry_count == 0 {
                "many".to_owned()
            } else {
                status.total_entry_count.to_string()
            }
        ));
    }
    if status.entries.is_empty() {
        lines.push("No changes.".into());
    } else {
        let listed: Vec<_> = status.entries.iter().take(MAX_ENTRIES).collect();
        let more = if status.entries.len() > listed.len() {
            format!(" …and {} more.", status.entries.len() - listed.len())
        } else {
            String::new()
        };
        lines.push(format!(
            "Changes ({}): {}.{more}",
            status.entries.len(),
            listed
                .iter()
                .map(|entry| {
                    let letters: String = format!(
                        "{}{}",
                        kind_letter(entry.index_kind),
                        kind_letter(entry.worktree_kind)
                    );
                    format!("{} {}", letters.trim(), entry.display_path)
                })
                .collect::<Vec<_>>()
                .join("; ")
        ));
        if status.entries.iter().any(|entry| entry.conflicted) {
            lines.push("Some files are conflicted.".into());
        }
    }
    lines.join("\n")
}

fn ports_section(input: &ContextInput) -> String {
    let mut lines = Vec::new();
    if input.port_forwards.is_empty() {
        lines.push("No port forwards are active.".into());
    } else {
        lines.push(format!(
            "Forwards: {}.",
            input
                .port_forwards
                .iter()
                .take(MAX_PORTS)
                .map(|forward| format!(
                    "{}→localhost:{} ({})",
                    forward.remote_port,
                    forward.local_port,
                    match forward.state {
                        desktop_core::connection::ports::ForwardState::Starting => "starting",
                        desktop_core::connection::ports::ForwardState::Active => "active",
                        desktop_core::connection::ports::ForwardState::Failed => "failed",
                    }
                ))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if let Some(detected) = &input.detected_ports {
        lines.push(if detected.is_empty() {
            "Nothing is listening on the host (or the check failed).".to_owned()
        } else {
            format!(
                "Listening on the host: {}.",
                detected
                    .iter()
                    .take(MAX_PORTS)
                    .map(|port| match &port.process {
                        Some(process) => format!("{} ({})", port.port, process),
                        None => port.port.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
    }
    lines.join("\n")
}

/// The word the model reads for a run's status; the host reports these as
/// strings, and anything unknown is passed through.
fn run_status(status: &str) -> &str {
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

fn automations_section(input: &ContextInput) -> String {
    if let Some(error) = &input.automations_error {
        return format!("The Automations tab could not be read: {error}");
    }
    let Some(listing) = &input.automations else {
        return "The Automations tab was not reachable for this host.".into();
    };
    let mut lines = Vec::new();
    if !listing.file_error.is_empty() {
        lines.push(format!(
            "The host cannot read its automations file: {}",
            listing.file_error
        ));
    }
    if listing.automations.is_empty() {
        lines.push("No automations are defined on this host.".into());
        return lines.join("\n");
    }
    let today = chrono::Local::now().date_naive();
    for summary in listing.automations.iter().take(MAX_AUTOMATIONS) {
        let state = if !summary.definition_error.is_empty() {
            "invalid"
        } else if summary.definition.enabled {
            "on"
        } else {
            "paused"
        };
        let last = summary.recent_runs.first();
        let mut line = format!(
            "- {}: {} (`{}`), {}",
            if summary.definition.name.is_empty() {
                summary.definition.id.as_str()
            } else {
                &summary.definition.name
            },
            describe_schedule(&summary.definition.schedule),
            summary.definition.schedule,
            state,
        );
        if !summary.next_run.is_empty() {
            line.push_str(&format!(
                ", next {}",
                describe_minute(&summary.next_run, today)
            ));
        }
        if let Some(last) = last {
            line.push_str(&format!(
                "; last run #{} {}",
                last.number,
                run_status(&last.status)
            ));
        }
        if !summary.definition_error.is_empty() {
            line.push_str(&format!("; definition error: {}", summary.definition_error));
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// One source's section, under its tab name.
pub fn context_section(source: AiSource, input: &ContextInput) -> String {
    let body = match source {
        AiSource::Session => session_section(input),
        AiSource::Files => files_section(input),
        AiSource::Git => git_section(input),
        AiSource::Ports => ports_section(input),
        AiSource::Automations => automations_section(input),
    };
    format!("## {} tab\n{body}", source.label())
}

/// The snapshot the model is shown: each included source, under its tab name.
pub fn build_context(sources: &[AiSource], input: &ContextInput) -> String {
    AI_SOURCES
        .iter()
        .filter(|source| sources.contains(source))
        .map(|source| context_section(*source, input))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The system prompt. It fixes three things the answer goes wrong without:
/// what the app is, that the snapshot is the only evidence, and that a missing
/// section is answered as "that tab has nothing" rather than invented.
pub fn build_system_prompt(context: &str) -> String {
    format!(
        "You are the assistant inside Muxflow, a desktop app for working with tmux sessions on local and SSH hosts. \
Its right-hand panel has tabs — Files, Git, Ports, Automations and this AI tab — and the user is asking about the live session behind them. \
This conversation belongs to one tab of the strip: the window named in the Session section.\n\n\
Below is a snapshot taken from those tabs the moment the question was sent; each section is named for the tab it came from.\n\n\
{}\n\n\
You can act on the current tab with tools: send_to_pane types into a pane and run_automation starts a scheduled run — \
the user approves both before anything happens, and a denial is final, so report a denial plainly instead of retrying. \
read_pane shows what a pane's screen actually says (its output is not in the snapshot — read it instead of guessing), \
list_panes shows the tab's panes as they are now (call it after acting — the snapshot goes stale), and show_pane brings a pane on screen. \
Never claim an action ran unless its tool result says it did.\n\n\
Answer from the snapshot and the tool results only. When a section is missing or says it has nothing, say that tab has nothing to report — never invent what it might contain. \
Be brief, and name the tab when pointing at something.",
        if context.is_empty() {
            "(No context was included this time.)".to_owned()
        } else {
            context.to_owned()
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automations::model::{AutomationRun, AutomationSummary};
    use desktop_core::connection::automation::AutomationDefinition;
    use tmux_control::{Pane, Session, Window};

    fn session(id: &str, name: &str) -> Session {
        Session {
            id: id.into(),
            name: name.into(),
            window_count: 2,
            attached_clients: 1,
            order: 0,
            pinned: false,
        }
    }

    fn window(id: &str, session_id: &str, name: &str) -> Window {
        Window {
            id: id.into(),
            session_id: session_id.into(),
            index: 0,
            name: name.into(),
            active: true,
            layout: String::new(),
            zoomed: false,
            pinned: false,
        }
    }

    fn pane(id: &str, window_id: &str, path: &str, command: &str, active: bool) -> Pane {
        Pane {
            id: id.into(),
            session_id: String::new(),
            window_id: window_id.into(),
            index: id.trim_start_matches('%').parse().unwrap_or(1),
            active,
            width: 80,
            height: 24,
            left: 0,
            top: 0,
            current_path: path.into(),
            current_command: command.into(),
            pane_pid: 1,
            start_command: String::new(),
            title: String::new(),
            chat_adapter_id: String::new(),
        }
    }

    fn input(snapshot: TmuxSnapshot) -> ContextInput {
        ContextInput {
            host_label: "barch".into(),
            transport: "ssh",
            connected: true,
            snapshot: Arc::new(snapshot),
            active_session_id: Some("$1".into()),
            active_window_id: Some("@1".into()),
            active_pane_id: Some("%1".into()),
            agents: Vec::new(),
            open_tabs: Vec::new(),
            active_root_path: None,
            git_status: None,
            git_error: None,
            port_forwards: Vec::new(),
            detected_ports: None,
            automations: None,
            automations_error: None,
        }
    }

    #[test]
    fn the_session_section_names_the_host_the_workspaces_and_the_panes() {
        let snapshot = TmuxSnapshot {
            sessions: vec![session("$1", "work")],
            windows: vec![window("@1", "$1", "build")],
            panes: vec![
                pane("%1", "@1", "/srv/api", "cargo", true),
                pane("%2", "@1", "", "bash", false),
            ],
        };
        let section = context_section(AiSource::Session, &input(snapshot));
        assert!(section.starts_with("## Session tab\n"));
        assert!(section.contains("Host: barch (ssh, connected)."));
        assert!(section.contains("work (2 windows, 1 attached)"));
        assert!(section.contains("window \"build\" of workspace \"work\""));
        assert!(section.contains("#1 (active) in /srv/api running cargo"));
        assert!(section.contains("#2 in ? running bash"));
        assert!(section.contains("No coding agents reported by the host."));
    }

    #[test]
    fn a_source_off_says_nothing_about_it_and_sections_follow_tab_order() {
        let snapshot = TmuxSnapshot::default();
        let all = build_context(&AI_SOURCES, &input(snapshot.clone()));
        let order: Vec<usize> = [
            "## Session tab",
            "## Files tab",
            "## Git tab",
            "## Ports tab",
            "## Automations tab",
        ]
        .iter()
        .map(|heading| all.find(heading).unwrap())
        .collect();
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]));
        let only_session = build_context(&[AiSource::Session], &input(snapshot));
        assert!(only_session.starts_with("## Session tab"));
        assert!(!only_session.contains("## Git tab"));
        assert!(only_session.contains("No tmux sessions."));
    }

    #[test]
    fn the_system_prompt_frames_the_snapshot_and_the_tools() {
        let prompt = build_system_prompt("");
        assert!(prompt.contains("(No context was included this time.)"));
        assert!(prompt.contains("Never claim an action ran unless its tool result says it did."));
        assert!(build_system_prompt("## Session tab\nx").contains("## Session tab\nx"));
    }

    #[test]
    fn automations_read_as_a_short_table_or_an_absence() {
        let mut context = input(TmuxSnapshot::default());
        assert_eq!(
            context_section(AiSource::Automations, &context),
            "## Automations tab\nThe Automations tab was not reachable for this host."
        );
        context.automations = Some(AutomationListing {
            automations: vec![AutomationSummary {
                definition: AutomationDefinition {
                    id: "nightly".into(),
                    name: "Nightly".into(),
                    enabled: true,
                    schedule: "0 3 * * *".into(),
                    ..Default::default()
                },
                next_run: String::new(),
                recent_runs: vec![AutomationRun {
                    number: 4,
                    status: "needs_attention".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        });
        let section = context_section(AiSource::Automations, &context);
        assert!(section.contains("- Nightly:"), "{section}");
        assert!(section.contains("(`0 3 * * *`), on"));
        assert!(section.contains("last run #4 needs attention"));
    }
}
