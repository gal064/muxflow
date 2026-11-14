//! The Claude Code chat: `claude -p` speaking stream-json on its stdin and
//! stdout, which `chat-pipe` carries to the pane's socket.
//!
//! One JSON object per line each way. The agent's lines are its
//! conversation (`assistant`, `user`, `stream_event`, `result`), its state
//! (`system`, `command_lifecycle`) and requests it waits on
//! (`control_request`); the host's are user messages, answers and control
//! requests of its own. Everything read here is fail-soft: a line that is not
//! understood is skipped.
//!
//! The agent cannot go back in a conversation while it runs. A rewind stops
//! it and starts it again on the same session at the message before the one
//! rewound (`--resume-session-at`).

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::OsString,
    path::PathBuf,
};

use serde_json::{Value, json};
use tmux_agent_protocol::v1::{
    self, ChatConnection as Connection, ChatItemKind as Kind, ChatPermissionMode as Permission,
    ChatSpeed as Speed, ChatToolKind as ToolKind, ChatToolStatus as Status, ChatTurnState as Turn,
};

use super::{
    convert::{self, Arguments, Takes},
    items::{self, string},
    pane::{self, Tag},
    provider::{
        Change, ChatError, ChatProvider, ChatSession, ChatSink, Launch, PaneCommand, Resume,
        SessionCommand,
    },
    session::{Dialect, Framing, Out, Session},
};

mod history;

pub(super) const ADAPTER_ID: &str = "claude-code";
/// User messages waiting for the agent to be ready for them.
const MAX_HELD: usize = 32;
/// Sent messages whose item ids are remembered, to match the agent's echo.
const MAX_SENT: usize = 256;
/// What the agent writes as a user message when a turn is stopped.
const INTERRUPTED: &str = "[Request interrupted by user";

pub(super) struct ClaudeProvider;

/// The options of `claude` this reads, by what each takes. An option that is
/// not here makes the command line unreadable, and its pane stays a terminal.
const OPTIONS: &[(&[&str], Takes)] = &[
    (&["--model"], Takes::One),
    (&["--effort"], Takes::One),
    (&["--add-dir"], Takes::Many),
    (&["--name", "-n"], Takes::One),
    (&["--permission-mode"], Takes::One),
    (&["--dangerously-skip-permissions"], Takes::Nothing),
    (&["--allow-dangerously-skip-permissions"], Takes::Nothing),
    (&["--resume", "-r"], Takes::Maybe),
    (&["--continue", "-c"], Takes::Nothing),
    (&["--fork-session"], Takes::Nothing),
    (&["--session-id"], Takes::One),
    // Of the terminal alone.
    (&["--verbose"], Takes::Nothing),
    (&["--ide"], Takes::Nothing),
    (&["--chrome"], Takes::Nothing),
    (&["--no-chrome"], Takes::Nothing),
    // What shapes the session, which a chat takes as the terminal did.
    (&["--settings"], Takes::One),
    (&["--setting-sources"], Takes::One),
    (&["--mcp-config"], Takes::Many),
    (&["--strict-mcp-config"], Takes::Nothing),
    (&["--append-system-prompt"], Takes::One),
    (&["--system-prompt"], Takes::One),
    (&["--agent"], Takes::One),
    (&["--agents"], Takes::One),
    (&["--allowedTools", "--allowed-tools"], Takes::Many),
    (&["--disallowedTools", "--disallowed-tools"], Takes::Many),
    (&["--tools"], Takes::Many),
    (&["--plugin-dir"], Takes::One),
    (&["--fallback-model"], Takes::One),
];
const PASSED_THROUGH: &[&str] = &[
    "--settings",
    "--setting-sources",
    "--mcp-config",
    "--strict-mcp-config",
    "--append-system-prompt",
    "--system-prompt",
    "--agent",
    "--agents",
    "--allowedTools",
    "--disallowedTools",
    "--tools",
    "--plugin-dir",
    "--fallback-model",
];
/// First words that make `claude` something other than a session.
const SUBCOMMANDS: &[&str] = &[
    "agents",
    "attach",
    "auth",
    "auto-mode",
    "config",
    "doctor",
    "gateway",
    "import",
    "install",
    "kill",
    "logs",
    "mcp",
    "migrate-installer",
    "plugin",
    "plugins",
    "purge",
    "respawn",
    "rm",
    "setup-token",
    "stop",
    "ultrareview",
    "update",
    "upgrade",
];

/// Where an agent keeps its sessions, given what its environment says and
/// the account's home.
fn configuration_directory(
    configured: Option<OsString>,
    home: Option<OsString>,
    account_home: Option<PathBuf>,
) -> PathBuf {
    let given = |value: Option<OsString>| value.filter(|value| !value.is_empty());
    match (given(configured), given(home)) {
        (Some(configured), _) => PathBuf::from(configured),
        (None, Some(home)) => PathBuf::from(home).join(".claude"),
        (None, None) => account_home.unwrap_or_default().join(".claude"),
    }
}

/// Where the agent in `pane_id` keeps its sessions.
fn configuration(pane_id: &str) -> PathBuf {
    let mut variables =
        convert::agent_variables(pane_id, ADAPTER_ID, &["CLAUDE_CONFIG_DIR", "HOME"]).into_iter();
    configuration_directory(
        variables.next().flatten(),
        variables.next().flatten(),
        crate::paths::account_home().ok(),
    )
}

fn is_session_id(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok()
}

