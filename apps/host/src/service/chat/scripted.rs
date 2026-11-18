//! A provider that talks to no agent: it replays a fixed conversation, so the
//! chat clients and the provider tests have every kind of item, request and
//! control to work against without running a real one. Registered only when
//! `MUXFLOW_CHAT_SCRIPTED=1`.
//!
//! A pane opens on a long finished transcript. Each message sent starts one of
//! two turns in rotation: the first stops on a question, the second on a
//! permission request. One step runs every `step`, so a turn can be watched
//! and interrupted.
//!
//! A message that is one of these words, sent while no turn runs, starts the
//! turn it names instead, and leaves the rotation where it was:
//!
//! - `demo markdown`: a reply with a heading, inline styles, a code block
//!   with a long line, a table, a nested list and a quote.
//! - `demo long`: about 40 seconds of reading, searching, running, editing
//!   and fetching, with one command that fails.
//! - `demo subagent`: a subagent in the foreground, its activity and its
//!   figures changing as it works.
//! - `demo background`: a command and a subagent left running in the
//!   background, listed and stoppable. The subagent ends by itself.
//! - `demo compact`: the context compacting for about 3 seconds.
//! - `demo attachments`: a message from another client with an image and a
//!   file attached.

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
/// How many steps the context compacts for: about 3 seconds.
const COMPACTING_STEPS: usize = 25;
/// How many steps a background subagent runs for before it ends by itself.
const BACKGROUND_STEPS: u32 = 60;
/// The pause after each row of the long turn, in steps.
const LONG_PAUSE: usize = 13;

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
            ending: Vec::new(),
            open: Vec::new(),
            turns: 0,
            demos: 0,
            turn_id: String::new(),
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
    /// Nothing, for as long as a step takes.
    Wait,
    Compacting(bool),
    /// The context is this many tokens, where `Usage` adds to it.
    Used(u64),
    /// Lists a background task. One given a count of steps and its finished
    /// row ends by itself that many steps later.
    Start(v1::ChatBackgroundTask, Option<(u32, v1::ChatItem)>),
}

struct Script {
    sink: ChatSink,
    state: v1::ChatState,
    /// What is left of the running turn.
    steps: VecDeque<Step>,
    /// The background tasks that end by themselves: the steps each has left,
    /// and its row as it ends. They count whatever the turn does.
    ending: Vec<(u32, v1::ChatItem)>,
    /// Items published as streaming or running and not yet finished.
    open: Vec<v1::ChatItem>,
    /// How many turns of the rotation have started.
    turns: u32,
    /// How many turns asked for by name have started.
    demos: u32,
    /// What the ids of the running turn's items start with: `turn-3`,
    /// `demo-1`.
    turn_id: String,
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

    /// Counts a step for each background task that ends by itself, and ends
    /// those it was the last for: the row is finished and the task leaves the
    /// list. False when none is counting.
    fn end_tasks(&mut self) -> bool {
        let counting = !self.ending.is_empty();
        let mut ended = Vec::new();
        self.ending.retain_mut(|(left, row)| {
            *left = left.saturating_sub(1);
            if *left == 0 {
                ended.push(row.clone());
            }
            *left > 0
        });
        for row in ended {
            self.state.background.retain(|task| task.id != row.id);
            self.publish_item(row);
            self.publish_state();
        }
        counting
    }

    /// Runs the turn's next step, and counts it for the background tasks.
    /// False when there was neither to do now.
    fn run_step(&mut self) -> bool {
        let counted = self.end_tasks();
        if self.state.pending.is_some() {
            return counted;
        }
        let Some(step) = self.steps.pop_front() else {
            return counted;
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
            Step::Wait => {}
            Step::Compacting(compacting) => {
                self.state.compacting = compacting;
                self.publish_state();
            }
            Step::Used(tokens) => {
                self.state.usage.get_or_insert_default().used_tokens = tokens;
                self.publish_state();
            }
            Step::Start(mut task, ends) => {
                task.started_at_unix_millis = now_unix_millis();
                self.state.background.push(task);
                self.ending.extend(ends);
                self.publish_state();
            }
        }
        true
    }

