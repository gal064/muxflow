//! Chat mode's host runtime (docs/design/chat-plan.md §3).
//!
//! One process-global `ChatRuntime` holds a session per chat pane: its state,
//! its catalog and a bounded timeline. A provider reports changes through a
//! `ChatSink`; the runtime applies each one, numbers it with the pane's next
//! revision, and sends it to the connections that opened that pane and to no
//! others. Every string and list that crosses the wire is cut to a bound here,
//! so no provider has to remember to.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError},
    time::Duration,
};

use prost::Message as _;
use serde::{Deserialize, Serialize};
use tmux_agent_protocol::v1;

use super::events::{Delivery, send_control_event_to, send_required_control_event_to};
use super::snapshot::{load_sidecar, save_sidecar};

mod provider;
mod scripted;
#[cfg(test)]
mod tests;

use provider::{Change, ChatProvider, ChatSession, ChatSink};
pub(crate) use provider::{ChatError, SessionCommand};

/// The most items one snapshot or one older page carries.
const MAX_PAGE_ITEMS: usize = 200;
/// The most a snapshot's or a page's items weigh, so the frame that carries
/// them stays far under `MAX_FRAME_BYTES`. One item always fits.
const MAX_PAGE_BYTES: usize = 1024 * 1024;
/// The most text one item holds; text past it is dropped.
const MAX_ITEM_TEXT_BYTES: usize = 256 * 1024;
/// The most of a tool's output an item carries.
const MAX_OUTPUT_PREVIEW_BYTES: usize = 16 * 1024;
/// The most a tool's or a pending request's `detail` holds: a command line can
/// be a whole heredoc.
const MAX_DETAIL_BYTES: usize = 16 * 1024;
const MAX_PATH_BYTES: usize = 4096;
/// The most any other string holds: a title, a label, a description, an id.
const MAX_LABEL_BYTES: usize = 1024;
/// The longest item id. An item with a longer one is dropped, since a cut id
/// would name a different item.
const MAX_ID_BYTES: usize = 128;
const MAX_QUESTIONS: usize = 8;
const MAX_QUESTION_OPTIONS: usize = 16;
const MAX_MODELS: usize = 64;
const MAX_MODEL_EFFORTS: usize = 16;
const MAX_COMMANDS: usize = 256;
/// The most an ANSWER's payload weighs.
const MAX_ANSWER_BYTES: usize = 64 * 1024;
/// The most items a pane's timeline holds, and the most their text weighs; the
/// oldest are dropped past either.
const MAX_TIMELINE_ITEMS: usize = 2_000;
const MAX_TIMELINE_BYTES: usize = 32 * 1024 * 1024;

const SETTINGS_LABEL: &str = "settings";

/// The host state directory's `settings.json`.
#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Settings {
    /// "chat", or anything else for the terminal.
    agent_mode: String,
}

type SharedSession = Arc<Mutex<Box<dyn ChatSession>>>;

struct Pane {
    revision: u64,
    state: v1::ChatState,
    catalog: v1::ChatCatalog,
    /// Oldest first.
    items: Vec<v1::ChatItem>,
    /// The timeline has dropped its oldest items.
    trimmed: bool,
    /// The connections that opened this pane.
    subscribers: BTreeSet<u64>,
    /// Absent only while the provider is connecting.
    session: Option<SharedSession>,
}

/// One pane of one tmux server. A pane id is only unique within its server.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PaneKey {
    pub(crate) server_identity: String,
    pub(crate) pane_id: String,
}

/// Every chat pane.
#[derive(Default)]
pub(super) struct Panes(BTreeMap<PaneKey, Pane>);

pub(crate) struct ChatRuntime {
    providers: BTreeMap<&'static str, Arc<dyn ChatProvider>>,
    panes: Arc<Mutex<Panes>>,
    settings_path: PathBuf,
    /// The stored agent mode, read from `settings_path` on first use.
    mode: Mutex<Option<v1::ChatAgentMode>>,
}

static GLOBAL: OnceLock<ChatRuntime> = OnceLock::new();

