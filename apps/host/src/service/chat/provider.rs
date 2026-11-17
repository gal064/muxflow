//! What a chat adapter implements. Nothing above this trait names a provider.

use std::{
    path::Path,
    sync::{Mutex, PoisonError, Weak},
};

use serde::{Deserialize, Serialize};
use tmux_agent_protocol::v1;

pub(crate) use v1::chat_event::Change;

/// A refused chat request: a stable `error_code` and a line for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl ChatError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// How an agent was started in a pane, read from its command line, which is
/// how its chat is started in that pane. Kept on the pane, so the chat can be
/// started again the same way.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Launch {
    /// What runs the agent: `claude`, or `node` and the script it was given.
    pub(crate) program: Vec<String>,
    /// The folder the agent was told to work in, when that is not the one it
    /// was started in.
    pub(crate) cwd: String,
    /// Empty for the agent's own default.
    pub(crate) model: String,
    pub(crate) effort: String,
    /// Full when the agent was started with its permission checks off.
    pub(crate) full_access: bool,
    /// Directories the agent may work in besides its own.
    pub(crate) directories: Vec<String>,
    /// The name the session goes by, when one was given.
    pub(crate) name: String,
    /// Arguments the agent's chat takes exactly as its terminal did.
    pub(crate) passthrough: Vec<String>,
    pub(crate) resume: Resume,
    /// The last message kept of a conversation resumed part of the way.
    pub(crate) resume_at: String,
    /// The message given on the command line, sent as the chat's first.
    #[serde(skip)]
    pub(crate) prompt: String,
}

/// The conversation a chat starts on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Resume {
    #[default]
    Fresh,
    /// The most recent conversation in the agent's folder.
    Latest,
    Session {
        id: String,
        /// Continue in a copy, leaving the session itself as it is.
        fork: bool,
    },
}

/// What runs in a chat pane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PaneCommand {
    pub(crate) argv: Vec<String>,
    /// The agent speaks on its own stdin and stdout, which `chat-pipe` turns
    /// into the pane's socket. Otherwise the agent listens on the socket
    /// itself.
    pub(crate) piped: bool,
    /// The agent's id for the conversation this starts, when the command
    /// line settles it. Empty when only the agent can say.
    pub(crate) session: String,
}

/// One chat adapter, registered in `ChatRuntime` under its id.
pub(crate) trait ChatProvider: Send + Sync {
    fn adapter_id(&self) -> &'static str;

    /// What the provider can do, before a session has reported its catalog.
    fn capabilities(&self) -> v1::ChatCapabilities;

    /// The messages a pane's conversation already holds, oldest first. Runs
    /// on the lane of the connection that first opens the pane, ahead of that
    /// connection's later requests, so it must not wait on anything slow. A
    /// provider with a pane process reads its history on its session's own
    /// task instead, and hands it over with `ChatSink::reset`.
    fn load_history(&self, pane_id: &str) -> Vec<v1::ChatItem>;

    /// Starts the session for one pane. Everything it has to say goes through
    /// `sink`, from any thread, including from inside this call. Runs where
    /// `load_history` does. The session is dropped when the pane is gone, and
    /// from then on nothing of it may publish: the sink names the pane, and
    /// the same pane may by then hold a new session.
    fn connect(&self, pane_id: &str, sink: ChatSink) -> Box<dyn ChatSession>;

    /// Reads the command line of an agent of this adapter, `arguments` being
    /// what follows the program. `resumed` is the session the agent itself
    /// said it resumed, when it said so. `None` when that start cannot be a
    /// chat, and the pane stays a terminal.
    fn launch(&self, _arguments: &[String], _resumed: Option<&str>) -> Option<Launch> {
        None
    }

    /// What runs in the pane for `launch`, the agent talking over `socket`.
    /// `None` for a provider that has no pane process.
    fn pane_command(&self, _launch: &Launch, _socket: &Path) -> Option<PaneCommand> {
        None
    }

    /// Whether the agent says it has started, through its `SessionStart`
    /// hook, as soon as it is up. One that does becomes a chat when it says
    /// so, which is also when it says which session it resumed; any other
    /// when its process is seen.
    fn announces_start(&self) -> bool {
        false
    }

    /// The arguments that start the agent in a terminal set as `options`
    /// are: what a fork of a chat is started with, so that it has its
    /// source's model and permissions.
    fn terminal_options(&self, _options: &v1::ChatOptions) -> Vec<String> {
        Vec::new()
    }
}

/// One pane's live conversation with its provider.
pub(crate) trait ChatSession: Send {
    /// Accepts or refuses one command. Its effects arrive through the sink.
    /// Runs on the lane of the connection that sent it: hand slow work to the
    /// session's own thread and return.
    fn command(&mut self, command: SessionCommand) -> Result<(), ChatError>;

    /// Where a copy of the conversation through the end of the turn `at`
    /// names starts from, for the fork of a reply (D6). Answers the work that
    /// settles it, which may read files: the runtime runs it off every lock.
    /// The runtime has already checked that the timeline holds the reply,
    /// and that a turn still running is not the reply's last.
    fn fork(&mut self, _at: &ForkAt) -> Result<ForkPlan, ChatError> {
        Err(cannot_fork())
    }
}