    /// Ends the running turn where it stands: nothing of it is left streaming,
    /// running or compacting, and nothing is pending. What runs in the
    /// background is no part of it, and runs on.
    fn stop(&mut self, notice: Option<&str>) {
        self.steps.clear();
        for mut item in std::mem::take(&mut self.open) {
            if self.state.background.iter().any(|task| task.id == item.id) {
                self.open.push(item);
                continue;
            }
            item.streaming = false;
            if let Some(tool) = &mut item.tool {
                tool.status = Status::Failed.into();
                tool.result_summary = "stopped".into();
            }
            self.sink.publish(Change::Updated(item));
        }
        if let Some(notice) = notice {
            let id = format!("{}-stopped", self.turn_id);
            self.publish_item(text(id, Kind::Notice, notice));
        }
        self.state.compacting = false;
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
                    let used = self.state.usage.unwrap_or_default().used_tokens;
                    self.steps = if let Some(steps) = demo(&message, self.demos + 1, used) {
                        self.demos += 1;
                        self.turn_id = format!("demo-{}", self.demos);
                        steps
                    } else {
                        self.turns += 1;
                        self.turn_id = format!("turn-{}", self.turns);
                        if self.turns % 2 == 1 {
                            question_turn(self.turns)
                        } else {
                            permission_turn(self.turns)
                        }
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
            // The runtime lets through only a task the state lists. Its row
            // has the task's id.
            SessionCommand::StopTask { task_id } => {
                let listed = self.state.background.len();
                self.state.background.retain(|task| task.id != task_id);
                if self.state.background.len() == listed {
                    return Err(super::cannot_stop());
                }
                self.ending.retain(|(_, row)| row.id != task_id);
                if let Some(index) = self.open.iter().position(|open| open.id == task_id) {
                    let row = finished(self.open.remove(index), Status::Failed, "stopped");
                    self.sink.publish(Change::Updated(row));
                }
                self.publish_state();
            }
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
        attachments: true,
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

/// The turn `message` asks for by name, one of those the module's description
/// lists. Its items' ids start with `demo-{number}`.
fn demo(message: &str, number: u32, used_tokens: u64) -> Option<VecDeque<Step>> {
    let id = |name: &str| format!("demo-{number}-{name}");
    Some(match message.trim().to_lowercase().as_str() {
        "demo markdown" => markdown_demo(id),
        "demo long" => long_demo(id),
        "demo subagent" => subagent_demo(id),
        "demo background" => background_demo(id),
        "demo compact" => compact_demo(id, used_tokens),
        "demo attachments" => attachments_demo(id),
        _ => return None,
    })
}

/// Every block of Markdown a client draws, in the parts it arrives in. One
/// line of the code is longer than a pane is wide.
const MARKDOWN: [&str; 5] = [
    "## Why wrapped paths are missed\n\n\
     The detector **joins** rows only when the terminal marks them *soft-wrapped*. \
     Boxed output never is, so `join_rows` skips it. \
     The rule is in the [wrapping notes](https://example.com/docs).\n\n",
    concat!(
        "```rust\n",
        "fn join_rows(rows: &[Row]) -> String {\n",
        "    let mut joined = String::new();\n",
        "    for row in rows {\n",
        "        // A border ends the continuation, so it is stripped before deciding whether the next row carries on with the same path.\n",
        "        joined.push_str(row.text.trim_end_matches(BORDER));\n",
        "    }\n",
        "    joined\n",
        "}\n",
        "```\n\n",
    ),
    "| Case | Wrap | Detected |\n\
     | --- | --- | --- |\n\
     | Plain output | soft | yes |\n\
     | Boxed output | hard | no |\n\
     | Indented continuation | hard | yes |\n\n",
    concat!(
        "1. Strip the border before the join.\n",
        "   - `│` and `|` at the end of a row\n",
        "   - the padding before them\n",
        "2. Join a row that ends in `/` with the next.\n\n",
    ),
    "> A path that breaks right after a `/` is the common case.\n",
];

fn markdown_demo(id: impl Fn(&str) -> String) -> VecDeque<Step> {
    let mut steps = VecDeque::new();
    stream(&mut steps, id("reply"), &MARKDOWN);
    steps.push_back(Step::Idle);
    steps
}

fn long_demo(id: impl Fn(&str) -> String) -> VecDeque<Step> {
    let ok = |row: v1::ChatItem, steps: usize, summary: &str| {
        (row.clone(), steps, finished(row, Status::Ok, summary))
    };
    let read = |name: &str, path: &str| {
        let mut row = tool(id(name), Tool::Read, "read", path);
        if let Some(tool) = &mut row.tool {
            tool.path = path.into();
        }
        ok(row, 1, "done")
    };
    let search = |name: &str, query: &str, summary: &str| {
        ok(tool(id(name), Tool::Search, "search", query), 2, summary)
    };
    let fetch = |name: &str, url: &str| ok(tool(id(name), Tool::Web, "fetch", url), 11, "done");
    let edit = |name: &str| (file_tool(id(name), Tool::Edit, "edit"), 2, edited(id(name)));
    let fmt = |name: &str| tool(id(name), Tool::Command, "run", "cargo fmt --check");
    let mut unformatted = finished(fmt("format"), Status::Failed, "exit 1");
    if let Some(tool) = &mut unformatted.tool {
        tool.output_preview = format!("Diff in {FILE}:212:\n-        joined\n+    joined\n");
    }
    // Each row as it starts, the steps it runs for, and as it ends.
    let rows = [
        read("read-links", FILE),
        search("search-wrap", "hard_wrap", "6 matches"),
        read("read-wrap", "crates/terminal-interactions/src/wrap.rs"),
        (tests(id("test")), 24, tests_passed(id("test"))),
        fetch("fetch-width", "docs.rs/unicode-width"),
        read("read-fixture", "tests/fixtures/boxed.txt"),
        search("search-trim", "trim_end_matches", "3 matches"),
        edit("edit"),
        (fmt("format"), 8, unformatted),
        edit("edit-format"),
        ok(fmt("format-again"), 8, "clean"),
        search("search-join", "join_rows", "4 matches"),
        fetch("fetch-grid", "docs.rs/alacritty_terminal"),
        read("read-again", FILE),
        (tests(id("test-again")), 28, tests_passed(id("test-again"))),
    ];
    let mut steps = VecDeque::from([Step::Item(text(
        id("think"),
        Kind::Reasoning,
        "The join, its callers and the fixture all need a look before anything changes.",
    ))]);
    for (number, (running, runs, ended)) in rows.into_iter().enumerate() {
        steps.push_back(Step::Item(running));
        steps.extend(wait(runs));
        steps.push_back(Step::Item(ended));
        if number == 7 {
            steps.push_back(Step::Item(text(
                id("progress"),
                Kind::Assistant,
                "The border is what ends the join, so it is stripped first. Checking the rest.",
            )));
        }
        steps.extend(wait(LONG_PAUSE));
    }
    steps.extend([
        Step::Item(text(
            id("done"),
            Kind::Assistant,
            &format!(
                "Wrapped paths inside a box are joined now. The fix is in [links.rs]({FILE})."
            ),
        )),
        Step::Usage(12_000),
        Step::Idle,
    ]);
    steps
}

fn subagent_demo(id: impl Fn(&str) -> String) -> VecDeque<Step> {
    const ACTIVITIES: [&str; 5] = [
        "reading crates/terminal-interactions/src/wrap.rs",
        "searching hard_wrap",
        "reading crates/terminal-interactions/src/links.rs",
        "searching join_rows",
        "running cargo test -p terminal-interactions wrap",
    ];
    // Five updates to an activity, each with one more tool call made.
    let row = |update: u32| {
        let activity = ACTIVITIES[(update as usize / 5).min(ACTIVITIES.len() - 1)];
        subagent(id("agent"), activity, update, 1_850 * u64::from(update))
    };
    let mut steps: VecDeque<Step> = (0..25).map(|update| Step::Item(row(update))).collect();
    steps.extend([
        Step::Item(finished(row(25), Status::Ok, "3 files")),
        Step::Item(text(
            id("done"),
            Kind::Assistant,
            "Three other files call it, and all of them pass whole rows.",
        )),
        Step::Idle,
    ]);
    steps
}

fn background_demo(id: impl Fn(&str) -> String) -> VecDeque<Step> {
    let dev = tool(id("dev"), Tool::Command, "run", "pnpm dev");
    let agent = |tool_uses, tokens| {
        in_background(subagent(
            id("agent"),
            "searching join_rows",
            tool_uses,
            tokens,
        ))
    };
    // A task has its row's id, which is how stopping it finds the row.
    let task = |name: &str, kind: v1::ChatTaskKind, description: &str| v1::ChatBackgroundTask {
        id: id(name),
        kind: kind.into(),
        description: description.into(),
        stoppable: true,
        ..Default::default()
    };
    VecDeque::from([
        Step::Item(dev.clone()),
        Step::Item(finished(dev, Status::Ok, "in the background")),
        Step::Start(task("dev", v1::ChatTaskKind::Command, "pnpm dev"), None),
        Step::Item(agent(1, 2_300)),
        Step::Start(
            task(
                "agent",
                v1::ChatTaskKind::Subagent,
                "Find the other callers",
            ),
            Some((
                BACKGROUND_STEPS,
                finished(agent(14, 31_400), Status::Ok, "3 files"),
            )),
        ),
        Step::Item(text(
            id("done"),
            Kind::Assistant,
            "The dev server is up and a subagent is looking for the other callers. \
             Both run in the background.",
        )),
        Step::Idle,
    ])
}

fn compact_demo(id: impl Fn(&str) -> String, used_tokens: u64) -> VecDeque<Step> {
    let notice = format!(
        "Context compacted, was {} tokens",
        super::claude::tokens(used_tokens)
    );
    let mut steps = VecDeque::from([Step::Compacting(true)]);
    steps.extend(wait(COMPACTING_STEPS));
    steps.extend([
        Step::Compacting(false),
        Step::Item(text(id("notice"), Kind::Notice, &notice)),
        Step::Used(40_000),
        Step::Idle,
    ]);
    steps
}

fn attachments_demo(id: impl Fn(&str) -> String) -> VecDeque<Step> {
    let attachment = |path: &str, name: &str, media_type: &str, size_bytes| v1::ChatAttachment {
        path: path.into(),
        name: name.into(),
        media_type: media_type.into(),
        size_bytes,
    };
    // As another client's message arrives: an item this one did not send.
    let mut ask = text(id("ask"), Kind::User, "Why is the header cut off here?");
    ask.attachments = vec![
        // An image the agent's record holds inline has no path.
        attachment("", "screenshot.png", "image/png", 248_113),
        attachment("/tmp/layout.log", "layout.log", "text/plain", 18_432),
    ];
    VecDeque::from([
        Step::Item(ask),
        Step::Item(text(
            id("reply"),
            Kind::Assistant,
            "The header's row has a fixed height, so the second line of the title is clipped.",
        )),
        Step::Idle,
    ])
}

fn wait(steps: usize) -> impl Iterator<Item = Step> {
    std::iter::repeat_with(|| Step::Wait).take(steps)
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

/// A running subagent row: what it does now, and its figures so far.
fn subagent(id: String, activity: &str, tool_uses: u32, tokens: u64) -> v1::ChatItem {
    let mut item = tool(id, Tool::Subagent, "agent", "Find the other callers");
    if let Some(tool) = &mut item.tool {
        tool.agent_type = "Explore".into();
        tool.activity = activity.into();
        tool.tool_uses = tool_uses;
        tool.tokens = tokens;
    }
    item
}

fn in_background(mut item: v1::ChatItem) -> v1::ChatItem {
    if let Some(tool) = &mut item.tool {
        tool.background = true;
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