impl ChatRuntime {
    pub(crate) fn global() -> &'static Self {
        GLOBAL.get_or_init(|| {
            // Tests share this one runtime: they get the scripted provider
            // without its pauses, and a state directory that is not the real
            // one.
            let (state_dir, step) = if cfg!(test) {
                let name = format!("muxflow-host-test-{}", std::process::id());
                (std::env::temp_dir().join(name), Duration::ZERO)
            } else {
                (crate::paths::state_dir(), scripted::STEP)
            };
            let switch = std::env::var_os(scripted::SWITCH);
            let providers = registered_providers(cfg!(test), switch.as_deref(), step);
            Self::new(state_dir.join("settings.json"), providers)
        })
    }

    fn new(settings_path: PathBuf, providers: Vec<Arc<dyn ChatProvider>>) -> Self {
        Self {
            providers: providers
                .into_iter()
                .map(|provider| (provider.adapter_id(), provider))
                .collect(),
            panes: Arc::default(),
            settings_path,
            mode: Mutex::new(None),
        }
    }

    fn panes(&self) -> MutexGuard<'_, Panes> {
        self.panes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The chat adapter a pane is reported with, given the one it is tagged
    /// with. The scripted provider has no pane process to tag, so while it is
    /// registered every untagged pane is its chat.
    pub(crate) fn adapter_for(&self, tagged: &str) -> String {
        if tagged.is_empty() && self.providers.contains_key(scripted::ADAPTER_ID) {
            scripted::ADAPTER_ID.to_owned()
        } else {
            tagged.to_owned()
        }
    }

    /// Makes sure the pane has a session, starting one with the provider
    /// registered as `adapter_id`.
    fn ensure_session(&self, key: &PaneKey, adapter_id: &str) -> Result<(), ChatError> {
        if self.panes().0.contains_key(key) {
            return Ok(());
        }
        let provider = self.providers.get(adapter_id).ok_or_else(unavailable)?;
        let pane_id = key.pane_id.as_str();
        // Read outside the lock: a provider's history is a file on disk.
        let mut items = provider.load_history(pane_id);
        items.retain_mut(bound_item);
        {
            let mut panes = self.panes();
            if panes.0.contains_key(key) {
                return Ok(());
            }
            let mut catalog = v1::ChatCatalog {
                capabilities: Some(provider.capabilities()),
                ..Default::default()
            };
            bound_catalog(&mut catalog);
            let mut pane = Pane {
                revision: 0,
                state: v1::ChatState {
                    pane_id: pane_id.to_owned(),
                    adapter_id: provider.adapter_id().to_owned(),
                    turn: v1::ChatTurnState::Idle.into(),
                    connection: v1::ChatConnection::Connecting.into(),
                    history_complete: true,
                    ..Default::default()
                },
                catalog,
                items,
                trimmed: false,
                subscribers: BTreeSet::new(),
                session: None,
            };
            if pane.trim() {
                pane.state.history_complete = false;
            }
            panes.0.insert(key.clone(), pane);
        }
        // Connected outside the lock, with the pane already in place: the
        // provider may publish from inside `connect`.
        let session = provider.connect(
            pane_id,
            ChatSink {
                panes: Arc::downgrade(&self.panes),
                key: key.clone(),
            },
        );
        // The pane may have been forgotten meanwhile, and even opened again.
        // A session nothing took is dropped outside the lock: it may publish
        // as it ends.
        let unused = match self.panes().0.get_mut(key) {
            Some(pane) if pane.session.is_none() => {
                pane.session = Some(Arc::new(Mutex::new(session)));
                None
            }
            _ => Some(session),
        };
        drop(unused);
        Ok(())
    }

    /// Runs `use_pane` on the pane's chat, under the runtime's lock, starting
    /// the chat first if there is none.
    ///
    /// A chat nobody has open can be forgotten between its start and its
    /// first use, by a look at tmux taken before the pane existed. It is
    /// then started again.
    fn with_pane<T>(
        &self,
        key: &PaneKey,
        adapter_id: &str,
        use_pane: impl FnOnce(&mut Pane) -> Result<T, ChatError>,
    ) -> Result<T, ChatError> {
        loop {
            self.ensure_session(key, adapter_id)?;
            if let Some(pane) = self.panes().0.get_mut(key) {
                return use_pane(pane);
            }
        }
    }

    /// Subscribes `connection_id` to the pane and queues the pane's snapshot
    /// for it, as a chat event. Queued under the runtime's lock, so no later
    /// change can reach the connection ahead of it.
    pub(crate) fn open(
        &self,
        connection_id: u64,
        key: &PaneKey,
        adapter_id: &str,
    ) -> Result<(), ChatError> {
        self.with_pane(key, adapter_id, |pane| {
            let snapshot = pane.event(Change::Snapshot(pane.snapshot()));
            if send_control_event_to(connection_id, snapshot) != Delivery::Sent {
                return Err(ChatError::new(
                    "chat_queue_full",
                    "The connection is too busy to open this chat. Try again.",
                ));
            }
            pane.subscribers.insert(connection_id);
            Ok(())
        })
    }

    /// Ends `connection_id`'s subscription to one pane.
    pub(crate) fn close(&self, connection_id: u64, key: &PaneKey) {
        if let Some(pane) = self.panes().0.get_mut(key) {
            pane.subscribers.remove(&connection_id);
        }
    }

    /// Forgets chats whose pane is gone: every chat that is not of a pane
    /// `is_live` finds on `server_identity`, the tmux server as `viewer` sees
    /// it.
    ///
    /// A connection's view may be the stale one, so it only speaks for the
    /// chats it has open: it stops hearing from them, and one nobody else has
    /// open is forgotten. `None` is the host's own fresh look at tmux, which
    /// forgets every such chat nobody has open.
    pub(crate) fn forget_closed(
        &self,
        viewer: Option<u64>,
        server_identity: &str,
        is_live: impl Fn(&str) -> bool,
    ) {
        let forgotten: Vec<Pane> = {
            let mut panes = self.panes();
            let mut closed = Vec::new();
            for (key, pane) in &mut panes.0 {
                if key.server_identity == server_identity && is_live(&key.pane_id) {
                    continue;
                }
                let let_go = match viewer {
                    Some(connection_id) => pane.subscribers.remove(&connection_id),
                    None => true,
                };
                if let_go && pane.subscribers.is_empty() {
                    closed.push(key.clone());
                }
            }
            closed
                .iter()
                .filter_map(|key| panes.0.remove(key))
                .collect()
        };
        // Dropped outside the lock: a session may publish as it ends.
        drop(forgotten);
    }

    /// Ends every subscription of a connection that has gone.
    pub(crate) fn disconnect(&self, connection_id: u64) {
        for pane in self.panes().0.values_mut() {
            pane.subscribers.remove(&connection_id);
        }
    }

    /// Checks one command against the pane's state and hands it to the
    /// provider.
    pub(crate) fn command(
        &self,
        key: &PaneKey,
        adapter_id: &str,
        mut command: SessionCommand,
    ) -> Result<(), ChatError> {
        loop {
            let session = self
                .with_pane(key, adapter_id, |pane| Ok(pane.session.clone()))?
                .ok_or_else(|| {
                    ChatError::new("chat_not_ready", "The chat is still connecting. Try again.")
                })?;
            // One command at a time per pane, from its check to its last
            // effect: two connections may have the pane open. The session's
            // lock is taken before the runtime's, the order a session
            // publishes in.
            let mut running = session.lock().unwrap_or_else(PoisonError::into_inner);
            match self.panes().0.get_mut(key) {
                Some(pane)
                    if pane
                        .session
                        .as_ref()
                        .is_some_and(|its| Arc::ptr_eq(its, &session)) =>
                {
                    pane.admit(&mut command)?;
                }
                // Forgotten while the lock was waited for, and perhaps
                // started again: the command is for the chat as it is now.
                _ => continue,
            }
            // Called without the runtime's lock: the session publishes from
            // here.
            return running.command(command);
        }
    }

    /// The page of items before `cursor`, oldest first, with the cursor for
    /// the page before that and the revision the page is current to.
    pub(crate) fn load_older(
        &self,
        key: &PaneKey,
        adapter_id: &str,
        cursor: &str,
    ) -> Result<v1::ChatResponse, ChatError> {
        self.with_pane(key, adapter_id, |pane| {
            let end = pane
                .items
                .iter()
                .position(|item| !cursor.is_empty() && item.id == cursor)
                .ok_or_else(|| {
                    ChatError::new(
                        "chat_cursor_invalid",
                        "Those messages are no longer available. Reopen the chat.",
                    )
                })?;
            let (older, older_cursor) = pane.page(end);
            Ok(v1::ChatResponse {
                older,
                older_cursor,
                revision: pane.revision,
                ..Default::default()
            })
        })
    }

    pub(crate) fn agent_mode(&self) -> v1::ChatAgentMode {
        *self
            .mode
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(|| {
                // A file that cannot be read is the default, Terminal.
                let settings: Settings =
                    load_sidecar(&self.settings_path, SETTINGS_LABEL).unwrap_or_default();
                if settings.agent_mode == "chat" {
                    v1::ChatAgentMode::Chat
                } else {
                    v1::ChatAgentMode::Terminal
                }
            })
    }

    /// Stores the host's agent mode. The next agent snapshot carries it.
    pub(crate) fn set_agent_mode(&self, mode: v1::ChatAgentMode) -> Result<(), ChatError> {
        let agent_mode = match mode {
            v1::ChatAgentMode::Terminal => "terminal",
            v1::ChatAgentMode::Chat => "chat",
            v1::ChatAgentMode::Unspecified => {
                return Err(ChatError::new(
                    "chat_invalid_request",
                    "The agent mode must be Terminal or Chat.",
                ));
            }
        };
        let mut stored = self.mode.lock().unwrap_or_else(PoisonError::into_inner);
        save_sidecar(
            &self.settings_path,
            SETTINGS_LABEL,
            &Settings {
                agent_mode: agent_mode.to_owned(),
            },
        )
        .map_err(|error| {
            ChatError::new(
                "chat_mode_not_saved",
                format!("The agent mode could not be saved: {error}"),
            )
        })?;
        *stored = Some(mode);
        Ok(())
    }
}