impl ChatProvider for ClaudeProvider {
    fn adapter_id(&self) -> &'static str {
        ADAPTER_ID
    }

    fn capabilities(&self) -> v1::ChatCapabilities {
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

    /// Read by the session itself, once it knows which session it is.
    fn load_history(&self, _pane_id: &str) -> Vec<v1::ChatItem> {
        Vec::new()
    }

    fn connect(&self, _pane_id: &str, sink: ChatSink) -> Box<dyn ChatSession> {
        Box::new(Session::start(ADAPTER_ID, Claude::default(), sink))
    }

    fn launch(&self, arguments: &[String], resumed: Option<&str>) -> Option<Launch> {
        let read = Arguments::read(arguments, OPTIONS)?;
        let prompt = match read.positional.as_slice() {
            [] => String::new(),
            [first] if !SUBCOMMANDS.contains(&first.as_str()) => first.clone(),
            _ => return None,
        };
        // A session that may change nothing has no chat mode to become.
        if read.value("--permission-mode") == Some("plan") {
            return None;
        }
        let named = read.value("--resume").filter(|id| is_session_id(id));
        let fork = read.has("--fork-session");
        let session = |id: &str, fork| Resume::Session {
            id: id.to_owned(),
            fork,
        };
        let resume = match (named, resumed) {
            // The agent has not copied the session yet: its chat does.
            (Some(id), _) if fork => session(id, true),
            // What the agent says it resumed is what its user picked.
            (_, Some(id)) if is_session_id(id) => session(id, false),
            (Some(id), _) => session(id, false),
            // A picker nobody has answered yet.
            _ if read.has("--resume") => return None,
            _ if read.has("--continue") => Resume::Latest,
            _ => Resume::Fresh,
        };
        let mut passthrough = Vec::new();
        for (name, values) in &read.options {
            if PASSED_THROUGH.contains(name) {
                passthrough.push((*name).to_owned());
                passthrough.extend(values.iter().cloned());
            }
        }
        Some(Launch {
            model: read.value("--model").unwrap_or_default().to_owned(),
            effort: read.value("--effort").unwrap_or_default().to_owned(),
            full_access: read.has("--dangerously-skip-permissions")
                || read.value("--permission-mode") == Some("bypassPermissions"),
            directories: read.values("--add-dir").cloned().collect(),
            name: read.value("--name").unwrap_or_default().to_owned(),
            passthrough,
            resume,
            prompt,
            ..Default::default()
        })
    }

    fn pane_command(&self, launch: &Launch, _socket: &std::path::Path) -> Option<PaneCommand> {
        fn push(argv: &mut Vec<String>, words: &[&str]) {
            argv.extend(words.iter().map(|word| (*word).to_owned()));
        }
        let mut argv = launch.program.clone();
        let permission = if launch.full_access {
            "bypassPermissions"
        } else {
            "auto"
        };
        push(
            &mut argv,
            &[
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--replay-user-messages",
                // Permission prompts and questions come to the chat.
                "--permission-prompt-tool",
                "stdio",
                // So that Full can be chosen later, whatever it starts in.
                "--allow-dangerously-skip-permissions",
                "--permission-mode",
                permission,
            ],
        );
        if !launch.model.is_empty() {
            push(&mut argv, &["--model", &launch.model]);
        }
        if !launch.effort.is_empty() {
            push(&mut argv, &["--effort", &launch.effort]);
        }
        if !launch.name.is_empty() {
            push(&mut argv, &["--name", &launch.name]);
        }
        argv.extend(launch.passthrough.iter().cloned());
        if !launch.directories.is_empty() {
            push(&mut argv, &["--add-dir"]);
            argv.extend(launch.directories.iter().cloned());
        }
        let new_session = uuid::Uuid::new_v4().to_string();
        let session = match &launch.resume {
            Resume::Fresh => {
                push(&mut argv, &["--session-id", &new_session]);
                new_session
            }
            Resume::Latest => {
                push(&mut argv, &["--continue"]);
                String::new()
            }
            Resume::Session { id, fork: true } => {
                let copy = [
                    "--resume",
                    id,
                    "--fork-session",
                    "--session-id",
                    &new_session,
                ];
                push(&mut argv, &copy);
                new_session
            }
            Resume::Session { id, fork: false } => {
                push(&mut argv, &["--resume", id]);
                if !launch.resume_at.is_empty() {
                    push(&mut argv, &["--resume-session-at", &launch.resume_at]);
                }
                id.clone()
            }
        };
        Some(PaneCommand {
            argv,
            piped: true,
            session,
        })
    }

    fn announces_start(&self) -> bool {
        true
    }

    fn terminal_options(&self, options: &v1::ChatOptions) -> Vec<String> {
        let mut arguments = Vec::new();
        // The catalog's name for the agent's own choice is not a model.
        if !options.model.is_empty() && options.model != "default" {
            arguments.extend(["--model".to_owned(), options.model.clone()]);
        }
        if !options.effort.is_empty() {
            arguments.extend(["--effort".to_owned(), options.effort.clone()]);
        }
        if options.permission == i32::from(Permission::Full) {
            arguments.push("--dangerously-skip-permissions".to_owned());
        }
        arguments
    }
}

/// A control request of the host's, waiting for its answer.
enum Asked {
    Initialize,
    Settings,
    Usage,
    /// Answered with nothing the chat needs.
    Other,
}

/// A request of the agent's, waiting for the user.
struct Pending {
    request_id: String,
    tool: String,
    input: Value,
    title: String,
    detail: String,
}

/// A user message waiting for the agent to be ready for it.
struct Held {
    item_id: String,
    /// The agent's id for it.
    uuid: String,
    message: String,
}

struct Rewind {
    /// The message being rewound, as the agent knows it.
    uuid: String,
    after_item_id: String,
    /// The agent is being started again.
    restarting: bool,
}

pub(super) enum Job {
    History {
        pane_id: String,
        session: String,
        rewound_to: String,
    },
    /// Note the session's id on the pane, for the daemon that comes next.
    Tag { pane_id: String, session: String },
    Rewind {
        pane_id: String,
        session: String,
        uuid: String,
        launch: Box<Launch>,
    },
}

pub(super) enum Done {
    History(history::History),
    Tagged,
    Rewound(Result<(), String>),
}

/// The message whose blocks are arriving, and how far it has got.
struct Block {
    message: String,
    index: u32,
    /// An `assistant` message has taken this index.
    claimed: bool,
}

/// Turns the agent's conversation entries into items. The same entries come
/// over the stream and lie in the session's file, so both are read here.
#[derive(Default)]
pub(super) struct Reader {
    /// Streaming text and running tools, by item id.
    open: BTreeMap<String, v1::ChatItem>,
    block: Option<Block>,
}

impl Reader {
    fn next_index(&mut self, message: &str) -> u32 {
        match &mut self.block {
            Some(block) if block.message == message => {
                if block.claimed {
                    block.index += 1;
                }
                block.claimed = true;
                block.index
            }
            _ => {
                self.block = Some(Block {
                    message: message.to_owned(),
                    index: 0,
                    claimed: true,
                });
                0
            }
        }
    }

