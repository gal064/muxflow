//! How a pane becomes a chat (docs/design/chat-plan.md §3).
//!
//! In Chat mode an agent started in a terminal is started again, in the same
//! pane, as the headless process a chat talks to. What it was started with
//! is read from its process: its command line, its environment and its
//! folder. Only an agent that has just started is replaced, whatever says it
//! started, so turning Chat mode on never takes a running terminal session
//! away, and neither does a session that starts inside one.
//!
//! Two things say an agent has started. Its own `SessionStart` hook, which
//! also says which session it resumed; and, for an agent whose hook is not
//! installed or comes too late, its process appearing in the pane.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Mutex, PoisonError},
    time::Duration,
};

use tmux_agent_protocol::v1;

use super::{
    ChatRuntime, PaneKey, SessionCommand, pane,
    processes::{self, MAX_ENVIRONMENT_BYTES, Processes},
    provider::{ChatProvider, Launch, Resume},
};
use crate::service::{agents::adapters, history::History};

/// The oldest a process may be when it is seen and still be replaced. Its
/// user has typed nothing into it yet.
const MAX_SEEN_AGE: Duration = Duration::from_secs(10);
/// The oldest a `SessionStart` may be and still start a chat. One that waited
/// in the mailbox of a daemon that was down is about a session somebody may
/// be typing into by now, and replacing that would take their work away.
const MAX_HOOK_AGE_MILLIS: i64 = 10_000;
/// The oldest a process may be when its `SessionStart` arrives and still be
/// replaced. The hook names a pane, not a process: an agent that has run for
/// longer is not the one that just said it started, but one that resumed
/// another session or started an agent of its own.
const MAX_HOOKED_AGE: Duration = Duration::from_secs(20);
const MAX_PROCESSES: usize = 32;
const MAX_DEPTH: usize = 4;
/// Agents seen and left alone, remembered so each is looked at once.
const MAX_DECLINED: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Trigger {
    /// The agent's process is in the pane, beneath the pane's own process
    /// `pane_pid`.
    Seen { pane_pid: u32 },
    /// The agent said it started, on `session`; `resumed` when it said that
    /// is a session it resumed.
    Hooked {
        session: String,
        resumed: bool,
        occurred_at: i64,
        sequence: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Candidate {
    key: PaneKey,
    adapter_id: &'static str,
    trigger: Trigger,
}

#[derive(Default)]
struct Work {
    /// Waiting to be looked at, or being looked at.
    queued: BTreeSet<Candidate>,
    /// Agents seen that are not to become chats, by pane and process.
    declined: BTreeSet<(PaneKey, u32)>,
    /// A new process observed while its startup hook waits for folder trust.
    awaiting: BTreeMap<PaneKey, (u32, u64)>,
    /// Recent hook ordering also prevents a detection queued before activity
    /// from registering that process as still waiting for startup.
    hooks: BTreeMap<PaneKey, (u64, i64)>,
    next_hook: u64,
}

static WORK: Mutex<Work> = Mutex::new(Work {
    queued: BTreeSet::new(),
    declined: BTreeSet::new(),
    awaiting: BTreeMap::new(),
    hooks: BTreeMap::new(),
    next_hook: 0,
});

impl Work {
    fn hook(&mut self, key: &PaneKey, at: i64, starts: bool) -> u64 {
        if self.hooks.len() >= MAX_DECLINED && !self.hooks.contains_key(key) {
            // Losing ordering evidence must also lose eligibility.
            self.hooks.clear();
            self.awaiting.clear();
        }
        self.next_hook += 1;
        self.hooks.insert(key.clone(), (self.next_hook, at));
        if !starts {
            self.awaiting.remove(key);
        }
        self.next_hook
    }

    fn await_start(&mut self, key: &PaneKey, agent: &Agent, now: i64) {
        let (Some(age), Some(stamp)) = (agent.age, agent.start_stamp) else {
            return;
        };
        let born = now.saturating_sub(i64::try_from(age.as_millis()).unwrap_or(i64::MAX));
        if self.hooks.get(key).is_some_and(|(_, at)| *at >= born) {
            return;
        }
        if self.awaiting.len() >= MAX_DECLINED {
            self.awaiting.clear();
        }
        self.awaiting.insert(key.clone(), (agent.pid, stamp));
    }

    fn take_start(
        &mut self,
        key: &PaneKey,
        sequence: u64,
        occurred_at: i64,
        now: i64,
    ) -> Option<Option<(u32, u64)>> {
        if !self.current_start(key, sequence, occurred_at, now) {
            return None;
        }
        Some(self.awaiting.remove(key))
    }

    fn current_start(&self, key: &PaneKey, sequence: u64, occurred_at: i64, now: i64) -> bool {
        hook_is_fresh(occurred_at, now)
            && self.hooks.get(key).map(|(latest, _)| *latest) == Some(sequence)
    }
}
/// One conversion at a time: each reads a pane and then replaces what runs
/// in it.
static RUNNING: Mutex<()> = Mutex::new(());

/// The agents process detection found, one per pane at most. Called on every
/// look at tmux, so it does nothing but queue.
pub(crate) fn agents_detected<'a>(
    server_identity: &str,
    agents: impl Iterator<Item = (&'a tmux_control::Pane, &'static str)>,
) {
    let runtime = ChatRuntime::global();
    if runtime.agent_mode() != v1::ChatAgentMode::Chat {
        return;
    }
    for (pane, adapter_id) in agents {
        if pane.chat_adapter_id.is_empty() && runtime.provider(adapter_id).is_some() {
            submit(Candidate {
                key: PaneKey {
                    server_identity: server_identity.to_owned(),
                    pane_id: pane.id.clone(),
                },
                adapter_id,
                trigger: Trigger::Seen {
                    pane_pid: pane.pane_pid,
                },
            });
        }
    }
}

/// One hook event, as the agent runtime takes it in.
pub(crate) fn agent_hooked(event: &v1::AgentHookEvent, active_server_identity: &str) {
    let runtime = ChatRuntime::global();
    if runtime.agent_mode() != v1::ChatAgentMode::Chat
        || event.origin_server_identity != active_server_identity
    {
        return;
    }
    let adapter = if event.adapter_id.is_empty() {
        adapters::adapter(v1::AgentAdapterKind::try_from(event.adapter).unwrap_or_default())
    } else {
        adapters::by_id(&event.adapter_id)
    };
    let Some(adapter_id) = adapter.map(|adapter| adapter.kind()) else {
        return;
    };
    if !runtime
        .provider(adapter_id)
        .is_some_and(|provider| provider.announces_start())
    {
        return;
    }
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&event.payload_json) else {
        return;
    };
    let text = |name: &str| payload.get(name).and_then(serde_json::Value::as_str);
    let key = PaneKey {
        server_identity: active_server_identity.to_owned(),
        pane_id: event.pane_id.clone(),
    };
    let source = text(adapters::SESSION_START_SOURCE_FIELD).unwrap_or("startup");
    let starts = text("hook_event_name") == Some("SessionStart")
        && is_start(source)
        && hook_is_fresh(event.occurred_at_unix_millis, super::now_unix_millis());
    let sequence = work().hook(&key, super::now_unix_millis(), starts);
    if !starts {
        // Once a process has done anything beyond startup, a later resume
        // or nested session must not take over its terminal.
        return;
    }
    let session = if event.native_session_id.is_empty() {
        text("session_id").unwrap_or_default()
    } else {
        &event.native_session_id
    };
    submit(Candidate {
        key,
        adapter_id,
        trigger: Trigger::Hooked {
            session: session.to_owned(),
            resumed: source == "resume",
            occurred_at: event.occurred_at_unix_millis,
            sequence,
        },
    });
}

