//! A provider that talks to no agent: it replays a fixed conversation, so the
//! chat clients and the provider tests have every kind of item, request and
//! control to work against without running a real one. Registered only when
//! `MUXFLOW_CHAT_SCRIPTED=1`.
//!
//! A pane opens on a long finished transcript. Each message sent starts one of
//! two turns in rotation: the first stops on a question, the second on a
//! permission request. One step runs every `step`, so a turn can be watched
//! and interrupted.

use std::{
    collections::VecDeque,
    ffi::OsStr,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use tmux_agent_protocol::v1::{
    self, ChatConnection, ChatItemKind as Kind, ChatPermissionMode, ChatSpeed,
    ChatToolKind as Tool, ChatToolStatus as Status, ChatTurnState as Turn,
};
use tokio::sync::mpsc;

use super::provider::{Change, ChatError, ChatProvider, ChatSession, ChatSink, SessionCommand};

pub(super) const ADAPTER_ID: &str = "scripted";
pub(super) const SWITCH: &str = "MUXFLOW_CHAT_SCRIPTED";
/// The pause between two steps of a turn.
pub(super) const STEP: Duration = Duration::from_millis(120);

/// How many question-and-answer pairs open the transcript, ahead of the items
/// that show every kind: enough that a snapshot cannot hold them all.
const EARLIER_EXCHANGES: usize = 110;
/// 2026-01-01T00:00:00Z. The transcript's items are a minute apart from here.
const HISTORY_START_UNIX_MILLIS: i64 = 1_767_225_600_000;
const FILE: &str = "crates/terminal-interactions/src/links.rs";

pub(super) fn enabled(switch: Option<&OsStr>) -> bool {
    switch == Some(OsStr::new("1"))
}

pub(super) struct ScriptedProvider {
    step: Duration,
}

impl ScriptedProvider {
    pub(super) fn new(step: Duration) -> Self {
        Self { step }
    }
}

impl ChatProvider for ScriptedProvider {
    fn adapter_id(&self) -> &'static str {
        ADAPTER_ID
    }

    fn capabilities(&self) -> v1::ChatCapabilities {
        capabilities()
    }

    fn load_history(&self, _pane_id: &str) -> Vec<v1::ChatItem> {
        let mut items = Vec::new();
        for number in 1..=EARLIER_EXCHANGES {
            items.push(text(
                format!("history-q{number:03}"),
                Kind::User,
                &format!("Earlier question {number}"),
            ));
            items.push(text(
                format!("history-a{number:03}"),
                Kind::Assistant,
                &format!("Earlier answer {number}."),
            ));
        }
        let id = |name: &str| format!("history-{name}");
        let mut failed = finished(
            tool(id("clippy"), Tool::Command, "run", "cargo clippy"),
            Status::Failed,
            "exit 101",
        );
        if let Some(tool) = &mut failed.tool {
            tool.output_preview = "error: this function has too many arguments\n".repeat(40);
            tool.output_truncated = true;
        }
        items.extend([
            text(
                id("ask"),
                Kind::User,
                "The link detector misses paths that wrap inside boxed output. Find out why.",
            ),
            text(
                id("think"),
                Kind::Reasoning,
                "Boxed output is hard-wrapped by the agent, so the soft-wrap join never sees it.",
            ),
            finished(read(id("read")), Status::Ok, "done"),
            finished(
                tool(id("search"), Tool::Search, "search", "hard_wrap"),
                Status::Ok,
                "6 matches",
            ),
            tests_passed(id("test")),
            finished(
                tool(id("web"), Tool::Web, "fetch", "docs.rs/unicode-width"),
                Status::Ok,
                "done",
            ),
            finished(
                tool(
                    id("mcp"),
                    Tool::Mcp,
                    "docs",
                    "resolve-library-id unicode-width",
                ),
                Status::Ok,
                "1 library",
            ),
            finished(
                tool(
                    id("agent"),
                    Tool::Subagent,
                    "agent",
                    "Find the other callers",
                ),
                Status::Ok,
                "3 files",
            ),
            edited(id("edit")),
            failed,
            finished(
                tool(id("todo"), Tool::Other, "todo", "Update the task list"),
                Status::Ok,
                "done",
            ),
            text(
                id("reply"),
                Kind::Assistant,
                &format!(
                    "A path that breaks right after a `/` inside a box border is dropped, \
                     because the border ends the continuation.\n\n\
                     - The join is in [links.rs]({FILE}).\n\
                     - The fixture is `tests/fixtures/boxed.txt`.\n"
                ),
            ),
            text(
                id("notice"),
                Kind::Notice,
                "Context compacted, was 182k tokens",
            ),
        ]);
        for (index, item) in items.iter_mut().enumerate() {
            item.at_unix_millis = HISTORY_START_UNIX_MILLIS + 60_000 * index as i64;
        }
        items
    }

    fn connect(&self, pane_id: &str, sink: ChatSink) -> Box<dyn ChatSession> {
        let script = Arc::new(Mutex::new(Script {
            sink,
            state: v1::ChatState {
                native_session_id: format!("scripted-{pane_id}"),
                turn: Turn::Idle.into(),
                connection: ChatConnection::Ready.into(),
                options: Some(v1::ChatOptions {
                    model: "opus-5-5".into(),
                    effort: "high".into(),
                    permission: ChatPermissionMode::Auto.into(),
                    speed: ChatSpeed::Standard.into(),
                }),
                usage: Some(v1::ChatUsage {
                    used_tokens: 209_000,
                    window_tokens: 1_000_000,
                }),
                history_complete: true,
                ..Default::default()
            },
            steps: VecDeque::new(),
            open: Vec::new(),
            turns: 0,
        }));
        {
            let script = lock(&script);
            script.sink.publish(Change::Catalog(catalog()));
            script.publish_state();
        }
        let (wake, mut woken) = mpsc::channel(1);
        let step = self.step;
        let ticking = Arc::downgrade(&script);
        tokio::spawn(async move {
            // Ends when the session is dropped: the script goes with it, and
            // so does the sender a waiting ticker is listening to.
            while let Some(script) = ticking.upgrade() {
                let ran = lock(&script).run_step();
                drop(script);
                if ran {
                    tokio::time::sleep(step).await;
                } else if woken.recv().await.is_none() {
                    break;
                }
            }
        });
        Box::new(ScriptedSession { script, wake })
    }
}

