//! Every state of the chat design (`docs/design/chat-mock.html`), drawn in
//! the app's window against a host that answers from here. Each frame is
//! checked against the one before it, so a state that never drew fails.

use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use desktop_core::{
    app_state::{AppStateStore, PersistedAppState},
    connection::{
        ConnectionSpec, ProfileStore,
        chat::{
            ChatCapabilities, ChatCatalog, ChatChange, ChatCommand, ChatCommandKind,
            ChatConnection, ChatEvent, ChatItem, ChatItemKind, ChatModel, ChatOption, ChatOptions,
            ChatPendingRequest, ChatPermissionMode, ChatQuestion, ChatRequest, ChatRequestKind,
            ChatResponse, ChatSnapshot, ChatSpeed, ChatState, ChatTool, ChatToolKind,
            ChatToolStatus, ChatTurnState, ChatUsage,
        },
        helper::HelperInstallReport,
    },
    platform::{EventBody, EventSink},
};
use futures::future::{BoxFuture, FutureExt};
use gpui_kit::{
    AnyWindowHandle, AppContext, Entity, HeadlessAppContext, IntoElement, ParentElement, Styled,
    div,
    test::{TestQueryExt, TestWindowExt},
};
use muxflow_gpui::{
    chat,
    connection_ui::{
        ConnectionUi,
        backend::{ConnectionBackend, EngineChannel, TerminalBackend},
        controller::{AgentServiceFrame, ConnectionController, ConnectionEvent},
        helper_upgrade::RemoteHelperProbe,
    },
    theme,
};
use serde_json::{Value, json};
use terminal_engine::{EngineOutput, HubLimits};

const PANE: &str = "%1";

#[derive(Default)]
struct HostState {
    sinks: Vec<Arc<dyn EventSink>>,
    /// The next `OPEN` is refused with this.
    open_refusal: Option<String>,
}

#[derive(Clone, Default)]
struct Host(Arc<Mutex<HostState>>);

impl TerminalBackend for Host {
    fn spawn_engine(
        &self,
        outputs: std::sync::mpsc::Sender<EngineOutput>,
    ) -> Box<dyn EngineChannel> {
        Box::new(terminal_engine::spawn(HubLimits::default(), outputs))
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
        _: muxflow_gpui::terminal::integration::transfers::UploadRequest,
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
        _: ChatRequest,
    ) -> BoxFuture<'static, Result<ChatResponse, String>> {
        let refusal = self.0.lock().unwrap().open_refusal.take();
        async move { refusal.map_or(Ok(ChatResponse::default()), Err) }.boxed()
    }

    /// Every path but one that is named as missing is there.
    fn file_request(&self, _: String, command: Value) -> BoxFuture<'static, Result<Value, String>> {
        let missing = command["path"]
            .as_str()
            .is_some_and(|path| path.contains("gone"));
        async move {
            if missing {
                Err("file_not_found: No such file.".to_owned())
            } else {
                Ok(json!({}))
            }
        }
        .boxed()
    }
}

