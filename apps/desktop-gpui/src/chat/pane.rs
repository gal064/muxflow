//! One chat pane: what it holds, and everything it says to the host. The
//! drawing is in [`super::view`] and [`super::composer`].
//!
//! The pane follows its host's link. It opens the chat when the link is live,
//! and again after a resync, a reconnect, a lost change, or a refusal that
//! says to; `chat.proto` has the rules. Whatever the host has not confirmed
//! yet (a message being sent, a page being loaded) is kept beside the model,
//! never in it.

use std::{
    cell::Cell,
    collections::{HashMap, HashSet, VecDeque},
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use desktop_core::connection::chat::{
    ChatAction, ChatAnswer, ChatCapabilities, ChatCommand, ChatConnection, ChatEvent, ChatItem,
    ChatItemKind, ChatModel, ChatOptions, ChatPendingRequest, ChatRequest, ChatRequestKind,
    ChatResponse, ChatTurnState,
};
use futures::future::BoxFuture;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, Global, Pixels, Point,
    ScrollHandle, SharedString, Subscription, Task, Window,
    component::{
        input::{InputEvent, InputState, TextareaState},
        message_scroller::MessageScrollerState,
    },
};
use serde_json::json;
use tmux_control::Pane;

use crate::{
    agents::{Agents, BRANCH_PANE_WAIT_MS, PendingOpen, open_when_present},
    connection_ui::{
        connection_reducer::ConnectionPhase,
        controller::{ConnectionController, ConnectionEvent},
    },
    terminal::integration::agents::note_activity,
};

use super::{
    links::{self, Probe},
    model::{Chat, Outcome, PageOutcome, PageRequest},
    slash,
};

/// How long a refused `OPEN` waits before it is sent again, and how many
/// times in a row before the refusal is shown instead.
const OPEN_RETRY: Duration = Duration::from_millis(250);
const OPEN_RETRIES: u32 = 20;
/// How long an answered `OPEN` is given to bring its snapshot. Until then a
/// gap in the revisions is what the snapshot is about to mend.
const SNAPSHOT_WAIT: Duration = Duration::from_secs(5);
/// Inline code paths asked about for one chat before the answers are dropped.
const MAX_PROBES: usize = 512;

/// The host's refusal as `"<error_code>: <display message>"`: its code.
pub(super) fn error_code(error: &str) -> &str {
    match error.split_once(": ") {
        Some((code, _)) if is_code(code) => code,
        _ => "",
    }
}

/// The part of a refusal a person reads.
pub(super) fn error_message(error: &str) -> &str {
    match error.split_once(": ") {
        Some((code, message)) if is_code(code) => message,
        _ => error,
    }
}

fn is_code(code: &str) -> bool {
    !code.is_empty()
        && code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
}

/// The connection a chat was opened on.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Live {
    client_id: String,
    terminal_epoch: u64,
}

/// A message on its way to the host, shown until its item arrives.
pub(super) struct Sending {
    pub id: String,
    pub text: SharedString,
}

/// What has been picked for one question of a pending request.
#[derive(Clone, Default)]
pub(super) struct Pick {
    pub options: Vec<String>,
    pub other: bool,
    pub other_text: String,
}

impl Pick {
    pub fn is_answer(&self) -> bool {
        !self.options.is_empty() || (self.other && !self.other_text.trim().is_empty())
    }
}

/// The answer being put together for the pending question request.
pub(super) struct AnswerDraft {
    pub request_id: String,
    /// The question on show.
    pub index: usize,
    pub picks: Vec<Pick>,
    /// The field of the shown question's "Other" row.
    pub other: Entity<InputState>,
    _changes: Subscription,
}

/// What the timeline's rows are, in order: the header, the items, the
/// messages being sent, and the pending request's card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Row {
    Header,
    Item(usize),
    Sending(usize),
    Request,
}

/// How many panes hold each chat open, so a pane that is replaced in place
/// does not close the chat its replacement has just opened.
#[derive(Default)]
struct OpenChats(HashMap<(String, String, String), usize>);

impl Global for OpenChats {}

/// A composer's unsent text, kept while its pane is off screen.
#[derive(Default)]
struct Drafts(HashMap<(String, String, String), String>);

impl Global for Drafts {}