/// The turn a reply's fork is cut after, as the pane's timeline has it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ForkAt {
    /// The user's messages from the reply back to the conversation's start,
    /// newest first: the one that opened the reply's turn is among them.
    pub(crate) before: Vec<String>,
    /// The user's message after the reply, which starts the next turn.
    /// Empty when the reply is the newest.
    pub(crate) next: String,
    /// The last item of the reply.
    pub(crate) through: String,
}

/// Settles a fork's launch, off every lock. The launch is the provider's
/// own: the runtime gives it its name and starts it.
pub(crate) type ForkPlan = Box<dyn FnOnce() -> Result<Launch, ChatError> + Send>;

/// A FORK of a reply for a provider that cannot start a copy at a turn.
pub(crate) fn cannot_fork() -> ChatError {
    ChatError::new(
        "chat_fork_unavailable",
        "This agent cannot fork from a reply.",
    )
}

/// A FORK of a reply the agent has not finished.
pub(crate) fn still_writing() -> ChatError {
    ChatError::new(
        "chat_turn_busy",
        "That reply is still being written. Fork it once it has finished.",
    )
}

/// The chat actions a provider carries out, one at a time per pane. The
/// runtime has already checked what does not depend on the provider: that an
/// answer names the pending request, that a rewind names a rewindable user
/// message, that a message sent mid-turn is one the provider's capabilities
/// allow, and that options are ones its catalog offers.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SessionCommand {
    Send {
        /// Empty only when the message is its attachments.
        text: String,
        /// The id the USER item must carry; never empty.
        item_id: String,
        /// Staged files, checked and with inline images read; the USER item
        /// carries them.
        attachments: Vec<super::Attached>,
    },
    Interrupt,
    Answer {
        request_id: String,
        answer: v1::ChatAnswer,
    },
    /// Forget `item_id` and everything after it. A session that accepts
    /// publishes `Change::Truncate` with `after_item_id` at the point where
    /// that takes effect: after the last thing it has to say about what is
    /// being forgotten, and before anything of the conversation that follows.
    Rewind {
        item_id: String,
        /// The item before `item_id`; empty when there is none.
        after_item_id: String,
    },
    /// The options asked for, whole: what the request left out is already
    /// filled in from the current ones. The session publishes the options it
    /// ends up with.
    SetOptions(v1::ChatOptions),
    /// Stop one of the tasks the state lists in `background`, one it says is
    /// `stoppable`. It leaves that list when the agent says it has ended.
    StopTask {
        task_id: String,
    },
}

/// Where a session reports what changed in its pane. `Change::Item` is a new
/// item, after every other; `Change::Updated` is the new whole of one already
/// published, which keeps its place and its kind. An update to an item the
/// timeline no longer holds, because it was rewound or trimmed away, is
/// dropped. The runtime cuts what is published to the wire's bounds; an item
/// whose id is empty or longer than 128 bytes is dropped.
#[derive(Clone)]
pub(crate) struct ChatSink {
    pub(super) panes: Weak<Mutex<super::Panes>>,
    pub(super) key: super::PaneKey,
}

impl ChatSink {
    /// Applies one change to the pane's timeline and state, and sends it to the
    /// connections that opened the pane. Answers the title the pane is to be
    /// given, when the change gives it another: a provider with a pane
    /// process sets it, off the lock.
    pub(crate) fn publish(&self, change: Change) -> Option<String> {
        let panes = self.panes.upgrade()?;
        let mut panes = panes.lock().unwrap_or_else(PoisonError::into_inner);
        panes.publish(&self.key, change)
    }

    /// Takes back an item published earlier that turned out to have nothing
    /// to show, such as thinking that ended with no text. Answers a new
    /// title as [`Self::publish`] does.
    pub(crate) fn withdraw(&self, item: v1::ChatItem) -> Option<String> {
        let panes = self.panes.upgrade()?;
        let mut panes = panes.lock().unwrap_or_else(PoisonError::into_inner);
        panes.withdraw(&self.key, item)
    }

    /// Replaces the pane's timeline with the conversation as the provider
    /// read it, oldest first. `complete` is false when the provider could not
    /// read all of it. The connections that opened the pane get a snapshot.
    /// Answers a new title as [`Self::publish`] does.
    pub(crate) fn reset(&self, items: Vec<v1::ChatItem>, complete: bool) -> Option<String> {
        let panes = self.panes.upgrade()?;
        let mut panes = panes.lock().unwrap_or_else(PoisonError::into_inner);
        panes.reset(&self.key, items, complete)
    }

    /// Takes the provider's own name for the conversation: a session's
    /// name, a thread's. Empty when it has none. Answers a new title as
    /// [`Self::publish`] does.
    pub(crate) fn name(&self, name: &str) -> Option<String> {
        let panes = self.panes.upgrade()?;
        let mut panes = panes.lock().unwrap_or_else(PoisonError::into_inner);
        panes.name(&self.key, name)
    }

    /// The pane this sink reports for.
    pub(crate) fn key(&self) -> &super::PaneKey {
        &self.key
    }
}