    /// The items of one `assistant` entry: one per block.
    pub(super) fn assistant(&mut self, entry: &Value) -> Vec<v1::ChatItem> {
        let message = &entry["message"];
        let message_id = string(message, "id");
        let at = items::unix_millis(string(entry, "timestamp"));
        let blocks = message["content"].as_array().map_or(&[][..], Vec::as_slice);
        let mut produced = Vec::new();
        for (offset, block) in blocks.iter().enumerate() {
            let index = match entry["apiBlockIndex"].as_u64() {
                Some(index) => {
                    let index = u32::try_from(index).unwrap_or(0) + offset as u32;
                    self.block = Some(Block {
                        message: message_id.to_owned(),
                        index,
                        claimed: true,
                    });
                    index
                }
                None => self.next_index(message_id),
            };
            let id = format!("{message_id}:{index}");
            let mut item = match string(block, "type") {
                "text" => items::text(id, Kind::Assistant, string(block, "text")),
                "thinking" => items::text(id, Kind::Reasoning, string(block, "thinking")),
                "tool_use" => {
                    tool_item(string(block, "id"), string(block, "name"), &block["input"])
                }
                _ => continue,
            };
            item.at_unix_millis = at;
            if item.tool.is_some() {
                self.open.insert(item.id.clone(), item.clone());
            } else {
                let streamed = self.open.remove(&item.id);
                // Thinking the agent keeps to itself arrives as an empty
                // block: what was streamed of it, if anything, is all of it.
                if item.text.is_empty() {
                    produced.extend(streamed.map(|mut streamed| {
                        streamed.streaming = false;
                        streamed
                    }));
                    continue;
                }
            }
            produced.push(item);
        }
        produced
    }

    /// The tools a `user` entry's results finish.
    pub(super) fn results(&mut self, entry: &Value) -> Vec<v1::ChatItem> {
        let blocks = entry["message"]["content"]
            .as_array()
            .map_or(&[][..], Vec::as_slice);
        let mut finished = Vec::new();
        for block in blocks {
            if string(block, "type") != "tool_result" {
                continue;
            }
            let Some(mut item) = self.open.remove(string(block, "tool_use_id")) else {
                continue;
            };
            let failed = block["is_error"].as_bool() == Some(true);
            if let Some(tool) = &mut item.tool {
                tool.status = if failed { Status::Failed } else { Status::Ok }.into();
                tool.result_summary = if failed { "failed" } else { "done" }.to_owned();
                tool.output_preview = match &block["content"] {
                    Value::String(text) => text.clone(),
                    Value::Array(parts) => parts
                        .iter()
                        .map(|part| string(part, "text"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                };
            }
            finished.push(item);
        }
        finished
    }

    /// Something is still streaming or running.
    pub(super) fn has_open(&self) -> bool {
        !self.open.is_empty()
    }

    /// Ends everything still streaming or running: the turn was stopped, and
    /// nothing more will be said of them.
    fn stop_open(&mut self) -> Vec<v1::ChatItem> {
        let mut stopped: Vec<_> = std::mem::take(&mut self.open).into_values().collect();
        stopped.iter_mut().for_each(items::stop);
        stopped
    }
}

/// What a `user` entry's text is to the timeline.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Said {
    /// A message of the user's.
    User(String),
    Interrupted,
    /// Something the agent wrote to itself in the user's name.
    Nothing,
}

/// The text of a `user` entry that is not tool results.
pub(super) fn said(entry: &Value) -> Said {
    let content = &entry["message"]["content"];
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => {
            if blocks
                .iter()
                .any(|block| string(block, "type") == "tool_result")
            {
                return Said::Nothing;
            }
            let texts: Vec<&str> = blocks
                .iter()
                .filter(|block| string(block, "type") == "text")
                .map(|block| string(block, "text"))
                .collect();
            texts.join("\n")
        }
        _ => return Said::Nothing,
    };
    let hidden = ["isMeta", "isCompactSummary", "isVisibleInTranscriptOnly"];
    if hidden
        .iter()
        .any(|flag| entry[flag].as_bool() == Some(true))
    {
        return Said::Nothing;
    }
    let text = text.trim();
    if text.starts_with(INTERRUPTED) {
        return Said::Interrupted;
    }
    // A slash command is recorded as markup naming it.
    if let Some(name) = between(text, "<command-name>", "</command-name>") {
        let arguments = between(text, "<command-args>", "</command-args>").unwrap_or_default();
        return Said::User(format!("{name} {arguments}").trim().to_owned());
    }
    // Anything else in markup is the agent's own: a command's output, a
    // reminder, a task's notice.
    let markup = text.strip_prefix('<').is_some_and(|rest| {
        rest.split_once('>').is_some_and(|(name, _)| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
        })
    });
    if markup || text.is_empty() {
        Said::Nothing
    } else {
        Said::User(text.to_owned())
    }
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let (_, rest) = text.split_once(open)?;
    Some(rest.split_once(close)?.0.trim())
}

/// The row a tool call is drawn as.
fn tool_item(id: &str, name: &str, input: &Value) -> v1::ChatItem {
    let field = |name: &str| string(input, name);
    let compact = || {
        if input.as_object().is_some_and(|object| !object.is_empty()) {
            input.to_string()
        } else {
            String::new()
        }
    };
    let mut item = match name {
        "Bash" => items::tool(id, ToolKind::Command, "run", field("command")),
        "Read" => items::tool(id, ToolKind::Read, "read", field("file_path")),
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => {
            let title = if name == "Write" { "write" } else { "edit" };
            let path = if name == "NotebookEdit" {
                field("notebook_path")
            } else {
                field("file_path")
            };
            let mut item = items::tool(id, ToolKind::Edit, title, path);
            if let Some(tool) = &mut item.tool {
                let lines = |name: &str| items::line_count(field(name));
                (tool.added, tool.removed) = match name {
                    "Write" => (lines("content"), 0),
                    "NotebookEdit" => (lines("new_source"), 0),
                    _ => {
                        let edits = input["edits"].as_array().map_or(&[][..], Vec::as_slice);
                        let count = |name: &str| -> u32 {
                            let nested: u32 = edits
                                .iter()
                                .map(|edit| items::line_count(string(edit, name)))
                                .sum();
                            nested + lines(name)
                        };
                        (count("new_string"), count("old_string"))
                    }
                };
            }
            item
        }
        "Grep" | "Glob" => {
            let mut detail = field("pattern").to_owned();
            if !field("path").is_empty() {
                detail = format!("{detail} in {}", field("path"));
            }
            items::tool(id, ToolKind::Search, "search", detail)
        }
        "WebFetch" => items::tool(id, ToolKind::Web, "fetch", field("url")),
        "WebSearch" => items::tool(id, ToolKind::Web, "search", field("query")),
        "Task" | "Agent" => items::tool(id, ToolKind::Subagent, "agent", field("description")),
        "AskUserQuestion" => {
            let question = string(&input["questions"][0], "question");
            items::tool(id, ToolKind::Other, "ask", question)
        }
        "TodoWrite" => items::tool(id, ToolKind::Other, "todo", "Update the task list"),
        _ => match name
            .strip_prefix("mcp__")
            .and_then(|rest| rest.split_once("__"))
        {
            Some((server, tool)) => items::tool(id, ToolKind::Mcp, server, tool),
            None => items::tool(id, ToolKind::Other, &name.to_lowercase(), compact()),
        },
    };
    if let Some(tool) = &mut item.tool
        && matches!(
            name,
            "Read" | "Edit" | "MultiEdit" | "Write" | "NotebookEdit"
        )
    {
        tool.path.clone_from(&tool.detail);
    }
    item
}

