//! What a chat adapter implements. Nothing above this trait names a provider.

use std::sync::{Mutex, PoisonError, Weak};

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

/// One chat adapter, registered in `ChatRuntime` under its id.
///
/// Stream B adds what a pane process needs (`pane_command`, and the socket
/// `connect` reads from); the scripted provider has no process.
pub(crate) trait ChatProvider: Send + Sync {
    fn adapter_id(&self) -> &'static str;

    /// What the provider can do, before a session has reported its catalog.
    fn capabilities(&self) -> v1::ChatCapabilities;

    /// The messages a pane's conversation already holds, oldest first. Runs
    /// on the lane of the connection that first opens the pane, ahead of that
    /// connection's later requests, so it must not wait on anything slow.
    fn load_history(&self, pane_id: &str) -> Vec<v1::ChatItem>;

    /// Starts the session for one pane. Everything it has to say goes through
    /// `sink`, from any thread, including from inside this call. Runs where
    /// `load_history` does. The session is dropped when the pane is gone, and
    /// from then on nothing of it may publish: the sink names the pane, and
    /// the same pane may by then hold a new session.
    fn connect(&self, pane_id: &str, sink: ChatSink) -> Box<dyn ChatSession>;
}

/// One pane's live conversation with its provider.
pub(crate) trait ChatSession: Send {
    /// Accepts or refuses one command. Its effects arrive through the sink.
    /// Runs on the lane of the connection that sent it: hand slow work to the
    /// session's own thread and return.
    fn command(&mut self, command: SessionCommand) -> Result<(), ChatError>;
}

/// The chat actions a provider carries out, one at a time per pane. The
/// runtime has already checked what does not depend on the provider: that an
/// answer names the pending request, that a rewind names a rewindable user
/// message, that a message sent mid-turn is one the provider's capabilities
/// allow, and that options are ones its catalog offers.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SessionCommand {
    Send {
        text: String,
        /// The id the USER item must carry; never empty.
        item_id: String,
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
    /// connections that opened the pane.
    pub(crate) fn publish(&self, change: Change) {
        if let Some(panes) = self.panes.upgrade() {
            panes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .publish(&self.key, change);
        }
    }
}