impl Panes {
    /// Applies one change and sends it to the pane's subscribers.
    pub(super) fn publish(&mut self, key: &PaneKey, change: Change) {
        let Some(pane) = self.0.get_mut(key) else {
            return;
        };
        let Some(change) = pane.apply(change) else {
            return;
        };
        pane.emit(change);
        if pane.trim() {
            pane.state.history_complete = false;
            pane.emit(Change::State(pane.state.clone()));
        }
    }
}

impl Pane {
    /// Numbers a change that has been applied and sends it to every
    /// subscriber. A connection that has gone is forgotten; one whose queue is
    /// full is told to resync, and opens the chat again when it has.
    fn emit(&mut self, change: Change) {
        self.revision += 1;
        let event = self.event(change);
        self.subscribers.retain(|connection_id| {
            send_required_control_event_to(*connection_id, event.clone()) != Delivery::Gone
        });
    }

    /// A chat event for this pane at its current revision.
    fn event(&self, change: Change) -> v1::HostEvent {
        v1::HostEvent {
            kind: v1::EventKind::Chat.into(),
            scope: self.state.pane_id.clone(),
            chat: Some(v1::ChatEvent {
                pane_id: self.state.pane_id.clone(),
                revision: self.revision,
                change: Some(change),
            }),
            ..Default::default()
        }
    }