/// The id the agent knows a user message by. The agent wants a UUID: an id
/// that is one is used as it is, and any other is turned into one that is
/// always the same for it.
fn message_uuid(item_id: &str) -> String {
    match uuid::Uuid::parse_str(item_id) {
        Ok(uuid) => uuid.to_string(),
        Err(_) => {
            let hash = blake3::hash(item_id.as_bytes());
            let mut bytes = [0; 16];
            bytes.copy_from_slice(&hash.as_bytes()[..16]);
            uuid::Builder::from_random_bytes(bytes)
                .into_uuid()
                .to_string()
        }
    }
}

/// One model as the agent lists it.
struct Model {
    id: String,
    resolved: String,
}

#[derive(Default)]
pub(super) struct Claude {
    launch: Launch,
    session: String,
    rewound_to: String,
    /// The conversation so far has been read for this session.
    history_read: bool,
    /// The agent has answered `initialize` on this link.
    ready: bool,
    ended: bool,
    next_request: u64,
    asked: BTreeMap<String, Asked>,
    /// User messages to send once the agent is ready.
    held: VecDeque<Held>,
    state: v1::ChatState,
    models: Vec<Model>,
    /// The same models, as the catalog lists them.
    listed_models: Vec<v1::ChatModel>,
    commands: Vec<v1::ChatCommand>,
    skills: BTreeSet<String>,
    /// The model last asked for by its id in the catalog.
    chosen_model: String,
    /// Fast was asked for, and whether it took is not yet known.
    asked_fast: bool,
    /// User messages the agent has not finished with, by its id for them.
    /// True for one it has put behind the turn that is running.
    outstanding: BTreeMap<String, bool>,
    /// The agent said it was in a turn when it was reached.
    busy: bool,
    pending: VecDeque<Pending>,
    /// The agent runtime has been told the agent waits on the user.
    reported_blocked: bool,
    /// The newest messages sent: the agent's id for each, and its item's.
    sent: VecDeque<(String, String)>,
    /// Messages sent that the agent has not yet echoed.
    unconfirmed: Vec<v1::ChatItem>,
    reader: Reader,
    rewind: Option<Rewind>,
    notices: u64,
}

type Output = Out<Job>;

impl Claude {
    fn request(&mut self, body: Value, asked: Asked, out: &mut Output) {
        self.next_request += 1;
        let id = format!("muxflow-{}", self.next_request);
        self.asked.insert(id.clone(), asked);
        out.send(json!({"type": "control_request", "request_id": id, "request": body}).to_string());
    }

    fn turn(&self) -> Turn {
        if !self.pending.is_empty() {
            Turn::Waiting
        } else if self.busy || !self.outstanding.is_empty() {
            Turn::Working
        } else {
            Turn::Idle
        }
    }

    fn publish_state(&mut self, out: &mut Output) {
        let turn = self.turn();
        self.state.turn = turn.into();
        self.state.pending = self.pending.front().map(pending_request);
        self.state.native_session_id.clone_from(&self.session);
        out.publish(Change::State(self.state.clone()));
        // No hook of the agent's says it waits on a question, and none says
        // that a turn stopped while it waited.
        let blocked = turn == Turn::Waiting;
        if blocked != self.reported_blocked {
            self.reported_blocked = blocked;
            let event = match turn {
                Turn::Waiting => "PermissionRequest",
                Turn::Idle => "Stop",
                _ => "PostToolUse",
            };
            out.report(json!({"hook_event_name": event, "session_id": self.session}));
        }
    }

    fn notice(&mut self, text: &str, out: &mut Output) {
        self.notices += 1;
        let mut item = items::text(
            format!("notice-{}-{}", super::now_unix_millis(), self.notices),
            Kind::Notice,
            text,
        );
        item.at_unix_millis = super::now_unix_millis();
        out.publish(Change::Item(item));
    }

    fn learn_session(&mut self, session: &str, out: &mut Output) {
        if session.is_empty() || session == self.session || !is_session_id(session) {
            return;
        }
        self.session = session.to_owned();
        out.job(Job::Tag {
            pane_id: out.pane_id().to_owned(),
            session: self.session.clone(),
        });
        if !std::mem::replace(&mut self.history_read, true) {
            out.job(Job::History {
                pane_id: out.pane_id().to_owned(),
                session: self.session.clone(),
                rewound_to: String::new(),
            });
        }
    }

    fn publish_catalog(&self, out: &mut Output) {
        // Muxflow's own come first: a list too long to send whole is cut at
        // its end.
        let mut commands = Vec::from(super::muxflow_commands());
        commands.extend(self.commands.iter().cloned().map(|mut command| {
            if self.skills.contains(&command.name) {
                command.kind = v1::ChatCommandKind::Skill.into();
            }
            command
        }));
        out.publish(Change::Catalog(v1::ChatCatalog {
            capabilities: None,
            models: self.listed_models.clone(),
            commands,
        }));
    }

    fn initialized(&mut self, response: &Value, out: &mut Output) {
        let list = |name: &str| response[name].as_array().map_or(&[][..], Vec::as_slice);
        if response["models"].is_array() {
            self.models.clear();
            self.listed_models.clear();
            for model in list("models") {
                let id = string(model, "value");
                if id.is_empty() {
                    continue;
                }
                self.models.push(Model {
                    id: id.to_owned(),
                    resolved: string(model, "resolvedModel").to_owned(),
                });
                self.listed_models.push(v1::ChatModel {
                    id: id.to_owned(),
                    label: string(model, "displayName").to_owned(),
                    efforts: model["supportedEffortLevels"]
                        .as_array()
                        .map_or(&[][..], Vec::as_slice)
                        .iter()
                        .filter_map(|effort| effort.as_str().map(str::to_owned))
                        .collect(),
                    supports_fast: model["supportsFastMode"].as_bool() == Some(true),
                });
            }
        }
        if response["commands"].is_array() {
            self.commands = list("commands").iter().map(command).collect();
        }
        self.publish_catalog(out);

        let options = self.state.options.get_or_insert_default();
        options.permission = permission(string(response, "current_permission_mode")).into();
        let fast = string(response, "fast_mode_state") == "on";
        options.speed = if fast { Speed::Fast } else { Speed::Standard }.into();
        if std::mem::take(&mut self.asked_fast) && !fast {
            let reason = string(response, "fast_mode_disabled_reason");
            let text = if reason.is_empty() {
                "Fast mode is not available.".to_owned()
            } else {
                format!("Fast mode is not available ({reason}).")
            };
            self.notice(&text, out);
        }
        if !self.ready {
            self.ready = true;
            self.state.connection = Connection::Ready.into();
            self.busy = matches!(
                string(response, "session_state"),
                "running" | "requires_action"
            );
        }
        // What the agent was waiting on before this link, as it first asked.
        for name in [
            "pending_permission_requests",
            "pending_user_dialog_requests",
        ] {
            for request in list(name) {
                self.asked_by_agent(request, out);
            }
        }
        if self.rewind.is_none() {
            self.send_held(out);
        }
        self.publish_state(out);
    }