pub struct ChatPane {
    pub(super) profile_id: String,
    pub(super) server_identity: String,
    pub(super) pane_id: SharedString,
    controller: Entity<ConnectionController>,
    /// The tmux pane, for its directory and its route.
    tmux: Pane,
    pub(super) chat: Chat,
    live: Option<Live>,
    /// Counts `OPEN`s, so only the newest one's answer is read.
    open_attempt: u64,
    /// The link's restore count the last `OPEN` was sent at.
    opened_at_restore: u64,
    opening: bool,
    /// Refused `OPEN`s sent again since the last one that was answered.
    open_retries: u32,
    /// When the snapshot of the last answered `OPEN` is given up on.
    snapshot_by: Option<Instant>,
    pub(super) open_error: Option<SharedString>,
    retry: Option<Task<()>>,
    pub(super) list: Entity<MessageScrollerState>,
    /// The item and trailing rows the list was last told of.
    rows: (usize, usize),
    pub(super) composer: Entity<TextareaState>,
    /// The message being edited: sending rewinds to it first.
    pub(super) editing: Option<String>,
    /// The slash list's selected row, and the text it was dismissed for.
    pub(super) slash_active: usize,
    pub(super) slash_dismissed: Option<String>,
    pub(super) slash_scroll: ScrollHandle,
    pub(super) sending: Vec<Sending>,
    pub(super) answer: Option<AnswerDraft>,
    /// The request an answer is on its way for.
    pub(super) answering: Option<String>,
    /// Tool rows showing their output.
    pub(super) expanded: HashSet<String>,
    probes: HashMap<String, Probe>,
    probe_queue: VecDeque<String>,
    probing: bool,
    /// Each agent message as shown, by item id, with the length it was made from.
    shown: HashMap<String, (usize, SharedString)>,
    pub(super) loading_older: bool,
    /// Counts `LOAD_OLDER`s, so only the newest one's answer is read.
    older_attempt: u64,
    /// The last page failed: no more are asked for until the user asks.
    pub(super) older_failed: bool,
    /// The last refusal, shown over the composer until the next action.
    pub(super) notice: Option<SharedString>,
    pub(super) focus: FocusHandle,
    /// The pane's width at the last layout.
    pub(super) width: Rc<Cell<Option<Pixels>>>,
    /// Where the last left press landed, to tell a click on a link from a
    /// drag that ends on one.
    pub(super) pressed: Arc<Mutex<Option<Point<Pixels>>>>,
    /// Text a refused send gives back to the composer at the next draw.
    pub(super) restore: Option<String>,
    /// The text of the message that last failed to send, and its id.
    resend: Option<(String, String)>,
    /// Text put back in the composer whose message turned out to be sent.
    pub(super) withdraw: Option<String>,
    /// What the empty composer says, as last told to it.
    pub(super) placeholder: &'static str,
    _subscriptions: Vec<Subscription>,
}

impl ChatPane {
    pub fn new(
        profile_id: String,
        server_identity: String,
        tmux: Pane,
        controller: Entity<ConnectionController>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let key = (profile_id.clone(), server_identity.clone(), tmux.id.clone());
        let draft = cx.default_global::<Drafts>().0.remove(&key);
        *cx.default_global::<OpenChats>()
            .0
            .entry(key.clone())
            .or_default() += 1;
        let composer = cx.new(|cx| {
            let state = TextareaState::new(window, cx)
                .auto_grow(1, 10)
                .submit_on_enter(true);
            match draft {
                Some(text) => state.default_value(text),
                None => state,
            }
        });
        let list = cx.new(|cx| MessageScrollerState::new(1, cx));
        let focus = cx.focus_handle();
        let subscriptions = vec![
            cx.observe(&controller, |this, _, cx| this.follow_link(cx)),
            cx.subscribe(&controller, |this, _, event, cx| match event {
                ConnectionEvent::Chat { profile_id, event }
                    if *profile_id == this.profile_id && event.pane_id == *this.pane_id =>
                {
                    this.on_event(ChatEvent::clone(event), cx);
                }
                ConnectionEvent::LinkRestored { profile_id } if *profile_id == this.profile_id => {
                    this.follow_link(cx);
                    // Unless the chat was already opened over the link as
                    // it is now: a new connection opens it by itself.
                    let restores = this.controller.read(cx).link_restores(&this.profile_id);
                    if this.opened_at_restore != restores {
                        this.open(cx);
                    }
                }
                _ => {}
            }),
            cx.subscribe_in(&composer, window, Self::on_composer),
            // The list scrolled: the jump button and the header follow it.
            cx.observe(&list, |_, _, cx| cx.notify()),
            // A press on the conversation lands on the pane itself, which
            // takes no text: what is typed next is for the composer.
            cx.on_focus(&focus, window, |this, window, cx| {
                if this.focus.is_focused(window) {
                    window.focus(&this.input_focus(cx), cx);
                    // GPUI draws nothing after a focus listener moves focus,
                    // and the composer takes text only once it is drawn so.
                    window.defer(cx, |window, _| window.refresh());
                }
            }),
            cx.on_release(move |this, cx| this.released(key.clone(), cx)),
        ];
        let mut this = Self {
            profile_id,
            server_identity,
            pane_id: tmux.id.clone().into(),
            controller,
            tmux,
            chat: Chat::default(),
            live: None,
            open_attempt: 0,
            opened_at_restore: 0,
            opening: false,
            open_retries: 0,
            snapshot_by: None,
            open_error: None,
            retry: None,
            list,
            rows: (0, 0),
            composer,
            editing: None,
            slash_active: 0,
            slash_dismissed: None,
            slash_scroll: ScrollHandle::new(),
            sending: Vec::new(),
            answer: None,
            answering: None,
            expanded: HashSet::new(),
            probes: HashMap::new(),
            probe_queue: VecDeque::new(),
            probing: false,
            shown: HashMap::new(),
            loading_older: false,
            older_attempt: 0,
            older_failed: false,
            notice: None,
            focus,
            width: Rc::default(),
            pressed: Arc::default(),
            restore: None,
            resend: None,
            withdraw: None,
            placeholder: "",
            _subscriptions: subscriptions,
        };
        this.follow_link(cx);
        this
    }