/// Whether a `SessionStart` that gives this as its source is an agent
/// starting: on a new session, on one it resumed, or on its own copy of one
/// (`--fork-session`). A session also "starts" when it is cleared or
/// compacted, which is the same agent carrying on.
fn is_start(source: &str) -> bool {
    matches!(source, "startup" | "resume" | "fork")
}

/// A hook that does not say when it happened may have happened at any time.
fn hook_is_fresh(occurred_at_unix_millis: i64, now_unix_millis: i64) -> bool {
    occurred_at_unix_millis > 0 && now_unix_millis - occurred_at_unix_millis <= MAX_HOOK_AGE_MILLIS
}

/// What is known of an agent when it is decided whether it becomes a chat.
struct Facts {
    /// Its process was seen; otherwise it said it started.
    seen: bool,
    /// How long it has run, when that could be read.
    age: Option<Duration>,
    /// How long Chat mode has been on, when that is known.
    chat_mode_age: Option<Duration>,
    /// It says when it starts, and its hook is installed.
    announces: bool,
    /// This exact process was observed young and has not emitted another hook.
    awaiting_start: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Replace,
    AwaitStart,
    /// Not this time.
    Leave,
    /// Not this process, however often it is seen.
    Decline,
}

fn verdict(facts: &Facts) -> Verdict {
    // An agent that was running before Chat mode was turned on was started
    // in a terminal on purpose, and stays one whatever it says later.
    let predates_mode = facts
        .age
        .is_some_and(|age| facts.chat_mode_age.is_some_and(|on| age >= on));
    if !facts.seen {
        let young = facts.age.is_some_and(|age| age <= MAX_HOOKED_AGE);
        return if (young || (facts.awaiting_start && facts.age.is_some())) && !predates_mode {
            Verdict::Replace
        } else {
            Verdict::Leave
        };
    }
    let young = facts.age.is_some_and(|age| age <= MAX_SEEN_AGE) && !predates_mode;
    // One whose hook is installed says it started, and says more.
    if young {
        if facts.announces {
            Verdict::AwaitStart
        } else {
            Verdict::Replace
        }
    } else {
        Verdict::Decline
    }
}

fn work() -> std::sync::MutexGuard<'static, Work> {
    WORK.lock().unwrap_or_else(PoisonError::into_inner)
}