    fn send_held(&mut self, out: &mut Output) {
        for held in std::mem::take(&mut self.held) {
            self.outstanding.insert(held.uuid, false);
            out.send(held.message);
        }
    }

    /// The model, among the catalog's, that `resolved` is.
    fn model_id(&self, resolved: &str) -> String {
        let chosen = |model: &&Model| {
            model.id == self.chosen_model && (model.resolved == resolved || model.id == resolved)
        };
        let listed = |model: &&Model| model.resolved == resolved || model.id == resolved;
        self.models
            .iter()
            .find(chosen)
            .or_else(|| self.models.iter().find(listed))
            .map_or_else(|| resolved.to_owned(), |model| model.id.clone())
    }

    fn asked_by_agent(&mut self, message: &Value, out: &mut Output) {
        let request_id = string(message, "request_id");
        let request = &message["request"];
        if request_id.is_empty()
            || self
                .pending
                .iter()
                .any(|pending| pending.request_id == request_id)
        {
            return;
        }
        if string(request, "subtype") != "can_use_tool" {
            // Nothing else the agent may ask is something a chat can answer.
            out.send(
                json!({"type": "control_response", "response": {
                    "subtype": "error", "request_id": request_id,
                    "error": "Not supported by this client."}})
                .to_string(),
            );
            return;
        }
        let input = &request["input"];
        let detail = ["command", "file_path", "notebook_path", "url", "pattern"]
            .iter()
            .map(|name| string(input, name))
            .find(|value| !value.is_empty())
            .map(str::to_owned)
            .or_else(|| Some(string(request, "description").to_owned()))
            .filter(|detail| !detail.is_empty())
            .unwrap_or_else(|| input.to_string());
        let tool = string(request, "tool_name");
        let display = string(request, "display_name");
        self.pending.push_back(Pending {
            request_id: request_id.to_owned(),
            tool: tool.to_owned(),
            input: input.clone(),
            title: if display.is_empty() { tool } else { display }.to_owned(),
            detail,
        });
    }

    fn answer(
        &mut self,
        request_id: &str,
        answer: &v1::ChatAnswer,
        out: &mut Output,
    ) -> Result<(), ChatError> {
        let front = self
            .pending
            .front()
            .filter(|pending| pending.request_id == request_id)
            .ok_or_else(|| {
                ChatError::new(
                    "chat_request_not_pending",
                    "The agent is no longer waiting for that answer.",
                )
            })?;
        let deny = |message: &str| json!({"behavior": "deny", "message": message});
        let decision = if front.tool == "AskUserQuestion" {
            if answer.skipped {
                deny("The user declined to answer.")
            } else {
                let mut input = front.input.clone();
                input["answers"] = question_answers(&front.input, &answer.answers);
                json!({"behavior": "allow", "updatedInput": input})
            }
        } else if answer.allow {
            json!({"behavior": "allow", "updatedInput": front.input})
        } else {
            deny("The user denied this action.")
        };
        out.send(
            json!({"type": "control_response", "response": {
                "subtype": "success", "request_id": request_id, "response": decision}})
            .to_string(),
        );
        self.pending.pop_front();
        self.publish_state(out);
        Ok(())
    }

    fn set_options(&mut self, wanted: &v1::ChatOptions, out: &mut Output) {
        let current = self.state.options.clone().unwrap_or_default();
        let setting =
            |settings: Value| json!({"subtype": "apply_flag_settings", "settings": settings});
        if wanted.model != current.model {
            self.chosen_model.clone_from(&wanted.model);
            let request = json!({"subtype": "set_model", "model": wanted.model});
            self.request(request, Asked::Other, out);
        }
        if wanted.effort != current.effort && !wanted.effort.is_empty() {
            self.request(
                setting(json!({"effortLevel": wanted.effort})),
                Asked::Other,
                out,
            );
        }
        if wanted.speed != current.speed {
            let fast = wanted.speed == i32::from(Speed::Fast);
            self.asked_fast = fast;
            self.request(setting(json!({"fastMode": fast})), Asked::Other, out);
        }
        if wanted.permission != current.permission {
            let mode = if wanted.permission == i32::from(Permission::Full) {
                "bypassPermissions"
            } else {
                "auto"
            };
            let request = json!({"subtype": "set_permission_mode", "mode": mode});
            self.request(request, Asked::Other, out);
        }
        // What the agent ended up with is what the chat shows.
        self.request(json!({"subtype": "initialize"}), Asked::Initialize, out);
        self.request(json!({"subtype": "get_settings"}), Asked::Settings, out);
    }

    fn start_rewind(&mut self, out: &mut Output) {
        let Some(rewind) = &mut self.rewind else {
            return;
        };
        rewind.restarting = true;
        let mut launch = self.launch.clone();
        let options = self.state.options.clone().unwrap_or_default();
        // The agent starts again as it is now, not as it was first started.
        launch.model.clone_from(&options.model);
        launch.effort.clone_from(&options.effort);
        launch.full_access = options.permission == i32::from(Permission::Full);
        out.job(Job::Rewind {
            pane_id: out.pane_id().to_owned(),
            session: self.session.clone(),
            uuid: rewind.uuid.clone(),
            launch: Box::new(launch),
        });
    }