    pub fn server_identity(&self) -> &str {
        &self.server_identity
    }

    /// What typing goes to: the composer.
    pub fn input_focus(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).focus_handle(cx)
    }

    /// The pane's own handle, which holds whatever inside it has the keyboard.
    pub fn root_focus(&self) -> &FocusHandle {
        &self.focus
    }

    pub(super) fn tmux_pane(&self) -> &Pane {
        &self.tmux
    }

    /// The tmux pane as the last topology described it.
    pub fn set_tmux_pane(&mut self, pane: &Pane, cx: &mut Context<Self>) {
        if self.tmux.current_path != pane.current_path {
            // A relative path names another file from another directory.
            self.forget_probes(cx);
        }
        if self.tmux != *pane {
            self.tmux = pane.clone();
        }
    }

    // The link.

    /// The last pane holding this chat closes it; the chat itself runs on.
    fn released(&mut self, key: (String, String, String), cx: &mut App) {
        let text = self.composer.read(cx).value().to_string();
        if !text.trim().is_empty() && self.editing.is_none() {
            cx.default_global::<Drafts>().0.insert(key.clone(), text);
        }
        let open = cx.default_global::<OpenChats>();
        let held_elsewhere = match open.0.get_mut(&key) {
            Some(count) if *count > 1 => {
                *count -= 1;
                true
            }
            _ => {
                open.0.remove(&key);
                false
            }
        };
        if held_elsewhere {
            return;
        }
        let close = ChatRequest {
            action: ChatAction::Close.into(),
            ..Default::default()
        };
        if let Some(answer) = self.request(close, cx) {
            // Nobody waits: a chat the host has already let go is the outcome.
            cx.background_spawn(async move {
                let _ = answer.await;
            })
            .detach();
        }
    }

    fn follow_link(&mut self, cx: &mut Context<Self>) {
        let live = self
            .controller
            .read(cx)
            .link(&self.profile_id)
            .and_then(|link| {
                let state = &link.host_state;
                let here = state.phase == ConnectionPhase::Connected
                    && link.terminal_epoch != 0
                    && state.server_identity.as_deref() == Some(self.server_identity.as_str());
                let client_id = link.client_id.clone().filter(|_| here)?;
                Some(Live {
                    client_id,
                    terminal_epoch: link.terminal_epoch,
                })
            });
        if live == self.live {
            return;
        }
        self.live = live;
        if self.live.is_some() {
            // Whatever was asked over the old connection has no answer coming.
            self.forget_probes(cx);
            self.open(cx);
        }
        cx.notify();
    }

    pub(super) fn is_live(&self) -> bool {
        self.live.is_some()
    }

    fn request(
        &self,
        mut request: ChatRequest,
        cx: &App,
    ) -> Option<BoxFuture<'static, Result<ChatResponse, String>>> {
        let live = self.live.as_ref()?;
        request.pane_id = self.pane_id.to_string();
        request.expected_server_identity = self.server_identity.clone();
        Some(
            self.controller
                .read(cx)
                .backend()
                .chat_request(live.client_id.clone(), request),
        )
    }

    /// Sends one action and hands its answer to `then`. Without a live link
    /// `then` gets that as a refusal.
    fn send(
        &mut self,
        request: ChatRequest,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, Result<ChatResponse, String>, &mut Context<Self>) + 'static,
    ) {
        let answer = self.request(request, cx);
        cx.spawn(async move |this, cx| {
            let result = match answer {
                Some(answer) => answer.await,
                None => Err("The host is not connected.".to_owned()),
            };
            this.update(cx, |this, cx| then(this, result, cx)).ok();
        })
        .detach();
    }

    /// Asks for the chat's snapshot. Also the answer to every lost change.
    pub(super) fn open(&mut self, cx: &mut Context<Self>) {
        if self.live.is_none() {
            return;
        }
        self.open_attempt += 1;
        self.opened_at_restore = self.controller.read(cx).link_restores(&self.profile_id);
        let attempt = self.open_attempt;
        self.opening = true;
        self.retry = None;
        // From here, not from the answer: the snapshot is an event, and may
        // be here before the answer is.
        self.snapshot_by = Some(Instant::now() + SNAPSHOT_WAIT);
        let open = ChatRequest {
            action: ChatAction::Open.into(),
            ..Default::default()
        };
        self.send(open, cx, move |this, result, cx| {
            if attempt != this.open_attempt {
                return;
            }
            this.opening = false;
            match result {
                Ok(_) => {
                    this.open_error = None;
                    this.open_retries = 0;
                }
                // The snapshot could not be queued, or the topology this
                // connection sent with has not caught up with the pane yet.
                Err(error)
                    if this.open_retries < OPEN_RETRIES
                        && matches!(
                            error_code(&error),
                            "chat_queue_full" | "chat_server_changed"
                        ) =>
                {
                    this.open_retries += 1;
                    let timer = cx.background_executor().timer(OPEN_RETRY);
                    this.retry = Some(cx.spawn(async move |this, cx| {
                        timer.await;
                        this.update(cx, |this, cx| this.open(cx)).ok();
                    }));
                }
                Err(error) => {
                    this.open_retries = 0;
                    this.snapshot_by = None;
                    this.open_error = Some(error_message(&error).to_owned().into());
                }
            }
            cx.notify();
        });
    }

    /// A change was lost. One `OPEN` mends it; while that one is on its way,
    /// or its snapshot is, every change still arriving looks lost too.
    fn reopen(&mut self, cx: &mut Context<Self>) {
        let mending = self.opening
            || self.retry.is_some()
            || self.snapshot_by.is_some_and(|by| Instant::now() < by);
        if !mending {
            self.open(cx);
        }
    }

    fn on_event(&mut self, event: ChatEvent, cx: &mut Context<Self>) {
        match self.chat.apply(event) {
            Outcome::Ignored => {}
            Outcome::Reopen => self.reopen(cx),
            outcome => self.changed(outcome, cx),
        }
        if let Some(page) = self.chat.page_due() {
            self.loading_older = false;
            self.paged(page, cx);
        }
    }

    // The rows.

    fn trail(&self) -> usize {
        self.sending.len() + usize::from(self.pending().is_some())
    }

    pub(super) fn row(&self, index: usize) -> Option<Row> {
        let items = self.chat.items.len();
        let sending = self.sending.len();
        match index {
            0 => Some(Row::Header),
            index if index <= items => Some(Row::Item(index - 1)),
            index if index <= items + sending => Some(Row::Sending(index - 1 - items)),
            index if index == items + sending + 1 && self.pending().is_some() => Some(Row::Request),
            _ => None,
        }
    }

    /// The request the agent waits on, while the host says it waits.
    pub(super) fn pending(&self) -> Option<&ChatPendingRequest> {
        self.chat
            .state
            .pending
            .as_ref()
            .filter(|_| self.chat.state.turn == i32::from(ChatTurnState::Waiting))
    }

    /// Tells the list what an event did to the items, and what that did to
    /// the rows after them.
    fn changed(&mut self, outcome: Outcome, cx: &mut Context<Self>) {
        let (old_items, old_trail) = self.rows;
        let chat = &self.chat;
        self.sending
            .retain(|sending| chat.item(&sending.id).is_none());
        if let Some((text, _)) = self.resend.take_if(|(_, id)| chat.item(id).is_some()) {
            // The host had taken it after all: it is in the conversation,
            // and its text is not left in the composer to be sent twice.
            self.withdraw = Some(text);
        }
        if self
            .editing
            .as_ref()
            .is_some_and(|id| self.chat.item(id).is_none())
        {
            self.editing = None;
        }
        let pending = self.pending().map(|request| request.request_id.as_str());
        let drafted = self.answer.as_ref().map(|draft| draft.request_id.as_str()) == pending;
        let answered = self.answering.as_deref() == pending;
        if !drafted {
            self.answer = None;
        }
        if !answered {
            self.answering = None;
        }
        let (items, trail) = (self.chat.items.len(), self.trail());
        let old_end = 1 + old_items + old_trail;
        let mut changed_item = None;
        self.list.update(cx, |list, cx| match outcome {
            Outcome::Reset => list.reset(1 + items + trail, cx),
            Outcome::Appended(count) => {
                list.splice(1 + old_items..old_end, count + trail, cx);
            }
            Outcome::Truncated(at) => {
                list.splice(1 + at.min(old_items)..old_end, trail, cx);
            }
            Outcome::Updated(index) => {
                list.remeasure_items(1 + index..2 + index, cx);
                changed_item = Some(index);
            }
            Outcome::State => {
                list.remeasure_items(0..1, cx);
            }
            Outcome::Ignored | Outcome::Reopen => {}
        });
        match outcome {
            Outcome::Reset => {
                // The `OPEN` has done its work, though its answer may still
                // be on its way: a change lost from here on is asked about.
                self.opening = false;
                self.snapshot_by = None;
                self.open_error = None;
                self.shown.clear();
                self.loading_older = false;
                self.older_failed = false;
            }
            Outcome::Truncated(_) => {
                let chat = &self.chat;
                self.shown.retain(|id, _| chat.item(id).is_some());
            }
            _ => {}
        }
        if let Some(item) = changed_item.and_then(|index| self.chat.items.get(index)) {
            self.shown.remove(&item.id);
        }
        self.rows = match outcome {
            Outcome::Updated(_) | Outcome::State => (old_items, old_trail),
            _ => (items, trail),
        };
        self.sync_trail(cx);
        cx.notify();
    }

    /// Tells the list about the rows after the items, which follow state
    /// that is not an item: a message being sent, the pending request.
    pub(super) fn sync_trail(&mut self, cx: &mut Context<Self>) {
        let (items, old_trail) = self.rows;
        let trail = self.trail();
        let start = 1 + items;
        self.list.update(cx, |list, cx| {
            if trail != old_trail {
                list.splice(start..start + old_trail, trail, cx);
            } else if trail > 0 {
                list.remeasure_items(start..start + trail, cx);
            }
        });
        self.rows = (items, trail);
    }

    /// A row's height changed without its item changing.
    pub(super) fn remeasure_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.list.update(cx, |list, cx| {
            list.remeasure_items(row..row + 1, cx);
        });
        cx.notify();
    }

    // Older pages.

    /// Asks for the page before the oldest item held.
    pub(super) fn load_older(&mut self, cx: &mut Context<Self>) {
        if self.loading_older {
            return;
        }
        let Some(page) = self.chat.page_request() else {
            return;
        };
        self.loading_older = true;
        self.older_failed = false;
        self.older_attempt += 1;
        let attempt = self.older_attempt;
        let request = ChatRequest {
            action: ChatAction::LoadOlder.into(),
            older_cursor: page.cursor.clone(),
            ..Default::default()
        };
        self.send(request, cx, move |this, result, cx| {
            // Asked before a snapshot replaced everything: a later request
            // is the one being waited on now.
            if attempt == this.older_attempt {
                this.older_loaded(&page, result, cx);
            }
        });
        self.remeasure_row(0, cx);
    }

    fn older_loaded(
        &mut self,
        request: &PageRequest,
        result: Result<ChatResponse, String>,
        cx: &mut Context<Self>,
    ) {
        self.loading_older = false;
        match result {
            Ok(response) => {
                let outcome = self.chat.page_answer(request, response);
                self.paged(outcome, cx);
            }
            Err(_) => self.older_failed = true,
        }
        self.remeasure_row(0, cx);
    }

    fn paged(&mut self, outcome: PageOutcome, cx: &mut Context<Self>) {
        match outcome {
            PageOutcome::Dropped => {}
            // Still loading, as far as anyone watching can tell.
            PageOutcome::Held => self.loading_older = true,
            PageOutcome::AskAgain => self.load_older(cx),
            PageOutcome::Merged(count) => {
                self.list.update(cx, |list, cx| {
                    // The list keeps its place when rows go in above it, but
                    // the header is above them: from there, stay on the item
                    // that was first.
                    let at_header =
                        list.logical_scroll_top().item_ix == 0 && !list.is_following_tail();
                    list.splice(1..1, count, cx);
                    if at_header && count > 0 {
                        list.scroll_to_item(count + 1, cx);
                    }
                });
                self.rows.0 += count;
            }
        }
        cx.notify();
    }

    // What the composer may do.

    pub(super) fn capabilities(&self) -> ChatCapabilities {
        self.chat.catalog.capabilities.unwrap_or_default()
    }

    pub(super) fn turn(&self) -> ChatTurnState {
        ChatTurnState::try_from(self.chat.state.turn).unwrap_or_default()
    }

    pub(super) fn connection(&self) -> ChatConnection {
        ChatConnection::try_from(self.chat.state.connection).unwrap_or_default()
    }

    pub(super) fn options(&self) -> ChatOptions {
        self.chat.state.options.clone().unwrap_or_default()
    }

    /// The selected model, when the catalog lists it.
    pub(super) fn model(&self) -> Option<&ChatModel> {
        let id = self.chat.state.options.as_ref()?.model.as_str();
        self.chat.catalog.models.iter().find(|model| model.id == id)
    }

    /// Why a message cannot be sent now, if it cannot.
    pub(super) fn send_refusal(&self) -> Option<&'static str> {
        self.agent_refusal().or_else(|| {
            if self.capabilities().send_mid_turn {
                return None;
            }
            match self.turn() {
                ChatTurnState::Idle => None,
                ChatTurnState::Waiting => Some("The agent is waiting for your answer."),
                _ => Some("The agent is working. Stop it, or wait for it to finish."),
            }
        })
    }

    /// Why nothing at all can be asked of the agent now.
    fn agent_refusal(&self) -> Option<&'static str> {
        if !self.is_live() {
            return Some("The host is not connected.");
        }
        if !self.chat.is_held() {
            return Some(if self.open_error.is_some() {
                "The chat is not open."
            } else {
                "The chat is still opening."
            });
        }
        match self.connection() {
            ChatConnection::Ready => None,
            ChatConnection::Ended => Some("The agent has exited."),
            ChatConnection::Failed => Some("The agent is not running."),
            _ => Some("The agent is still starting."),
        }
    }

    fn on_composer(
        &mut self,
        _: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.slash_active = 0;
                let text = self.composer.read(cx).value();
                if self.slash_dismissed.as_deref() != Some(text.as_ref()) {
                    self.slash_dismissed = None;
                }
                if text.is_empty() {
                    // What it was said about is gone.
                    self.notice = None;
                }
                cx.notify();
            }
            // Shift+Enter is a new line, which the field has already made.
            InputEvent::PressEnter { shift: false, .. } => self.submit(window, cx),
            _ => {}
        }
    }

    pub(super) fn set_composer(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_owned();
        self.composer.update(cx, |composer, cx| {
            let end = text.len();
            composer.set_value(text, window, cx);
            // What is typed next goes after it, as after a paste.
            composer.set_selected_range(end..end, cx);
        });
        self.slash_active = 0;
        self.slash_dismissed = None;
    }

    /// The commands a typed `/word` or `$word` offers, while the list is
    /// open, and the sigil they are named with.
    pub(super) fn slash_suggestions(&self, cx: &App) -> (char, Vec<ChatCommand>) {
        let composer = self.composer.read(cx);
        let text = composer.value();
        if self.slash_dismissed.as_deref() == Some(text.as_ref()) {
            return ('/', Vec::new());
        }
        match slash::query(&text, composer.cursor()) {
            Some(query) => (
                query.sigil(),
                slash::suggestions(&self.chat.catalog, &query)
                    .into_iter()
                    .cloned()
                    .collect(),
            ),
            None => ('/', Vec::new()),
        }
    }

    pub(super) fn slash_step(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let count = self.slash_suggestions(cx).1.len();
        if count == 0 {
            return false;
        }
        self.slash_active =
            (self.slash_active.min(count - 1) as isize + delta).rem_euclid(count as isize) as usize;
        self.slash_scroll.scroll_to_item(self.slash_active);
        cx.notify();
        true
    }

    /// Puts the selected command in the composer, ready for its arguments.
    pub(super) fn slash_accept(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (_, suggestions) = self.slash_suggestions(cx);
        let Some(command) = suggestions.get(index.unwrap_or(self.slash_active)) else {
            return false;
        };
        let (text, cursor) = {
            let composer = self.composer.read(cx);
            (composer.value(), composer.cursor())
        };
        match slash::query(&text, cursor) {
            // A skill named in the middle of a message: only its word changes.
            Some(query) if query.skill => {
                let named = format!("${} ", command.name);
                let after = text[cursor..].strip_prefix(' ').unwrap_or(&text[cursor..]);
                let end = query.start + named.len();
                let text = format!("{}{named}{after}", &text[..query.start]);
                self.composer.update(cx, |composer, cx| {
                    composer.set_value(text, window, cx);
                    composer.set_selected_range(end..end, cx);
                });
                self.slash_active = 0;
                self.slash_dismissed = None;
            }
            _ => self.set_composer(&format!("/{} ", command.name), window, cx),
        }
        window.focus(&self.input_focus(cx), cx);
        cx.notify();
        true
    }

    pub(super) fn slash_dismiss(&mut self, cx: &mut Context<Self>) -> bool {
        if self.slash_suggestions(cx).1.is_empty() {
            return false;
        }
        self.slash_dismissed = Some(self.composer.read(cx).value().to_string());
        cx.notify();
        true
    }

    // Actions.

    fn refused(&mut self, error: &str, cx: &mut Context<Self>) {
        self.notice = Some(error_message(error).to_owned().into());
        cx.notify();
    }

    /// Sends the composer's text: as a message, as the edit of an earlier
    /// one, or as a side question in a fork.
    pub(super) fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        note_activity(&self.profile_id, &self.pane_id, cx);
        self.notice = None;
        // A side question goes to a fork, so the turn here need not be idle.
        let side = slash::side_question(&self.chat.catalog, &text).map(str::to_owned);
        let refusal = match side {
            Some(_) => self.agent_refusal(),
            None => self.send_refusal(),
        };
        if let Some(reason) = refusal {
            return self.refused(reason, cx);
        }
        if let Some(question) = side {
            return self.side_fork(question, text, window, cx);
        }
        // The same text again goes under the id it had: if the host took it
        // and only the answer was lost, it says so instead of taking it twice.
        let id = match self.resend.take() {
            Some((sent, id)) if sent == text => id,
            _ => uuid::Uuid::new_v4().to_string(),
        };
        let rewind = self.editing.take();
        self.set_composer("", window, cx);
        self.sending.push(Sending {
            id: id.clone(),
            text: text.clone().into(),
        });
        self.sync_trail(cx);
        self.list.update(cx, |list, cx| list.scroll_to_end(cx));
        cx.notify();
        let Some(item_id) = rewind else {
            return self.deliver(id, text, cx);
        };
        // The host drops the edited message and what follows it; the new
        // text then goes as any message does.
        let rewind = ChatRequest {
            action: ChatAction::Rewind.into(),
            item_id: item_id.clone(),
            ..Default::default()
        };
        // Held until the new text is on its way: a pane that leaves the
        // screen meanwhile must not leave the conversation cut and unanswered.
        let pane = cx.entity();
        self.send(rewind, cx, move |this, result, cx| {
            let _pane = pane;
            match result {
                Ok(_) => this.deliver(id, text, cx),
                Err(error) => this.unsent(&id, &text, Some(item_id), &error, cx),
            }
        });
    }

    /// Sends one message under the id its pending bubble has.
    fn deliver(&mut self, id: String, text: String, cx: &mut Context<Self>) {
        let message = ChatRequest {
            action: ChatAction::Send.into(),
            text: text.clone(),
            client_message_id: id.clone(),
            ..Default::default()
        };
        self.send(message, cx, move |this, result, cx| match result {
            // Sent before, and the answer was lost: it is there.
            Err(error) if error_code(&error) == "chat_message_duplicate" => {
                let chat = &this.chat;
                this.sending
                    .retain(|sending| chat.item(&sending.id).is_none());
                this.sync_trail(cx);
                cx.notify();
            }
            Err(error) => this.unsent(&id, &text, None, &error, cx),
            Ok(_) => {}
        });
    }

    /// A message the host refused goes back where it can be sent again.
    fn unsent(
        &mut self,
        id: &str,
        text: &str,
        editing: Option<String>,
        error: &str,
        cx: &mut Context<Self>,
    ) {
        self.sending.retain(|sending| sending.id != id);
        self.sync_trail(cx);
        self.restore = Some(text.to_owned());
        self.resend = Some((text.to_owned(), id.to_owned()));
        if let Some(item_id) = editing.filter(|id| self.chat.item(id).is_some()) {
            self.editing = Some(item_id);
        }
        self.refused(error, cx);
    }

    fn side_fork(
        &mut self,
        question: String,
        typed: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if question.is_empty() {
            return self.refused("Type the question after the command.", cx);
        }
        if !self.capabilities().fork {
            return self.refused("This agent cannot fork its conversation.", cx);
        }
        self.set_composer("", window, cx);
        let fork = ChatRequest {
            action: ChatAction::SideFork.into(),
            text: question,
            ..Default::default()
        };
        self.send(fork, cx, move |this, result, cx| match result {
            Ok(response) => this.follow_fork(response.forked_pane_id, cx),
            Err(error) => {
                this.restore = Some(typed);
                this.refused(&error, cx);
            }
        });
    }

    /// Shows the fork's pane once the topology has it, as Branch does.
    fn follow_fork(&mut self, pane_id: String, cx: &mut Context<Self>) {
        let Some(agents) = cx.try_global::<Agents>() else {
            return;
        };
        // Someone who has moved to another host is not pulled back.
        if pane_id.is_empty() || agents.controller.read(cx).active_profile_id() != self.profile_id {
            return;
        }
        let now = agents.runtime.read(cx).now_ms();
        let pending = PendingOpen {
            profile_id: self.profile_id.clone(),
            pane_id,
            source: "The side question's fork".into(),
            expected_by: now + BRANCH_PANE_WAIT_MS,
        };
        cx.defer(move |cx| open_when_present(pending, cx));
    }

    pub(super) fn interrupt(&mut self, cx: &mut Context<Self>) {
        let stop = ChatRequest {
            action: ChatAction::Interrupt.into(),
            ..Default::default()
        };
        self.notice = None;
        self.send(stop, cx, |this, result, cx| {
            if let Err(error) = result {
                this.refused(&error, cx);
            }
        });
    }

    /// Answers the request a card showed. A card pressed as another request
    /// took its place answers nothing: that one was never read.
    pub(super) fn answer(&mut self, request_id: &str, answer: ChatAnswer, cx: &mut Context<Self>) {
        let shown = self
            .pending()
            .is_some_and(|request| request.request_id == request_id);
        if !shown || self.answering.is_some() {
            return;
        }
        note_activity(&self.profile_id, &self.pane_id, cx);
        self.answering = Some(request_id.to_owned());
        self.notice = None;
        let request = ChatRequest {
            action: ChatAction::Answer.into(),
            request_id: request_id.to_owned(),
            answer: Some(answer),
            ..Default::default()
        };
        let request_id = request_id.to_owned();
        self.send(request, cx, move |this, result, cx| {
            if this.answering.as_deref() == Some(request_id.as_str()) {
                this.answering = None;
            }
            match result {
                // Answered from somewhere else first: the card goes with the state.
                Err(error) if error_code(&error) != "chat_request_not_pending" => {
                    this.refused(&error, cx);
                }
                _ => cx.notify(),
            }
        });
        cx.notify();
    }

    /// Changes what the pills select. Empty and unspecified fields keep
    /// their value.
    pub(super) fn set_options(&mut self, options: ChatOptions, cx: &mut Context<Self>) {
        let request = ChatRequest {
            action: ChatAction::SetOptions.into(),
            options: Some(options),
            ..Default::default()
        };
        self.notice = None;
        self.send(request, cx, |this, result, cx| {
            if let Err(error) = result {
                this.refused(&error, cx);
            }
        });
    }

    /// Puts an earlier message back in the composer; sending it rewinds.
    pub(super) fn edit(&mut self, item_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.chat.item(item_id).filter(|item| item.rewindable) else {
            return;
        };
        let text = item.text.clone();
        self.editing = Some(item_id.to_owned());
        self.notice = None;
        self.set_composer(&text, window, cx);
        window.focus(&self.input_focus(cx), cx);
        cx.notify();
    }

    pub(super) fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            self.set_composer("", window, cx);
            cx.notify();
        }
    }

    /// Whether `item` is one of the user's own that may be edited.
    pub(super) fn editable(&self, item: &ChatItem) -> bool {
        item.kind == i32::from(ChatItemKind::User) && item.rewindable && self.capabilities().rewind
    }

    // The pending question.

    /// The draft for the pending question request, made when first drawn.
    pub(super) fn answer_draft(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<&mut AnswerDraft> {
        let request = self.pending()?;
        if request.kind != i32::from(ChatRequestKind::Question) {
            return None;
        }
        let (request_id, count) = (request.request_id.clone(), request.questions.len());
        if self.answer.is_none() {
            let other = cx.new(|cx| InputState::new(window, cx).placeholder("Type your answer"));
            let changes = cx.subscribe(&other, |this, input, event: &InputEvent, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let text = input.read(cx).value().to_string();
                if let Some(draft) = this.answer.as_mut()
                    && let Some(pick) = draft.picks.get_mut(draft.index)
                {
                    pick.other_text = text;
                    cx.notify();
                }
            });
            self.answer = Some(AnswerDraft {
                request_id,
                index: 0,
                picks: vec![Pick::default(); count],
                other,
                _changes: changes,
            });
        }
        self.answer.as_mut()
    }

    // File links.

    /// Drops what the host said about paths: every one is asked about again
    /// when its text is next drawn.
    fn forget_probes(&mut self, cx: &mut Context<Self>) {
        self.probes.clear();
        self.probe_queue.clear();
        if self.shown.is_empty() {
            return;
        }
        self.shown.clear();
        // A link shows a file's name where the text had its path.
        self.list.update(cx, |list, cx| list.remeasure(cx));
        cx.notify();
    }

    /// An agent message as drawn: its Markdown with file links. Inline code
    /// that could be a path is asked about, and becomes a link once the host
    /// says the file is there.
    pub(super) fn shown_text(&mut self, index: usize, cx: &mut Context<Self>) -> SharedString {
        let Self {
            chat,
            shown,
            probes,
            probe_queue,
            ..
        } = self;
        let Some(item) = chat.items.get(index) else {
            return SharedString::default();
        };
        if let Some((length, text)) = shown.get(&item.id)
            && *length == item.text.len()
        {
            return text.clone();
        }
        let streaming = item.streaming;
        let text: SharedString = links::rewrite(&item.text, &mut |path| {
            if let Some(known) = probes.get(path) {
                return *known;
            }
            // A path still being written is not asked about.
            if !streaming && probes.len() < MAX_PROBES {
                probes.insert(path.to_owned(), Probe::Pending);
                probe_queue.push_back(path.to_owned());
            }
            Probe::Pending
        })
        .into();
        shown.insert(item.id.clone(), (item.text.len(), text.clone()));
        if !self.probe_queue.is_empty() && !self.probing {
            let this = cx.entity().downgrade();
            cx.defer(move |cx| {
                this.update(cx, |this, cx| this.pump_probes(cx)).ok();
            });
        }
        text
    }

    /// Asks the host about the next path, one at a time.
    fn pump_probes(&mut self, cx: &mut Context<Self>) {
        if self.probing {
            return;
        }
        let Some(path) = self.probe_queue.pop_front() else {
            return;
        };
        let answer = self.live.as_ref().and_then(|live| {
            let controller = self.controller.read(cx);
            let generation = controller.link(&self.profile_id)?.host_state.generation;
            let command = json!({
                "operation": "resolveTerminalFile",
                "operationId": uuid::Uuid::new_v4().to_string(),
                "path": links::resolvable(&path),
                "paneId": self.tmux.id,
                "expectedServerIdentity": self.server_identity,
                "expectedTopologyGeneration": generation.to_string(),
                "expectedSessionId": self.tmux.session_id,
                "expectedWindowId": self.tmux.window_id,
                "expectedCwd": self.tmux.current_path,
            });
            Some(
                controller
                    .backend()
                    .file_request(live.client_id.clone(), command),
            )
        });
        let Some(answer) = answer else {
            // Asked again when the link is back.
            self.probes.clear();
            self.probe_queue.clear();
            return;
        };
        self.probing = true;
        cx.spawn(async move |this, cx| {
            let exists = answer.await.is_ok();
            this.update(cx, |this, cx| {
                this.probing = false;
                // Dropped meanwhile: the answer is about another directory.
                if let Some(probe) = this.probes.get_mut(&path) {
                    *probe = if exists {
                        Probe::Exists
                    } else {
                        Probe::Missing
                    };
                    if exists {
                        this.shown.clear();
                        this.list.update(cx, |list, cx| list.remeasure(cx));
                        cx.notify();
                    }
                }
                this.pump_probes(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Opens the file an edit row names in the Git diff tab, or the file
    /// itself when Git shows no change to it.
    pub(super) fn open_edit(&self, path: &str, cx: &mut App) {
        super::open::changes(&self.tmux, path, cx);
    }
}

impl Focusable for ChatPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