    /// Applies a provider's change to the pane, bounded. Returns what
    /// subscribers are told, or nothing when the change changes nothing.
    fn apply(&mut self, change: Change) -> Option<Change> {
        match change {
            Change::Item(mut item) => {
                if !bound_item(&mut item) {
                    return None;
                }
                if self.items.iter().any(|held| held.id == item.id) {
                    // Published as new under an id the timeline holds: it is
                    // a change to that item after all.
                    return self.apply(Change::Updated(item));
                }
                self.items.push(item.clone());
                Some(Change::Item(item))
            }
            Change::Updated(mut item) => {
                if !bound_item(&mut item) {
                    return None;
                }
                // An item the timeline no longer holds was rewound or trimmed
                // away, and does not come back as a new one.
                let held = self
                    .items
                    .iter_mut()
                    .rev()
                    .find(|held| held.id == item.id)?;
                // An item never changes kind, so another kind is a second
                // item under the first one's id. The first stays: it may be
                // a message whose id a client chose.
                if held.kind != item.kind || *held == item {
                    return None;
                }
                *held = item.clone();
                Some(Change::Updated(item))
            }
            Change::Delta(mut delta) => {
                let item = self
                    .items
                    .iter_mut()
                    .rev()
                    .find(|held| held.id == delta.item_id && held.streaming)?;
                cut(
                    &mut delta.text_append,
                    MAX_ITEM_TEXT_BYTES.saturating_sub(item.text.len()),
                );
                if delta.text_append.is_empty() {
                    return None;
                }
                item.text.push_str(&delta.text_append);
                Some(Change::Delta(delta))
            }
            Change::State(mut state) => {
                // The pane's identity is the runtime's, not the provider's.
                state.pane_id.clone_from(&self.state.pane_id);
                state.adapter_id.clone_from(&self.state.adapter_id);
                state.history_complete &= !self.trimmed;
                // A request is pending exactly while the turn waits on it.
                if state.turn != i32::from(v1::ChatTurnState::Waiting) {
                    state.pending = None;
                }
                bound_state(&mut state);
                if state == self.state {
                    return None;
                }
                self.state = state.clone();
                Some(Change::State(state))
            }
            Change::Catalog(mut catalog) => {
                // A catalog that leaves the capabilities out keeps them.
                if catalog.capabilities.is_none() {
                    catalog.capabilities = self.catalog.capabilities;
                }
                bound_catalog(&mut catalog);
                if catalog == self.catalog {
                    return None;
                }
                self.catalog = catalog.clone();
                Some(Change::Catalog(catalog))
            }
            Change::Truncate(truncate) => {
                let keep = if truncate.after_item_id.is_empty() {
                    0
                } else {
                    let after = |item: &v1::ChatItem| item.id == truncate.after_item_id;
                    self.items.iter().position(after)? + 1
                };
                if keep == self.items.len() {
                    return None;
                }
                self.items.truncate(keep);
                Some(Change::Truncate(truncate))
            }
            // Only the runtime makes one, for OPEN.
            Change::Snapshot(_) => None,
        }
    }

