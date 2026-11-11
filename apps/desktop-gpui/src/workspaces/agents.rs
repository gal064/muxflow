//! The seam the agents feature fills: what the sidebar and the tab strip show
//! about agents, and the Agents section under the sidebar divider.
//!
//! The workspace shell only reads this. Everything starts empty, which the
//! shell treats as "not known yet": a terminal tab's presence is
//! [`AgentPresence::Unknown`], so Close All Non-Agent Tabs leaves every
//! terminal open until agents are known.

use std::{collections::HashMap, rc::Rc};

use gpui_kit::{AnyElement, App, Global, Window};

/// One agent's state as the rows draw it. `Done` is an idle agent whose
/// finished turn nobody has looked at yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Working,
    Blocked,
    Idle,
    Unknown,
    Done,
}

impl AgentState {
    /// The word the rows and labels use for it.
    pub fn word(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Idle => "idle",
            Self::Unknown => "unknown",
            Self::Done => "done",
        }
    }
}

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
    pub state: AgentState,
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
}

/// What a terminal tab says about the agents in its window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowAgents {
    /// The loudest agent's state, and its adapter for the tab's mark.
    pub attention: Option<(AgentState, String)>,
    pub presence: AgentPresence,
}

type Section = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;
type Mark = Rc<dyn Fn(&str, AgentState, &App) -> AnyElement>;

/// Filled by the agents feature with `cx.set_global` or `cx.update_global`;
/// the shell re-renders when it changes.
#[derive(Clone, Default)]
pub struct AgentShell {
    /// The Agents section's list, under the sidebar divider. Without it the
    /// section says no agents were detected.
    pub section: Option<Section>,
    /// An agent's mark (adapter icon with its state); a state dot without it.
    pub mark: Option<Mark>,
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

/// The installed seam; `None` until the agents feature installs it.
pub fn agent_shell(cx: &App) -> Option<&AgentShell> {
    cx.try_global::<AgentShell>()
}