fn submit(candidate: Candidate) {
    // Outside the daemon's runtime there is nothing to start a chat with.
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    if !work().queued.insert(candidate.clone()) {
        return;
    }
    runtime.spawn_blocking(move || {
        {
            let _one = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
            convert(&candidate);
        }
        work().queued.remove(&candidate);
    });
}

fn decline(key: &PaneKey, pid: u32) {
    let mut work = work();
    if work.declined.len() >= MAX_DECLINED {
        work.declined.clear();
    }
    work.declined.insert((key.clone(), pid));
}

fn convert(candidate: &Candidate) {
    let runtime = ChatRuntime::global();
    let Candidate {
        key,
        adapter_id,
        trigger,
    } = candidate;
    let Some(provider) = runtime.provider(adapter_id).cloned() else {
        return;
    };
    if runtime.agent_mode() != v1::ChatAgentMode::Chat {
        return;
    }
    let mode_epoch = *runtime
        .chat_since
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let processes = processes::system();
    let seen = matches!(trigger, Trigger::Seen { .. });
    // Every look at tmux sees the same agents again: one that was left alone
    // is known by its process, before anything is asked of tmux.
    if let Trigger::Seen { pane_pid } = trigger {
        let known = find_agent(&processes, *pane_pid, adapter_id)
            .is_some_and(|agent| work().declined.contains(&(key.clone(), agent.pid)));
        if known {
            return;
        }
    }
    // A pane that is a chat already is never made one again: its headless
    // agent starts a session too, and says so.
    let Some(tag) = pane::read(&key.pane_id).filter(|tag| tag.adapter.is_empty()) else {
        return;
    };
    let Some(agent) = find_agent(&processes, tag.pid, adapter_id) else {
        return;
    };
    let awaiting_start = if let Trigger::Hooked {
        occurred_at,
        sequence,
        ..
    } = trigger
    {
        let Some(waiting) =
            work().take_start(key, *sequence, *occurred_at, super::now_unix_millis())
        else {
            return;
        };
        agent
            .start_stamp
            .is_some_and(|stamp| waiting == Some((agent.pid, stamp)))
    } else {
        false
    };
    if seen && work().declined.contains(&(key.clone(), agent.pid)) {
        return;
    }
    match verdict(&Facts {
        seen,
        age: agent.age,
        chat_mode_age: runtime.chat_mode_age(),
        announces: seen && provider.announces_start() && hook_installed(adapter_id),
        awaiting_start,
    }) {
        Verdict::Replace => {}
        Verdict::Leave => return,
        Verdict::AwaitStart => {
            decline(key, agent.pid);
            work().await_start(key, &agent, super::now_unix_millis());
            return;
        }
        Verdict::Decline => {
            decline(key, agent.pid);
            return;
        }
    }
    // The session the agent said it is on, when it said.
    let session = match trigger {
        Trigger::Hooked { session, .. } => session.as_str(),
        Trigger::Seen { .. } => "",
    };
    let resumed = matches!(trigger, Trigger::Hooked { resumed: true, .. } if !session.is_empty())
        .then_some(session);
    let Some((program, arguments)) = split_program(&agent.argv) else {
        return;
    };
    let Some(mut launch) = provider.launch(arguments, resumed) else {
        if seen {
            decline(key, agent.pid);
        }
        return;
    };
    launch.program = program.to_vec();
    let prompt = std::mem::take(&mut launch.prompt);
    let guard = || {
        use anyhow::{Context, ensure};
        // Keep hook invalidation and mode changes serialized through respawn,
        // after all launch preparation. Hooks arriving next belong to the
        // replacement; an activity hook already ingested cancels conversion.
        let work = work();
        let mode = runtime.mode.lock().unwrap_or_else(PoisonError::into_inner);
        ensure!(*mode == Some(v1::ChatAgentMode::Chat), "agent mode changed");
        ensure!(
            *runtime
                .chat_since
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                == mode_epoch,
            "agent mode changed during conversion"
        );
        if let Trigger::Hooked {
            sequence,
            occurred_at,
            ..
        } = trigger
        {
            ensure!(
                work.current_start(key, *sequence, *occurred_at, super::now_unix_millis()),
                "startup was superseded"
            );
        }
        let current_tag = pane::read(&key.pane_id)
            .filter(|tag| tag.adapter.is_empty())
            .context("the terminal pane was replaced")?;
        let current =
            find_agent(&processes, current_tag.pid, adapter_id).context("the agent exited")?;
        ensure!(
            current.pid == agent.pid
                && agent.start_stamp.is_some()
                && current.start_stamp == agent.start_stamp
                && current.argv == agent.argv,
            "the agent process was replaced"
        );
        Ok((work, mode))
    };
    if replace(&key.pane_id, provider.as_ref(), &agent, &launch, "", guard).is_err() {
        if seen {
            decline(key, agent.pid);
        }
        return;
    }
    // The pane goes by the agent's name until its conversation names it:
    // the title it has is the terminal's.
    if let Some(adapter) = adapters::by_id(adapter_id) {
        pane::set_title(&key.pane_id, adapter.display_name());
    }
    note_replaced(&key.pane_id, adapter_id, &launch, session, tag.side);
    runtime.restart(key, adapter_id);
    super::topology_changed();
    if !prompt.trim().is_empty() {
        let _ = runtime.command(
            key,
            adapter_id,
            SessionCommand::Send {
                text: prompt,
                item_id: String::new(),
                attachments: Vec::new(),
            },
        );
    }
}