    fn stream_event(&mut self, message: &Value, out: &mut Output) {
        let event = &message["event"];
        let message_id = string(message, "api_message_id");
        let index = event["index"]
            .as_u64()
            .and_then(|index| u32::try_from(index).ok());
        match (string(event, "type"), index) {
            ("content_block_start", Some(index)) if !message_id.is_empty() => {
                self.reader.block = Some(Block {
                    message: message_id.to_owned(),
                    index,
                    claimed: false,
                });
                let block = &event["content_block"];
                let mut item = match string(block, "type") {
                    "text" => items::text(
                        format!("{message_id}:{index}"),
                        Kind::Assistant,
                        string(block, "text"),
                    ),
                    "tool_use" => tool_item(string(block, "id"), string(block, "name"), &json!({})),
                    // Thinking is shown once there is some to show.
                    _ => return,
                };
                item.streaming = item.tool.is_none();
                item.at_unix_millis = super::now_unix_millis();
                self.reader.open.insert(item.id.clone(), item.clone());
                out.publish(Change::Item(item));
            }
            ("content_block_delta", Some(index)) if !message_id.is_empty() => {
                let delta = &event["delta"];
                let (kind, text) = match string(delta, "type") {
                    "text_delta" => (Kind::Assistant, string(delta, "text")),
                    "thinking_delta" => (Kind::Reasoning, string(delta, "thinking")),
                    _ => return,
                };
                if text.is_empty() {
                    return;
                }
                let id = format!("{message_id}:{index}");
                match self.reader.open.get_mut(&id) {
                    Some(item) => {
                        item.text.push_str(text);
                        out.publish(Change::Delta(v1::ChatItemDelta {
                            item_id: id,
                            text_append: text.to_owned(),
                        }));
                    }
                    // Its start was before this link, or it is thinking. The
                    // message that ends it takes this block's place, also
                    // when the conversation that was read already has it.
                    None => {
                        self.reader.block = Some(Block {
                            message: message_id.to_owned(),
                            index,
                            claimed: false,
                        });
                        let mut item = items::text(id.clone(), kind, text);
                        item.streaming = true;
                        item.at_unix_millis = super::now_unix_millis();
                        self.reader.open.insert(id, item.clone());
                        out.publish(Change::Item(item));
                    }
                }
            }
            _ => {}
        }
    }

    fn user(&mut self, message: &Value, out: &mut Output) {
        for item in self.reader.results(message) {
            out.publish(Change::Item(item));
        }
        let uuid = string(message, "uuid");
        let at = items::unix_millis(string(message, "timestamp"));
        match said(message) {
            Said::User(text) if message["isReplay"].as_bool() == Some(true) => {
                let sent = self.sent.iter().find(|(sent, _)| sent == uuid);
                let id = sent.map_or(uuid, |(_, id)| id.as_str()).to_owned();
                self.unconfirmed.retain(|item| item.id != id);
                let mut item = items::text(id, Kind::User, text);
                item.rewindable = true;
                item.at_unix_millis = at;
                out.publish(Change::Item(item));
            }
            Said::Interrupted if !uuid.is_empty() => {
                let mut item = items::text(uuid, Kind::Notice, "Interrupted");
                item.at_unix_millis = at;
                out.publish(Change::Item(item));
            }
            _ => {}
        }
    }

    fn system(&mut self, message: &Value, out: &mut Output) {
        match string(message, "subtype") {
            "init" => {
                let names = |name: &str| -> BTreeSet<String> {
                    message[name]
                        .as_array()
                        .map_or(&[][..], Vec::as_slice)
                        .iter()
                        .filter_map(|name| name.as_str().map(str::to_owned))
                        .collect()
                };
                let skills = names("skills");
                if skills != self.skills {
                    self.skills = skills;
                    self.publish_catalog(out);
                }
                let resolved = string(message, "model");
                let model = self.model_id(resolved);
                let options = self.state.options.get_or_insert_default();
                if !resolved.is_empty() {
                    options.model = model;
                }
                let mode = string(message, "permissionMode");
                if !mode.is_empty() {
                    options.permission = permission(mode).into();
                }
                self.publish_state(out);
            }
            "status" => {
                let mode = string(message, "permissionMode");
                if !mode.is_empty() {
                    self.state.options.get_or_insert_default().permission = permission(mode).into();
                    self.publish_state(out);
                }
            }
            "commands_changed" => {
                if let Some(commands) = message["commands"].as_array() {
                    self.commands = commands.iter().map(command).collect();
                    self.publish_catalog(out);
                }
            }
            "compact_boundary" => {
                let uuid = string(message, "uuid");
                if !uuid.is_empty() {
                    let mut item = items::text(uuid, Kind::Notice, "Conversation compacted.");
                    item.at_unix_millis = super::now_unix_millis();
                    out.publish(Change::Item(item));
                }
            }
            _ => {}
        }
    }

    fn result(&mut self, message: &Value, out: &mut Output) {
        self.busy = false;
        // A turn has ended, and with it every message but the ones the agent
        // said it is keeping for the next.
        self.outstanding.retain(|_, queued| *queued);
        // Whatever was cut short will say nothing more.
        for item in self.reader.stop_open() {
            out.publish(Change::Item(item));
        }
        self.pending.clear();
        let waited = self.reported_blocked;
        let stopped = string(message, "terminal_reason").starts_with("aborted");
        if message["is_error"].as_bool() == Some(true) && !stopped {
            let text = match string(message, "result") {
                "" => "The turn ended with an error.",
                text => text,
            };
            self.notice(text, out);
        }
        self.publish_state(out);
        // No hook of the agent's says that a turn was stopped.
        if stopped && !waited && self.turn() == Turn::Idle {
            out.report(json!({"hook_event_name": "Stop", "session_id": self.session}));
        }
        if self
            .rewind
            .as_ref()
            .is_some_and(|rewind| !rewind.restarting)
        {
            if self.turn() == Turn::Idle {
                self.start_rewind(out);
            } else {
                // A message the agent had put behind that turn is not to
                // run either.
                self.request(json!({"subtype": "interrupt"}), Asked::Other, out);
            }
            return;
        }
        // The turn may have changed what the options are, and has changed
        // how full the context is.
        self.request(json!({"subtype": "get_context_usage"}), Asked::Usage, out);
        self.request(json!({"subtype": "get_settings"}), Asked::Settings, out);
    }

    fn answered(&mut self, message: &Value, out: &mut Output) {
        let response = &message["response"];
        let Some(asked) = self.asked.remove(string(response, "request_id")) else {
            return;
        };
        if string(response, "subtype") != "success" {
            // An answer the agent could not give leaves things as they were;
            // `initialize` must still let the chat go on.
            if matches!(asked, Asked::Initialize) && !self.ready {
                self.initialized(&Value::Null, out);
            }
            return;
        }
        let response = &response["response"];
        match asked {
            Asked::Initialize => {
                self.initialized(response, out);
                if self.state.usage.is_none() {
                    self.request(json!({"subtype": "get_context_usage"}), Asked::Usage, out);
                }
                self.request(json!({"subtype": "get_settings"}), Asked::Settings, out);
            }
            Asked::Settings => {
                let applied = &response["applied"];
                let model = self.model_id(string(applied, "model"));
                let options = self.state.options.get_or_insert_default();
                if !string(applied, "model").is_empty() {
                    options.model = model;
                }
                if !string(applied, "effort").is_empty() {
                    options.effort = string(applied, "effort").to_owned();
                }
                self.publish_state(out);
            }
            Asked::Usage => {
                let tokens = |name: &str| response[name].as_u64().unwrap_or(0);
                self.state.usage = Some(v1::ChatUsage {
                    used_tokens: tokens("totalTokens"),
                    window_tokens: tokens("maxTokens"),
                });
                self.publish_state(out);
            }
            Asked::Other => {}
        }
    }
}