    /// Drops the oldest items past the timeline's bounds. True the first time
    /// it drops any.
    fn trim(&mut self) -> bool {
        let weight = |item: &v1::ChatItem| {
            let tool = item.tool.as_ref();
            item.text.len() + tool.map_or(0, |tool| tool.detail.len() + tool.output_preview.len())
        };
        let mut bytes: usize = self.items.iter().map(weight).sum();
        let mut excess = 0;
        while excess < self.items.len()
            && (self.items.len() - excess > MAX_TIMELINE_ITEMS || bytes > MAX_TIMELINE_BYTES)
        {
            bytes -= weight(&self.items[excess]);
            excess += 1;
        }
        if excess == 0 {
            return false;
        }
        self.items.drain(..excess);
        !std::mem::replace(&mut self.trimmed, true)
    }

    /// The newest page ending before `items[end]`, and the cursor for the page
    /// before it: the id of the page's first item, or empty at the start.
    fn page(&self, end: usize) -> (Vec<v1::ChatItem>, String) {
        let mut start = end;
        let mut bytes = 0;
        while start > 0 && end - start < MAX_PAGE_ITEMS {
            bytes += self.items[start - 1].encoded_len();
            if bytes > MAX_PAGE_BYTES && start < end {
                break;
            }
            start -= 1;
        }
        let cursor = if start > 0 && start < end {
            self.items[start].id.clone()
        } else {
            String::new()
        };
        (self.items[start..end].to_vec(), cursor)
    }

    fn snapshot(&self) -> v1::ChatSnapshot {
        let (items, older_cursor) = self.page(self.items.len());
        v1::ChatSnapshot {
            revision: self.revision,
            state: Some(self.state.clone()),
            catalog: Some(self.catalog.clone()),
            items,
            older_cursor,
        }
    }