/// Replaces the agent running in a chat pane with one started from `launch`:
/// how a provider whose agent cannot go back in a conversation while it runs
/// starts it again from an earlier point. `rewound_to` is the pane's note of
/// that point.
pub(super) fn restart_agent(
    pane_id: &str,
    provider: &dyn ChatProvider,
    launch: &Launch,
    rewound_to: &str,
) -> anyhow::Result<()> {
    use anyhow::Context;
    let tag = pane::read(pane_id).context("the pane is gone")?;
    let agent = find_agent(&processes::system(), tag.pid, provider.adapter_id())
        .context("the agent is not running")?;
    replace(pane_id, provider, &agent, launch, rewound_to, || Ok(()))?;
    note_replaced(
        pane_id,
        provider.adapter_id(),
        launch,
        &tag.session,
        tag.side,
    );
    Ok(())
}

/// Tells the session history that the agent in a pane was replaced by one
/// started from `launch`. `session` is the one the agent that went was on,
/// when that is known: unless the new agent carries it on, it is no session
/// of its own any more. In a side fork's pane (`side`), the session that
/// takes its place is that fork.
fn note_replaced(pane_id: &str, adapter_id: &str, launch: &Launch, session: &str, side: bool) {
    let carried_on = matches!(&launch.resume, Resume::Session { id, fork: false } if id == session);
    if carried_on {
        return;
    }
    let history = History::global();
    if !session.is_empty() {
        history.replaced(adapter_id, session);
    }
    if side {
        history.hide_pane(pane_id);
    }
}

/// Starts `launch` in the pane in place of `agent`, in its folder and with
/// its environment, and tags the pane.
fn replace<G>(
    pane_id: &str,
    provider: &dyn ChatProvider,
    agent: &Agent,
    launch: &Launch,
    rewound_to: &str,
    guard: impl FnOnce() -> anyhow::Result<G>,
) -> anyhow::Result<()> {
    let start = Start::prepare(provider, agent, launch)?;
    let tags = start.tags(provider, launch, rewound_to);
    let tags = tags.each_ref().map(|(name, value)| (*name, value.as_str()));
    let started = guard().and_then(|_guard| pane::respawn(pane_id, &start.cwd, &start.line, &tags));
    if started.is_err() {
        start.discard();
    }
    started
}

/// Opens a chat started from `launch` in a split beside `source_pane_id`,
/// in the folder and with the environment of the chat's agent there (save
/// `TMUX_PANE`, which `chat-pipe` sets to the new pane's), and answers the
/// new pane: how a reply is forked (chat-polish-plan §3.8).
/// Tagged in the same step as the split, so nobody sees the new agent
/// without its tag and takes it for a terminal one. Never marked as a side
/// question's pane: the fork is listed in History (D12).
pub(crate) fn open_fork(
    source_pane_id: &str,
    provider: &dyn ChatProvider,
    launch: &Launch,
) -> anyhow::Result<String> {
    use anyhow::Context;
    let tag = pane::read(source_pane_id).context("the pane is gone")?;
    let agent = find_agent(&processes::system(), tag.pid, provider.adapter_id())
        .context("the agent is not running")?;
    let start = Start::prepare(provider, &agent, launch)?;
    let tags = start.tags(provider, launch, "");
    let tags = tags.each_ref().map(|(name, value)| (*name, value.as_str()));
    let opened = pane::split(source_pane_id, &start.cwd, &start.line, &tags, &launch.name);
    if opened.is_err() {
        start.discard();
    }
    opened
}

/// What a chat pane runs for one launch, with what it was given ready on
/// disk.
struct Start {
    cwd: String,
    socket: std::path::PathBuf,
    environment_path: std::path::PathBuf,
    line: String,
    session: String,
}

impl Start {
    /// The shell line that starts `launch` as `agent` was started: in its
    /// folder, with its environment, which is written for the line to read.
    fn prepare(
        provider: &dyn ChatProvider,
        agent: &Agent,
        launch: &Launch,
    ) -> anyhow::Result<Self> {
        use anyhow::Context;
        use std::{io::Write, os::unix::fs::OpenOptionsExt};

        let processes = processes::system();
        let environment = processes
            .environment(agent.pid)
            .context("read the environment")?;
        if environment.len() > MAX_ENVIRONMENT_BYTES {
            anyhow::bail!("the agent's environment is too large");
        }
        let started_in = processes.folder(agent.pid).context("read the folder")?;
        let cwd = if launch.cwd.is_empty() {
            started_in
        } else {
            started_in.join(&launch.cwd)
        };
        let (socket, environment_path) = pane::new_paths()?;
        let command = provider
            .pane_command(launch, &socket)
            .context("the provider has no pane process")?;
        // A helper that was replaced on disk can start nothing: the agent is
        // left as it is rather than stopped for a chat that cannot start.
        let helper = std::env::current_exe().context("locate the helper")?;
        if !helper.is_file() {
            anyhow::bail!("the helper is no longer where it was started from");
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&environment_path)
            .and_then(|mut file| file.write_all(&environment))
            .context("write the environment")?;
        let line = pane::shell_line(
            &helper,
            Some(&environment_path),
            command.piped.then_some(socket.as_path()),
            &command.argv,
        );
        Ok(Self {
            cwd: cwd.to_string_lossy().into_owned(),
            socket,
            environment_path,
            line,
            session: command.session,
        })
    }

