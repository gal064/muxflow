//! The chat model's client rules, and a chat pane in the real window against
//! a fake host: what it asks the host, and what it shows for what comes back.

use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
};

use desktop_core::{
    app_state::{AppStateStore, PersistedAppState},
    connection::{
        ConnectionSpec, ProfileStore,
        chat::{
            ChatAction, ChatAgentMode, ChatCapabilities, ChatCatalog, ChatChange, ChatCommand,
            ChatCommandKind, ChatConnection, ChatEvent, ChatItem, ChatItemDelta, ChatItemKind,
            ChatModel, ChatOption, ChatOptions, ChatPendingRequest, ChatPermissionMode,
            ChatQuestion, ChatRequest, ChatRequestKind, ChatResponse, ChatSnapshot, ChatState,
            ChatTool, ChatToolKind, ChatToolStatus, ChatTruncate, ChatTurnState, ChatUsage,
        },
        helper::HelperInstallReport,
    },
    platform::{EventBody, EventSink},
};
use futures::future::{BoxFuture, FutureExt};
use gpui_kit::{
    AppContext, Entity, ScrollDelta, TestAppContext, VisualTestContext, point, px, size,
    test::{TestQueryExt, TestWindowExt},
};
use serde_json::{Value, json};
use tmux_control::{Pane, TmuxSnapshot};

use super::model::{Chat, Outcome, PageOutcome};
use crate::{
    app::{self, Core},
    connection_ui::{
        ConnectionUi,
        backend::{ConnectionBackend, EngineChannel, TerminalBackend, inline::InlineEngine},
        controller::{AgentServiceFrame, ConnectionController, ConnectionEvent},
        helper_upgrade::RemoteHelperProbe,
    },
    settings,
    terminal::link::POLL_IDLE,
    workspaces::model::fixtures::{pane, session, window},
};

const PANE: &str = "%1";

fn item(id: &str, kind: ChatItemKind, text: &str) -> ChatItem {
    ChatItem {
        id: id.into(),
        kind: kind.into(),
        text: text.into(),
        rewindable: kind == ChatItemKind::User,
        ..Default::default()
    }
}

