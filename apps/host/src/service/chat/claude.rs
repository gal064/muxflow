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
//! rewound (`--resume-session-at`). A reply is forked the same way, in a
//! pane of its own: a copy of the session (`--fork-session`) cut after the
//! reply.

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
    attach::{self, Attached},
    convert::{self, Arguments, Takes},
    items::{self, string},
    naming::{self, Naming},
    pane::{self, Tag},
    provider::{
        Change, ChatError, ChatProvider, ChatSession, ChatSink, ForkAt, ForkPlan, Launch,
        PaneCommand, Resume, SessionCommand, still_writing,
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
/// How the summary the agent writes after compacting starts. It comes as a
/// user message, and is not one.
const COMPACT_SUMMARY: &str = "This session is being continued from a previous conversation";
/// Tasks the agent has said started and not yet said ended, remembered for
/// when they started and which row they belong to.
const MAX_TASKS: usize = 256;
/// What has the agent answer one prompt, given on its stdin, with a title:
/// in plan mode, so that it changes nothing, leaving no session behind, and
/// running none of the user's hooks (with hooks, 2.1.326 runs `SessionStart`
/// even for this; without, it does not). The small model is named by its
/// alias, which every account has.
const TITLE_ARGUMENTS: &[&str] = &[
    "-p",
    "--output-format",
    "text",
    "--model",
    "haiku",
    "--permission-mode",
    "plan",
    "--no-session-persistence",
    "--settings",
    r#"{"disableAllHooks":true}"#,
    // Given no MCP configuration, it starts none of the user's servers.
    "--strict-mcp-config",
];
/// How long one ask for a title may take. Mostly the agent starting.
const TITLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
            attachments: true,
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
                // A reply's fork keeps the conversation through that reply.
                // Taken in print mode only (verified on 2.1.326), which is
                // what runs here; a side question's fork has no cut.
                if !launch.resume_at.is_empty() {
                    push(&mut argv, &["--resume-session-at", &launch.resume_at]);
                }
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
    /// `stop_task`: a refusal is said in the chat.
    StopTask,
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
        /// A copy cut at a reply: the session it copies and the entry it
        /// was cut at, read while the copy's own record is empty. The agent
        /// writes none until something is said in it.
        copied: Option<(String, String)>,
    },
    /// Note the session's id on the pane, for the daemon that comes next.
    Tag { pane_id: String, session: String },
    Rewind {
        pane_id: String,
        session: String,
        uuid: String,
        launch: Box<Launch>,
    },
    /// Ask the agent for a title, beside the chat.
    Title(naming::Ask),
}

pub(super) enum Done {
    History(history::History),
    Tagged,
    Rewound(Result<(), String>),
    Titled(naming::Titled),
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
    /// The item id of a thinking block that has started and has no item yet.
    thinking: Option<String>,
    /// Thinking items that ended with no text, to be taken back.
    silent: Vec<v1::ChatItem>,
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
                if self.thinking.as_deref() == Some(item.id.as_str()) {
                    self.thinking = None;
                }
                // Thinking the agent keeps to itself arrives as an empty
                // block: what was streamed of it, if anything, is all of it.
                // An item that only said "thinking" while it lasted goes.
                if item.text.is_empty() {
                    if let Some(mut streamed) = streamed {
                        streamed.streaming = false;
                        if streamed.text.is_empty() {
                            self.silent.push(streamed);
                        } else {
                            produced.push(streamed);
                        }
                    }
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
        self.thinking = None;
        let (silent, mut stopped): (Vec<_>, Vec<_>) = std::mem::take(&mut self.open)
            .into_values()
            .partition(is_silent_thinking);
        self.silent.extend(silent);
        stopped.iter_mut().for_each(items::stop);
        stopped
    }

    /// Ends the thinking item `id` if it has said nothing: its block is over.
    fn end_thinking(&mut self, id: &str) {
        if self.thinking.as_deref() == Some(id) {
            self.thinking = None;
        }
        if self.open.get(id).is_some_and(is_silent_thinking)
            && let Some(item) = self.open.remove(id)
        {
            self.silent.push(item);
        }
    }
}

/// A thinking item that has no text: one made only to say that the agent
/// thinks.
fn is_silent_thinking(item: &v1::ChatItem) -> bool {
    item.kind == i32::from(Kind::Reasoning) && item.text.is_empty()
}

/// What a `user` entry's text is to the timeline.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Said {
    /// A message of the user's, and the files sent with it.
    User(String, Vec<v1::ChatAttachment>),
    Interrupted,
    /// Something the agent wrote to itself in the user's name.
    Nothing,
}

/// A user message as the agent takes it: the text alone, or with files a
/// text block and an image block each, verified on Claude Code 2.1.326. An
/// image too large to go inline, and any other file, is named by its path
/// under the text (D9).
fn content(text: &str, attachments: &[Attached]) -> Value {
    if attachments.is_empty() {
        return Value::String(text.to_owned());
    }
    let named = attachments
        .iter()
        .filter(|attached| attached.inline.is_none())
        .map(|attached| attached.path.as_str());
    let text = attach::with_paths(text, named);
    let mut blocks = Vec::new();
    // The agent refuses a text block of nothing but blank space.
    if !text.trim().is_empty() {
        blocks.push(json!({"type": "text", "text": text}));
    }
    for attached in attachments {
        if let Some(data) = &attached.inline {
            blocks.push(json!({"type": "image", "source": {
                "type": "base64", "media_type": attached.media_type, "data": data,
            }}));
        }
    }
    Value::Array(blocks)
}