    /// The options that make the pane this chat's.
    fn tags(
        &self,
        provider: &dyn ChatProvider,
        launch: &Launch,
        rewound_to: &str,
    ) -> [(&'static str, String); 5] {
        [
            (pane::ADAPTER, provider.adapter_id().to_owned()),
            (pane::SOCKET, self.socket.to_string_lossy().into_owned()),
            (pane::SESSION, self.session.clone()),
            (pane::REWOUND_TO, rewound_to.to_owned()),
            (pane::START, pane::encode_launch(launch)),
        ]
    }

    /// The chat did not start: nothing will read what it was given.
    fn discard(self) {
        let _ = std::fs::remove_file(&self.environment_path);
    }
}

/// What the pane's agent has for `names` in its environment, for a provider
/// that must look where its agent looks. Nothing else of the environment
/// leaves here.
pub(super) fn agent_variables(
    pane_id: &str,
    adapter_id: &str,
    names: &[&str],
) -> Vec<Option<std::ffi::OsString>> {
    let environment = agent_environment(pane_id, adapter_id).unwrap_or_default();
    names
        .iter()
        .map(|name| {
            environment
                .iter()
                .find(|(given, _)| given == name)
                .map(|(_, value)| value.clone())
        })
        .collect()
}

/// The whole environment of the pane's agent, for running the agent's own
/// command line as it would run: where it keeps its settings and its login.
/// `None` when the agent is not running.
pub(super) fn agent_environment(
    pane_id: &str,
    adapter_id: &str,
) -> Option<Vec<(std::ffi::OsString, std::ffi::OsString)>> {
    let processes = processes::system();
    pane::read(pane_id)
        .and_then(|tag| find_agent(&processes, tag.pid, adapter_id))
        .and_then(|agent| processes.environment(agent.pid).ok())
        .filter(|bytes| bytes.len() <= MAX_ENVIRONMENT_BYTES)
        .map(|bytes| crate::chat_pipe::parse_environment(&bytes))
}

fn hook_installed(adapter_id: &str) -> bool {
    crate::service::agents::HookManager::system_default()
        .and_then(|manager| manager.managed_adapters())
        .is_ok_and(|managed| managed.contains(&adapter_id))
}

/// The agent's own arguments, and what runs it: the program, or an
/// interpreter and the script it was given.
pub(super) fn split_program(argv: &[String]) -> Option<(&[String], &[String])> {
    let interpreted = matches!(argv.first()?.rsplit('/').next()?, "node" | "nodejs" | "bun");
    argv.split_at_checked(if interpreted { 2 } else { 1 })
}

pub(super) struct Agent {
    pub(super) pid: u32,
    pub(super) argv: Vec<String>,
    /// How long ago it started, when that could be read.
    age: Option<Duration>,
    start_stamp: Option<u64>,
}

/// The pane's agent: the process nearest the pane's own that is one of
/// `adapter_id`'s.
fn find_agent(processes: &impl Processes, root_pid: u32, adapter_id: &str) -> Option<Agent> {
    find_any_agent(processes, root_pid)
        .filter(|(kind, _)| *kind == adapter_id)
        .map(|(_, agent)| agent)
}

/// The pane's agent, whichever adapter's it is, with that adapter's id: the
/// process nearest the pane's own that is an agent. Looking stops at the
/// first one, since what runs beneath an agent is that agent's business.
pub(super) fn find_any_agent(
    processes: &impl Processes,
    root_pid: u32,
) -> Option<(&'static str, Agent)> {
    if root_pid == 0 {
        return None;
    }
    let mut queue = std::collections::VecDeque::from([(root_pid, 0)]);
    let mut visited = BTreeSet::new();
    while let Some((pid, depth)) = queue.pop_front() {
        if visited.len() >= MAX_PROCESSES || !visited.insert(pid) {
            continue;
        }
        if let Some(argv) = processes.argv(pid)
            && let Some(adapter) = adapters::detect_argv(&argv)
        {
            let agent = Agent {
                pid,
                age: processes.age(pid),
                start_stamp: processes.start_stamp(pid),
                argv,
            };
            return Some((adapter.kind(), agent));
        }
        if depth >= MAX_DEPTH {
            continue;
        }
        queue.extend(
            processes
                .children(pid)
                .into_iter()
                .map(|pid| (pid, depth + 1)),
        );
    }
    None
}

/// How many values a command-line option takes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Takes {
    Nothing,
    One,
    /// One, when the next argument is not an option.
    Maybe,
    /// Every argument up to the next option.
    Many,
}

