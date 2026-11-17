//! One chat pane: what it holds, and everything it says to the host. The
//! drawing is in [`super::view`] and [`super::composer`].
//!
//! The pane follows its host's link. It opens the chat when the link is live,
//! and again after a resync, a reconnect, a lost change, or a refusal that
//! says to; `chat.proto` has the rules. Whatever the host has not confirmed
//! yet (a message being sent, a page being loaded, a pill's choice, a Stop)
//! is kept beside the model, never in it.
//!
//! Its window keeps a pane open while it is off screen (see
//! [`super::KeptChats`]): its events still apply, and an `OPEN` it needs then
//! waits until it is shown again.

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
    ChatResponse, ChatSpeed, ChatToolKind, ChatTurnState,
};
use futures::future::BoxFuture;
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, Focusable, Global, Pixels,
    Point, ScrollHandle, SharedString, Subscription, Task, Window,
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
    timeline::{self, Entry},
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
/// How long a choice the host took is shown when its state says otherwise:
/// the agent settled on something else, as fast mode does on an account
/// without it. Then the pill shows what the host says.
const WANTED_WAIT: Duration = Duration::from_secs(3);
/// How long Stop stays pending without the turn ending, so an answer lost
/// on the way cannot leave it off for good.
const STOP_WAIT: Duration = Duration::from_secs(10);
/// How long Copy shows its check.
const COPIED_FOR: Duration = Duration::from_secs(1);
/// The working line's seconds tick this often.
const TICK: Duration = Duration::from_secs(1);

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

/// What the timeline's rows are, in order: the header, the entries the items
/// are laid out in, the messages being sent, the working line, and the
/// pending request's card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Row {
    Header,
    Entry(usize),
    Sending(usize),
    Working,
    Request,
}

/// When a thought this pane watched stream began, and once it ended, how
/// long it took.
#[derive(Clone, Copy)]
enum Thought {
    Since(Instant),
    Took(Duration),
}

/// Choices sent to the host and not yet in its state, one per pill, drawn
/// over the state's options until it shows them. Each remembers the request
/// it went with, so that request's refusal takes back only its own choice
/// and not one made after it.
#[derive(Default)]
pub(super) struct Wanted {
    model: Option<(u64, String)>,
    effort: Option<(u64, String)>,
    permission: Option<(u64, i32)>,
    speed: Option<(u64, i32)>,
}

impl Wanted {
    /// What request `choice` asks for. Empty and unspecified fields ask
    /// for nothing.
    fn want(&mut self, choice: u64, options: &ChatOptions) {
        if !options.model.is_empty() {
            self.model = Some((choice, options.model.clone()));
        }
        if !options.effort.is_empty() {
            self.effort = Some((choice, options.effort.clone()));
        }
        if options.permission != 0 {
            self.permission = Some((choice, options.permission));
        }
        if options.speed != 0 {
            self.speed = Some((choice, options.speed));
        }
    }

    /// Takes back what request `choice` asked for. True if anything was.
    fn forget(&mut self, choice: u64) -> bool {
        let model = self.model.take_if(|(by, _)| *by == choice).is_some();
        let effort = self.effort.take_if(|(by, _)| *by == choice).is_some();
        let permission = self.permission.take_if(|(by, _)| *by == choice).is_some();
        let speed = self.speed.take_if(|(by, _)| *by == choice).is_some();
        model || effort || permission || speed
    }

    /// Drops every choice the host's state already shows.
    fn settle(&mut self, confirmed: &ChatOptions) {
        let fast = |speed: i32| speed == i32::from(ChatSpeed::Fast);
        self.model.take_if(|(_, model)| *model == confirmed.model);
        self.effort
            .take_if(|(_, effort)| *effort == confirmed.effort);
        self.permission
            .take_if(|(_, permission)| *permission == confirmed.permission);
        // Unspecified is standard: the toggle draws them alike.
        self.speed
            .take_if(|(_, speed)| fast(*speed) == fast(confirmed.speed));
    }