impl ConnectionBackend for Host {
    fn start_terminal(
        &self,
        _: String,
        _: ConnectionSpec,
        _: bool,
        sink: Arc<dyn EventSink>,
    ) -> Result<String, String> {
        let mut state = self.0.lock().unwrap();
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

/// One workspace whose window holds the agent's pane, a chat `chat` of the
/// window's 120 columns wide, and a shell in the rest.
fn topology(generation: u64, chat: u16) -> Vec<u8> {
    let pane = |id: &str, left: u16, width: u16, active: bool, adapter: &str| {
        json!({
            "id": id, "sessionId": "$1", "windowId": "@1", "index": 0, "active": active,
            "width": width, "height": 40, "left": left, "top": 0,
            "currentPath": "/home/dev/muxflow", "currentCommand": "claude", "panePid": 1,
            "startCommand": "", "title": "", "chatAdapterId": adapter,
        })
    };
    let panes = if chat < 120 {
        vec![
            pane(PANE, 0, chat, true, "claude"),
            pane("%2", chat + 1, 119 - chat, false, ""),
        ]
    } else {
        vec![pane(PANE, 0, 120, true, "claude")]
    };
    let body = json!({
        "sequence": 0, "generation": generation, "serverIdentity": "tmux",
        "authoritative": true,
        "snapshot": {
            "sessions": [{
                "id": "$1", "name": "muxflow", "windowCount": 1, "attachedClients": 0,
                "order": 0, "pinned": false,
            }],
            "windows": [{
                "id": "@1", "sessionId": "$1", "index": 0, "name": "Wrapped link bug",
                "active": true, "layout": "", "zoomed": false, "pinned": false,
            }],
            "panes": panes,
        },
    });
    frame(7, "snapshot", body.to_string().as_bytes())
}

fn item(id: &str, kind: ChatItemKind, text: &str) -> ChatItem {
    ChatItem {
        id: id.into(),
        kind: kind.into(),
        text: text.into(),
        rewindable: kind == ChatItemKind::User,
        ..Default::default()
    }
}

fn tool(id: &str, kind: ChatToolKind, title: &str, detail: &str, summary: &str) -> ChatItem {
    ChatItem {
        id: id.into(),
        kind: ChatItemKind::Tool.into(),
        tool: Some(ChatTool {
            kind: kind.into(),
            title: title.into(),
            detail: detail.into(),
            status: ChatToolStatus::Ok.into(),
            result_summary: summary.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn catalog() -> ChatCatalog {
    let command = |name: &str, description: &str, hint: &str, kind: ChatCommandKind| ChatCommand {
        name: name.into(),
        description: description.into(),
        argument_hint: hint.into(),
        kind: kind.into(),
    };
    let model = |id: &str, label: &str, efforts: &[&str], fast: bool| ChatModel {
        id: id.into(),
        label: label.into(),
        efforts: efforts.iter().map(|effort| (*effort).to_owned()).collect(),
        supports_fast: fast,
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
            model(
                "opus",
                "Opus 5.5",
                &["low", "medium", "high", "xhigh", "max"],
                true,
            ),
            model("fable", "Fable 5.2", &["low", "medium", "high"], false),
            model("sonnet", "Sonnet 5.5", &[], true),
        ],
        commands: vec![
            command(
                "compact",
                "Summarise the conversation to free context",
                "",
                ChatCommandKind::Command,
            ),
            command(
                "clear",
                "Start over in this session",
                "",
                ChatCommandKind::Command,
            ),
            command(
                "code-review",
                "Review the current diff for bugs",
                "[level] [target]",
                ChatCommandKind::Skill,
            ),
            command(
                "diagnose-crash",
                "Find out why a program crashed",
                "",
                ChatCommandKind::Skill,
            ),
            command(
                "btw",
                "Ask on the side, in a fork",
                "<question>",
                ChatCommandKind::Muxflow,
            ),
        ],
    }
}

fn state(turn: ChatTurnState, pending: Option<ChatPendingRequest>) -> ChatState {
    ChatState {
        pane_id: PANE.into(),
        adapter_id: "claude".into(),
        turn: turn.into(),
        connection: ChatConnection::Ready.into(),
        options: Some(ChatOptions {
            model: "opus".into(),
            effort: "high".into(),
            permission: ChatPermissionMode::Auto.into(),
            speed: ChatSpeed::Standard.into(),
        }),
        usage: Some(ChatUsage {
            used_tokens: 209_000,
            window_tokens: 1_000_000,
        }),
        pending,
        history_complete: true,
        ..Default::default()
    }
}

fn permission() -> ChatPendingRequest {
    ChatPendingRequest {
        request_id: "r1".into(),
        kind: ChatRequestKind::Permission.into(),
        title: "Auto mode is asking: edit this file?".into(),
        detail: "crates/terminal-interactions/src/links.rs".into(),
        questions: Vec::new(),
    }
}

fn questions() -> ChatPendingRequest {
    let option = |id: &str, label: &str, description: &str| ChatOption {
        id: id.into(),
        label: label.into(),
        description: description.into(),
    };
    ChatPendingRequest {
        request_id: "r2".into(),
        kind: ChatRequestKind::Question.into(),
        questions: vec![
            ChatQuestion {
                id: "scope".into(),
                header: "Scope".into(),
                prompt: "How should the wrapped-path fix be scoped?".into(),
                options: vec![
                    option(
                        "boxed",
                        "Boxed output only",
                        "Strip box borders before joining rows. Smallest change, no effect on plain output.",
                    ),
                    option(
                        "any",
                        "Any hard-wrapped path",
                        "Also join rows without a border when the next row continues a path. Catches more, with some risk of joining two separate paths.",
                    ),
                ],
                multi_select: false,
                allow_other: true,
            },
            ChatQuestion {
                id: "checks".into(),
                header: "Checks".into(),
                prompt: "Which checks should run before the change is done?".into(),
                options: vec![
                    option("unit", "Unit tests", "cargo test -p terminal-interactions"),
                    option("lint", "Clippy", "The workspace lints, warnings denied."),
                ],
                multi_select: true,
                allow_other: false,
            },
        ],
        ..Default::default()
    }
}

/// The conversation of the design's first mock.
fn conversation() -> Vec<ChatItem> {
    vec![
        item(
            "u1",
            ChatItemKind::User,
            "The link detector misses paths that wrap inside Claude's boxed output. Find out why.",
        ),
        item(
            "a1",
            ChatItemKind::Assistant,
            "The detector joins rows that the terminal marks as soft-wrapped. Boxed output is hard-wrapped by the agent, so it takes the second path, which needs a delimiter or an indented continuation.",
        ),
        tool(
            "t1",
            ChatToolKind::Read,
            "read",
            "crates/terminal-interactions/src/links.rs",
            "",
        ),
        tool(
            "t2",
            ChatToolKind::Command,
            "run",
            "cargo test -p terminal-interactions links",
            "14 passed",
        ),
        item(
            "a2",
            ChatItemKind::Assistant,
            "A path that breaks right after a `/` inside a box border is dropped, because the border character ends the continuation. I can add that case and a fixture for it.\n\nThe join is in `crates/terminal-interactions/src/links.rs`, and the plan in [the design notes](docs/design/chat-plan.md). There is no `src/gone.rs`.",
        ),
    ]
}

struct Stage {
    cx: HeadlessAppContext,
    handle: AnyWindowHandle,
    host: Host,
    controller: Entity<ConnectionController>,
    theme: &'static str,
    revision: u64,
    /// The pixels of the frame saved last. Its type belongs to a crate this
    /// one does not name.
    last: Option<Vec<u8>>,
    _dir: tempfile::TempDir,
}

impl Stage {
    fn new(theme: &'static str, width: f32, height: f32) -> Self {
        Self::refusing(theme, width, height, None)
    }

    /// [`Self::new`], on a host that refuses the pane's first `OPEN`.
    fn refusing(theme: &'static str, width: f32, height: f32, refusal: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let profiles = dir.path().join("profiles.json");
        let saved = json!({
            "schemaVersion": 1, "lastProfileId": "local",
            "profiles": [
                { "id": "local", "label": "devbox", "connection": { "mode": "local" }, "shown": true },
            ],
        });
        std::fs::write(&profiles, saved.to_string()).unwrap();
        let state_path = dir.path().join("app-state.json");
        let state = json!({
            "schemaVersion": 1, "appTabs": [],
            "workspaceUi": [{
                "hostProfileId": "local", "serverIdentity": "tmux",
                "sessionId": "$1", "sessionName": "muxflow",
            }],
            "shell": { "theme": theme, "uiScale": 1.0, "panelOpen": false },
        });
        std::fs::write(&state_path, state.to_string()).unwrap();
        let mut cx = super::context(Path::new(&state_path));
        let host = Host::default();
        host.0.lock().unwrap().open_refusal = refusal.map(str::to_owned);
        let controller = cx.update(|cx| {
            let backend: Arc<dyn ConnectionBackend> = Arc::new(host.clone());
            let store = Arc::new(ProfileStore::load(profiles).unwrap());
            let app_state = Arc::new(AppStateStore::load(state_path.clone()));
            let controller = cx.new(|cx| ConnectionController::new(backend, store, app_state, cx));
            cx.set_global(ConnectionUi(controller.clone()));
            chat::start(controller.clone(), Arc::new(host.clone()), cx);
            controller
        });
        cx.run_until_parked();
        let handle = super::open(&mut cx, width, height);
        cx.update_window(handle, |_, window, _| window.activate_window())
            .unwrap();
        let mut stage = Self {
            cx,
            handle,
            host,
            controller,
            theme,
            revision: 0,
            last: None,
            _dir: dir,
        };
        stage.publish(vec![
            frame(10, "terminal", &1u64.to_be_bytes()),
            frame(6, "connected", &[]),
            topology(1, 120),
        ]);
        stage
    }

    fn settle(&mut self) {
        // The engine's thread, the link's drain, and the animations that
        // bring a menu or a dialog in.
        for _ in 0..4 {
            self.cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(15));
            self.cx.advance_clock(Duration::from_millis(120));
        }
        self.cx.run_until_parked();
        self.cx
            .update_window(self.handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
        self.cx.run_until_parked();
    }

    fn publish(&mut self, frames: Vec<Vec<u8>>) {
        let sink = self.host.0.lock().unwrap().sinks[0].clone();
        for frame in frames {
            sink.send(EventBody::Raw(frame)).unwrap();
        }
        self.settle();
    }

    fn chat(&mut self, change: ChatChange) {
        self.revision += 1;
        let event = ChatEvent {
            pane_id: PANE.into(),
            revision: self.revision,
            change: Some(change),
        };
        let controller = self.controller.clone();
        self.cx.update(|cx| {
            controller.update(cx, |_, cx| {
                cx.emit(ConnectionEvent::Chat {
                    profile_id: "local".into(),
                    event: Box::new(event),
                });
            });
        });
        self.settle();
    }

    fn snapshot(&mut self, state: ChatState, items: Vec<ChatItem>) {
        self.revision += 1;
        let snapshot = ChatSnapshot {
            revision: self.revision,
            state: Some(state),
            catalog: Some(catalog()),
            items,
            older_cursor: String::new(),
        };
        self.revision -= 1;
        self.chat(ChatChange::Snapshot(snapshot));
    }

    fn window(&mut self, act: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App)) {
        self.cx
            .update_window(self.handle, |_, window, cx| act(window, cx))
            .unwrap();
        self.settle();
    }

    fn click(&mut self, id: &'static str) {
        self.window(|window, cx| window.click(id, cx));
    }

    /// Clicks the Settings section with this name; the sidebar has an
    /// "Agents" of its own.
    fn click_section(&mut self, label: &'static str) {
        self.window(|window, cx| {
            let id = window
                .find_all_by_label(label)
                .into_iter()
                .find(|tab| tab.path().contains(&"settings-tabs".into()))
                .expect("the section's tab")
                .path()
                .last()
                .cloned()
                .expect("an element with an id");
            window.within("settings-tabs").click(id, cx);
        });
    }

    fn hover(&mut self, id: &'static str) {
        self.window(|window, cx| window.hover(id, cx));
    }

    fn input(&mut self, text: &'static str) {
        self.window(|window, cx| window.input(text, cx));
    }

    fn press(&mut self, key: &'static str) {
        self.window(|window, cx| window.press(key, cx));
    }

    /// Saves the window as `chat-<theme>-<name>`, which must differ from the
    /// frame saved before it.
    fn shoot(&mut self, name: &str) {
        self.settle();
        let frame = self
            .cx
            .capture_screenshot(self.handle)
            .expect("offscreen rendering");
        let label = format!("chat-{}-{name}", self.theme);
        if let Ok(out) = std::env::var("MUXFLOW_GPUI_FRAME_DIR") {
            std::fs::create_dir_all(&out).unwrap();
            frame
                .save(format!("{out}/{label}.png"))
                .expect("save the frame");
        }
        let pixels = frame.as_raw().clone();
        if let Some(last) = &self.last {
            let changed = last
                .as_chunks::<4>()
                .0
                .iter()
                .zip(pixels.as_chunks::<4>().0)
                .filter(|(a, b)| a != b)
                .count();
            assert!(changed > 400, "{label}: nothing new was drawn ({changed})");
        }
        self.last = Some(pixels);
        println!("{label}: ok");
    }
}

/// The whole window, as the design's first mock, and every piece after it.
fn pieces(theme: &'static str) {
    let mut stage = Stage::new(theme, 1280., 800.);
    stage.shoot("opening");

    // Waiting for the user: the request is a card in the conversation.
    stage.snapshot(
        state(ChatTurnState::Waiting, Some(permission())),
        conversation(),
    );
    stage.shoot("permission");

    // A question, one option per row; then its second question, with boxes.
    stage.chat(ChatChange::State(state(
        ChatTurnState::Waiting,
        Some(questions()),
    )));
    stage.click("chat-option-0");
    stage.shoot("question");
    stage.click("chat-option-other");
    stage.input("Only paths that end at a border");
    stage.shoot("question-other");
    stage.click("chat-submit");
    stage.click("chat-option-0");
    stage.shoot("question-checks");

    // Idle: slash commands and skills over the composer.
    stage.chat(ChatChange::State(state(ChatTurnState::Idle, None)));
    stage.shoot("idle");
    stage.click("chat-composer");
    stage.input("/co");
    stage.shoot("slash");
    stage.press("escape");
    stage.press("backspace");
    stage.press("backspace");
    stage.press("backspace");

    // Editing an earlier message.
    stage.hover("chat-item-u1");
    stage.click("chat-edit-u1");
    stage.shoot("editing");
    stage.click("chat-edit-cancel");

    // The pills' menus.
    stage.click("chat-model");
    stage.shoot("menu-model");
    stage.press("escape");
    stage.click("chat-effort");
    stage.shoot("menu-effort");
    stage.press("escape");
    stage.click("chat-permission");
    stage.shoot("menu-permission");
    stage.press("escape");

    // Working: an edit with its line counts, a tool still running, Stop.
    let mut edit = tool(
        "t3",
        ChatToolKind::Edit,
        "edit",
        "crates/terminal-interactions/src/links.rs",
        "",
    );
    {
        let tool = edit.tool.as_mut().unwrap();
        tool.path = "crates/terminal-interactions/src/links.rs".into();
        tool.added = 3;
        tool.removed = 1;
    }
    stage.chat(ChatChange::Item(edit));
    let mut running = tool(
        "t4",
        ChatToolKind::Command,
        "run",
        "cargo test -p terminal-interactions links",
        "",
    );
    running.tool.as_mut().unwrap().status = ChatToolStatus::Running.into();
    stage.chat(ChatChange::Item(running.clone()));
    stage.chat(ChatChange::State(state(ChatTurnState::Working, None)));
    stage.shoot("working");

    // The tool finished, badly, and its output is opened.
    {
        let tool = running.tool.as_mut().unwrap();
        tool.status = ChatToolStatus::Failed.into();
        tool.result_summary = "1 failed".into();
        tool.output_preview = "running 15 tests\ntest links::joins_soft_wrapped_rows ... ok\ntest links::joins_boxed_rows ... FAILED\n\nfailures:\n    links::joins_boxed_rows\n\ntest result: FAILED. 14 passed; 1 failed; 0 ignored".into();
        tool.output_truncated = true;
    }
    stage.chat(ChatChange::Updated(running));
    stage.click("chat-tool-open-t4");
    stage.shoot("tool-output");

    // The agent is gone: said over the composer, which sends nothing.
    let mut ended = state(ChatTurnState::Idle, None);
    ended.connection = ChatConnection::Failed.into();
    ended.failure = "claude exited: the session could not be resumed.".into();
    stage.chat(ChatChange::State(ended));
    stage.shoot("failed");

    // Beside a shell: the column narrows with its pane.
    stage.chat(ChatChange::State(state(ChatTurnState::Idle, None)));
    stage.publish(vec![topology(2, 70)]);
    stage.snapshot(state(ChatTurnState::Idle, None), conversation());
    stage.shoot("split");

    // As narrow as a fork beside its conversation: the pills wrap.
    stage.publish(vec![topology(3, 34)]);
    stage.snapshot(
        state(ChatTurnState::Waiting, Some(permission())),
        conversation(),
    );
    stage.shoot("narrow");
}

/// The column beside the details card, with a stand-in where the card goes.
fn beside_the_details_card(theme: &'static str) {
    let mut stage = Stage::new(theme, 1500., 800.);
    stage.snapshot(
        state(ChatTurnState::Waiting, Some(permission())),
        conversation(),
    );
    stage.shoot("wide");
    stage.cx.update(|cx| {
        chat::set_details_card(
            std::rc::Rc::new(|_, _, cx| {
                let palette = theme::palette(cx);
                Some(
                    div()
                        .h(gpui_kit::px(150.))
                        .border_1()
                        .border_color(palette.chrome_border)
                        .rounded(gpui_kit::px(10.))
                        .bg(palette.chrome_bg)
                        .p(gpui_kit::px(10.))
                        .text_color(palette.chrome_faint)
                        .child("details card")
                        .into_any_element(),
                )
            }),
            cx,
        );
    });
    stage.shoot("wide-details");
}

/// A chat the host refuses to open says so, and offers to try again.
fn refused(theme: &'static str) {
    let refusal = "chat_unavailable: This pane has no chat to open.";
    let mut stage = Stage::refusing(theme, 900., 500., Some(refusal));
    stage.shoot("refused");
    stage.click("chat-open-retry");
    stage.shoot("opening-small");
    stage.host.0.lock().unwrap().open_refusal = Some(refusal.into());
    // A lost change asks again, and this time the host refuses.
    stage.snapshot(state(ChatTurnState::Idle, None), Vec::new());
    stage.shoot("empty");
    stage.revision += 5;
    stage.chat(ChatChange::State(state(ChatTurnState::Idle, None)));
    stage.shoot("after-refusal");
}

/// Settings, on the section that holds the host's agent mode.
fn settings(theme: &'static str) {
    let mut stage = Stage::new(theme, 1280., 800.);
    stage.snapshot(state(ChatTurnState::Idle, None), conversation());
    stage.shoot("settings-before");
    // The host says what it does with a new agent in its agent snapshot.
    let controller = stage.controller.clone();
    stage.cx.update(|cx| {
        controller.update(cx, |_, cx| {
            cx.emit(ConnectionEvent::AgentService(AgentServiceFrame {
                profile_id: "local".into(),
                server_identity: "tmux".into(),
                terminal_epoch: 1,
                scope: "snapshot".into(),
                payload: json!({ "agentMode": "chat" }).to_string(),
            }));
        });
    });
    stage.press("secondary-,");
    stage.click_section("Agents");
    stage.shoot("settings-agents");
}

pub fn run(theme: &'static str) {
    pieces(theme);
    beside_the_details_card(theme);
    refused(theme);
    settings(theme);
}