fn tool(id: &str, kind: ChatToolKind, title: &str, detail: &str) -> ChatItem {
    ChatItem {
        id: id.into(),
        kind: ChatItemKind::Tool.into(),
        tool: Some(ChatTool {
            kind: kind.into(),
            title: title.into(),
            detail: detail.into(),
            status: ChatToolStatus::Ok.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn catalog() -> ChatCatalog {
    let command = |name: &str, description: &str, kind: ChatCommandKind| ChatCommand {
        name: name.into(),
        description: description.into(),
        argument_hint: String::new(),
        kind: kind.into(),
    };
    ChatCatalog {
        capabilities: Some(ChatCapabilities {
            rewind: true,
            send_mid_turn: true,
            model: true,
            effort: true,
            speed: true,
            permission: true,
            fork: true,
            questions: true,
        }),
        models: vec![
            ChatModel {
                id: "opus".into(),
                label: "Opus 5.5".into(),
                efforts: vec!["low".into(), "high".into()],
                supports_fast: true,
            },
            ChatModel {
                id: "fable".into(),
                label: "Fable 5.2".into(),
                efforts: Vec::new(),
                supports_fast: false,
            },
        ],
        commands: vec![
            command(
                "compact",
                "Summarise the conversation",
                ChatCommandKind::Command,
            ),
            command("clear", "Start over", ChatCommandKind::Command),
            command("code-review", "Review the diff", ChatCommandKind::Skill),
            command("btw", "Ask on the side", ChatCommandKind::Muxflow),
        ],
    }
}

fn state(turn: ChatTurnState) -> ChatState {
    ChatState {
        pane_id: PANE.into(),
        adapter_id: "claude".into(),
        turn: turn.into(),
        connection: ChatConnection::Ready.into(),
        options: Some(ChatOptions {
            model: "opus".into(),
            effort: "high".into(),
            permission: ChatPermissionMode::Auto.into(),
            ..Default::default()
        }),
        usage: Some(ChatUsage {
            used_tokens: 209_000,
            window_tokens: 1_000_000,
        }),
        history_complete: true,
        ..Default::default()
    }
}

fn snapshot(revision: u64, items: Vec<ChatItem>, older_cursor: &str) -> ChatEvent {
    snapshot_in(revision, state(ChatTurnState::Idle), items, older_cursor)
}

fn snapshot_in(
    revision: u64,
    state: ChatState,
    items: Vec<ChatItem>,
    older_cursor: &str,
) -> ChatEvent {
    ChatEvent {
        pane_id: PANE.into(),
        revision,
        change: Some(ChatChange::Snapshot(ChatSnapshot {
            revision,
            state: Some(state),
            catalog: Some(catalog()),
            items,
            older_cursor: older_cursor.into(),
        })),
    }
}

fn event(revision: u64, change: ChatChange) -> ChatEvent {
    ChatEvent {
        pane_id: PANE.into(),
        revision,
        change: Some(change),
    }
}

fn delta(revision: u64, item_id: &str, text: &str) -> ChatEvent {
    event(
        revision,
        ChatChange::Delta(ChatItemDelta {
            item_id: item_id.into(),
            text_append: text.into(),
        }),
    )
}

fn page(revision: u64, items: Vec<ChatItem>, older_cursor: &str) -> ChatResponse {
    ChatResponse {
        older: items,
        older_cursor: older_cursor.into(),
        revision,
        ..Default::default()
    }
}

fn ids(chat: &Chat) -> Vec<&str> {
    chat.items.iter().map(|item| item.id.as_str()).collect()
}

fn held() -> Chat {
    let mut chat = Chat::default();
    let items = vec![
        item("u1", ChatItemKind::User, "first"),
        item("a1", ChatItemKind::Assistant, "an"),
    ];
    assert_eq!(chat.apply(snapshot(10, items, "older-1")), Outcome::Reset);
    chat
}

// The model: `chat.proto`'s client rules.

#[test]
fn nothing_applies_before_a_snapshot_and_a_snapshot_replaces_everything() {
    let mut chat = Chat::default();
    assert_eq!(chat.apply(delta(3, "a1", "lost")), Outcome::Ignored);
    assert!(!chat.is_held());
    assert_eq!(chat.page_request(), None);

    let mut chat = held();
    assert!(chat.is_held());
    assert_eq!(ids(&chat), ["u1", "a1"]);
    let again = vec![item("u9", ChatItemKind::User, "other")];
    // Even one whose revision is behind: it is what the host holds now.
    assert_eq!(chat.apply(snapshot(4, again, "")), Outcome::Reset);
    assert_eq!(ids(&chat), ["u9"]);
    assert_eq!(chat.older_cursor, "");
    assert_eq!(chat.apply(delta(5, "u9", "!")), Outcome::Updated(0));
}

#[test]
fn a_change_applies_only_as_the_next_revision() {
    let mut chat = held();
    // At or behind what is held: already included.
    assert_eq!(chat.apply(delta(10, "a1", "x")), Outcome::Ignored);
    assert_eq!(chat.apply(delta(9, "a1", "x")), Outcome::Ignored);
    assert_eq!(chat.items[1].text, "an");
    // A gap: something was lost.
    assert_eq!(chat.apply(delta(12, "a1", "x")), Outcome::Reopen);
    assert_eq!(chat.items[1].text, "an");
    // The next one still applies once it comes.
    assert_eq!(chat.apply(delta(11, "a1", "swer")), Outcome::Updated(1));
    assert_eq!(chat.items[1].text, "answer");
    assert_eq!(chat.apply(delta(12, "a1", "!")), Outcome::Updated(1));
}

#[test]
fn items_append_update_and_follow_the_state() {
    let mut chat = held();
    let run = tool("t1", ChatToolKind::Command, "run", "cargo test");
    assert_eq!(
        chat.apply(event(11, ChatChange::Item(run.clone()))),
        Outcome::Appended(1)
    );
    let mut done = run;
    done.tool.as_mut().unwrap().result_summary = "14 passed".into();
    assert_eq!(
        chat.apply(event(12, ChatChange::Updated(done))),
        Outcome::Updated(2)
    );
    assert_eq!(
        chat.items[2].tool.as_ref().unwrap().result_summary,
        "14 passed"
    );
    assert_eq!(
        chat.apply(event(13, ChatChange::State(state(ChatTurnState::Working)))),
        Outcome::State
    );
    assert_eq!(chat.state.turn, i32::from(ChatTurnState::Working));
    assert_eq!(
        chat.apply(event(14, ChatChange::Catalog(ChatCatalog::default()))),
        Outcome::State
    );
    assert!(chat.catalog.models.is_empty());
}

#[test]
fn a_change_to_an_item_not_loaded_is_ignored_but_counted() {
    let mut chat = held();
    assert_eq!(chat.apply(delta(11, "older", "x")), Outcome::Ignored);
    let update = item("older", ChatItemKind::Assistant, "edited");
    assert_eq!(
        chat.apply(event(12, ChatChange::Updated(update))),
        Outcome::Ignored
    );
    assert_eq!(ids(&chat), ["u1", "a1"]);
    // The revision moved on all the same.
    assert_eq!(chat.apply(delta(13, "a1", "!")), Outcome::Updated(1));
}

#[test]
fn a_truncate_drops_what_follows_its_item() {
    let mut chat = held();
    let truncate = |after: &str| {
        ChatChange::Truncate(ChatTruncate {
            after_item_id: after.into(),
        })
    };
    assert_eq!(chat.apply(event(11, truncate("u1"))), Outcome::Truncated(1));
    assert_eq!(ids(&chat), ["u1"]);
    assert_eq!(chat.older_cursor, "older-1");
    // Rewound to an item this client never loaded: its place is unknown.
    assert_eq!(chat.apply(event(12, truncate("u0"))), Outcome::Reopen);
    // Everything, with nothing older left to ask for.
    assert_eq!(chat.apply(event(13, truncate(""))), Outcome::Truncated(0));
    assert!(chat.items.is_empty());
    assert_eq!(chat.page_request(), None);
}

#[test]
fn an_older_page_goes_before_the_first_item_without_repeats() {
    let mut chat = held();
    let request = chat.page_request().expect("there is something older");
    assert_eq!(request.cursor, "older-1");
    let older = vec![
        item("u0", ChatItemKind::User, "zero"),
        item("u1", ChatItemKind::User, "first"),
    ];
    assert_eq!(
        chat.page_answer(&request, page(10, older, "older-2")),
        PageOutcome::Merged(1)
    );
    assert_eq!(ids(&chat), ["u0", "u1", "a1"]);
    assert_eq!(chat.page_request().unwrap().cursor, "older-2");
    // The same answer again, as a retry would bring it.
    assert_eq!(
        chat.page_answer(&request, page(10, Vec::new(), "older-3")),
        PageOutcome::Dropped
    );
}

#[test]
fn a_page_asked_before_the_snapshot_held_is_dropped() {
    let mut chat = held();
    let request = chat.page_request().unwrap();
    let fresh = vec![item("u5", ChatItemKind::User, "new")];
    assert_eq!(chat.apply(snapshot(20, fresh, "older-1")), Outcome::Reset);
    let stale = vec![item("u0", ChatItemKind::User, "zero")];
    assert_eq!(
        chat.page_answer(&request, page(10, stale, "")),
        PageOutcome::Dropped
    );
    assert_eq!(ids(&chat), ["u5"]);
    assert_eq!(chat.older_cursor, "older-1");
}

#[test]
fn a_page_ahead_of_the_chat_waits_for_its_revision() {
    let mut chat = held();
    let request = chat.page_request().unwrap();
    let older = vec![item("u0", ChatItemKind::User, "zero")];
    assert_eq!(
        chat.page_answer(&request, page(12, older, "")),
        PageOutcome::Held
    );
    assert_eq!(ids(&chat), ["u1", "a1"]);
    assert_eq!(chat.apply(delta(11, "a1", "s")), Outcome::Updated(1));
    assert_eq!(chat.page_due(), None);
    assert_eq!(chat.apply(delta(12, "a1", "w")), Outcome::Updated(1));
    assert_eq!(chat.page_due(), Some(PageOutcome::Merged(1)));
    assert_eq!(ids(&chat), ["u0", "u1", "a1"]);
    assert_eq!(chat.page_due(), None);
}

#[test]
fn a_page_is_asked_again_when_a_change_to_it_was_missed() {
    let mut chat = held();
    let request = chat.page_request().unwrap();
    // A change to an item of the page, ignored because it was not loaded yet.
    assert_eq!(chat.apply(delta(11, "u0", "!")), Outcome::Ignored);
    let older = vec![item("u0", ChatItemKind::User, "zero")];
    // Read before that change: what it holds is stale.
    assert_eq!(
        chat.page_answer(&request, page(10, older.clone(), "")),
        PageOutcome::AskAgain
    );
    assert_eq!(ids(&chat), ["u1", "a1"]);
    // Read after it: the change is in it.
    assert_eq!(
        chat.page_answer(&request, page(11, older, "")),
        PageOutcome::Merged(1)
    );
}

// The pane, in the app's window.

#[derive(Default)]
struct HostState {
    sinks: Vec<Arc<dyn EventSink>>,
    chats: Vec<ChatRequest>,
    mode_initializations: Vec<ChatRequest>,
    /// Answers for the chat requests to come, in order; then plain success.
    chat_answers: VecDeque<Result<ChatResponse, String>>,
    /// The next chat request is answered only once it is released.
    hold: bool,
    held: Option<futures::channel::oneshot::Sender<()>>,
    /// The paths asked about, and whether each is there.
    files: Vec<String>,
    existing: Vec<String>,
}

#[derive(Clone, Default)]
struct FakeHost(Arc<Mutex<HostState>>);

impl FakeHost {
    fn state(&self) -> std::sync::MutexGuard<'_, HostState> {
        self.0.lock().unwrap()
    }

    fn actions(&self) -> Vec<ChatAction> {
        self.state()
            .chats
            .iter()
            .map(|request| ChatAction::try_from(request.action).unwrap())
            .collect()
    }

    fn last(&self, action: ChatAction) -> ChatRequest {
        let found = self
            .state()
            .chats
            .iter()
            .rev()
            .find(|request| request.action == i32::from(action))
            .cloned();
        found.unwrap_or_else(|| panic!("no {action:?} in {:?}", self.actions()))
    }

    fn count(&self, action: ChatAction) -> usize {
        self.actions()
            .iter()
            .filter(|sent| **sent == action)
            .count()
    }

    fn refuse_next(&self, error: &str) {
        self.state().chat_answers.push_back(Err(error.to_owned()));
    }

    /// The next chat request waits for [`Self::release`] before it is answered.
    fn hold_next(&self) {
        self.state().hold = true;
    }

    fn release(&self) {
        let held = self.state().held.take().expect("a held request");
        let _ = held.send(());
    }
}

impl TerminalBackend for FakeHost {
    fn spawn_engine(
        &self,
        outputs: std::sync::mpsc::Sender<terminal_engine::EngineOutput>,
    ) -> Box<dyn EngineChannel> {
        Box::new(InlineEngine::new(outputs))
    }

    fn request_seed(&self, _: String, _: String) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn request_history(
        &self,
        _: String,
        _: String,
        _: u32,
        _: u32,
    ) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn set_pane_visibility(
        &self,
        _: String,
        _: String,
        _: bool,
        _: bool,
        _: u64,
        _: u64,
    ) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn resize_client(
        &self,
        _: String,
        _: u16,
        _: u16,
        _: String,
        _: String,
    ) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn send_input(&self, _: String, _: String, _: Vec<u8>) -> Result<(), String> {
        Ok(())
    }

    fn send_paste(&self, _: String, _: String, _: Vec<u8>) -> Result<(), String> {
        Ok(())
    }

    fn record_incident(&self, _: &str, _: Value) {}

    fn set_terminal_colors(&self, _: String, _: String) -> Result<(), String> {
        Ok(())
    }

    fn tmux_action(&self, _: String, _: Value) -> BoxFuture<'static, Result<Value, String>> {
        async { Ok(json!({ "topologyGeneration": 1 })) }.boxed()
    }

    fn start_upload(
        &self,
        _: crate::terminal::integration::transfers::UploadRequest,
        _: Arc<dyn EventSink>,
    ) -> Result<String, String> {
        Err("no uploads here".into())
    }

    fn cancel_upload(&self, _: String) -> Result<Value, String> {
        Ok(json!({}))
    }

    fn chat_request(
        &self,
        _: String,
        request: ChatRequest,
    ) -> BoxFuture<'static, Result<ChatResponse, String>> {
        if request.initialize_if_unset {
            self.state().mode_initializations.push(request.clone());
            return async move {
                Ok(ChatResponse {
                    mode: request.mode,
                    ..Default::default()
                })
            }
            .boxed();
        }
        let mut state = self.state();
        state.chats.push(request);
        let answer = state
            .chat_answers
            .pop_front()
            .unwrap_or_else(|| Ok(ChatResponse::default()));
        if !std::mem::take(&mut state.hold) {
            return async move { answer }.boxed();
        }
        let (release, released) = futures::channel::oneshot::channel();
        state.held = Some(release);
        async move {
            let _ = released.await;
            answer
        }
        .boxed()
    }

    fn file_request(&self, _: String, command: Value) -> BoxFuture<'static, Result<Value, String>> {
        let mut state = self.state();
        let path = command["path"].as_str().unwrap_or_default().to_owned();
        let exists = state.existing.contains(&path);
        state.files.push(path);
        async move {
            if exists {
                Ok(json!({}))
            } else {
                Err("file_not_found: No such file.".to_owned())
            }
        }
        .boxed()
    }
}