    /// `confirmed` with the choices on their way laid over it.
    fn over(&self, mut options: ChatOptions) -> ChatOptions {
        if let Some((_, model)) = &self.model {
            options.model = model.clone();
        }
        if let Some((_, effort)) = &self.effort {
            options.effort = effort.clone();
        }
        if let Some((_, permission)) = self.permission {
            options.permission = permission;
        }
        if let Some((_, speed)) = self.speed {
            options.speed = speed;
        }
        options
    }

    pub fn model(&self) -> bool {
        self.model.is_some()
    }

    pub fn effort(&self) -> bool {
        self.effort.is_some()
    }

    pub fn permission(&self) -> bool {
        self.permission.is_some()
    }

    pub fn speed(&self) -> bool {
        self.speed.is_some()
    }
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
    /// On screen. A pane its window keeps off screen sends nothing on its
    /// own and reports no activity.
    on_screen: bool,
    /// The chat is to be opened again once it is shown.
    open_due: bool,
    /// What is shown is held from before and the host is sending it again.
    pub(super) updating: bool,
    /// The pills' choices the host has not shown yet.
    pub(super) wanted: Wanted,
    /// Counts `SET_OPTIONS`, so each answer finds its own choice.
    choices: u64,
    /// Stop was pressed and the turn has not ended yet.
    pub(super) stopping: bool,
    stop_wait: Option<Task<()>>,
    pub(super) list: Entity<MessageScrollerState>,
    /// The items as laid out in rows, as the list was last told of them.
    pub(super) entries: Vec<Entry>,
    /// The entry and trailing rows the list was last told of.
    rows: (usize, usize),
    /// The groups of steps shown open, by their first item's id.
    pub(super) open_groups: HashSet<String>,
    /// Thoughts this pane watched stream, by item id.
    thoughts: HashMap<String, Thought>,
    /// When the turn in progress started, as near as this pane knows.
    turn_started: Option<Instant>,
    /// When the agent started compacting, as near as this pane knows.
    compacting_since: Option<Instant>,
    /// Ticks the working line's seconds, and the background list's, while
    /// either is on screen.
    ticker: Option<Task<()>>,
    /// The background tasks' list is open above its chip.
    pub(super) tasks_open: bool,
    /// Background tasks Stop was pressed for, until they leave the list or
    /// the wait for that runs out.
    pub(super) stopping_tasks: HashMap<String, Task<()>>,
    /// The message whose Copy shows its check, until the check goes.
    copied: Option<(String, Task<()>)>,
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
            // Any key with the keyboard in the chat, as in a terminal pane,
            // says the agent's call has been seen. Observed after the key's
            // own work, so one an action takes counts too.
            cx.observe_keystrokes(|this, event, window, cx| {
                if !event.keystroke.is_modifier_key() && this.focus.contains_focused(window, cx) {
                    this.seen(cx);
                }
            }),
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
            // Made to be mounted.
            on_screen: true,
            open_due: false,
            updating: false,
            wanted: Wanted::default(),
            choices: 0,
            stopping: false,
            stop_wait: None,
            list,
            entries: Vec::new(),
            rows: (0, 0),
            open_groups: HashSet::new(),
            thoughts: HashMap::new(),
            turn_started: None,
            compacting_since: None,
            ticker: None,
            tasks_open: false,
            stopping_tasks: HashMap::new(),
            copied: None,
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

    /// Which chat this is, as its window keeps it.
    pub fn key(&self) -> super::ChatKey {
        (
            self.profile_id.clone(),
            self.server_identity.clone(),
            self.pane_id.to_string(),
        )
    }