struct ScriptedSession {
    script: Arc<Mutex<Script>>,
    wake: mpsc::Sender<()>,
}

impl ChatSession for ScriptedSession {
    fn command(&mut self, command: SessionCommand) -> Result<(), ChatError> {
        let result = lock(&self.script).command(command);
        // Full means a wake is already waiting.
        let _ = self.wake.try_send(());
        result
    }

    /// A copy cut at the reply's last item, as a real provider settles one.
    /// There is no agent to start it with, so it never opens.
    fn fork(
        &mut self,
        at: &super::provider::ForkAt,
    ) -> Result<super::provider::ForkPlan, ChatError> {
        let launch = super::provider::Launch {
            resume: super::provider::Resume::Session {
                id: ADAPTER_ID.to_owned(),
                fork: true,
            },
            resume_at: at.through.clone(),
            ..Default::default()
        };
        Ok(Box::new(move || Ok(launch)))
    }
}

fn lock(script: &Mutex<Script>) -> std::sync::MutexGuard<'_, Script> {
    script.lock().unwrap_or_else(PoisonError::into_inner)
}

enum Step {
    Item(v1::ChatItem),
    Delta {
        item_id: String,
        text: &'static str,
    },
    Usage(u64),
    /// Stops the turn on a request until it is answered.
    Ask(v1::ChatPendingRequest),
    Idle,
}

struct Script {
    sink: ChatSink,
    state: v1::ChatState,
    /// What is left of the running turn.
    steps: VecDeque<Step>,
    /// Items published as streaming or running and not yet finished.
    open: Vec<v1::ChatItem>,
    /// How many turns have started.
    turns: u32,
}

impl Script {
    fn publish_state(&self) {
        self.sink.publish(Change::State(self.state.clone()));
    }

    fn set_turn(&mut self, turn: Turn, pending: Option<v1::ChatPendingRequest>) {
        self.state.turn = turn.into();
        self.state.pending = pending;
        self.publish_state();
    }

    fn publish_item(&mut self, mut item: v1::ChatItem) {
        // An item that finishes keeps the time it appeared.
        let appeared = self.open.iter().position(|open| open.id == item.id);
        item.at_unix_millis = match appeared {
            Some(index) => self.open.remove(index).at_unix_millis,
            None => now_unix_millis(),
        };
        let running = |tool: &v1::ChatTool| tool.status == i32::from(Status::Running);
        if item.streaming || item.tool.as_ref().is_some_and(running) {
            self.open.push(item.clone());
        }
        self.sink.publish(match appeared {
            Some(_) => Change::Updated(item),
            None => Change::Item(item),
        });
    }