impl ConnectionBackend for FakeHost {
    fn start_terminal(
        &self,
        _: String,
        _: ConnectionSpec,
        _: bool,
        sink: Arc<dyn EventSink>,
    ) -> Result<String, String> {
        let mut state = self.state();
        state.sinks.push(sink);
        Ok(format!("client-{}", state.sinks.len()))
    }

    fn stop_terminal(&self, _: String) {}

    fn acknowledge_delivery(
        &self,
        _: String,
        _: u64,
        _: u64,
        _: u64,
    ) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn probe_link(&self, _: String) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn discard_ssh_masters(&self) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn probe_helper(
        &self,
        _: ConnectionSpec,
    ) -> BoxFuture<'static, Result<RemoteHelperProbe, String>> {
        async { Err("not probed here".to_owned()) }.boxed()
    }

    fn install_helper(
        &self,
        _: ConnectionSpec,
        _: bool,
    ) -> BoxFuture<'static, Result<HelperInstallReport, String>> {
        async { Err("not installed here".to_owned()) }.boxed()
    }

    fn save_app_state(&self, _: PersistedAppState) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn late_requests(&self, _: &str) -> Option<u32> {
        None
    }

    fn now_ms(&self) -> u64 {
        1_000
    }

    fn network_online(&self) -> bool {
        true
    }
}