    /// Put on screen or taken off it by its window, which keeps it open
    /// meanwhile. Off screen its events still apply, so it is current when
    /// it comes back; only a new snapshot waits for that.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if self.on_screen == shown {
            return;
        }
        self.on_screen = shown;
        if shown && self.open_due {
            self.open(cx);
        }
        self.sync_ticker(cx);
        cx.notify();
    }

    /// Says the agent's call has been seen, from a pane on screen only.
    fn seen(&self, cx: &mut App) {
        if self.on_screen {
            note_activity(&self.profile_id, &self.pane_id, cx);
        }
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
        // The answer to a Stop sent over the old connection is not coming.
        self.stopped(cx);
        if self.live.is_none() {
            // Nothing is on its way while the link is down.
            self.updating = false;
        } else {
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
        if !self.on_screen {
            // Asked for once shown: what is held shows at once meanwhile, and
            // a reconnect does not ask for every kept chat's snapshot at once
            // over a slow link.
            self.open_due = true;
            self.retry = None;
            return;
        }
        self.open_due = false;
        // What is held stays on show, said to be on its way again.
        self.updating = self.chat.is_held();
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
                    this.updating = false;
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
        self.sending.len() + usize::from(self.working()) + usize::from(self.pending().is_some())
    }

    pub(super) fn row(&self, index: usize) -> Option<Row> {
        let entries = self.entries.len();
        let sending = entries + self.sending.len();
        let working = sending + usize::from(self.working());
        match index {
            0 => Some(Row::Header),
            index if index <= entries => Some(Row::Entry(index - 1)),
            index if index <= sending => Some(Row::Sending(index - 1 - entries)),
            index if index <= working => Some(Row::Working),
            index if index == working + 1 && self.pending().is_some() => Some(Row::Request),
            _ => None,
        }
    }

    /// The working line is shown: the turn works, or the agent compacts,
    /// whatever the turn says. Not while it waits on the user, whose card is
    /// the status then.
    pub(super) fn working(&self) -> bool {
        self.chat.is_held() && (self.turn() == ChatTurnState::Working || self.compacting())
    }

    /// The agent is compacting the conversation's context.
    pub(super) fn compacting(&self) -> bool {
        self.chat.state.compacting
    }

    /// How long what the working line says has run: the compaction, else
    /// the turn in progress.
    pub(super) fn elapsed(&self, cx: &App) -> Duration {
        let now = cx.background_executor().now();
        let since = if self.compacting() {
            self.compacting_since
        } else {
            self.turn_started
        };
        since
            .map(|started| now.saturating_duration_since(started))
            .unwrap_or_default()
    }

    /// How long ago the host's clock said `at_unix_millis`, by this
    /// machine's clock; `None` when the host gave no time.
    pub(super) fn since_unix_millis(at_unix_millis: i64) -> Option<Duration> {
        if at_unix_millis <= 0 {
            return None;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |now| now.as_millis() as i64);
        // A host clock ahead of this one is not a time still to come.
        Some(Duration::from_millis(
            u64::try_from(now - at_unix_millis).unwrap_or(0),
        ))
    }

    /// How long a thought this pane watched stream took.
    pub(super) fn thought_took(&self, id: &str) -> Option<Duration> {
        match self.thoughts.get(id) {
            Some(Thought::Took(took)) => Some(*took),
            _ => None,
        }
    }

    /// Keeps the turn's start: from when this pane saw it start, else from
    /// the user's newest message, never later than now.
    fn follow_turn(&mut self, reset: bool, now: Instant) {
        if !timeline::is_busy(self.turn()) {
            self.turn_started = None;
            return;
        }
        if !reset {
            self.turn_started.get_or_insert(now);
            return;
        }
        let sent = self
            .chat
            .items
            .iter()
            .rev()
            .find(|item| item.kind == i32::from(ChatItemKind::User))
            .map(|item| item.at_unix_millis)
            .filter(|at| *at > 0);
        let since = sent.map_or(now, |at| {
            let wall = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |wall| wall.as_millis() as i64);
            let ago = Duration::from_millis(u64::try_from(wall - at).unwrap_or(0));
            now.checked_sub(ago).unwrap_or(now)
        });
        // A turn that began after the one this pane last knew of.
        self.turn_started = Some(self.turn_started.map_or(since, |known| known.max(since)));
    }

    /// Follows what the state says runs outside the turn: when compacting
    /// began, and which tasks are still being stopped. The list closes with
    /// its last task.
    fn follow_background(&mut self, now: Instant) {
        if self.compacting() {
            self.compacting_since.get_or_insert(now);
        } else {
            self.compacting_since = None;
        }
        let background = &self.chat.state.background;
        self.stopping_tasks
            .retain(|id, _| background.iter().any(|task| task.id == *id));
        if background.is_empty() {
            self.tasks_open = false;
        }
    }

    /// Opens or closes the background tasks' list.
    pub(super) fn set_tasks_open(&mut self, open: bool, cx: &mut Context<Self>) {
        let open = open && !self.chat.state.background.is_empty();
        if self.tasks_open != open {
            self.tasks_open = open;
            self.seen(cx);
            self.sync_ticker(cx);
            cx.notify();
        }
    }

    /// Stops one background task. Its row stays dimmed until it leaves the
    /// list, or for as long as a Stop waits for its turn to end, so a lost
    /// answer cannot leave it so for good.
    pub(super) fn stop_task(&mut self, task_id: &str, cx: &mut Context<Self>) {
        self.seen(cx);
        if self.stopping_tasks.contains_key(task_id) || self.agent_refusal().is_some() {
            return;
        }
        self.notice = None;
        let id = task_id.to_owned();
        let timer = cx.background_executor().timer(STOP_WAIT);
        let wait = cx.spawn({
            let id = id.clone();
            async move |this, cx| {
                timer.await;
                this.update(cx, |this, cx| {
                    if this.stopping_tasks.remove(&id).is_some() {
                        cx.notify();
                    }
                })
                .ok();
            }
        });
        self.stopping_tasks.insert(id.clone(), wait);
        let request = ChatRequest {
            action: ChatAction::StopTask.into(),
            task_id: id.clone(),
            ..Default::default()
        };
        self.send(request, cx, move |this, result, cx| {
            if let Err(error) = result {
                this.stopping_tasks.remove(&id);
                this.refused(&error, cx);
            }
            cx.notify();
        });
        cx.notify();
    }

    /// Ticks the working line once a second while it is on screen, from the
    /// turn's start so the seconds turn over evenly, and the background
    /// list's times while it is open. A pane kept off screen, or one with
    /// neither, has no timer and draws nothing.
    fn sync_ticker(&mut self, cx: &mut Context<Self>) {
        if !(self.on_screen && (self.working() || self.tasks_open)) {
            self.ticker = None;
            return;
        }
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(wait) = this.update(cx, |this, cx| {
                    let into = this.elapsed(cx).as_nanos() % TICK.as_nanos();
                    TICK - Duration::from_nanos(into as u64)
                }) else {
                    return;
                };
                cx.background_executor().timer(wait).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        }));
    }

    /// Whether the working line's timer runs.
    #[cfg(test)]
    pub(super) fn is_ticking(&self) -> bool {
        self.ticker.is_some()
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
        let now = cx.background_executor().now();
        if matches!(outcome, Outcome::Reset | Outcome::State) {
            self.follow_turn(outcome == Outcome::Reset, now);
            self.follow_background(now);
        }
        match outcome {
            Outcome::Reset => {
                // The `OPEN` has done its work, though its answer may still
                // be on its way: a change lost from here on is asked about.
                self.opening = false;
                self.snapshot_by = None;
                self.open_error = None;
                self.updating = false;
                self.shown.clear();
                self.loading_older = false;
                self.older_failed = false;
                // Not watched from their start.
                self.thoughts.clear();
                self.entries = timeline::layout(&self.chat.items, self.turn());
                let rows = (self.entries.len(), self.trail());
                self.list
                    .update(cx, |list, cx| list.reset(1 + rows.0 + rows.1, cx));
                self.rows = rows;
            }
            Outcome::Appended(count) => {
                let start = self.chat.items.len().saturating_sub(count);
                for item in &self.chat.items[start..] {
                    if item.kind == i32::from(ChatItemKind::Reasoning) && item.streaming {
                        self.thoughts.insert(item.id.clone(), Thought::Since(now));
                    }
                }
                self.relayout(0, cx);
            }
            Outcome::Truncated(_) => {
                let chat = &self.chat;
                self.shown.retain(|id, _| chat.item(id).is_some());
                self.thoughts.retain(|id, _| chat.item(id).is_some());
                self.open_groups.retain(|id| chat.item(id).is_some());
                self.relayout(0, cx);
            }
            Outcome::Updated(index) => {
                let item = &self.chat.items[index];
                self.shown.remove(&item.id);
                // An item never changes kind, so only thinking, which is
                // not drawn while it streams, and a subagent, which is not
                // drawn while it runs, can move a row.
                let subagent = item
                    .tool
                    .as_ref()
                    .is_some_and(|tool| tool.kind == i32::from(ChatToolKind::Subagent));
                if subagent {
                    self.relayout(0, cx);
                }
                let item = &self.chat.items[index];
                if item.kind == i32::from(ChatItemKind::Reasoning) {
                    if !item.streaming
                        && let Some(Thought::Since(since)) = self.thoughts.get(&item.id).copied()
                    {
                        let took = now.saturating_duration_since(since);
                        self.thoughts.insert(item.id.clone(), Thought::Took(took));
                    }
                    self.relayout(0, cx);
                }
                self.remeasure_item(index, cx);
            }
            Outcome::State => {
                self.list
                    .update(cx, |list, cx| list.remeasure_items(0..1, cx));
                self.relayout(0, cx);
            }
            Outcome::Ignored | Outcome::Reopen => {}
        }
        if matches!(outcome, Outcome::Reset | Outcome::State) {
            self.wanted.settle(&self.confirmed_options());
            if self.turn() != ChatTurnState::Working {
                self.stopped(cx);
            }
        }
        self.sync_trail(cx);
        self.sync_ticker(cx);
        cx.notify();
    }

    /// Lays the items out again and tells the list about the entries that
    /// differ, found as the run between the longest common start and end:
    /// every other row keeps its measured height. `shift` items were put
    /// before the first since the last layout.
    fn relayout(&mut self, shift: usize, cx: &mut Context<Self>) {
        let new = timeline::layout(&self.chat.items, self.turn());
        let old = std::mem::replace(&mut self.entries, new);
        let new = &self.entries;
        let same = |old: &Entry, new: &Entry| old.shifted(shift) == *new;
        let start = old
            .iter()
            .zip(new)
            .take_while(|(old, new)| same(old, new))
            .count();
        let room = old.len().min(new.len()) - start;
        let end = old
            .iter()
            .rev()
            .zip(new.iter().rev())
            .take(room)
            .take_while(|(old, new)| same(old, new))
            .count();
        let (removed, added) = (old.len() - start - end, new.len() - start - end);
        if removed > 0 || added > 0 {
            let first = 1 + start;
            self.list.update(cx, |list, cx| {
                list.splice(first..first + removed, added, cx);
            });
        }
        self.rows.0 = new.len();
    }

    /// The row of item `index` changed height: its entry's row.
    pub(super) fn remeasure_item(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(entry) = timeline::entry_of(&self.entries, index) {
            self.remeasure_row(1 + entry, cx);
        }
    }

    /// Opens or closes the group whose first item is `key`.
    pub(super) fn toggle_group(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.open_groups.remove(key) {
            self.open_groups.insert(key.to_owned());
        }
        let items = &self.chat.items;
        let entry = self
            .entries
            .iter()
            .position(|entry| items.get(entry.first()).is_some_and(|item| item.id == key));
        if let Some(entry) = entry {
            self.remeasure_row(1 + entry, cx);
        }
    }

    /// Tells the list about the rows after the entries, which follow state
    /// that is not an item: a message being sent, the working line, the
    /// pending request.
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
                // The list keeps its place when rows go in above it, but the
                // header is above them: from there, stay on the entry that
                // was first.
                let at_header = {
                    let list = self.list.read(cx);
                    list.logical_scroll_top().item_ix == 0 && !list.is_following_tail()
                };
                let before = self.entries.len();
                // A run that the page continues joins the group it starts.
                self.relayout(count, cx);
                let added = self.entries.len().saturating_sub(before);
                if at_header && added > 0 {
                    self.list.update(cx, |list, cx| {
                        list.scroll_to_item(added + 1, cx);
                    });
                }
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

    /// The options as the host's state says them.
    fn confirmed_options(&self) -> ChatOptions {
        self.chat.state.options.clone().unwrap_or_default()
    }

    /// The options as the pills draw them: the host's, under the choices
    /// still on their way.
    pub(super) fn options(&self) -> ChatOptions {
        self.wanted.over(self.confirmed_options())
    }

    /// The selected model, when the catalog lists it.
    pub(super) fn model(&self) -> Option<&ChatModel> {
        let id = self.options().model;
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
    pub(super) fn agent_refusal(&self) -> Option<&'static str> {
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
                // Typed, pasted or composed: the field sends no event for
                // the text set here.
                self.seen(cx);
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
                // Only what was typed of the name is replaced: what follows
                // the cursor may be a word of its own.
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
        self.seen(cx);
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

    /// Asks the agent to stop. Stop shows it at once and is off until the
    /// turn ends, so a press the host has not answered yet is not sent twice.
    pub(super) fn interrupt(&mut self, cx: &mut Context<Self>) {
        if self.stopping {
            return;
        }
        let stop = ChatRequest {
            action: ChatAction::Interrupt.into(),
            ..Default::default()
        };
        self.notice = None;
        self.stopping = true;
        let wait = cx.background_executor().timer(STOP_WAIT);
        self.stop_wait = Some(cx.spawn(async move |this, cx| {
            wait.await;
            this.update(cx, |this, cx| this.stopped(cx)).ok();
        }));
        self.send(stop, cx, |this, result, cx| {
            if let Err(error) = result {
                this.stopped(cx);
                this.refused(&error, cx);
            }
        });
        cx.notify();
    }

    /// Stop is Stop again: the turn ended, the host refused, or no answer came.
    fn stopped(&mut self, cx: &mut Context<Self>) {
        self.stop_wait = None;
        if std::mem::take(&mut self.stopping) {
            cx.notify();
        }
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
        self.seen(cx);
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
    /// their value. The pill shows the choice at once, until the host's
    /// state shows it or the host turns it down.
    pub(super) fn set_options(&mut self, options: ChatOptions, cx: &mut Context<Self>) {
        self.choices += 1;
        let choice = self.choices;
        self.wanted.want(choice, &options);
        // Asked for what is already so: nothing to wait for.
        self.wanted.settle(&self.confirmed_options());
        let request = ChatRequest {
            action: ChatAction::SetOptions.into(),
            options: Some(options),
            ..Default::default()
        };
        self.notice = None;
        self.send(request, cx, move |this, result, cx| match result {
            Err(error) => {
                this.wanted.forget(choice);
                this.refused(&error, cx);
            }
            // The state that shows the choice is usually here first. If the
            // agent settled on something else, the pill follows the host.
            Ok(_) => {
                let wait = cx.background_executor().timer(WANTED_WAIT);
                cx.spawn(async move |this, cx| {
                    wait.await;
                    this.update(cx, |this, cx| {
                        if this.wanted.forget(choice) {
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
            }
        });
        cx.notify();
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

    /// Copies a message: the user's text, or the whole reply an agent's
    /// text ends, as its Markdown. Its Copy shows a check for a second.
    pub(super) fn copy_message(&mut self, index: usize, cx: &mut Context<Self>) {
        let items = &self.chat.items;
        let Some(item) = items.get(index) else {
            return;
        };
        let text = if item.kind == i32::from(ChatItemKind::Assistant) {
            let start = items[..index]
                .iter()
                .rposition(|item| item.kind == i32::from(ChatItemKind::User))
                .map_or(0, |at| at + 1);
            items[start..=index]
                .iter()
                .filter(|item| item.kind == i32::from(ChatItemKind::Assistant))
                .map(|item| item.text.trim())
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        } else {
            item.text.clone()
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        let id = item.id.clone();
        let wait = cx.background_executor().timer(COPIED_FOR);
        let done = cx.spawn(async move |this, cx| {
            wait.await;
            this.update(cx, |this, cx| {
                this.copied = None;
                cx.notify();
            })
            .ok();
        });
        self.copied = Some((id, done));
        cx.notify();
    }

    /// Whether the message `id`'s Copy shows its check.
    pub(super) fn just_copied(&self, id: &str) -> bool {
        self.copied.as_ref().is_some_and(|(copied, _)| copied == id)
    }

    /// Forks the conversation through the end of the reply whose last text
    /// is `item_id` (D6), into a split, as Fork from here does. The one place
    /// `CHAT_ACTION_FORK` is to be sent from; until the host carries it out,
    /// it does nothing.
    pub(super) fn fork_reply(&mut self, item_id: &str, cx: &mut Context<Self>) {
        let _ = (item_id, cx);
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