    /// Runs the turn's next step. False when there is none to run now.
    fn run_step(&mut self) -> bool {
        if self.state.pending.is_some() {
            return false;
        }
        let Some(step) = self.steps.pop_front() else {
            return false;
        };
        match step {
            Step::Item(item) => self.publish_item(item),
            Step::Delta { item_id, text } => {
                if let Some(open) = self.open.iter_mut().find(|open| open.id == item_id) {
                    open.text.push_str(text);
                }
                self.sink.publish(Change::Delta(v1::ChatItemDelta {
                    item_id,
                    text_append: text.to_owned(),
                }));
            }
            Step::Usage(tokens) => {
                self.state.usage.get_or_insert_default().used_tokens += tokens;
                self.publish_state();
            }
            Step::Ask(request) => self.set_turn(Turn::Waiting, Some(request)),
            Step::Idle => self.set_turn(Turn::Idle, None),
        }
        true
    }

    /// Ends the running turn where it stands: nothing is left streaming or
    /// running, and nothing is pending.
    fn stop(&mut self, notice: Option<&str>) {
        self.steps.clear();
        for mut item in std::mem::take(&mut self.open) {
            item.streaming = false;
            if let Some(tool) = &mut item.tool {
                tool.status = Status::Failed.into();
                tool.result_summary = "stopped".into();
            }
            self.sink.publish(Change::Updated(item));
        }
        if let Some(notice) = notice {
            let id = format!("turn-{}-stopped", self.turns);
            self.publish_item(text(id, Kind::Notice, notice));
        }
        self.set_turn(Turn::Idle, None);
    }

    fn command(&mut self, command: SessionCommand) -> Result<(), ChatError> {
        match command {
            SessionCommand::Send {
                text: message,
                item_id,
                attachments,
            } => {
                let mut item = text(item_id, Kind::User, &message);
                item.attachments = attachments.iter().map(super::Attached::wire).collect();
                self.publish_item(item);
                // A message sent mid-turn joins the turn that is running.
                if self.state.turn == i32::from(Turn::Idle) {
                    self.turns += 1;
                    self.steps = if self.turns % 2 == 1 {
                        question_turn(self.turns)
                    } else {
                        permission_turn(self.turns)
                    };
                    self.set_turn(Turn::Working, None);
                }
            }
            SessionCommand::Interrupt => {
                if self.state.turn != i32::from(Turn::Idle) {
                    self.stop(Some("Interrupted."));
                }
            }
            SessionCommand::Answer { answer, .. } => {
                let Some(request) = self.state.pending.take() else {
                    return Ok(());
                };
                self.steps = if request.kind == i32::from(v1::ChatRequestKind::Question) {
                    after_question(self.turns, &request, &answer)
                } else {
                    after_permission(self.turns, &request, answer.allow)
                };
                self.set_turn(Turn::Working, None);
            }
            SessionCommand::Rewind { after_item_id, .. } => {
                self.stop(None);
                let truncate = v1::ChatTruncate { after_item_id };
                self.sink.publish(Change::Truncate(truncate));
            }
            SessionCommand::SetOptions(mut options) => {
                // A model that lacks the effort or the speed in use gets its
                // highest effort, and standard speed.
                if let Some(model) = catalog()
                    .models
                    .iter()
                    .find(|model| model.id == options.model)
                {
                    if !model.efforts.contains(&options.effort) {
                        options.effort = model.efforts.last().cloned().unwrap_or_default();
                    }
                    if !model.supports_fast {
                        options.speed = ChatSpeed::Standard.into();
                    }
                }
                self.state.options = Some(options);
                self.publish_state();
            }
            // It lists no background tasks, so the runtime lets none through.
            SessionCommand::StopTask { .. } => return Err(super::cannot_stop()),
        }
        Ok(())
    }
}

fn capabilities() -> v1::ChatCapabilities {
    v1::ChatCapabilities {
        rewind: true,
        send_mid_turn: true,
        model: true,
        effort: true,
        speed: true,
        permission: true,
        fork: true,
        questions: true,
    }
}