/// The text of a `user` entry that is not tool results, and the files it was
/// sent with: an image the record holds inline, which has kept no name, and
/// the staged paths named under the text.
pub(super) fn said(entry: &Value) -> Said {
    said_in(entry, attach::staging_directory().as_deref())
}

/// [`said`], with the paths under the text read as attachments when they lie
/// in `staging`.
fn said_in(entry: &Value, staging: Option<&std::path::Path>) -> Said {
    let content = &entry["message"]["content"];
    let mut attachments = Vec::new();
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
            for block in blocks
                .iter()
                .filter(|block| string(block, "type") == "image")
            {
                let source = &block["source"];
                attachments.push(attach::inline_image(
                    string(source, "media_type"),
                    string(source, "data").len(),
                ));
            }
            texts.join("\n")
        }
        _ => return Said::Nothing,
    };
    let (text, named) = match staging {
        Some(staging) => attach::split_paths_in(&text, staging),
        None => (text, Vec::new()),
    };
    // The record's order: what was named under the text, then the images.
    let attachments: Vec<v1::ChatAttachment> = named.into_iter().chain(attachments).collect();
    // What the agent wrote in the user's name. A background subagent's
    // report comes so: `isSynthetic` on the stream, `isMeta` in the record
    // (2.1.326).
    let hidden = [
        "isMeta",
        "isSynthetic",
        "isCompactSummary",
        "isVisibleInTranscriptOnly",
    ];
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
    if text.starts_with(COMPACT_SUMMARY) {
        return Said::Nothing;
    }
    // A slash command is recorded as markup naming it.
    if let Some(name) = between(text, "<command-name>", "</command-name>") {
        let arguments = between(text, "<command-args>", "</command-args>").unwrap_or_default();
        return Said::User(format!("{name} {arguments}").trim().to_owned(), attachments);
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
    if markup || (text.is_empty() && attachments.is_empty()) {
        Said::Nothing
    } else {
        Said::User(text.to_owned(), attachments)
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
        "Task" | "Agent" => {
            let mut item = items::tool(id, ToolKind::Subagent, "agent", field("description"));
            if let Some(tool) = &mut item.tool {
                field("subagent_type").clone_into(&mut tool.agent_type);
                tool.background = input["run_in_background"].as_bool() == Some(true);
            }
            item
        }
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

/// The launch of a copy of `session` that ends where the reply before the
/// message `next` ends, or with the newest message when there is none. The
/// cut is the message before `next`, the one a rewind of `next` goes back
/// to.
///
/// A copy nothing has been said in has no record yet: what it shows is its
/// source's up to the cut, so it is forked as a copy of that.
fn fork_launch(
    configuration: &std::path::Path,
    session: &str,
    copied: Option<(String, String)>,
    next: Option<&str>,
    mut launch: Launch,
) -> Result<Launch, ChatError> {
    let (session, cut) = recorded_in(configuration, session, copied);
    let through = match next {
        Some(next) => match history::message_before(configuration, &session, next) {
            Some(Some(entry)) => entry,
            // The agent has not taken the next message yet: it may still be
            // adding to the reply.
            None => return Err(still_writing()),
            Some(None) => {
                return Err(ChatError::new(
                    "chat_fork_unavailable",
                    "That reply cannot be forked.",
                ));
            }
        },
        None => match cut {
            Some(cut) => cut,
            None => history::last_message(configuration, &session).ok_or_else(nothing_to_fork)?,
        },
    };
    launch.resume = Resume::Session {
        id: session,
        fork: true,
    };
    launch.resume_at = through;
    Ok(launch)
}

/// The session whose record holds what `session` shows: its own, or for a
/// copy with no record yet (`copied`), its source's, with the entry the copy
/// was cut at.
fn recorded_in(
    configuration: &std::path::Path,
    session: &str,
    copied: Option<(String, String)>,
) -> (String, Option<String>) {
    match copied {
        Some((source, cut)) if !history::has_record(configuration, session) => (source, Some(cut)),
        _ => (session.to_owned(), None),
    }
}

/// Sets `launch` to start the agent again for a rewind of the message `uuid`
/// of `session`: on the session, cut before the message, or as a new
/// conversation when nothing came before it. A copy with no record yet is
/// cut from its source's record instead, as a new copy. Answers the message
/// the record that goes on still holds, for its reader to leave out.
fn rewind_launch(
    configuration: &std::path::Path,
    session: &str,
    uuid: &str,
    launch: &mut Launch,
) -> Result<String, String> {
    let (record, cut) = recorded_in(configuration, session, copied(launch));
    let kept = history::message_before(configuration, &record, uuid)
        .ok_or("That message is not in the session's record yet.")?;
    let Some(kept) = kept else {
        // Nothing came before it: the conversation starts over, as a new
        // session the message is not in.
        launch.resume = Resume::Fresh;
        launch.resume_at.clear();
        return Ok(String::new());
    };
    launch.resume = Resume::Session {
        id: record,
        fork: cut.is_some(),
    };
    launch.resume_at = kept;
    // A new copy holds nothing past its cut.
    Ok(match cut {
        Some(_) => String::new(),
        None => uuid.to_owned(),
    })
}

/// The session a launch copies and where it was cut, for a reply's fork.
fn copied(launch: &Launch) -> Option<(String, String)> {
    match &launch.resume {
        Resume::Session { id, fork: true } if !launch.resume_at.is_empty() => {
            Some((id.clone(), launch.resume_at.clone()))
        }
        _ => None,
    }
}

fn nothing_to_fork() -> ChatError {
    ChatError::new(
        "chat_fork_unavailable",
        "This chat has no conversation to fork yet.",
    )
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
    /// Final assistant text already displayed in the current turn.
    turn_text: BTreeSet<String>,
    /// The tasks the agent has said started, and not yet said ended, by its
    /// id for each: a command, a subagent.
    tasks: BTreeMap<String, Task>,
    /// The rows of subagents that run on in the background after their tool
    /// call returned, by its id, until their task says it has ended: their
    /// task's lines go on changing them.
    detached: BTreeMap<String, v1::ChatItem>,
    naming: Naming,
    /// The conversation is being read for this link: until it has been, the
    /// first message the chat holds may not be the conversation's.
    history_pending: bool,
}

/// A task the agent has said started, or listed as running.
#[derive(Default)]
struct Task {
    /// When it was first heard of.
    started_at: i64,
    /// The tool call it runs for.
    tool_use_id: String,
    /// The list of what runs in the background has held it.
    listed: bool,
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
                copied: self.copied(),
            });
        }
    }

    fn publish_catalog(&self, out: &mut Output) {
        // Muxflow's own come first: a list too long to send whole is cut at
        // its end.
        let mut commands = Vec::from(super::muxflow_commands());
        commands.extend(naming::with_command(self.commands.iter().cloned().map(
            |mut command| {
                if self.skills.contains(&command.name) {
                    command.kind = v1::ChatCommandKind::Skill.into();
                }
                command
            },
        )));
        out.publish(Change::Catalog(v1::ChatCatalog {
            capabilities: None,
            models: self.listed_models.clone(),
            commands,
        }));
    }

    /// Asks for a title for a chat that has no name yet, from its first
    /// message. Once per session: a chat whose agent could not give one
    /// keeps the first line of that message. Not before the conversation has
    /// been read, nor when only its end could be: the first message held is
    /// then not its first.
    fn ask_title(&mut self, out: &mut Output) {
        if self.history_pending || !self.state.history_complete || !self.can_title() {
            return;
        }
        let first = out.first_message();
        if let Some(ticket) = self.naming.wants_title(first.as_deref()) {
            self.title_job(ticket, first.unwrap_or_default(), false, out);
        }
    }

    /// Whether the agent can be asked for a title as the chat's runs. One
    /// started with settings of its own may reach another account or
    /// endpoint than the agent's defaults would, and is not asked rather
    /// than asked elsewhere.
    fn can_title(&self) -> bool {
        !self
            .launch
            .passthrough
            .iter()
            .any(|word| word == "--settings" || word == "--setting-sources")
    }

    fn title_job(&self, ticket: u64, first: String, asked_by_user: bool, out: &mut Output) {
        let mut argv = self.launch.program.clone();
        if argv.is_empty() {
            argv.push("claude".to_owned());
        }
        argv.extend(TITLE_ARGUMENTS.iter().map(|word| (*word).to_owned()));
        out.background(Job::Title(naming::Ask {
            pane_id: out.pane_id().to_owned(),
            adapter_id: ADAPTER_ID,
            argv,
            answer_to: None,
            more_arguments: naming::no_more_arguments,
            timeout: TITLE_TIMEOUT,
            first,
            ticket,
            given: self.naming.given(),
            asked_by_user,
        }));
    }

    /// The chat goes by `name` from now on, and the agent is told, as a
    /// rename of the user's: it writes the name into the session's file,
    /// where the chat finds it the next time it opens.
    fn rename(&mut self, name: String, out: &mut Output) {
        self.naming.heard(&name);
        out.name(&name);
        if self.ready {
            self.tell_name(name, out);
        } else {
            self.naming.unsent = Some(name);
        }
    }

    /// Verified on 2.1.326: the agent answers `success`, says
    /// `session_title_changed`, and records a `custom-title` entry.
    fn tell_name(&mut self, name: String, out: &mut Output) {
        let request = json!({"subtype": "rename_session", "title": name});
        self.request(request, Asked::Other, out);
    }

    fn initialized(&mut self, response: &Value, out: &mut Output) {
        let list = |name: &str| response[name].as_array().map_or(&[][..], Vec::as_slice);
        if response["models"].is_array() {
            self.models.clear();
            self.listed_models.clear();
            let models = list("models");
            for model in models {
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
                    label: if id == DEFAULT_MODEL {
                        default_label(model, models)
                    } else {
                        string(model, "displayName").to_owned()
                    },
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
            // A name chosen while nothing listened.
            if let Some(name) = self.naming.unsent.take() {
                self.tell_name(name, out);
            }
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

    /// How the agent is started to be as it is now, not as it was first
    /// started: with the model, effort and permissions the chat is set to.
    fn launch_now(&self) -> Launch {
        let mut launch = self.launch.clone();
        let options = self.state.options.clone().unwrap_or_default();
        launch.model.clone_from(&options.model);
        launch.effort.clone_from(&options.effort);
        launch.full_access = options.permission == i32::from(Permission::Full);
        launch
    }

    /// The session this chat's is a copy of and where it was cut, for a
    /// chat started as a reply's fork.
    fn copied(&self) -> Option<(String, String)> {
        copied(&self.launch)
    }

    fn start_rewind(&mut self, out: &mut Output) {
        let launch = self.launch_now();
        let Some(rewind) = &mut self.rewind else {
            return;
        };
        rewind.restarting = true;
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
                let id = format!("{message_id}:{index}");
                self.reader.thinking = None;
                let mut item = match string(block, "type") {
                    "text" => items::text(id, Kind::Assistant, string(block, "text")),
                    "tool_use" => tool_item(string(block, "id"), string(block, "name"), &json!({})),
                    // Thinking is shown once there is some to show, or once
                    // the agent says that it thinks (`thinking_tokens`).
                    "thinking" => {
                        self.reader.thinking = Some(id);
                        return;
                    }
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
            ("content_block_stop", Some(index)) if !message_id.is_empty() => {
                self.reader.end_thinking(&format!("{message_id}:{index}"));
                self.withdraw_silent(out);
            }
            _ => {}
        }
    }

    /// The first `thinking_tokens` of a thinking block that streams no text
    /// makes its item, empty, so the chat can say that the agent thinks. Text
    /// that streams later goes into the same item.
    fn thinking(&mut self, out: &mut Output) {
        let Some(id) = self.reader.thinking.take() else {
            return;
        };
        if self.reader.open.contains_key(&id) {
            return;
        }
        let mut item = items::text(id.clone(), Kind::Reasoning, "");
        item.streaming = true;
        item.at_unix_millis = super::now_unix_millis();
        self.reader.open.insert(id, item.clone());
        out.publish(Change::Item(item));
    }

    /// Takes back the thinking items that ended with nothing to show.
    fn withdraw_silent(&mut self, out: &mut Output) {
        for item in std::mem::take(&mut self.reader.silent) {
            out.withdraw(item);
        }
    }

    fn user(&mut self, message: &Value, out: &mut Output) {
        for mut item in self.reader.results(message) {
            if self.runs_on(&item) {
                let tool = item.tool.get_or_insert_default();
                tool.status = Status::Running.into();
                tool.result_summary.clear();
                // The tool says only that it started the subagent, in words
                // for the model.
                tool.output_preview.clear();
                if self.detached.len() >= MAX_TASKS {
                    self.detached.pop_first();
                }
                self.detached.insert(item.id.clone(), item.clone());
            }
            out.publish(Change::Item(item));
        }
        let uuid = string(message, "uuid");
        let at = items::unix_millis(string(message, "timestamp"));
        match said(message) {
            Said::User(text, read) if message["isReplay"].as_bool() == Some(true) => {
                let sent = self.sent.iter().find(|(sent, _)| sent == uuid);
                let id = sent.map_or(uuid, |(_, id)| id.as_str()).to_owned();
                // A message sent from here keeps the files as they were sent:
                // the agent's copy of an image has no name or path.
                let sent_with = self
                    .unconfirmed
                    .iter()
                    .find(|item| item.id == id)
                    .map(|item| item.attachments.clone());
                self.unconfirmed.retain(|item| item.id != id);
                let mut item = items::text(id, Kind::User, text);
                item.attachments = sent_with.unwrap_or(read);
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
        // A subagent compacts a conversation of its own, not this one.
        let nested = !message["parent_tool_use_id"].is_null();
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
                }
                // `status: null` ends a compaction. A status line without
                // the key says nothing of it.
                if let Some(status) = message.get("status")
                    && !nested
                {
                    self.state.compacting = status.as_str() == Some("compacting");
                }
                self.publish_state(out);
            }
            "task_started" => self.task_started(message, out),
            "task_progress" => self.task_figures(message, out),
            "task_updated" => self.task_updated(message, out),
            "task_notification" => {
                self.task_figures(message, out);
                self.task_ended(message, out);
                self.tasks.remove(string(message, "task_id"));
            }
            "background_tasks_changed" => self.background_changed(message, out),
            // Sent while the model thinks, also when it streams no thinking.
            "thinking_tokens" => self.thinking(out),
            "commands_changed" => {
                if let Some(commands) = message["commands"].as_array() {
                    self.commands = commands.iter().map(command).collect();
                    self.publish_catalog(out);
                }
            }
            // Said when the session is renamed, by this chat or another.
            "session_title_changed" if !nested => {
                let title = string(message, "title");
                if !title.is_empty() && self.naming.heard(title) {
                    out.name(title);
                }
            }
            "compact_boundary" => {
                let uuid = string(message, "uuid");
                if !uuid.is_empty() {
                    let text = compacted(&message["compact_metadata"]);
                    let mut item = items::text(uuid, Kind::Notice, text);
                    item.at_unix_millis = super::now_unix_millis();
                    out.publish(Change::Item(item));
                }
                if !nested {
                    self.set_compacting(false, out);
                }
            }
            _ => {}
        }
    }

    fn result(&mut self, message: &Value, out: &mut Output) {
        self.busy = false;
        self.state.turns_ended += 1;
        // A compaction is over with its turn, however it ended.
        self.state.compacting = false;
        // A turn has ended, and with it every message but the ones the agent
        // said it is keeping for the next.
        self.outstanding.retain(|_, queued| *queued);
        // Whatever was cut short will say nothing more.
        for item in self.reader.stop_open() {
            out.publish(Change::Item(item));
        }
        self.withdraw_silent(out);
        self.pending.clear();
        let waited = self.reported_blocked;
        let stopped = string(message, "terminal_reason").starts_with("aborted");
        if message["is_error"].as_bool() == Some(true) && !stopped {
            let text = match string(message, "result") {
                "" => "The turn ended with an error.",
                text => text,
            };
            if !self.turn_text.contains(text) {
                self.notice(text, out);
            }
        }
        self.turn_text.clear();
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
            match asked {
                Asked::Initialize if !self.ready => self.initialized(&Value::Null, out),
                // The task stays in the list, and the chat says why.
                Asked::StopTask => {
                    let text = match string(response, "error") {
                        "" => "The task could not be stopped.".to_owned(),
                        error => format!("The task could not be stopped: {error}"),
                    };
                    self.notice(&text, out);
                }
                _ => {}
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
            Asked::StopTask | Asked::Other => {}
        }
    }

    /// The tool row a task runs for, while it is still open: a subagent's,
    /// also one that runs on after its tool call returned.
    fn subagent_row(&mut self, tool_use_id: &str) -> Option<&mut v1::ChatItem> {
        let open = self.reader.open.get_mut(tool_use_id).filter(|item| {
            item.tool
                .as_ref()
                .is_some_and(|tool| tool.kind == i32::from(ToolKind::Subagent))
        });
        open.or_else(|| self.detached.get_mut(tool_use_id))
    }

    /// `item`, whose tool call has just returned, is a subagent that runs on
    /// in the background: its task has not said it ended. Its tool says only
    /// that it was started (2.1.326).
    fn runs_on(&self, item: &v1::ChatItem) -> bool {
        item.tool
            .as_ref()
            .is_some_and(|tool| tool.kind == i32::from(ToolKind::Subagent) && tool.background)
            && self.tasks.values().any(|task| task.tool_use_id == item.id)
    }

    /// `task_started`: when it started, and for a subagent its type on its
    /// row. A command run in the foreground starts a task too; its row is
    /// left as it is.
    fn task_started(&mut self, message: &Value, out: &mut Output) {
        if let Some(task) = self.task(string(message, "task_id")) {
            string(message, "tool_use_id").clone_into(&mut task.tool_use_id);
        }
        let kind = string(message, "subagent_type").to_owned();
        let backgrounded = message["is_backgrounded"].as_bool() == Some(true);
        if let Some(row) = self.subagent_row(string(message, "tool_use_id")) {
            let tool = row.tool.get_or_insert_default();
            if !kind.is_empty() {
                tool.agent_type = kind;
            }
            tool.background |= backgrounded;
            let row = row.clone();
            out.publish(Change::Item(row));
        }
    }

    /// `task_progress` and `task_notification`: a subagent's figures as it
    /// works, and its last ones. What it does now is the agent's own words
    /// for it, else the tool it last used.
    fn task_figures(&mut self, message: &Value, out: &mut Output) {
        let usage = &message["usage"];
        let activity = match string(message, "description") {
            "" => string(message, "last_tool_name"),
            description => description,
        };
        let activity = activity.to_owned();
        let progress = string(message, "subtype") == "task_progress";
        let Some(row) = self.subagent_row(string(message, "tool_use_id")) else {
            return;
        };
        let tool = row.tool.get_or_insert_default();
        // A notification's description is the task's, not what it does now.
        if progress && !activity.is_empty() {
            tool.activity = activity;
        }
        if let Some(uses) = usage["tool_uses"].as_u64() {
            tool.tool_uses = u32::try_from(uses).unwrap_or(u32::MAX);
        }
        if let Some(tokens) = usage["total_tokens"].as_u64() {
            tool.tokens = tokens;
        }
        let row = row.clone();
        out.publish(Change::Item(row));
    }

    /// `task_updated`: a task that has ended closes the row of a subagent
    /// that ran on in the background. Its last figures come with its
    /// `task_notification`.
    fn task_updated(&mut self, message: &Value, out: &mut Output) {
        let status = string(&message["patch"], "status");
        if !matches!(status, "completed" | "failed" | "killed" | "stopped") {
            return;
        }
        if let Some(task) = self.tasks.get(string(message, "task_id")) {
            let row = task.tool_use_id.clone();
            self.close_detached(&row, status, out);
        }
    }

    /// `task_notification`, after its figures: a subagent that ran on in the
    /// background is done with, and its row says how it ended and what it
    /// reported.
    fn task_ended(&mut self, message: &Value, out: &mut Output) {
        let Some(mut row) = self.detached.remove(string(message, "tool_use_id")) else {
            return;
        };
        let report = match string(&message["handback_report"], "text") {
            "" => string(message, "summary"),
            report => report,
        };
        if let Some(tool) = &mut row.tool {
            report.clone_into(&mut tool.output_preview);
        }
        finish(&mut row, string(message, "status"));
        out.publish(Change::Item(row));
    }

    /// Closes the row of the subagent that ran on after its tool call `id`
    /// returned, if it still says it runs, as its task ended (`status`).
    fn close_detached(&mut self, id: &str, status: &str, out: &mut Output) {
        let Some(row) = self.detached.get_mut(id) else {
            return;
        };
        if row
            .tool
            .as_ref()
            .is_some_and(|tool| tool.status == i32::from(Status::Running))
        {
            finish(row, status);
            out.publish(Change::Item(row.clone()));
        }
    }

    /// The agent, or the link to it, is gone: no subagent's row will hear of
    /// its end.
    fn stop_detached(&mut self, out: &mut Output) {
        for mut row in std::mem::take(&mut self.detached).into_values() {
            items::stop(&mut row);
            out.publish(Change::Item(row));
        }
    }

    /// `background_tasks_changed`: the whole list of what runs outside the
    /// turn, each with when it started as far as this chat knows. A subagent
    /// that has left it runs no more, whether or not its end was said yet.
    fn background_changed(&mut self, message: &Value, out: &mut Output) {
        let tasks = message["tasks"].as_array().map_or(&[][..], Vec::as_slice);
        let mut background = Vec::new();
        for listed in tasks {
            let id = string(listed, "task_id");
            let Some(task) = self.task(id) else {
                continue;
            };
            task.listed = true;
            background.push(v1::ChatBackgroundTask {
                id: id.to_owned(),
                kind: task_kind(string(listed, "task_type")).into(),
                description: string(listed, "description").to_owned(),
                started_at_unix_millis: task.started_at,
                // Claude Code stops any task it lists (`stop_task`).
                stoppable: true,
            });
        }
        let mut left = Vec::new();
        for (id, task) in &mut self.tasks {
            if task.listed && !background.iter().any(|listed| listed.id == *id) {
                task.listed = false;
                left.push(task.tool_use_id.clone());
            }
        }
        for row in left {
            self.close_detached(&row, "", out);
        }
        self.state.background = background;
        self.publish_state(out);
    }

    /// Task `id`, first heard of now if it has not been: by its
    /// `task_started` or the list it first showed in, whichever came first.
    /// Neither line says when it started. `None` for no id.
    fn task(&mut self, id: &str) -> Option<&mut Task> {
        if id.is_empty() {
            return None;
        }
        if self.tasks.len() >= MAX_TASKS && !self.tasks.contains_key(id) {
            // So many are left only by tasks whose end went unsaid.
            self.tasks.pop_first();
        }
        Some(self.tasks.entry(id.to_owned()).or_insert_with(|| Task {
            started_at: super::now_unix_millis(),
            ..Task::default()
        }))
    }

    /// The agent process is gone: nothing it ran runs any more. Published
    /// with the next state.
    fn forget_tasks(&mut self, out: &mut Output) {
        self.stop_detached(out);
        self.tasks.clear();
        self.state.background.clear();
        self.state.compacting = false;
    }

    fn set_compacting(&mut self, compacting: bool, out: &mut Output) {
        if self.state.compacting != compacting {
            self.state.compacting = compacting;
            self.publish_state(out);
        }
    }

    /// The agent has started a turn of its own, with no message of the
    /// user's: what it does once a background task has ended.
    fn turn_begun(&mut self, out: &mut Output) {
        if !self.busy {
            self.busy = true;
            self.publish_state(out);
        }
    }
}

/// Ends a subagent's row as its task ended: `completed`, `failed`, or
/// stopped (`killed`, `stopped`, or not said).
fn finish(row: &mut v1::ChatItem, status: &str) {
    if let Some(tool) = &mut row.tool {
        let (status, summary) = match status {
            "completed" => (Status::Ok, "done"),
            "failed" => (Status::Failed, "failed"),
            _ => (Status::Failed, "stopped"),
        };
        tool.status = status.into();
        summary.clone_into(&mut tool.result_summary);
    }
}

/// What kind of task Claude Code's `task_type` names.
fn task_kind(task_type: &str) -> v1::ChatTaskKind {
    match task_type {
        "local_bash" => v1::ChatTaskKind::Command,
        "local_agent" | "remote_agent" => v1::ChatTaskKind::Subagent,
        _ => v1::ChatTaskKind::Other,
    }
}

/// `182k`, `24.1k`: a token count as the chat says it.
pub(super) fn tokens(count: u64) -> String {
    // One decimal, rounded half up, and none when it is zero.
    let tenths = |unit_size: u64, unit: &str| {
        let tenths = (count + unit_size / 20) / (unit_size / 10);
        match tenths % 10 {
            0 => format!("{}{unit}", tenths / 10),
            tenth => format!("{}.{tenth}{unit}", tenths / 10),
        }
    };
    match count {
        0..1_000 => count.to_string(),
        1_000..99_950 => tenths(1_000, "k"),
        99_950..999_500 => format!("{}k", (count + 500) / 1_000),
        _ => tenths(1_000_000, "M"),
    }
}

/// The notice a compaction leaves: with the size before it, when the agent
/// says what that was.
pub(super) fn compacted(metadata: &Value) -> String {
    let before = metadata["pre_tokens"]
        .as_u64()
        .or_else(|| metadata["preTokens"].as_u64())
        .filter(|before| *before > 0);
    match before {
        Some(before) => format!("Context compacted, was {} tokens", tokens(before)),
        None => "Context compacted".to_owned(),
    }
}

fn permission(mode: &str) -> Permission {
    if mode == "bypassPermissions" {
        Permission::Full
    } else {
        Permission::Auto
    }
}

/// The `value` of the entry Claude Code runs when no model is chosen.
const DEFAULT_MODEL: &str = "default";

/// The default entry named after the model it runs: "Opus 5.5 (default)".
/// Claude Code calls it "Default (recommended)", which hides that model. The
/// name is another entry's for the same model, else the start of the entry's
/// description ("Opus 5.5 · Best for everyday, complex tasks"), else the
/// model's id; with none of them, Claude Code's own name stays.
fn default_label(entry: &Value, models: &[Value]) -> String {
    let resolved = string(entry, "resolvedModel");
    let named = models
        .iter()
        .filter(|model| string(model, "value") != DEFAULT_MODEL)
        .find(|model| !resolved.is_empty() && string(model, "resolvedModel") == resolved)
        .map(|model| string(model, "displayName"))
        .filter(|name| !name.is_empty());
    let described = || {
        string(entry, "description")
            .split_once(" \u{b7} ")
            .map(|(name, _)| name.trim())
            .filter(|name| !name.is_empty())
    };
    match named
        .or_else(described)
        .or(Some(resolved).filter(|id| !id.is_empty()))
    {
        Some(name) => format!("{name} (default)"),
        None => string(entry, "displayName").to_owned(),
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
                copied,
            } => {
                let configuration = configuration(&pane_id);
                let read = history::load(&configuration, &session, &rewound_to);
                Done::History(match copied {
                    // Nothing said in the copy yet: it holds what its source
                    // held up to the cut.
                    Some((source, through)) if read.items.is_empty() => {
                        history::load_through(&configuration, &source, &through)
                    }
                    _ => read,
                })
            }
            Job::Tag { pane_id, session } => {
                let _ = pane::set(&pane_id, &[(pane::SESSION, &session)]);
                Done::Tagged
            }
            Job::Title(ask) => Done::Titled(naming::run(ask)),
            Job::Rewind {
                pane_id,
                session,
                uuid,
                mut launch,
            } => Done::Rewound((|| {
                let dropped =
                    rewind_launch(&configuration(&pane_id), &session, &uuid, &mut launch)?;
                convert::restart_agent(&pane_id, &ClaudeProvider, &launch, &dropped)
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
                self.history_pending = false;
                // The name the session's file has is the agent's own, and
                // comes before the one it was started under: the user may
                // have renamed it since. A fork's file has only its own name
                // (2.1.326 records `--name` as `custom-title`), and one with
                // nothing said in it yet is read without its source's.
                if !history.name.is_empty() && self.naming.heard(&history.name) {
                    out.name(&history.name);
                }
                out.reset(history.items, history.complete);
                self.ask_title(out);
            }
            Done::Tagged => {}
            Done::Titled(titled) => {
                if self.ended || !self.naming.still_wanted(titled.ticket) {
                    return;
                }
                match titled.title {
                    Some(title) => self.rename(title, out),
                    None if titled.asked_by_user => {
                        self.notice(naming::NOT_NAMED, out);
                    }
                    None => {}
                }
            }
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
                        // The agent was replaced, and its tasks with it.
                        self.forget_tasks(out);
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
        // A subagent that ran on may have ended unheard: its row is read
        // again with the rest.
        self.stop_detached(out);
        self.turn_text.clear();
        // A session started under a name goes by it until it has a name of
        // its own; one it has keeps it over a new link.
        if self.naming.name.is_empty() && !self.launch.name.is_empty() {
            let name = self.launch.name.clone();
            self.naming.heard(&name);
        }
        out.name(&self.naming.name);
        self.history_pending = true;
        // A request the agent waited on is asked again when it answers:
        // losing the link is no word that the agent stopped waiting.
        self.reported_blocked = false;
        // A compaction the link was lost in says its end to nobody.
        self.state.compacting = false;
        self.state.connection = Connection::Connecting.into();
        self.state.history_complete = true;
        self.publish_state(out);
        self.history_read = !self.session.is_empty();
        if self.history_read {
            out.job(Job::History {
                pane_id: out.pane_id().to_owned(),
                session: self.session.clone(),
                rewound_to: self.rewound_to.clone(),
                copied: self.copied(),
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
        // The agent speaks only in a turn. One it starts by itself, as it does
        // when a background task ends, has no message of the user's to say
        // so.
        if !nested && matches!(string(&message, "type"), "stream_event" | "assistant") {
            self.turn_begun(out);
        }
        match string(&message, "type") {
            "stream_event" if !nested => self.stream_event(&message, out),
            "assistant" if !nested => {
                for item in self.reader.assistant(&message) {
                    if item.kind == i32::from(Kind::Assistant) {
                        self.turn_text.insert(item.text.clone());
                    }
                    out.publish(Change::Item(item));
                }
                self.withdraw_silent(out);
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
            SessionCommand::Send {
                text,
                item_id,
                attachments,
            } => {
                let waits = !self.ready || self.rewind.is_some();
                if waits && self.held.len() >= MAX_HELD {
                    return Err(not_ready());
                }
                // A skill named with `$` at the start is one the agent takes
                // with a slash. Which of its commands are skills it says only
                // in its first turn: until then any of them counts. Elsewhere in a
                // message it has no way to be handed one, and reads the name
                // as text.
                let is_skill = |name: &str| match self.skills.is_empty() {
                    true => self.commands.iter().any(|command| command.name == name),
                    false => self.skills.contains(name),
                };
                let text = match text.strip_prefix('$') {
                    Some(rest)
                        if is_skill(rest.split(char::is_whitespace).next().unwrap_or(rest)) =>
                    {
                        format!("/{rest}")
                    }
                    _ => text,
                };
                let uuid = message_uuid(&item_id);
                let message = json!({
                    "type": "user",
                    "message": {"role": "user", "content": content(&text, &attachments)},
                    "uuid": uuid,
                    "parent_tool_use_id": null,
                })
                .to_string();
                let mut item = items::text(item_id.clone(), Kind::User, text);
                item.attachments = attachments.iter().map(Attached::wire).collect();
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
                self.ask_title(out);
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
            SessionCommand::StopTask { task_id } => {
                if !self.ready {
                    return Err(not_ready());
                }
                let request = json!({"subtype": "stop_task", "task_id": task_id});
                self.request(request, Asked::StopTask, out);
                Ok(())
            }
            SessionCommand::Rename { item_id, name } => {
                if name.is_empty() && !self.can_title() {
                    out.publish(Change::Item(naming::notice(item_id, naming::NOT_NAMED)));
                    return Ok(());
                }
                out.publish(Change::Item(naming::notice(
                    item_id,
                    &naming::renamed(&name),
                )));
                if name.is_empty() {
                    let ticket = self.naming.retitle();
                    let first = out.first_message().unwrap_or_default();
                    self.title_job(ticket, first, true, out);
                } else {
                    self.rename(name, out);
                }
                Ok(())
            }
        }
    }

    /// A copy of the session cut at the last message before the user's next
    /// message, or at the newest message for the newest reply: where the
    /// agent itself would go on from (D6).
    fn fork(&mut self, at: &ForkAt, out: &mut Output) -> Result<ForkPlan, ChatError> {
        if self.ended {
            return Err(ChatError::new("chat_unavailable", "The agent has exited."));
        }
        if self.session.is_empty() {
            return Err(nothing_to_fork());
        }
        if self.rewind.is_some() {
            return Err(ChatError::new(
                "chat_turn_busy",
                "The chat is already going back to an earlier message.",
            ));
        }
        let next = (!at.next.is_empty()).then(|| message_uuid(&at.next));
        // The next message waits behind the turn that runs, which is the
        // reply's own.
        if next
            .as_ref()
            .is_some_and(|next| self.outstanding.get(next) == Some(&true))
        {
            return Err(still_writing());
        }
        let pane_id = out.pane_id().to_owned();
        let session = self.session.clone();
        let launch = self.launch_now();
        let copied = self.copied();
        Ok(Box::new(move || {
            fork_launch(
                &configuration(&pane_id),
                &session,
                copied,
                next.as_deref(),
                launch,
            )
        }))
    }

    fn disconnected(&mut self, out: &mut Output) {
        self.ready = false;
        self.state.connection = Connection::Connecting.into();
        self.publish_state(out);
    }

    fn ended(&mut self, failure: &str, out: &mut Output) {
        self.ended = true;
        // A title asked for would name a chat that is over.
        self.naming.stop();
        self.ready = false;
        self.busy = false;
        self.outstanding.clear();
        self.pending.clear();
        self.held.clear();
        self.rewind = None;
        for item in self.reader.stop_open() {
            out.publish(Change::Item(item));
        }
        self.withdraw_silent(out);
        // Its tasks went with it.
        self.forget_tasks(out);
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