    /// Checks what does not depend on the provider, and completes the command.
    fn admit(&self, command: &mut SessionCommand) -> Result<(), ChatError> {
        let capabilities = self.catalog.capabilities.unwrap_or_default();
        match command {
            SessionCommand::Send { text, item_id } => {
                if text.trim().is_empty() {
                    return Err(invalid("A message needs some text."));
                }
                if text.len() > MAX_ITEM_TEXT_BYTES {
                    return Err(ChatError::new(
                        "chat_message_too_large",
                        "That message is too long to send.",
                    ));
                }
                if self.state.turn != i32::from(v1::ChatTurnState::Idle)
                    && !capabilities.send_mid_turn
                {
                    return Err(ChatError::new(
                        "chat_turn_busy",
                        "The agent is still working. Stop it, or wait for it to finish.",
                    ));
                }
                if item_id.is_empty() {
                    *item_id = format!("user-{}", uuid::Uuid::new_v4());
                } else if item_id.len() > MAX_ID_BYTES {
                    return Err(invalid("That message id is too long."));
                } else if self.items.iter().any(|item| item.id == *item_id) {
                    // Its own code: a client that lost the answer to a SEND
                    // and sends it again learns the first one arrived.
                    return Err(ChatError::new(
                        "chat_message_duplicate",
                        "That message was already sent.",
                    ));
                }
            }
            SessionCommand::Answer { request_id, answer } => {
                let pending = self.state.pending.as_ref();
                if !pending.is_some_and(|pending| pending.request_id == *request_id) {
                    return Err(ChatError::new(
                        "chat_request_not_pending",
                        "The agent is no longer waiting for that answer.",
                    ));
                }
                if answer.encoded_len() > MAX_ANSWER_BYTES {
                    return Err(ChatError::new(
                        "chat_message_too_large",
                        "That answer is too long to send.",
                    ));
                }
            }
            SessionCommand::Rewind {
                item_id,
                after_item_id,
            } => {
                let rewound = self.items.iter().position(|item| {
                    item.id == *item_id
                        && item.kind == i32::from(v1::ChatItemKind::User)
                        && item.rewindable
                });
                let Some(index) = rewound.filter(|_| capabilities.rewind) else {
                    return Err(ChatError::new(
                        "chat_item_not_rewindable",
                        "That message cannot be edited.",
                    ));
                };
                if let Some(previous) = index.checked_sub(1) {
                    after_item_id.clone_from(&self.items[previous].id);
                }
            }
            SessionCommand::SetOptions(options) => *options = self.merged_options(options)?,
            SessionCommand::Interrupt => {}
        }
        Ok(())
    }

    /// The options a SET_OPTIONS asks the provider for. An empty string or an
    /// unspecified enum keeps the current value; what is asked for must be
    /// something the provider can change and the catalog's model offers.
    /// What becomes of a kept effort or speed that a new model does not
    /// offer is the provider's to say.
    fn merged_options(&self, requested: &v1::ChatOptions) -> Result<v1::ChatOptions, ChatError> {
        let permission = v1::ChatPermissionMode::try_from(requested.permission)
            .map_err(|_| invalid("That permission mode is not available."))?;
        let speed = v1::ChatSpeed::try_from(requested.speed)
            .map_err(|_| invalid("That speed is not available."))?;
        let capabilities = self.catalog.capabilities.unwrap_or_default();
        let asked = [
            (!requested.model.is_empty(), capabilities.model),
            (!requested.effort.is_empty(), capabilities.effort),
            (
                permission != v1::ChatPermissionMode::Unspecified,
                capabilities.permission,
            ),
            (speed != v1::ChatSpeed::Unspecified, capabilities.speed),
        ];
        if asked.iter().any(|(asked, able)| *asked && !able) {
            return Err(invalid("This chat cannot change that."));
        }

        let mut options = self.state.options.clone().unwrap_or_default();
        if permission != v1::ChatPermissionMode::Unspecified {
            options.permission = permission.into();
        }
        if !requested.model.is_empty() {
            options.model.clone_from(&requested.model);
        }
        let listed = |model: &&v1::ChatModel| model.id == options.model;
        let Some(model) = self.catalog.models.iter().find(listed) else {
            // The catalog does not list the model: asked for, that is a
            // refusal. Kept, there is nothing to check an effort or a speed
            // against, so those stay as they are.
            let unchecked = !requested.model.is_empty()
                || !requested.effort.is_empty()
                || speed != v1::ChatSpeed::Unspecified;
            return if unchecked {
                Err(invalid("That model is not available."))
            } else {
                Ok(options)
            };
        };
        if !requested.effort.is_empty() {
            if !model.efforts.contains(&requested.effort) {
                return Err(invalid("That effort is not available for this model."));
            }
            options.effort.clone_from(&requested.effort);
        }
        if speed == v1::ChatSpeed::Fast && !model.supports_fast {
            return Err(invalid("This model has no fast mode."));
        }
        if speed != v1::ChatSpeed::Unspecified {
            options.speed = speed.into();
        }
        Ok(options)
    }
}