fn catalog() -> v1::ChatCatalog {
    let model = |id: &str, label: &str, efforts: &[&str], supports_fast| v1::ChatModel {
        id: id.into(),
        label: label.into(),
        efforts: efforts.iter().map(|effort| (*effort).to_owned()).collect(),
        supports_fast,
    };
    let command = |name: &str, description: &str, argument_hint: &str, kind| v1::ChatCommand {
        name: name.into(),
        description: description.into(),
        argument_hint: argument_hint.into(),
        kind: i32::from(kind),
    };
    use v1::ChatCommandKind::{Command, Muxflow, Skill};
    v1::ChatCatalog {
        capabilities: Some(capabilities()),
        models: vec![
            model(
                "opus-5-5",
                "Opus 5.5",
                &["low", "medium", "high", "xhigh", "max"],
                true,
            ),
            model("fable-5-2", "Fable 5.2", &["low", "medium", "high"], false),
            model("sonnet-5-5", "Sonnet 5.5", &[], true),
        ],
        commands: vec![
            command(
                "compact",
                "Summarise the conversation to free context",
                "",
                Command,
            ),
            command("clear", "Start over in this session", "", Command),
            command(
                "code-review",
                "Review the current diff for bugs",
                "[level] [target]",
                Skill,
            ),
            command(
                "diagnose-crash",
                "Find out why a program crashed",
                "",
                Skill,
            ),
            command(
                "btw",
                "Ask a side question in a fork",
                "<question>",
                Muxflow,
            ),
            command(
                "side",
                "Ask a side question in a fork",
                "<question>",
                Muxflow,
            ),
        ],
    }
}

fn question_turn(turn: u32) -> VecDeque<Step> {
    let id = |name: &str| format!("turn-{turn}-{name}");
    let mut steps = VecDeque::from([
        Step::Item(text(
            id("think"),
            Kind::Reasoning,
            "The join needs a delimiter or an indented continuation. Check the boxed case.",
        )),
        Step::Item(read(id("read"))),
        Step::Item(finished(read(id("read")), Status::Ok, "done")),
        Step::Item(tests(id("test"))),
        Step::Item(tests_passed(id("test"))),
        Step::Usage(1_500),
    ]);
    stream(
        &mut steps,
        id("reply"),
        &[
            "The detector joins rows the terminal marks as soft-wrapped. ",
            "Boxed output is hard-wrapped, so it takes the second path. ",
            "Before I change it, two things to settle.",
        ],
    );
    let option = |id: &str, label: &str, description: &str| v1::ChatOption {
        id: id.into(),
        label: label.into(),
        description: description.into(),
    };
    steps.push_back(Step::Ask(v1::ChatPendingRequest {
        request_id: id("question"),
        kind: v1::ChatRequestKind::Question.into(),
        title: "How should the wrapped-path fix be scoped?".into(),
        detail: String::new(),
        questions: vec![
            v1::ChatQuestion {
                id: "scope".into(),
                header: "Scope".into(),
                prompt: "How should the wrapped-path fix be scoped?".into(),
                options: vec![
                    option(
                        "boxed",
                        "Boxed output only",
                        "Strip box borders before joining rows. Smallest change.",
                    ),
                    option(
                        "any",
                        "Any hard-wrapped path",
                        "Also join rows without a border. Catches more, with some risk.",
                    ),
                ],
                multi_select: false,
                allow_other: true,
            },
            v1::ChatQuestion {
                id: "checks".into(),
                header: "Checks".into(),
                prompt: "Which checks should run afterwards?".into(),
                options: vec![
                    option("tests", "Unit tests", "cargo test for the crate."),
                    option("clippy", "Clippy", "With warnings denied."),
                    option("fixture", "A new fixture", "One boxed transcript."),
                ],
                multi_select: true,
                allow_other: false,
            },
        ],
    }));
    steps
}

fn after_question(
    turn: u32,
    request: &v1::ChatPendingRequest,
    answer: &v1::ChatAnswer,
) -> VecDeque<Step> {
    let id = |name: &str| format!("turn-{turn}-{name}");
    // The labels chosen, in question order, so a client can see its answer
    // arrived.
    let mut chosen = Vec::new();
    for question in &request.questions {
        for given in &answer.answers {
            if given.question_id != question.id {
                continue;
            }
            let picked = |option: &&v1::ChatOption| given.option_ids.contains(&option.id);
            chosen.extend(
                question
                    .options
                    .iter()
                    .filter(picked)
                    .map(|option| option.label.clone()),
            );
            if !given.other_text.is_empty() {
                chosen.push(given.other_text.clone());
            }
        }
    }
    let summary = if answer.skipped || chosen.is_empty() {
        "No answer, so I kept the smallest change.".to_owned()
    } else {
        format!("Going with: {}.", chosen.join(", "))
    };
    let reply = format!("{summary} The fix is in [links.rs]({FILE}).");
    VecDeque::from([
        Step::Item(file_tool(id("edit"), Tool::Edit, "edit")),
        Step::Item(edited(id("edit"))),
        Step::Item(text(id("done"), Kind::Assistant, &reply)),
        Step::Usage(4_000),
        Step::Idle,
    ])
}