/// A command line read against the options a provider knows.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Arguments {
    /// Each option as it was given, by the first of its names, with its
    /// values.
    pub(super) options: Vec<(&'static str, Vec<String>)>,
    pub(super) positional: Vec<String>,
}

impl Arguments {
    /// Reads `arguments` against `known`: each entry is an option's names
    /// and what it takes. `None` when an option is not a known one, since
    /// there is then no telling which of the arguments are its values.
    pub(super) fn read(arguments: &[String], known: &[(&[&'static str], Takes)]) -> Option<Self> {
        let mut read = Self::default();
        let mut rest = arguments.iter().peekable();
        while let Some(argument) = rest.next() {
            if argument == "--" {
                read.positional.extend(rest.cloned());
                break;
            }
            if !argument.starts_with('-') || argument == "-" {
                read.positional.push(argument.clone());
                continue;
            }
            let (name, attached) = match argument.split_once('=') {
                Some((name, value)) if name.starts_with("--") => (name, Some(value)),
                _ => (argument.as_str(), None),
            };
            let (names, takes) = known.iter().find(|(names, _)| names.contains(&name))?;
            let mut values = Vec::new();
            let is_value = |next: &&String| !next.starts_with('-') || next.as_str() == "-";
            match (takes, attached) {
                (Takes::Nothing, None) => {}
                (Takes::Nothing, Some(_)) => return None,
                (_, Some(value)) => values.push(value.to_owned()),
                (Takes::One, None) => values.push(rest.next()?.clone()),
                (Takes::Maybe, None) => values.extend(rest.next_if(is_value).cloned()),
                (Takes::Many, None) => {
                    while let Some(value) = rest.next_if(is_value) {
                        values.push(value.clone());
                    }
                }
            }
            read.options.push((names[0], values));
        }
        Some(read)
    }

    pub(super) fn has(&self, name: &str) -> bool {
        self.options.iter().any(|(given, _)| *given == name)
    }

    /// The last value given for `name`.
    pub(super) fn value<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        self.values(name).last().map(String::as_str)
    }

    /// Every value given for `name`, in order.
    pub(super) fn values<'a>(
        &'a self,
        name: &'a str,
    ) -> impl DoubleEndedIterator<Item = &'a String> {
        self.options
            .iter()
            .filter(move |(given, _)| *given == name)
            .flat_map(|(_, values)| values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    const KNOWN: &[(&[&str], Takes)] = &[
        (&["--model", "-m"], Takes::One),
        (&["--verbose"], Takes::Nothing),
        (&["--resume", "-r"], Takes::Maybe),
        (&["--add-dir"], Takes::Many),
    ];

    #[test]
    fn a_command_line_is_read_against_the_options_known() {
        let read = Arguments::read(
            &strings(&[
                "-m",
                "opus",
                "--add-dir",
                "/a",
                "/b",
                "--verbose",
                "-r",
                "--model=haiku",
                "fix",
                "--",
                "--not-an-option",
            ]),
            KNOWN,
        )
        .unwrap();
        assert_eq!(read.value("--model"), Some("haiku"));
        assert_eq!(
            read.values("--add-dir").cloned().collect::<Vec<_>>(),
            ["/a", "/b"]
        );
        assert!(read.has("--verbose"));
        // `-r` is followed by an option, so it has no value.
        assert_eq!(read.value("--resume"), None);
        assert!(read.has("--resume"));
        assert_eq!(read.positional, ["fix", "--not-an-option"]);

        // An optional value is taken when one follows.
        let read = Arguments::read(&strings(&["-r", "abc"]), KNOWN).unwrap();
        assert_eq!(read.value("--resume"), Some("abc"));
        assert!(read.positional.is_empty());
    }

    #[test]
    fn an_option_nobody_knows_makes_the_line_unreadable() {
        assert_eq!(Arguments::read(&strings(&["--mystery", "x"]), KNOWN), None);
        assert_eq!(Arguments::read(&strings(&["--verbose=1"]), KNOWN), None);
        // A value that is missing.
        assert_eq!(Arguments::read(&strings(&["--model"]), KNOWN), None);
    }

    #[test]
    fn the_program_is_what_runs_the_agent() {
        let argv = strings(&["/usr/bin/node", "/opt/claude-code/cli.js", "--model", "x"]);
        let (program, arguments) = split_program(&argv).unwrap();
        assert_eq!(program, &argv[..2]);
        assert_eq!(arguments, &argv[2..]);
        let argv = strings(&["claude"]);
        let (program, arguments) = split_program(&argv).unwrap();
        assert_eq!(program, &argv[..]);
        assert!(arguments.is_empty());
        assert!(split_program(&strings(&["node"])).is_none());
    }

    #[test]
    fn only_an_agent_that_has_just_started_is_replaced() {
        let seconds = Duration::from_secs;
        let facts = |seen, age: Option<u64>, mode: Option<u64>, announces| Facts {
            seen,
            age: age.map(seconds),
            chat_mode_age: mode.map(seconds),
            announces,
            awaiting_start: false,
        };
        // Seen: young, started after Chat mode came on, and with no hook to
        // say more.
        assert_eq!(
            verdict(&facts(true, Some(2), Some(600), false)),
            Verdict::Replace
        );
        assert_eq!(
            verdict(&facts(true, Some(2), None, false)),
            Verdict::Replace
        );
        assert_eq!(
            verdict(&facts(true, Some(11), Some(600), false)),
            Verdict::Decline
        );
        assert_eq!(
            verdict(&facts(true, None, Some(600), false)),
            Verdict::Decline
        );
        assert_eq!(
            verdict(&facts(true, Some(2), Some(600), true)),
            Verdict::AwaitStart
        );
        // Chat mode was turned on a second ago; the agent is older.
        assert_eq!(
            verdict(&facts(true, Some(2), Some(1), false)),
            Verdict::Decline
        );
        assert_eq!(
            verdict(&facts(true, Some(2), Some(2), false)),
            Verdict::Decline
        );

        // Hooked: the process in the pane is the one that said it started
        // only if it has just started too.
        let hooked = |age, mode| verdict(&facts(false, age, mode, false));
        assert_eq!(hooked(Some(5), Some(600)), Verdict::Replace);
        assert_eq!(hooked(Some(5), None), Verdict::Replace);
        // A session that has run for an hour, in which another one started:
        // it resumed one, or it started an agent of its own. A daemon that
        // was started since does not know when Chat mode came on.
        assert_eq!(hooked(Some(3600), None), Verdict::Leave);
        assert_eq!(hooked(Some(21), Some(600)), Verdict::Leave);
        assert_eq!(hooked(None, Some(600)), Verdict::Leave);
        // Started before Chat mode came on. It may say it started again.
        assert_eq!(hooked(Some(8), Some(5)), Verdict::Leave);
    }

    #[test]
    fn an_agent_starts_on_a_new_session_a_resumed_one_or_a_copy() {
        // What Claude gives as the source of a `SessionStart`.
        for source in ["startup", "resume", "fork"] {
            assert!(is_start(source), "{source}");
        }
        // The same agent carrying on.
        for source in ["clear", "compact", ""] {
            assert!(!is_start(source), "{source}");
        }
    }

    #[test]
    fn delayed_startup_is_allowed_only_for_an_observed_launch_in_the_same_mode() {
        let mut facts = Facts {
            seen: false,
            age: Some(Duration::from_secs(90)),
            chat_mode_age: Some(Duration::from_secs(600)),
            announces: true,
            awaiting_start: true,
        };
        assert_eq!(verdict(&facts), Verdict::Replace);
        facts.awaiting_start = false;
        assert_eq!(verdict(&facts), Verdict::Leave);
        facts.awaiting_start = true;
        facts.chat_mode_age = Some(Duration::from_secs(10));
        assert_eq!(verdict(&facts), Verdict::Leave);
        facts.chat_mode_age = Some(Duration::from_secs(600));
        facts.age = None;
        assert_eq!(verdict(&facts), Verdict::Leave);
        assert!(!hook_is_fresh(1, 20_000));
        assert!(!hook_is_fresh(0, 1));
        assert!(hook_is_fresh(19_999, 20_000));
    }

    #[test]
    fn startup_waiting_is_bound_to_the_process_and_cancelled_by_activity() {
        let key = PaneKey {
            server_identity: "server".into(),
            pane_id: "%1".into(),
        };
        let agent = Agent {
            pid: 42,
            argv: vec![],
            age: Some(Duration::from_secs(2)),
            start_stamp: Some(123),
        };
        let mut work = Work::default();
        work.await_start(&key, &agent, 10_000);
        assert_eq!(work.awaiting.get(&key), Some(&(42, 123)));
        let startup = work.hook(&key, 90_000, true);
        let newer = work.hook(&key, 90_001, true);
        assert_eq!(work.take_start(&key, startup, 90_000, 90_002), None);
        assert_eq!(
            work.take_start(&key, newer, 90_001, 90_002),
            Some(Some((42, 123)))
        );
        // Activity after launch preparation also cancels the final guard.
        assert!(work.current_start(&key, newer, 90_001, 90_002));
        work.hook(&key, 90_003, false);
        assert!(!work.current_start(&key, newer, 90_001, 90_004));
        // It is consumed once; a later resume cannot reuse it.
        assert_eq!(work.awaiting.remove(&key), None);

        let mut work = Work::default();
        work.await_start(&key, &agent, 10_000);
        let startup = work.hook(&key, 90_000, true);
        work.hook(&key, 90_000, false);
        assert!(!work.awaiting.contains_key(&key));
        assert_ne!(
            work.hooks[&key].0, startup,
            "same-millisecond activity supersedes startup"
        );
        // A detection queued before the hook cannot reinstate eligibility.
        work.await_start(&key, &agent, 90_001);
        assert!(!work.awaiting.contains_key(&key));

        // A fresh process in the same pane can wait again.
        work.await_start(&key, &agent, 100_000);
        let waiting = work.awaiting.remove(&key);
        assert_ne!(waiting, Some((43, 123)), "different PID");
        assert_ne!(waiting, Some((42, 124)), "reused PID");
    }

    /// The store is the process's: the sessions here are this test's own.
    #[test]
    fn the_history_is_told_which_session_a_replaced_agent_left_behind() {
        let listed = |session: &str| {
            let entries = History::global().list().entries;
            entries
                .iter()
                .any(|entry| entry.native_session_id == session)
        };
        let seen = |session: &str, pane_id: &str| crate::service::history::Seen {
            adapter_id: "claude-code".into(),
            native_session_id: session.into(),
            pane_id: pane_id.into(),
            ..Default::default()
        };
        let observe = |sessions: &[(&str, &str)]| {
            let sessions = sessions.iter();
            History::global().observe(
                sessions
                    .map(|(session, pane)| seen(session, pane))
                    .collect(),
            );
        };
        let launch = |resume| Launch {
            resume,
            ..Default::default()
        };
        let on = |id: &str, fork| Resume::Session {
            id: id.into(),
            fork,
        };
        observe(&[("convert-new", "%901"), ("convert-kept", "%902")]);

        // A terminal agent replaced by a chat on a session of its own, and
        // one whose chat carries its session on.
        note_replaced(
            "%901",
            "claude-code",
            &launch(Resume::Fresh),
            "convert-new",
            false,
        );
        let kept = launch(on("convert-kept", false));
        note_replaced("%902", "claude-code", &kept, "convert-kept", false);
        assert!(!listed("convert-new"));
        assert!(listed("convert-kept"));

        // A side fork's agent, replaced by a chat that copies the source
        // itself: neither the terminal's copy nor the chat's is listed. A
        // rewind that keeps the chat's session leaves the pane to it.
        History::global().hide_pane("%903");
        observe(&[("convert-kept", "%902"), ("fork-terminal", "%903")]);
        let copy = launch(on("fork-source", true));
        note_replaced("%903", "claude-code", &copy, "fork-terminal", true);
        observe(&[("convert-kept", "%902"), ("fork-chat", "%903")]);
        let same = launch(on("fork-chat", false));
        note_replaced("%903", "claude-code", &same, "fork-chat", true);
        observe(&[("convert-kept", "%902"), ("fork-later", "%903")]);
        assert!(!listed("fork-terminal") && !listed("fork-chat"));
        assert!(listed("fork-later"));
    }

    #[test]
    fn a_session_start_that_waited_starts_no_chat() {
        let now = 1_700_000_000_000;
        assert!(hook_is_fresh(now, now));
        assert!(hook_is_fresh(now - MAX_HOOK_AGE_MILLIS, now));
        assert!(!hook_is_fresh(now - MAX_HOOK_AGE_MILLIS - 1, now));
        // A clock that differs a little the other way.
        assert!(hook_is_fresh(now + 500, now));
        // One that does not say when it happened.
        assert!(!hook_is_fresh(0, now));
    }

    fn process(
        root: &std::path::Path,
        pid: u32,
        argv: &[&str],
        children: &[u32],
        start_ticks: u64,
    ) {
        let directory = root.join(pid.to_string());
        let task = directory.join("task").join(pid.to_string());
        std::fs::create_dir_all(&task).unwrap();
        let mut cmdline = argv.join("\0").into_bytes();
        cmdline.push(0);
        std::fs::write(directory.join("cmdline"), cmdline).unwrap();
        let children: Vec<String> = children.iter().map(u32::to_string).collect();
        std::fs::write(task.join("children"), children.join(" ")).unwrap();
        // A command name with spaces and a parenthesis, as a process may have.
        let fields = ["0"; 18].join(" ");
        std::fs::write(
            directory.join("stat"),
            format!("{pid} (a b) c) S {fields} {start_ticks} 0 0\n"),
        )
        .unwrap();
    }

    #[test]
    fn the_panes_agent_is_the_first_one_beneath_its_shell() {
        let root = tempfile::tempdir().unwrap();
        // SAFETY: `sysconf` reads a constant of the running system.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
        std::fs::write(root.path().join("uptime"), "1000.00 900.00\n").unwrap();
        process(root.path(), 10, &["-zsh"], &[11, 12], 0);
        process(root.path(), 11, &["sleep", "5"], &[], 0);
        process(
            root.path(),
            12,
            &["claude", "--model", "opus", ""],
            &[13],
            995 * ticks,
        );
        // An agent started by the agent is not the pane's.
        process(root.path(), 13, &["codex", "exec", "x"], &[], 999 * ticks);

        let agent = find_agent(&processes::Proc(root.path()), 10, "claude-code").unwrap();
        assert_eq!(agent.pid, 12);
        // An empty argument is an argument.
        assert_eq!(agent.argv, ["claude", "--model", "opus", ""]);
        assert_eq!(agent.age, Some(Duration::from_secs(5)));
        assert!(find_agent(&processes::Proc(root.path()), 10, "codex").is_none());
        assert!(find_agent(&processes::Proc(root.path()), 11, "claude-code").is_none());
        assert!(find_agent(&processes::Proc(root.path()), 0, "claude-code").is_none());
    }
}