fn permission(mode: &str) -> Permission {
    if mode == "bypassPermissions" {
        Permission::Full
    } else {
        Permission::Auto
    }
}

fn command(command: &Value) -> v1::ChatCommand {
    v1::ChatCommand {
        name: string(command, "name").to_owned(),
        description: string(command, "description").to_owned(),
        argument_hint: string(command, "argumentHint").to_owned(),
        kind: v1::ChatCommandKind::Command.into(),
    }
}

fn pending_request(pending: &Pending) -> v1::ChatPendingRequest {
    let questions = pending.input["questions"]
        .as_array()
        .filter(|_| pending.tool == "AskUserQuestion");
    let Some(questions) = questions else {
        return v1::ChatPendingRequest {
            request_id: pending.request_id.clone(),
            kind: v1::ChatRequestKind::Permission.into(),
            title: pending.title.clone(),
            detail: pending.detail.clone(),
            questions: Vec::new(),
        };
    };
    let question = |(index, question): (usize, &Value)| v1::ChatQuestion {
        id: index.to_string(),
        header: string(question, "header").to_owned(),
        prompt: string(question, "question").to_owned(),
        options: question["options"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .enumerate()
            .map(|(index, option)| v1::ChatOption {
                id: index.to_string(),
                label: string(option, "label").to_owned(),
                description: string(option, "description").to_owned(),
            })
            .collect(),
        multi_select: question["multiSelect"].as_bool() == Some(true),
        // The agent always takes an answer in the user's own words.
        allow_other: true,
    };
    v1::ChatPendingRequest {
        request_id: pending.request_id.clone(),
        kind: v1::ChatRequestKind::Question.into(),
        title: "Question".to_owned(),
        detail: String::new(),
        questions: questions.iter().enumerate().map(question).collect(),
    }
}

/// The answers as the agent's question tool takes them: each question's
/// text, and what was chosen for it.
fn question_answers(input: &Value, answers: &[v1::ChatQuestionAnswer]) -> Value {
    let questions = input["questions"].as_array().map_or(&[][..], Vec::as_slice);
    let mut given = serde_json::Map::new();
    for answer in answers {
        let Some(question) = answer
            .question_id
            .parse::<usize>()
            .ok()
            .and_then(|index| questions.get(index))
        else {
            continue;
        };
        let mut chosen: Vec<&str> = answer
            .option_ids
            .iter()
            .filter_map(|id| id.parse::<usize>().ok())
            .filter_map(|index| question["options"].get(index))
            .map(|option| string(option, "label"))
            .collect();
        if !answer.other_text.trim().is_empty() {
            chosen.push(answer.other_text.trim());
        }
        given.insert(
            string(question, "question").to_owned(),
            Value::String(chosen.join(", ")),
        );
    }
    Value::Object(given)
}

impl Dialect for Claude {
    const FRAMING: Framing = Framing::Lines;
    type Job = Job;
    type Done = Done;

    fn run(job: Job) -> Done {
        match job {
            Job::History {
                pane_id,
                session,
                rewound_to,
            } => Done::History(history::load(
                &configuration(&pane_id),
                &session,
                &rewound_to,
            )),
            Job::Tag { pane_id, session } => {
                let _ = pane::set(&pane_id, &[(pane::SESSION, &session)]);
                Done::Tagged
            }
            Job::Rewind {
                pane_id,
                session,
                uuid,
                mut launch,
            } => Done::Rewound((|| {
                let kept = history::parent_of(&configuration(&pane_id), &session, &uuid)
                    .ok_or("That message is not in the session's record yet.")?;
                match &kept {
                    Some(kept) => {
                        launch.resume = Resume::Session {
                            id: session,
                            fork: false,
                        };
                        launch.resume_at.clone_from(kept);
                    }
                    // Nothing came before it: the conversation starts over.
                    None => {
                        launch.resume = Resume::Fresh;
                        launch.resume_at.clear();
                    }
                }
                // A conversation that starts over is a new session, which
                // the message is not in.
                let dropped = if kept.is_some() { uuid.as_str() } else { "" };
                convert::restart_agent(&pane_id, &ClaudeProvider, &launch, dropped)
                    .map_err(|error| format!("The agent could not be started again: {error}"))
            })()),
        }
    }

    fn done(&mut self, done: Done, out: &mut Output) {
        match done {
            Done::History(mut history) => {
                // What was sent and not yet recorded is still part of it.
                // The record names a message by the id the agent was given
                // for it.
                for item in &self.unconfirmed {
                    let uuid = message_uuid(&item.id);
                    let recorded = |read: &v1::ChatItem| read.id == item.id || read.id == uuid;
                    if !history.items.iter().any(recorded) {
                        history.items.push(item.clone());
                    }
                }
                // A conversation that was resumed has earlier messages,
                // wherever the record of them is.
                if history.items.is_empty() && self.launch.resume != Resume::Fresh {
                    history.complete = false;
                }
                self.reader = history.reader;
                self.state.history_complete = history.complete;
                out.reset(history.items, history.complete);
            }
            Done::Tagged => {}
            Done::Rewound(outcome) => {
                let Some(rewind) = self.rewind.take() else {
                    return;
                };
                match outcome {
                    Ok(()) => {
                        // A message sent meanwhile is the next one of the
                        // conversation as it now is, and goes to the agent
                        // that is starting.
                        let held = &self.held;
                        self.unconfirmed
                            .retain(|item| held.iter().any(|held| held.item_id == item.id));
                        self.outstanding.clear();
                        out.publish(Change::Truncate(v1::ChatTruncate {
                            after_item_id: rewind.after_item_id,
                        }));
                        for item in &self.unconfirmed {
                            out.publish(Change::Item(item.clone()));
                        }
                        out.reconnect();
                    }
                    Err(reason) => {
                        self.notice(&reason, out);
                        self.send_held(out);
                        self.publish_state(out);
                    }
                }
            }
        }
    }

    fn connected(&mut self, tag: &Tag, out: &mut Output) {
        self.launch = tag.launch.clone();
        self.session.clone_from(&tag.session);
        self.rewound_to.clone_from(&tag.rewound_to);
        if self.chosen_model.is_empty() {
            self.chosen_model.clone_from(&self.launch.model);
        }
        self.ready = false;
        self.asked.clear();
        self.pending.clear();
        // A message that waits for this agent is still on its way.
        let held = &self.held;
        self.outstanding
            .retain(|uuid, _| held.iter().any(|held| held.uuid == *uuid));
        self.busy = false;
        // A rewind that waited for the turn to stop waits for a word that
        // may have been said to nobody. One that is starting the agent again
        // is what made this link, and ends by itself.
        if self
            .rewind
            .as_ref()
            .is_some_and(|rewind| !rewind.restarting)
        {
            self.rewind = None;
        }
        self.reader = Reader::default();
        // A session started under a name goes by it.
        out.name(&self.launch.name);
        // A request the agent waited on is asked again when it answers:
        // losing the link is no word that the agent stopped waiting.
        self.reported_blocked = false;
        self.state.connection = Connection::Connecting.into();
        self.state.history_complete = true;
        self.publish_state(out);
        self.history_read = !self.session.is_empty();
        if self.history_read {
            out.job(Job::History {
                pane_id: out.pane_id().to_owned(),
                session: self.session.clone(),
                rewound_to: self.rewound_to.clone(),
            });
        }
        self.request(json!({"subtype": "initialize"}), Asked::Initialize, out);
    }

    fn message(&mut self, message: &str, out: &mut Output) {
        let Ok(message) = serde_json::from_str::<Value>(message) else {
            return;
        };
        // What a subagent says is its own conversation, not this one.
        let nested = !message["parent_tool_use_id"].is_null();
        if !nested {
            self.learn_session(string(&message, "session_id"), out);
        }
        match string(&message, "type") {
            "stream_event" if !nested => self.stream_event(&message, out),
            "assistant" if !nested => {
                for item in self.reader.assistant(&message) {
                    out.publish(Change::Item(item));
                }
            }
            "user" if !nested => self.user(&message, out),
            "system" => self.system(&message, out),
            "command_lifecycle" => {
                let uuid = string(&message, "command_uuid").to_owned();
                match string(&message, "state") {
                    "queued" => {
                        self.outstanding.insert(uuid, true);
                    }
                    "started" => {
                        self.outstanding.insert(uuid, false);
                    }
                    "completed" | "cancelled" => {
                        self.outstanding.remove(&uuid);
                    }
                    _ => return,
                }
                self.publish_state(out);
            }
            "result" => self.result(&message, out),
            "control_request" => {
                self.asked_by_agent(&message, out);
                self.publish_state(out);
            }
            "control_cancel_request" => {
                let request_id = string(&message, "request_id");
                self.pending
                    .retain(|pending| pending.request_id != request_id);
                self.publish_state(out);
            }
            "control_response" => self.answered(&message, out),
            _ => {}
        }
    }

    fn command(&mut self, command: SessionCommand, out: &mut Output) -> Result<(), ChatError> {
        if self.ended {
            return Err(ChatError::new("chat_unavailable", "The agent has exited."));
        }
        let not_ready =
            || ChatError::new("chat_not_ready", "The chat is still connecting. Try again.");
        match command {
            SessionCommand::Send { text, item_id } => {
                let waits = !self.ready || self.rewind.is_some();
                if waits && self.held.len() >= MAX_HELD {
                    return Err(not_ready());
                }
                // A skill named with `$` at the start is one the agent takes
                // with a slash. Which of its commands are skills it says only
                // in its first turn, so any of them counts. Elsewhere in a
                // message it has no way to be handed one, and reads the name
                // as text.
                let text = match text.strip_prefix('$') {
                    Some(rest) if self.commands.iter().any(|command| {
                        command.name == rest.split(char::is_whitespace).next().unwrap_or(rest)
                    }) =>
                    {
                        format!("/{rest}")
                    }
                    _ => text,
                };
                let uuid = message_uuid(&item_id);
                let message = json!({
                    "type": "user",
                    "message": {"role": "user", "content": text},
                    "uuid": uuid,
                    "parent_tool_use_id": null,
                })
                .to_string();
                let mut item = items::text(item_id.clone(), Kind::User, text);
                item.rewindable = true;
                item.at_unix_millis = super::now_unix_millis();
                if self.unconfirmed.len() >= MAX_SENT {
                    self.unconfirmed.remove(0);
                }
                self.unconfirmed.push(item.clone());
                out.publish(Change::Item(item));
                if self.sent.len() >= MAX_SENT {
                    self.sent.pop_front();
                }
                self.sent.push_back((uuid.clone(), item_id.clone()));
                self.outstanding.insert(uuid.clone(), false);
                self.publish_state(out);
                if waits {
                    self.held.push_back(Held {
                        item_id,
                        uuid,
                        message,
                    });
                } else {
                    out.send(message);
                }
                Ok(())
            }
            SessionCommand::Interrupt => {
                if !self.ready {
                    return Err(not_ready());
                }
                if self.turn() != Turn::Idle {
                    self.request(json!({"subtype": "interrupt"}), Asked::Other, out);
                }
                Ok(())
            }
            SessionCommand::Answer { request_id, answer } => self.answer(&request_id, &answer, out),
            SessionCommand::Rewind {
                item_id,
                after_item_id,
            } => {
                if !self.ready || self.session.is_empty() {
                    return Err(not_ready());
                }
                if self.rewind.is_some() {
                    return Err(ChatError::new(
                        "chat_turn_busy",
                        "The chat is already going back to an earlier message.",
                    ));
                }
                self.rewind = Some(Rewind {
                    uuid: message_uuid(&item_id),
                    after_item_id,
                    restarting: false,
                });
                if self.turn() == Turn::Idle {
                    self.start_rewind(out);
                } else {
                    // The turn's `result` is what says it has stopped.
                    self.request(json!({"subtype": "interrupt"}), Asked::Other, out);
                }
                Ok(())
            }
            SessionCommand::SetOptions(options) => {
                if !self.ready {
                    return Err(not_ready());
                }
                self.set_options(&options, out);
                Ok(())
            }
        }
    }

    fn disconnected(&mut self, out: &mut Output) {
        self.ready = false;
        self.state.connection = Connection::Connecting.into();
        self.publish_state(out);
    }

    fn ended(&mut self, failure: &str, out: &mut Output) {
        self.ended = true;
        self.ready = false;
        self.busy = false;
        self.outstanding.clear();
        self.pending.clear();
        self.held.clear();
        self.rewind = None;
        for item in self.reader.stop_open() {
            out.publish(Change::Item(item));
        }
        self.state.connection = if failure.is_empty() {
            Connection::Ended
        } else {
            Connection::Failed
        }
        .into();
        self.state.failure = failure.to_owned();
        self.publish_state(out);
    }
}

#[cfg(test)]
mod tests;