fn frame(kind: u8, label: &str, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![kind];
    bytes.extend_from_slice(&(label.len() as u16).to_be_bytes());
    bytes.extend_from_slice(label.as_bytes());
    bytes.extend_from_slice(&0u64.to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn topology(generation: u64, panes: Vec<Pane>) -> Vec<u8> {
    let snapshot = TmuxSnapshot {
        sessions: vec![session("$1", "main", 0)],
        windows: vec![window("@1", "$1", 0, "agent", true)],
        panes,
    };
    let body = json!({
        "sequence": 0, "generation": generation, "serverIdentity": "tmux",
        "authoritative": true, "snapshot": snapshot,
    });
    frame(7, "snapshot", body.to_string().as_bytes())
}

/// The agent's pane, a chat, beside a shell.
fn panes() -> Vec<Pane> {
    vec![
        Pane {
            active: true,
            current_path: "/work/repo".into(),
            chat_adapter_id: "claude".into(),
            ..pane(PANE, "@1", "$1", (0, 0, 60, 24))
        },
        pane("%2", "@1", "$1", (61, 0, 39, 24)),
    ]
}

struct Shell {
    host: FakeHost,
    controller: Entity<ConnectionController>,
    visual: VisualTestContext,
    _dir: tempfile::TempDir,
}

impl Shell {
    fn publish(&mut self, frames: Vec<Vec<u8>>) {
        let sink = self.host.state().sinks[0].clone();
        for frame in frames {
            sink.send(EventBody::Raw(frame)).unwrap();
        }
        self.settle();
    }

    fn settle(&mut self) {
        self.visual.run_until_parked();
        // What the engine produced waits for the link's next drain.
        self.visual.executor().advance_clock(POLL_IDLE);
        self.visual.run_until_parked();
        self.visual.update(|window, cx| window.render_frame(cx));
        self.visual.run_until_parked();
    }

    /// A chat event, as the bridge hands the controller one.
    fn chat(&mut self, event: ChatEvent) {
        let controller = self.controller.clone();
        self.visual.update(|_, cx| {
            controller.update(cx, |_, cx| {
                cx.emit(ConnectionEvent::Chat {
                    profile_id: "local".into(),
                    event: Box::new(event),
                });
            });
        });
        self.settle();
    }

    fn shows(&mut self, id: &str) -> bool {
        let id = id.to_owned();
        self.visual
            .update(|window, _| window.try_find(id).is_some())
    }

    fn label(&mut self, id: &str) -> String {
        let id = id.to_owned();
        self.visual
            .update(|window, _| window.find(id).label().unwrap_or_default().to_owned())
    }

    fn named(&mut self, label: &str) -> bool {
        self.visual
            .update(|window, _| window.try_find_by_label(label).is_some())
    }

    fn click(&mut self, id: &str) {
        let id = id.to_owned();
        self.visual.update(|window, cx| window.click(id, cx));
        self.settle();
    }

    fn hover(&mut self, id: &str) {
        let id = id.to_owned();
        self.visual.update(|window, cx| window.hover(id, cx));
        self.settle();
    }

    fn input(&mut self, text: &str) {
        self.visual.update(|window, cx| window.input(text, cx));
        self.settle();
    }

    fn press(&mut self, key: &str) {
        self.visual.update(|window, cx| window.press(key, cx));
        self.settle();
    }
}

/// The app's window on a connected local host whose one window holds a chat
/// pane and a shell, the chat not yet answered.
fn shell(cx: &mut TestAppContext) -> Shell {
    let dir = tempfile::tempdir().unwrap();
    let profiles = dir.path().join("profiles.json");
    let saved = json!({
        "schemaVersion": 1, "lastProfileId": "local",
        "profiles": [
            { "id": "local", "label": "Local", "connection": { "mode": "local" }, "shown": true },
        ],
    });
    std::fs::write(&profiles, saved.to_string()).unwrap();
    let state_path = dir.path().join("app-state.json");
    let state = json!({
        "schemaVersion": 1, "appTabs": [], "shell": {},
        "workspaceUi": [{
            "hostProfileId": "local", "serverIdentity": "tmux",
            "sessionId": "$1", "sessionName": "main",
        }],
    });
    std::fs::write(&state_path, state.to_string()).unwrap();
    let host = FakeHost::default();
    let backend: Arc<dyn ConnectionBackend> = Arc::new(host.clone());
    let store = Arc::new(ProfileStore::load(profiles).unwrap());
    let app_state = Arc::new(AppStateStore::load(state_path.clone()));
    let controller = cx.new(|cx| ConnectionController::new(backend, store, app_state, cx));
    cx.run_until_parked();
    let window = cx.update(|cx| {
        gpui_kit::init(cx);
        app::init(Core::at(Path::new(&state_path)), cx);
        cx.set_global(ConnectionUi(controller.clone()));
        super::start(controller.clone(), Arc::new(host.clone()), cx);
        app::open_main_window(cx).unwrap()
    });
    let visual = VisualTestContext::from_window(window, cx);
    visual.simulate_resize(size(px(1280.), px(800.)));
    let mut shell = Shell {
        host,
        controller,
        visual,
        _dir: dir,
    };
    shell.publish(vec![
        frame(10, "terminal", &1u64.to_be_bytes()),
        frame(6, "connected", &[]),
        topology(1, panes()),
    ]);
    shell
}

/// [`shell`], with the chat opened on a short conversation.
fn open(cx: &mut TestAppContext) -> Shell {
    let mut shell = shell(cx);
    shell.chat(snapshot(
        10,
        vec![
            item(
                "u1",
                ChatItemKind::User,
                "Find out why wrapped paths are missed.",
            ),
            item("a1", ChatItemKind::Assistant, "The detector joins rows."),
            tool("t1", ChatToolKind::Command, "run", "cargo test links"),
        ],
        "",
    ));
    shell
}

#[gpui_kit::test]
fn a_chat_pane_opens_its_chat_and_shows_what_the_host_holds(cx: &mut TestAppContext) {
    let mut shell = shell(cx);
    // Only the pane with a chat adapter, and for the server it is on.
    assert_eq!(shell.host.actions(), [ChatAction::Open]);
    let opened = shell.host.last(ChatAction::Open);
    assert_eq!(opened.pane_id, PANE);
    assert_eq!(opened.expected_server_identity, "tmux");
    assert!(shell.named("Chat"));
    assert!(!shell.shows("chat-item-u1"), "nothing before the snapshot");

    shell.chat(snapshot(
        10,
        vec![
            item("u1", ChatItemKind::User, "Find out why."),
            item("a1", ChatItemKind::Assistant, "The detector joins rows."),
            tool("t1", ChatToolKind::Command, "run", "cargo test links"),
        ],
        "",
    ));
    assert_eq!(shell.label("chat-item-u1"), "Find out why.");
    assert!(shell.shows("chat-item-a1"));
    assert!(shell.shows("chat-tool-t1"));
    assert!(shell.named("Context used: 209k / 1M"));
    assert!(shell.named("Model: Opus 5.5"));
    assert!(shell.named("Effort: High"));
    assert!(shell.named("Permissions: Auto"));
    assert!(shell.named("Send"));

    // An item that arrives, and text that streams into it.
    let mut reply = item("a2", ChatItemKind::Assistant, "It ");
    reply.streaming = true;
    shell.chat(event(11, ChatChange::Item(reply)));
    shell.chat(delta(12, "a2", "works."));
    assert!(shell.shows("chat-item-a2"));
    assert_eq!(shell.host.actions(), [ChatAction::Open]);
}

#[gpui_kit::test]
fn a_lost_change_a_full_queue_and_a_restored_link_open_the_chat_again(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    // A gap in the revisions.
    shell.chat(delta(14, "a1", "lost"));
    assert_eq!(shell.host.count(ChatAction::Open), 2);
    // What still arrives before the snapshot is behind it too: one `OPEN`.
    shell.chat(delta(15, "a1", "lost"));
    assert_eq!(shell.host.count(ChatAction::Open), 2);
    shell.chat(snapshot(
        20,
        vec![item(
            "a1",
            ChatItemKind::Assistant,
            "The detector joins rows.",
        )],
        "",
    ));
    // The host could not queue the snapshot: asked again after a moment.
    shell.host.refuse_next("chat_queue_full: The chat is busy.");
    shell.chat(delta(30, "a1", "lost"));
    assert_eq!(shell.host.count(ChatAction::Open), 3);
    shell
        .visual
        .executor()
        .advance_clock(std::time::Duration::from_millis(300));
    shell.settle();
    assert_eq!(shell.host.count(ChatAction::Open), 4);
    assert!(!shell.shows("chat-open-error"));
    // The connection resynced: events may have been dropped with nothing
    // later to show it.
    shell.publish(vec![frame(6, "resyncing", &[]), frame(6, "connected", &[])]);
    assert_eq!(shell.host.count(ChatAction::Open), 5);
}

#[gpui_kit::test]
fn a_conversation_the_host_will_not_send_again_says_it_may_be_behind(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    shell
        .host
        .refuse_next("chat_unavailable: This pane has no chat to open.");
    shell.chat(delta(14, "a1", "lost"));
    assert_eq!(
        shell.label("chat-open-error"),
        "This conversation may be out of date. This pane has no chat to open."
    );
    assert!(shell.shows("chat-item-u1"), "what is held stays readable");
    shell.click("chat-open-retry");
    assert_eq!(shell.host.count(ChatAction::Open), 3);
    assert!(!shell.shows("chat-open-error"));
}

#[gpui_kit::test]
fn a_message_shows_at_once_and_is_sent_under_its_own_id(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    shell.input("Only look at boxed output.");
    shell.press("enter");
    let sent = shell.host.last(ChatAction::Send);
    assert_eq!(sent.text, "Only look at boxed output.");
    assert!(!sent.client_message_id.is_empty());
    let pending = format!("chat-sending-{}", sent.client_message_id);
    assert_eq!(shell.label(&pending), "Only look at boxed output.");

    // The host's item takes the pending bubble's place.
    let arrived = item(
        &sent.client_message_id,
        ChatItemKind::User,
        "Only look at boxed output.",
    );
    shell.chat(event(11, ChatChange::Item(arrived)));
    assert!(!shell.shows(&pending));
    assert!(shell.shows(&format!("chat-item-{}", sent.client_message_id)));

    // A refused message comes back to the composer, with the reason.
    shell
        .host
        .refuse_next("chat_agent_gone: The agent has exited.");
    shell.input("Still there?");
    shell.press("enter");
    assert_eq!(shell.label("chat-notice"), "The agent has exited.");
    assert_eq!(shell.host.count(ChatAction::Send), 2);
    shell.press("enter");
    let again = shell.host.last(ChatAction::Send);
    assert_eq!(again.text, "Still there?");
    assert_eq!(shell.host.count(ChatAction::Send), 3);
}

#[gpui_kit::test]
fn editing_an_earlier_message_rewinds_to_it_before_sending(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    // The button shows while the pointer is on the message.
    shell.hover("chat-item-u1");
    shell.click("chat-edit-u1");
    assert!(shell.named("Editing an earlier message"));
    shell.input(" Only boxed output.");
    shell.press("enter");
    let rewind = shell.host.last(ChatAction::Rewind);
    assert_eq!(rewind.item_id, "u1");
    let sent = shell.host.last(ChatAction::Send);
    assert_eq!(
        sent.text,
        "Find out why wrapped paths are missed. Only boxed output."
    );
    let actions = shell.host.actions();
    assert_eq!(
        actions[actions.len() - 2..],
        [ChatAction::Rewind, ChatAction::Send]
    );
    assert!(!shell.named("Editing an earlier message"));

    // Cancel leaves the conversation alone.
    shell.hover("chat-item-u1");
    shell.click("chat-edit-u1");
    shell.click("chat-edit-cancel");
    assert!(!shell.named("Editing an earlier message"));
    shell.press("enter");
    assert_eq!(shell.host.count(ChatAction::Rewind), 1);
}

#[gpui_kit::test]
fn stop_takes_sends_place_while_the_agent_works(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    shell.chat(event(11, ChatChange::State(state(ChatTurnState::Working))));
    assert!(shell.named("Stop"));
    assert!(!shell.named("Send"));
    shell.click("chat-send");
    assert_eq!(shell.host.count(ChatAction::Interrupt), 1);
    // Enter still sends while it works.
    shell.input("Also the fixture.");
    shell.press("enter");
    assert_eq!(shell.host.last(ChatAction::Send).text, "Also the fixture.");
}

#[gpui_kit::test]
fn escape_stops_the_agent_while_it_works(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    shell.chat(event(11, ChatChange::State(state(ChatTurnState::Working))));
    shell.press("escape");
    assert_eq!(shell.host.count(ChatAction::Interrupt), 1);
    // From a control elsewhere in the chat too: the earlier message's Edit.
    shell.press("shift-tab");
    shell.press("escape");
    assert_eq!(shell.host.count(ChatAction::Interrupt), 2);
    assert!(!shell.named("Editing an earlier message"));

    // A menu open in the chat has Escape to itself.
    shell.click("chat-model");
    assert!(shell.named("Fable 5.2"));
    shell.press("escape");
    assert!(!shell.named("Fable 5.2"));
    assert_eq!(shell.host.count(ChatAction::Interrupt), 2);
}

#[gpui_kit::test]
fn escape_closes_the_command_list_before_it_stops_anything(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    shell.chat(event(11, ChatChange::State(state(ChatTurnState::Working))));
    shell.input("/c");
    assert!(shell.named("Commands"));
    shell.press("escape");
    assert!(!shell.named("Commands"));
    assert_eq!(shell.host.count(ChatAction::Interrupt), 0);
    // The next one is the agent's.
    shell.press("escape");
    assert_eq!(shell.host.count(ChatAction::Interrupt), 1);
}

#[gpui_kit::test]
fn escape_cancels_an_edit_before_it_stops_anything(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    shell.chat(event(11, ChatChange::State(state(ChatTurnState::Working))));
    shell.hover("chat-item-u1");
    shell.click("chat-edit-u1");
    assert!(shell.named("Editing an earlier message"));
    shell.press("escape");
    assert!(!shell.named("Editing an earlier message"));
    assert_eq!(shell.host.count(ChatAction::Interrupt), 0);
    assert_eq!(shell.host.count(ChatAction::Rewind), 0);
}

#[gpui_kit::test]
fn escape_stops_nothing_while_the_agent_is_idle_or_waits_for_an_answer(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    shell.press("escape");
    shell.chat(event(11, ChatChange::State(permission("r1"))));
    assert!(shell.shows("chat-request"));
    shell.press("escape");
    assert_eq!(shell.host.actions(), [ChatAction::Open]);
}

#[gpui_kit::test]
fn typing_or_pressing_in_a_chat_says_its_agent_has_been_seen(cx: &mut TestAppContext) {
    use std::{cell::RefCell, rc::Rc};

    use crate::terminal::integration::agents::PaneActivity;

    let mut shell = open(cx);
    shell.visual.update(|window, _| window.activate_window());
    shell.settle();
    let noted = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
    shell.visual.update(|_, cx| {
        let noted = noted.clone();
        cx.set_global(PaneActivity(Rc::new(move |host, pane, _| {
            noted.borrow_mut().push((host.to_owned(), pane.to_owned()))
        })));
    });
    let this_pane = ("local".to_owned(), PANE.to_owned());
    let seen = |noted: &Rc<RefCell<Vec<(String, String)>>>| {
        let mut noted = noted.borrow_mut();
        let all = noted.iter().all(|note| *note == this_pane);
        let any = !noted.is_empty();
        noted.clear();
        any && all
    };

    shell.input("Still");
    assert!(seen(&noted), "typing in the composer");
    // A key that types nothing, on a control that is not the composer.
    shell.press("shift-tab");
    assert!(seen(&noted), "a key elsewhere in the chat");
    shell.click("chat-item-a1");
    assert!(seen(&noted), "a press on the conversation");
    shell
        .visual
        .update(|window, cx| window.right_click("chat-tool-t1", cx));
    shell.settle();
    assert!(seen(&noted), "any button");
}

#[gpui_kit::test]
fn a_permission_request_is_answered_from_its_card(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    let mut waiting = state(ChatTurnState::Waiting);
    waiting.pending = Some(ChatPendingRequest {
        request_id: "r1".into(),
        kind: ChatRequestKind::Permission.into(),
        title: "Auto mode is asking: run this command?".into(),
        detail: "git push --force origin gpui/chat".into(),
        questions: Vec::new(),
    });
    shell.chat(event(11, ChatChange::State(waiting)));
    assert!(shell.named("Auto mode is asking: run this command?"));
    shell.click("chat-deny");
    let answer = shell.host.last(ChatAction::Answer);
    assert_eq!(answer.request_id, "r1");
    assert!(!answer.answer.unwrap().allow);

    // The card goes when the host says the turn moved on.
    shell.chat(event(12, ChatChange::State(state(ChatTurnState::Working))));
    assert!(!shell.shows("chat-request"));
}

#[gpui_kit::test]
fn questions_are_answered_one_at_a_time(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    let option = |id: &str, label: &str| ChatOption {
        id: id.into(),
        label: label.into(),
        description: "What it means.".into(),
    };
    let mut waiting = state(ChatTurnState::Waiting);
    waiting.pending = Some(ChatPendingRequest {
        request_id: "r2".into(),
        kind: ChatRequestKind::Question.into(),
        questions: vec![
            ChatQuestion {
                id: "scope".into(),
                header: "Scope".into(),
                prompt: "How should the fix be scoped?".into(),
                options: vec![
                    option("boxed", "Boxed output only"),
                    option("any", "Any path"),
                ],
                multi_select: false,
                allow_other: true,
            },
            ChatQuestion {
                id: "checks".into(),
                header: "Checks".into(),
                prompt: "Which checks should run?".into(),
                options: vec![option("unit", "Unit tests"), option("lint", "Clippy")],
                multi_select: true,
                allow_other: false,
            },
        ],
        ..Default::default()
    });
    shell.chat(event(11, ChatChange::State(waiting)));
    assert!(shell.named("How should the fix be scoped?"));
    // Nothing picked: nothing to submit.
    shell.click("chat-submit");
    assert_eq!(shell.host.count(ChatAction::Answer), 0);
    shell.click("chat-option-0");
    shell.click("chat-option-1");
    shell.click("chat-submit");
    assert!(shell.named("Which checks should run?"));
    assert_eq!(shell.host.count(ChatAction::Answer), 0);
    shell.click("chat-option-0");
    shell.click("chat-option-1");
    shell.click("chat-submit");
    let answer = shell.host.last(ChatAction::Answer);
    assert_eq!(answer.request_id, "r2");
    let answers = answer.answer.unwrap().answers;
    assert_eq!(answers.len(), 2);
    assert_eq!(answers[0].question_id, "scope");
    // One choice: the second pick replaced the first.
    assert_eq!(answers[0].option_ids, ["any"]);
    assert_eq!(answers[1].question_id, "checks");
    assert_eq!(answers[1].option_ids, ["unit", "lint"]);
}

#[gpui_kit::test]
fn a_question_can_be_skipped_or_answered_in_other_words(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    let mut waiting = state(ChatTurnState::Waiting);
    waiting.pending = Some(ChatPendingRequest {
        request_id: "r3".into(),
        kind: ChatRequestKind::Question.into(),
        questions: vec![ChatQuestion {
            id: "scope".into(),
            prompt: "How should the fix be scoped?".into(),
            options: vec![ChatOption {
                id: "boxed".into(),
                label: "Boxed output only".into(),
                description: String::new(),
            }],
            allow_other: true,
            ..Default::default()
        }],
        ..Default::default()
    });
    shell.chat(event(11, ChatChange::State(waiting.clone())));
    shell.click("chat-option-other");
    shell.input("Only the tests");
    shell.click("chat-submit");
    let answer = shell.host.last(ChatAction::Answer).answer.unwrap();
    assert!(answer.answers[0].option_ids.is_empty());
    assert_eq!(answer.answers[0].other_text, "Only the tests");
    assert!(!answer.skipped);

    // The card goes and its field with it: the keyboard is the composer's.
    shell.chat(event(12, ChatChange::State(state(ChatTurnState::Idle))));
    shell.input("Thanks.");
    shell.press("enter");
    assert_eq!(shell.host.last(ChatAction::Send).text, "Thanks.");

    // Asked again, and skipped this time.
    waiting.pending.as_mut().unwrap().request_id = "r4".into();
    shell.chat(event(13, ChatChange::State(waiting)));
    shell.click("chat-skip");
    let skipped = shell.host.last(ChatAction::Answer);
    assert_eq!(skipped.request_id, "r4");
    assert!(skipped.answer.unwrap().skipped);
}

#[gpui_kit::test]
fn a_slash_lists_the_providers_commands_and_enter_takes_one(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    shell.input("/c");
    assert!(shell.named("Commands"));
    assert!(shell.named("/compact"));
    assert!(shell.named("/clear"));
    assert!(shell.named("/code-review"));
    assert!(!shell.named("/btw"), "nothing in it matches");
    shell.press("down");
    shell.press("enter");
    // Taken into the composer, not sent.
    assert_eq!(shell.host.count(ChatAction::Send), 0);
    assert!(!shell.named("Commands"));
    shell.press("enter");
    assert_eq!(shell.host.last(ChatAction::Send).text, "/clear");

    // Escape closes the list and leaves the text.
    shell.input("/co");
    assert!(shell.named("Commands"));
    shell.press("escape");
    assert!(!shell.named("Commands"));
    shell.press("enter");
    assert_eq!(shell.host.last(ChatAction::Send).text, "/co");
}

#[gpui_kit::test]
fn a_dollar_lists_the_skills_in_the_middle_of_a_message(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    shell.input("look at this with $co");
    assert!(shell.named("$code-review"));
    assert!(!shell.named("$compact"), "a command is not a skill");
    shell.press("enter");
    // Taken into the message, not sent.
    assert_eq!(shell.host.count(ChatAction::Send), 0);
    assert!(!shell.named("$code-review"));
    shell.press("enter");
    assert_eq!(
        shell.host.last(ChatAction::Send).text,
        "look at this with $code-review"
    );
}

#[gpui_kit::test]
fn a_side_question_forks_instead_of_sending(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    shell.input("/btw which file holds the join?");
    shell.press("enter");
    assert_eq!(shell.host.count(ChatAction::Send), 0);
    let fork = shell.host.last(ChatAction::SideFork);
    assert_eq!(fork.text, "which file holds the join?");

    // Refused: the question is back in the composer, with the reason.
    shell
        .host
        .refuse_next("chat_side_fork_unavailable: A fork cannot be made yet.");
    shell.input("/btw and the tests?");
    shell.press("enter");
    assert_eq!(shell.label("chat-notice"), "A fork cannot be made yet.");
    shell.press("enter");
    assert_eq!(shell.host.count(ChatAction::SideFork), 3);
    assert_eq!(shell.host.last(ChatAction::SideFork).text, "and the tests?");
}

#[gpui_kit::test]
fn earlier_messages_load_when_the_top_shows(cx: &mut TestAppContext) {
    let mut shell = shell(cx);
    shell.host.state().chat_answers.push_back(Ok(page(
        10,
        vec![item("u0", ChatItemKind::User, "Before that.")],
        "",
    )));
    // A short conversation: its top is on screen at once.
    shell.chat(snapshot(
        10,
        vec![item("u1", ChatItemKind::User, "Find out why.")],
        "older-1",
    ));
    let asked = shell.host.last(ChatAction::LoadOlder);
    assert_eq!(asked.older_cursor, "older-1");
    assert_eq!(shell.label("chat-item-u0"), "Before that.");
    assert_eq!(shell.host.count(ChatAction::LoadOlder), 1, "nothing older");

    // A page that fails is offered again, not asked for in a loop.
    shell.host.refuse_next("chat_history_unavailable: Not now.");
    shell.chat(snapshot(
        20,
        vec![item("u1", ChatItemKind::User, "Find out why.")],
        "older-9",
    ));
    assert_eq!(shell.host.count(ChatAction::LoadOlder), 2);
    assert!(shell.named("Load earlier messages"));
    shell.click("chat-load-older");
    assert_eq!(shell.host.count(ChatAction::LoadOlder), 3);
}

#[gpui_kit::test]
fn a_tool_row_opens_to_show_its_output(cx: &mut TestAppContext) {
    let mut shell = shell(cx);
    let mut run = tool("t1", ChatToolKind::Command, "run", "cargo test links");
    run.tool.as_mut().unwrap().output_preview = "test result: ok. 14 passed".into();
    let read = tool("t2", ChatToolKind::Read, "read", "src/links.rs");
    shell.chat(snapshot(10, vec![run, read], ""));
    assert!(!shell.shows("chat-tool-output-t1"));
    shell.click("chat-tool-open-t1");
    assert!(shell.shows("chat-tool-output-t1"));
    shell.click("chat-tool-open-t1");
    assert!(!shell.shows("chat-tool-output-t1"));
    // Nothing to show: not a control.
    assert!(!shell.shows("chat-tool-open-t2"));
}

#[gpui_kit::test]
fn inline_code_is_asked_about_once_the_text_is_whole(cx: &mut TestAppContext) {
    let mut shell = shell(cx);
    shell.host.state().existing.push("src/links.rs".into());
    let mut reply = item(
        "a1",
        ChatItemKind::Assistant,
        "It is in `src/links.rs`, not `src/gone.rs`. Run `cargo test`.",
    );
    reply.streaming = true;
    shell.chat(snapshot(10, vec![reply.clone()], ""));
    assert!(shell.host.state().files.is_empty(), "still being written");
    reply.streaming = false;
    shell.chat(event(11, ChatChange::Updated(reply)));
    // Each path once, one at a time; a command is not a path.
    assert_eq!(shell.host.state().files, ["src/links.rs", "src/gone.rs"]);
    shell.settle();
    assert_eq!(shell.host.state().files.len(), 2);
}

#[gpui_kit::test]
fn a_chat_is_closed_when_its_pane_goes(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    assert_eq!(shell.host.count(ChatAction::Close), 0);
    shell.publish(vec![topology(
        2,
        vec![Pane {
            active: true,
            ..pane("%2", "@1", "$1", (0, 0, 100, 24))
        }],
    )]);
    let closed = shell.host.last(ChatAction::Close);
    assert_eq!(closed.pane_id, PANE);
    assert!(!shell.named("Chat"));
}

#[gpui_kit::test]
fn the_hosts_agent_mode_follows_its_snapshot_and_is_changed_by_request(cx: &mut TestAppContext) {
    let mut shell = shell(cx);
    let mode = |shell: &mut Shell| shell.visual.update(|_, cx| settings::agent_mode(cx));
    assert_eq!(
        mode(&mut shell).chat,
        Some(false),
        "the handshake supplied the host mode"
    );
    let controller = shell.controller.clone();
    shell.visual.update(|_, cx| {
        controller.update(cx, |_, cx| {
            cx.emit(ConnectionEvent::AgentService(AgentServiceFrame {
                profile_id: "local".into(),
                server_identity: "tmux".into(),
                terminal_epoch: 1,
                scope: "snapshot".into(),
                payload: json!({ "agentMode": "terminal" }).to_string(),
            }));
        });
    });
    shell.settle();
    assert_eq!(mode(&mut shell).chat, Some(false));

    shell.host.state().chat_answers.push_back(Ok(ChatResponse {
        mode: ChatAgentMode::Chat.into(),
        ..Default::default()
    }));
    shell.visual.update(|_, cx| super::set_agent_mode(true, cx));
    shell.settle();
    let request = shell.host.last(ChatAction::SetMode);
    assert_eq!(request.mode, i32::from(ChatAgentMode::Chat));
    assert_eq!(
        mode(&mut shell),
        settings::AgentMode {
            chat: Some(true),
            changing: false
        }
    );

    // A refusal leaves what the host last said.
    shell.host.refuse_next("chat_invalid: Not now.");
    shell
        .visual
        .update(|_, cx| super::set_agent_mode(false, cx));
    shell.settle();
    assert_eq!(mode(&mut shell).chat, Some(true));
}

fn permission(request_id: &str) -> ChatState {
    let mut waiting = state(ChatTurnState::Waiting);
    waiting.pending = Some(ChatPendingRequest {
        request_id: request_id.into(),
        kind: ChatRequestKind::Permission.into(),
        title: "Run this command?".into(),
        detail: "cargo test".into(),
        questions: Vec::new(),
    });
    waiting
}

#[gpui_kit::test]
fn an_answer_goes_once_and_to_the_request_its_card_showed(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    shell.chat(event(11, ChatChange::State(permission("r1"))));
    shell.host.hold_next();
    shell.click("chat-allow");
    assert_eq!(shell.host.last(ChatAction::Answer).request_id, "r1");
    // Still on its way: what else the chat does meanwhile sends no second one.
    shell.chat(delta(12, "a1", " More."));
    shell.click("chat-allow");
    shell.click("chat-deny");
    assert_eq!(shell.host.count(ChatAction::Answer), 1);

    // Another request takes the card's place before the answer is back.
    shell.chat(event(13, ChatChange::State(permission("r2"))));
    shell.host.release();
    shell.settle();
    shell.click("chat-deny");
    let second = shell.host.last(ChatAction::Answer);
    assert_eq!(second.request_id, "r2");
    assert!(!second.answer.unwrap().allow);
    assert_eq!(shell.host.count(ChatAction::Answer), 2);
}

#[gpui_kit::test]
fn a_side_question_forks_while_a_provider_that_takes_no_message_works(cx: &mut TestAppContext) {
    let mut shell = shell(cx);
    let mut opened = snapshot_in(10, state(ChatTurnState::Working), Vec::new(), "");
    if let Some(ChatChange::Snapshot(snapshot)) = &mut opened.change {
        let catalog = snapshot.catalog.as_mut().unwrap();
        catalog.capabilities.as_mut().unwrap().send_mid_turn = false;
    }
    shell.chat(opened);
    shell.input("Also the fixture.");
    shell.press("enter");
    assert_eq!(shell.host.count(ChatAction::Send), 0);
    assert_eq!(
        shell.label("chat-notice"),
        "The agent is working. Stop it, or wait for it to finish."
    );
    for _ in 0.."Also the fixture.".len() {
        shell.press("backspace");
    }
    shell.input("/btw which file holds the join?");
    shell.press("enter");
    assert_eq!(
        shell.host.last(ChatAction::SideFork).text,
        "which file holds the join?"
    );
}

#[gpui_kit::test]
fn a_snapshot_that_beats_its_answer_leaves_the_next_lost_change_asked_about(
    cx: &mut TestAppContext,
) {
    let mut shell = open(cx);
    shell.host.hold_next();
    shell.chat(delta(14, "a1", "lost"));
    assert_eq!(shell.host.count(ChatAction::Open), 2);
    // The snapshot is an event: here before the answer to the `OPEN`.
    shell.chat(snapshot(
        20,
        vec![item(
            "a1",
            ChatItemKind::Assistant,
            "The detector joins rows.",
        )],
        "",
    ));
    // A change lost from here on is asked about, answer or no answer.
    shell.chat(delta(30, "a1", "lost"));
    assert_eq!(shell.host.count(ChatAction::Open), 3);
    shell.host.release();
    shell.settle();
    assert_eq!(shell.host.count(ChatAction::Open), 3);
}

#[gpui_kit::test]
fn an_edit_is_sent_even_if_its_pane_leaves_while_the_host_rewinds(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    shell.hover("chat-item-u1");
    shell.click("chat-edit-u1");
    shell.input(" Only boxed output.");
    shell.host.hold_next();
    shell.press("enter");
    assert_eq!(shell.host.count(ChatAction::Send), 0, "the rewind first");
    shell.publish(vec![topology(
        2,
        vec![Pane {
            active: true,
            ..pane("%2", "@1", "$1", (0, 0, 100, 24))
        }],
    )]);
    assert!(!shell.named("Chat"));
    shell.host.release();
    shell.settle();
    assert_eq!(
        shell.host.last(ChatAction::Send).text,
        "Find out why wrapped paths are missed. Only boxed output."
    );
}

#[gpui_kit::test]
fn shift_enter_makes_a_line_and_enter_sends_them_all(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    shell.input("First line");
    shell.press("shift-enter");
    assert_eq!(shell.host.count(ChatAction::Send), 0);
    shell.input("second line");
    shell.press("enter");
    assert_eq!(
        shell.host.last(ChatAction::Send).text,
        "First line\nsecond line"
    );
}

#[gpui_kit::test]
fn a_notice_goes_with_the_text_it_was_about(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    shell.host.refuse_next("The connection was lost.");
    shell.input("No");
    shell.press("enter");
    assert!(shell.shows("chat-notice"));
    shell.press("backspace");
    assert!(shell.shows("chat-notice"));
    shell.press("backspace");
    assert!(!shell.shows("chat-notice"));
}

#[gpui_kit::test]
fn the_wheel_over_long_output_moves_the_output_and_not_the_conversation(cx: &mut TestAppContext) {
    let mut shell = shell(cx);
    let mut items: Vec<ChatItem> = (0..40)
        .map(|number| {
            item(
                &format!("a{number}"),
                ChatItemKind::Assistant,
                &format!("Earlier answer {number}."),
            )
        })
        .collect();
    let mut run = tool("t1", ChatToolKind::Command, "run", "cargo clippy");
    run.tool.as_mut().unwrap().output_preview = "warning: unused variable\n".repeat(80);
    items.push(run);
    shell.chat(snapshot(10, items, ""));
    shell.click("chat-tool-open-t1");
    let top = |shell: &mut Shell| {
        shell
            .visual
            .update(|window, _| window.find("chat-tool-open-t1").bounds().top())
    };
    let wheel = |shell: &mut Shell, lines: f32| {
        shell.visual.update(|window, cx| {
            let delta = ScrollDelta::Lines(point(0., lines));
            window.scroll("chat-tool-output-t1", delta, cx);
        });
        shell.settle();
    };
    wheel(&mut shell, -6.);
    let before = top(&mut shell);
    // The wheel over the output is the output's, at its start too.
    wheel(&mut shell, 2.);
    wheel(&mut shell, 8.);
    assert_eq!(top(&mut shell), before);
    // Over the row's head it is the conversation's.
    shell.visual.update(|window, cx| {
        let delta = ScrollDelta::Lines(point(0., 4.));
        window.scroll("chat-tool-open-t1", delta, cx);
    });
    shell.settle();
    assert!(top(&mut shell) > before);
}

#[gpui_kit::test]
fn a_refused_message_comes_back_above_what_was_typed_since(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    shell
        .host
        .refuse_next("chat_agent_gone: The agent has exited.");
    shell.host.hold_next();
    shell.input("First.");
    shell.press("enter");
    shell.input("Second.");
    shell.host.release();
    shell.settle();
    shell.press("enter");
    assert_eq!(shell.host.last(ChatAction::Send).text, "First.\nSecond.");
}

#[gpui_kit::test]
fn the_same_text_sent_again_keeps_its_id_and_a_duplicate_means_it_arrived(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    shell.host.refuse_next("The connection was lost.");
    shell.input("Only look at boxed output.");
    shell.press("enter");
    let first = shell.host.last(ChatAction::Send);
    assert_eq!(shell.label("chat-notice"), "The connection was lost.");

    // Sent again before anything else is known: the host had taken it.
    shell
        .host
        .refuse_next("chat_message_duplicate: That message was already sent.");
    shell.press("enter");
    assert_eq!(shell.host.count(ChatAction::Send), 2);
    let again = shell.host.last(ChatAction::Send);
    assert_eq!(again.client_message_id, first.client_message_id);
    assert!(!shell.shows("chat-notice"));
    let arrived = |id: &str, text: &str| ChatChange::Item(item(id, ChatItemKind::User, text));
    shell.chat(event(
        11,
        arrived(&first.client_message_id, "Only look at boxed output."),
    ));
    assert!(!shell.shows(&format!("chat-sending-{}", first.client_message_id)));
    assert!(shell.shows(&format!("chat-item-{}", first.client_message_id)));

    // A message that shows up while its text waits to be sent again takes
    // the text with it, and the same words later are a new message.
    shell.host.refuse_next("The connection was lost.");
    shell.input("Yes.");
    shell.press("enter");
    let lost = shell.host.last(ChatAction::Send);
    shell.chat(event(12, arrived(&lost.client_message_id, "Yes.")));
    assert!(!shell.shows("chat-notice"));
    shell.press("enter");
    assert_eq!(
        shell.host.count(ChatAction::Send),
        3,
        "nothing left to send"
    );
    shell.input("Yes.");
    shell.press("enter");
    assert_eq!(shell.host.count(ChatAction::Send), 4);
    assert_ne!(
        shell.host.last(ChatAction::Send).client_message_id,
        lost.client_message_id
    );
}

#[gpui_kit::test]
fn another_panes_or_hosts_chat_changes_nothing_here(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    let mut elsewhere = snapshot(
        40,
        vec![item("x1", ChatItemKind::User, "Not for this pane.")],
        "",
    );
    elsewhere.pane_id = "%9".into();
    shell.chat(elsewhere);
    let controller = shell.controller.clone();
    let other_host = snapshot(
        41,
        vec![item("x2", ChatItemKind::User, "Not for this host.")],
        "",
    );
    shell.visual.update(|_, cx| {
        controller.update(cx, |_, cx| {
            cx.emit(ConnectionEvent::Chat {
                profile_id: "other".into(),
                event: Box::new(other_host),
            });
        });
    });
    shell.settle();
    assert!(shell.shows("chat-item-u1"));
    assert!(!shell.shows("chat-item-x1"));
    assert!(!shell.shows("chat-item-x2"));
    assert_eq!(shell.host.count(ChatAction::Open), 1);
}

#[gpui_kit::test]
fn a_press_on_the_conversation_leaves_the_keyboard_with_the_composer(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    // Focus is only followed in the window that has the keyboard.
    shell.visual.update(|window, _| window.activate_window());
    shell.settle();
    shell.click("chat-item-u1");
    shell.settle();
    shell.input("Still");
    shell.click("chat-tool-t1");
    shell.settle();
    shell.input(" typing.");
    shell.press("enter");
    assert_eq!(shell.host.last(ChatAction::Send).text, "Still typing.");
}

#[gpui_kit::test]
fn the_keyboard_leaves_the_composer_for_the_controls_and_presses_them(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    // Back from the composer: the earlier message's Edit.
    shell.press("shift-tab");
    shell.press("enter");
    assert!(shell.named("Editing an earlier message"));
    shell.click("chat-edit-cancel");

    // A question's options take Space once the keyboard is on them.
    let mut waiting = state(ChatTurnState::Waiting);
    waiting.pending = Some(ChatPendingRequest {
        request_id: "r5".into(),
        kind: ChatRequestKind::Question.into(),
        questions: vec![ChatQuestion {
            id: "checks".into(),
            prompt: "Which checks should run?".into(),
            options: vec![ChatOption {
                id: "unit".into(),
                label: "Unit tests".into(),
                description: String::new(),
            }],
            multi_select: true,
            ..Default::default()
        }],
        ..Default::default()
    });
    shell.chat(event(11, ChatChange::State(waiting)));
    shell.click("chat-option-0");
    shell.press("space");
    shell.click("chat-submit");
    assert_eq!(shell.host.count(ChatAction::Answer), 0, "picked, then not");
    shell.click("chat-option-0");
    shell.press("space");
    shell.press("space");
    shell.click("chat-submit");
    let answer = shell.host.last(ChatAction::Answer).answer.unwrap();
    assert_eq!(answer.answers[0].option_ids, ["unit"]);
}

#[gpui_kit::test]
fn a_card_of_several_questions_goes_back_to_change_an_answer(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    let question = |id: &str, prompt: &str, options: [&str; 2]| ChatQuestion {
        id: id.into(),
        prompt: prompt.into(),
        options: options
            .iter()
            .map(|option| ChatOption {
                id: (*option).into(),
                label: (*option).into(),
                description: String::new(),
            })
            .collect(),
        ..Default::default()
    };
    let mut waiting = state(ChatTurnState::Waiting);
    waiting.pending = Some(ChatPendingRequest {
        request_id: "r6".into(),
        kind: ChatRequestKind::Question.into(),
        questions: vec![
            question("scope", "Which scope?", ["boxed", "any"]),
            question("checks", "Which checks?", ["unit", "lint"]),
        ],
        ..Default::default()
    });
    shell.chat(event(11, ChatChange::State(waiting)));
    assert!(!shell.shows("chat-back"), "nothing before the first");
    shell.click("chat-option-0");
    shell.click("chat-submit");
    shell.click("chat-back");
    assert!(shell.named("Which scope?"));
    shell.click("chat-option-1");
    shell.click("chat-submit");
    shell.click("chat-option-0");
    shell.click("chat-submit");
    let answer = shell.host.last(ChatAction::Answer).answer.unwrap();
    assert_eq!(answer.answers[0].option_ids, ["any"]);
    assert_eq!(answer.answers[1].option_ids, ["unit"]);
}

#[gpui_kit::test]
fn a_right_click_copies_what_cannot_be_selected(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    let copy = |shell: &mut Shell, id: &'static str, entry: &'static str| {
        shell.visual.update(|window, cx| window.right_click(id, cx));
        shell.settle();
        shell.visual.update(|window, cx| {
            let found = window.find_by_label(entry);
            let item = found.path().last().cloned().expect("a menu item");
            window.click(item, cx);
        });
        shell.settle();
        shell
            .visual
            .read_from_clipboard()
            .and_then(|item| item.text())
    };
    assert_eq!(
        copy(&mut shell, "chat-item-u1", "Copy message").as_deref(),
        Some("Find out why wrapped paths are missed.")
    );
    assert_eq!(
        copy(&mut shell, "chat-tool-t1", "Copy").as_deref(),
        Some("cargo test links")
    );
}

#[gpui_kit::test]
fn a_narrow_chat_shows_its_details_only_from_the_details_button(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    assert!(shell.shows("chat-details-toggle"));
    assert!(!shell.shows("chat-details-card"));
    shell.click("chat-details-toggle");
    assert!(shell.named("Session details"));
    assert_eq!(shell.label("chat-details-folder"), "Folder: /work/repo");
    assert_eq!(
        shell.label("chat-details-context"),
        "Context used: 209k / 1M"
    );
    // No repository is observed here, so no Git row is offered.
    assert!(!shell.shows("chat-details-branch"));
    shell.click("chat-details-toggle");
    assert!(!shell.shows("chat-details-card"));
}

#[gpui_kit::test]
fn a_wide_chat_shows_its_details_beside_the_conversation(cx: &mut TestAppContext) {
    let mut shell = open(cx);
    shell.visual.simulate_resize(size(px(3000.), px(800.)));
    shell.settle();
    assert!(shell.shows("chat-details-card"));
    assert!(!shell.shows("chat-details-toggle"));
}

#[gpui_kit::test]
fn only_a_successful_explicit_choice_becomes_the_default_for_new_hosts(cx: &mut TestAppContext) {
    let mut shell = shell(cx);
    shell
        .visual
        .update(|_, cx| assert!(!settings::get(cx).new_host_chat));
    shell.host.state().chat_answers.push_back(Ok(ChatResponse {
        mode: ChatAgentMode::Chat.into(),
        ..Default::default()
    }));
    shell.visual.update(|_, cx| super::set_agent_mode(true, cx));
    shell.settle();
    shell
        .visual
        .update(|_, cx| assert!(settings::get(cx).new_host_chat));
    shell.host.refuse_next("could not save host settings");
    shell
        .visual
        .update(|_, cx| super::set_agent_mode(false, cx));
    shell.settle();
    shell
        .visual
        .update(|_, cx| assert!(settings::get(cx).new_host_chat));
}

#[gpui_kit::test]
fn a_stale_explicit_mode_reply_cannot_change_the_remembered_preference(cx: &mut TestAppContext) {
    let mut shell = shell(cx);
    shell.host.state().chat_answers.push_back(Ok(ChatResponse {
        mode: ChatAgentMode::Chat.into(),
        ..Default::default()
    }));
    shell.host.hold_next();
    shell.visual.update(|_, cx| super::set_agent_mode(true, cx));
    shell.publish(vec![frame(6, "disconnected", &[])]);
    shell.host.release();
    shell.settle();
    shell
        .visual
        .update(|_, cx| assert!(!settings::get(cx).new_host_chat));
    shell.publish(vec![frame(6, "connected", &[])]);
    shell
        .visual
        .update(|_, cx| assert_eq!(settings::agent_mode(cx).chat, Some(false)));
}

#[gpui_kit::test]
fn applying_to_all_hosts_reports_offline_hosts_without_queuing_changes(cx: &mut TestAppContext) {
    let mut shell = shell(cx);
    let messages = Arc::new(Mutex::new(Vec::new()));
    let writer = messages.clone();
    let controller = shell.controller.clone();
    shell.visual.update(|_, cx| {
        cx.subscribe(&controller, move |_, event, _| {
            if let ConnectionEvent::Status(message) = event {
                writer.lock().unwrap().push(message.clone());
            }
        })
        .detach();
    });
    shell.host.state().chat_answers.push_back(Ok(ChatResponse {
        mode: ChatAgentMode::Chat.into(),
        ..Default::default()
    }));
    shell.visual.update(|_, cx| {
        super::apply_to_hosts(
            true,
            vec![
                ("local".into(), "Local".into()),
                ("offline".into(), "Offline host".into()),
            ],
            controller,
            cx,
        )
    });
    shell.settle();
    assert_eq!(shell.host.count(ChatAction::SetMode), 1);
    assert!(
        messages
            .lock()
            .unwrap()
            .iter()
            .any(|message| message.contains("Applied Chat to 1 hosts")
                && message.contains("Offline host: not connected"))
    );
    shell
        .visual
        .update(|_, cx| assert!(settings::get(cx).new_host_chat));
}

#[gpui_kit::test]
fn applying_to_all_hosts_keeps_success_in_settings(cx: &mut TestAppContext) {
    let mut shell = shell(cx);
    let messages = Arc::new(Mutex::new(Vec::new()));
    let writer = messages.clone();
    let controller = shell.controller.clone();
    shell.visual.update(|_, cx| {
        cx.subscribe(&controller, move |_, event, _| {
            if let ConnectionEvent::Status(message) = event {
                writer.lock().unwrap().push(message.clone());
            }
        })
        .detach();
    });
    shell.host.state().chat_answers.push_back(Ok(ChatResponse {
        mode: ChatAgentMode::Chat.into(),
        ..Default::default()
    }));
    shell.visual.update(|_, cx| {
        super::apply_to_hosts(true, vec![("local".into(), "Local".into())], controller, cx)
    });
    shell.settle();
    shell
        .visual
        .update(|_, cx| assert_eq!(super::apply_result(cx), Some("Applied Chat to 1 hosts.")));
    assert!(
        !messages
            .lock()
            .unwrap()
            .iter()
            .any(|message| message.starts_with("Applied "))
    );
}

#[gpui_kit::test]
fn choosing_chat_on_local_initializes_the_next_new_remote_as_chat(cx: &mut TestAppContext) {
    let mut shell = shell(cx);
    shell.host.state().chat_answers.push_back(Ok(ChatResponse {
        mode: ChatAgentMode::Chat.into(),
        ..Default::default()
    }));
    shell.visual.update(|_, cx| super::set_agent_mode(true, cx));
    shell.settle();
    let controller = shell.controller.clone();
    shell.visual.update(|_, cx| {
        controller.update(cx, |controller, cx| {
            controller.add_host(cx);
            controller.set_ssh_target("new-qa-host".into(), cx);
            controller.connect(cx);
        })
    });
    shell.settle();
    let sink = shell.host.state().sinks.last().unwrap().clone();
    sink.send(EventBody::Raw(frame(6, "connected", &[])))
        .unwrap();
    shell.settle();
    let state = shell.host.state();
    assert!(state.mode_initializations.len() >= 2);
    assert_eq!(
        state.mode_initializations.last().unwrap().mode,
        i32::from(ChatAgentMode::Chat)
    );
}

#[gpui_kit::test]
fn agent_radios_remember_selected_keyboard_activation_without_duplicating_changes(
    cx: &mut TestAppContext,
) {
    let mut shell = shell(cx);
    let controller = shell.controller.clone();
    shell.visual.update(|window, cx| {
        settings::dialog::open_with(settings::dialog::Tab::Agents, Some(controller), window, cx)
    });
    shell.settle();
    fn focus_radio(shell: &mut Shell, index: usize) {
        for _ in 0..30 {
            if shell.visual.update(|window, _| {
                window.within("settings-agent-mode").find(index).focused() == Some(true)
            }) {
                return;
            }
            shell.visual.update(|window, cx| window.focus_next(cx));
            shell.settle();
        }
        panic!("radio did not receive focus");
    }
    focus_radio(&mut shell, 0);
    for key in ["space", "enter"] {
        shell.host.state().chat_answers.push_back(Ok(ChatResponse {
            mode: ChatAgentMode::Terminal.into(),
            ..Default::default()
        }));
        let before = shell.host.count(ChatAction::SetMode);
        shell.press(key);
        assert_eq!(shell.host.count(ChatAction::SetMode), before + 1);
        assert_eq!(
            shell.host.last(ChatAction::SetMode).mode,
            i32::from(ChatAgentMode::Terminal)
        );
    }
    focus_radio(&mut shell, 1);
    shell.host.state().chat_answers.push_back(Ok(ChatResponse {
        mode: ChatAgentMode::Chat.into(),
        ..Default::default()
    }));
    let before = shell.host.count(ChatAction::SetMode);
    shell.press("space");
    assert_eq!(shell.host.count(ChatAction::SetMode), before + 1);
    assert_eq!(
        shell.host.last(ChatAction::SetMode).mode,
        i32::from(ChatAgentMode::Chat)
    );
    shell
        .visual
        .update(|_, cx| assert!(settings::get(cx).new_host_chat));
    // Focus leaves the radio group. Activating Apply only opens its confirmation.
    shell.visual.update(|window, cx| window.focus_next(cx));
    shell.settle();
    shell.press("space");
    assert_eq!(shell.host.count(ChatAction::SetMode), before + 1);
}

#[gpui_kit::test]
fn replies_from_different_hosts_do_not_reverse_the_latest_explicit_preference(
    cx: &mut TestAppContext,
) {
    let mut shell = shell(cx);
    shell.host.state().chat_answers.push_back(Ok(ChatResponse {
        mode: ChatAgentMode::Chat.into(),
        ..Default::default()
    }));
    shell.host.hold_next();
    shell.visual.update(|_, cx| super::set_agent_mode(true, cx));
    let controller = shell.controller.clone();
    shell.visual.update(|_, cx| {
        controller.update(cx, |controller, cx| {
            controller.add_host(cx);
            controller.set_ssh_target("another-qa-host".into(), cx);
            controller.connect(cx);
        })
    });
    shell.settle();
    let sink = shell.host.state().sinks.last().unwrap().clone();
    sink.send(EventBody::Raw(frame(6, "connected", &[])))
        .unwrap();
    shell.settle();
    shell.host.state().chat_answers.push_back(Ok(ChatResponse {
        mode: ChatAgentMode::Terminal.into(),
        ..Default::default()
    }));
    shell
        .visual
        .update(|_, cx| super::set_agent_mode(false, cx));
    shell.settle();
    shell.host.release();
    shell.settle();
    shell.visual.update(|_, cx| {
        assert_eq!(super::agent_mode("local", cx), Some(true));
        assert!(
            !settings::get(cx).new_host_chat,
            "the newer Terminal choice wins over the older delayed Chat reply"
        );
    });
}