/// The providers a build registers. The scripted one is for this crate's
/// tests and for whoever sets its switch; nothing else registers it.
fn registered_providers(
    test_build: bool,
    scripted_switch: Option<&std::ffi::OsStr>,
    step: Duration,
) -> Vec<Arc<dyn ChatProvider>> {
    if test_build || scripted::enabled(scripted_switch) {
        vec![Arc::new(scripted::ScriptedProvider::new(step))]
    } else {
        Vec::new()
    }
}

fn invalid(message: &str) -> ChatError {
    ChatError::new("chat_invalid_request", message)
}

fn unavailable() -> ChatError {
    ChatError::new("chat_unavailable", "This pane is not running a chat.")
}

/// Cuts `text` to at most `limit` bytes, on a character boundary. True when it
/// cut anything.
fn cut(text: &mut String, limit: usize) -> bool {
    if text.len() <= limit {
        return false;
    }
    text.truncate(text.floor_char_boundary(limit));
    true
}

fn cut_labels<const N: usize>(labels: [&mut String; N]) {
    for label in labels {
        cut(label, MAX_LABEL_BYTES);
    }
}

/// Cuts an item to its bounds. False for an item with no usable id.
fn bound_item(item: &mut v1::ChatItem) -> bool {
    if item.id.is_empty() || item.id.len() > MAX_ID_BYTES {
        return false;
    }
    cut(&mut item.text, MAX_ITEM_TEXT_BYTES);
    if let Some(tool) = &mut item.tool {
        cut_labels([&mut tool.title, &mut tool.result_summary]);
        cut(&mut tool.detail, MAX_DETAIL_BYTES);
        cut(&mut tool.path, MAX_PATH_BYTES);
        tool.output_truncated |= cut(&mut tool.output_preview, MAX_OUTPUT_PREVIEW_BYTES);
    }
    true
}

fn bound_state(state: &mut v1::ChatState) {
    cut_labels([&mut state.native_session_id, &mut state.failure]);
    if let Some(options) = &mut state.options {
        cut_labels([&mut options.model, &mut options.effort]);
    }
    let Some(pending) = &mut state.pending else {
        return;
    };
    cut_labels([&mut pending.request_id, &mut pending.title]);
    cut(&mut pending.detail, MAX_DETAIL_BYTES);
    pending.questions.truncate(MAX_QUESTIONS);
    for question in &mut pending.questions {
        cut_labels([&mut question.id, &mut question.header, &mut question.prompt]);
        question.options.truncate(MAX_QUESTION_OPTIONS);
        for option in &mut question.options {
            cut_labels([&mut option.id, &mut option.label, &mut option.description]);
        }
    }
}

fn bound_catalog(catalog: &mut v1::ChatCatalog) {
    catalog.models.truncate(MAX_MODELS);
    for model in &mut catalog.models {
        cut_labels([&mut model.id, &mut model.label]);
        model.efforts.truncate(MAX_MODEL_EFFORTS);
        for effort in &mut model.efforts {
            cut(effort, MAX_LABEL_BYTES);
        }
    }
    catalog.commands.truncate(MAX_COMMANDS);
    for command in &mut catalog.commands {
        cut_labels([
            &mut command.name,
            &mut command.description,
            &mut command.argument_hint,
        ]);
    }
}
