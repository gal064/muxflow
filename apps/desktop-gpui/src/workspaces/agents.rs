//! What the sidebar rows and the tab strip show about agents. The agents
//! feature fills it from its runtime ([`crate::agents::publish_shell`]); the
//! workspace shell only reads it.
//!
//! Everything starts empty, which the shell treats as "not known yet": a
//! terminal tab's presence is [`AgentPresence::Unknown`], so Close All
//! Non-Agent Tabs leaves every terminal open until agents are known.

use std::collections::HashMap;

use gpui_kit::{App, Global};

use crate::agents::model::DisplayState;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentPresence {
    Present,
    Absent,
    #[default]
    Unknown,
}

/// One agent line on a workspace row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowAgent {
    pub id: String,
    pub adapter_id: String,
    pub name: String,
    pub state: DisplayState,
}

/// What a workspace row says about its agents.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceAgents {
    /// The loudest few, loudest first.
    pub agents: Vec<RowAgent>,
    /// How many more there are than `agents` lists.
    pub overflow: usize,
    /// Agents waiting on a person.
    pub unread: usize,
    pub working: bool,
    /// The loudest state across the workspace.
    pub state: Option<DisplayState>,
}

/// What a terminal tab says about the agents in its window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowAgents {
    /// The loudest agent's state, and its adapter for the tab's mark.
    pub attention: Option<(DisplayState, String)>,
    pub presence: AgentPresence,
}

/// Set by the agents feature with `cx.set_global`; the shell re-renders when
/// it changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentShell {
    /// Keyed by (host profile id, session id).
    pub workspaces: HashMap<(String, String), WorkspaceAgents>,
    /// Keyed by (host profile id, window id). The keys carry no connection:
    /// whoever fills this clears a host's entries when its connection or tmux
    /// server changes, or a stale "no agent" would match a reused `@N`.
    pub windows: HashMap<(String, String), WindowAgents>,
    /// Per host profile, the topology generation the presence above was
    /// computed against. A bulk close that protects agents waits for this to
    /// reach each close's generation before trusting "no agent" again.
    pub generations: HashMap<String, u64>,
}

impl Global for AgentShell {}

impl AgentShell {
    pub fn workspace(&self, host: &str, session: &str) -> Option<&WorkspaceAgents> {
        self.workspaces.get(&(host.to_owned(), session.to_owned()))
    }

    pub fn window(&self, host: &str, window: &str) -> Option<&WindowAgents> {
        self.windows.get(&(host.to_owned(), window.to_owned()))
    }

    pub fn current_through(&self, host: &str, generation: u64) -> bool {
        self.generations
            .get(host)
            .is_some_and(|seen| *seen >= generation)
    }
}

/// The installed seam; `None` until the workspaces feature starts.
pub fn agent_shell(cx: &App) -> Option<&AgentShell> {
    cx.try_global::<AgentShell>()
}