fn permission_turn(turn: u32) -> VecDeque<Step> {
    let id = |name: &str| format!("turn-{turn}-{name}");
    let search = tool(id("search"), Tool::Search, "search", "force");
    let mut steps = VecDeque::from([
        Step::Item(search.clone()),
        Step::Item(finished(search, Status::Ok, "2 matches")),
    ]);
    stream(
        &mut steps,
        id("reply"),
        &["The branch has to be pushed ", "over the remote one."],
    );
    steps.push_back(Step::Ask(v1::ChatPendingRequest {
        request_id: id("permission"),
        kind: v1::ChatRequestKind::Permission.into(),
        title: "Auto mode is asking: run this command?".into(),
        detail: "git push --force origin gpui/chat".into(),
        questions: Vec::new(),
    }));
    steps
}

fn after_permission(turn: u32, request: &v1::ChatPendingRequest, allow: bool) -> VecDeque<Step> {
    let id = |name: &str| format!("turn-{turn}-{name}");
    let push = tool(id("push"), Tool::Command, "run", &request.detail);
    let (outcome, reply) = if allow {
        (
            finished(push.clone(), Status::Ok, "pushed"),
            "Pushed. The remote branch now matches.",
        )
    } else {
        (
            finished(push.clone(), Status::Failed, "denied"),
            "Not pushed. The remote branch is unchanged.",
        )
    };
    VecDeque::from([
        Step::Item(push),
        Step::Item(outcome),
        Step::Item(text(id("done"), Kind::Assistant, reply)),
        Step::Usage(2_000),
        Step::Idle,
    ])
}

/// An assistant message that arrives as an empty streaming item, its text in
/// `parts`, then the whole item again, finished.
fn stream(steps: &mut VecDeque<Step>, id: String, parts: &[&'static str]) {
    let mut item = text(id.clone(), Kind::Assistant, "");
    item.streaming = true;
    steps.push_back(Step::Item(item));
    for part in parts {
        steps.push_back(Step::Delta {
            item_id: id.clone(),
            text: part,
        });
    }
    steps.push_back(Step::Item(text(id, Kind::Assistant, &parts.concat())));
}

fn text(id: String, kind: Kind, text: &str) -> v1::ChatItem {
    v1::ChatItem {
        id,
        kind: kind.into(),
        text: text.to_owned(),
        rewindable: kind == Kind::User,
        ..Default::default()
    }
}

/// A tool row that is still running.
fn tool(id: String, kind: Tool, title: &str, detail: &str) -> v1::ChatItem {
    v1::ChatItem {
        id,
        kind: Kind::Tool.into(),
        tool: Some(v1::ChatTool {
            kind: kind.into(),
            title: title.into(),
            detail: detail.into(),
            status: Status::Running.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn finished(mut item: v1::ChatItem, status: Status, summary: &str) -> v1::ChatItem {
    if let Some(tool) = &mut item.tool {
        tool.status = status.into();
        tool.result_summary = summary.into();
    }
    item
}

/// A running tool row that acts on the file.
fn file_tool(id: String, kind: Tool, title: &str) -> v1::ChatItem {
    let mut item = tool(id, kind, title, FILE);
    if let Some(tool) = &mut item.tool {
        tool.path = FILE.into();
    }
    item
}

fn read(id: String) -> v1::ChatItem {
    file_tool(id, Tool::Read, "read")
}

fn tests(id: String) -> v1::ChatItem {
    tool(
        id,
        Tool::Command,
        "run",
        "cargo test -p terminal-interactions links",
    )
}

fn tests_passed(id: String) -> v1::ChatItem {
    let mut item = finished(tests(id), Status::Ok, "14 passed");
    if let Some(tool) = &mut item.tool {
        tool.output_preview =
            "running 14 tests\n..............\ntest result: ok. 14 passed; 0 failed\n".into();
    }
    item
}

/// A finished file edit with its line counts.
fn edited(id: String) -> v1::ChatItem {
    let mut item = finished(file_tool(id, Tool::Edit, "edit"), Status::Ok, "");
    if let Some(tool) = &mut item.tool {
        tool.added = 3;
        tool.removed = 1;
    }
    item
}

fn now_unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}
