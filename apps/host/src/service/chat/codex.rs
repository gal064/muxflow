//! The Codex chat: `codex app-server` listening on the pane's socket, one
//! server per chat pane, speaking JSON-RPC over WebSocket.
//!
//! The host asks (`thread/start`, `turn/start`, `turn/interrupt`,
//! `thread/revert`), the server tells (`item/*`, `turn/*`) and now and then
//! asks back (`item/*/requestApproval`, `item/tool/requestUserInput`). A
//! thread is always named by its id, so a server shared between panes would
//! change nothing here but which socket is used.
//!
//! The server keeps the conversation: what a chat opens on is what
//! `thread/resume` returns, and a turn goes on while nobody is connected.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::{Value, json};
use tmux_agent_protocol::v1::{
    self, ChatConnection as Connection, ChatItemKind as Kind, ChatPermissionMode as Permission,
    ChatSpeed as Speed, ChatToolKind as ToolKind, ChatToolStatus as Status, ChatTurnState as Turn,
};

use super::{
    attach::{self, Attached},
    convert::{Arguments, Takes},
    items::{self, string},
    naming::{self, Naming},
    pane::{self, Tag},
    provider::{
        Change, ChatError, ChatProvider, ChatSession, ChatSink, ForkAt, ForkPlan, Launch,
        PaneCommand, Resume, SessionCommand, still_writing,
    },
    session::{Dialect, Framing, Out, Session},
};

pub(super) const ADAPTER_ID: &str = "codex";
/// User messages waiting for the thread to be ready for them.
const MAX_HELD: usize = 32;
/// The service tier that is the fast one.
const FAST_TIER: &str = "priority";
/// Lets the agent ask the user a question outside its planning mode. The
/// server warns that the feature is unfinished; without it there are no
/// questions to show at all.
const QUESTIONS_FEATURE: &str = "features.default_mode_request_user_input=true";
/// Keeps that warning, which every thread of the server would repeat, out of
/// the chat: the chat turned the feature on, not the user.
const QUIET_FEATURES: &str = "suppress_unstable_features_warning=true";
/// What has the agent answer one prompt, given on its stdin, with a title:
/// with nothing it may change, leaving no session behind, thinking little,
/// and running none of the user's hooks or `notify` program (both taken by
/// 0.169.0). They follow `exec` and what the chat passes through of its own
/// start, so that they win over it; the thread's model and the file to write
/// the answer to follow them.
const TITLE_ARGUMENTS: &[&str] = &[
    "--ephemeral",
    "--skip-git-repo-check",
    "-s",
    "read-only",
    "-c",
    "model_reasoning_effort=low",
    "--disable",
    "hooks",
    "-c",
    "notify=[]",
];
/// How long one ask for a title may take. Longer than Claude's: on 0.169.0 a
/// run that answers in seconds can spend half a minute first, refreshing its
/// list of models and starting the MCP servers it is configured with.
const TITLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

pub(super) struct CodexProvider;

/// The options of `codex` this reads. An option that is not here makes the
/// command line unreadable, and its pane stays a terminal.
const OPTIONS: &[(&[&str], Takes)] = &[
    (&["--config", "-c"], Takes::One),
    (&["--enable"], Takes::One),
    (&["--disable"], Takes::One),
    (&["--model", "-m"], Takes::One),
    (&["--sandbox", "-s"], Takes::One),
    (&["--ask-for-approval", "-a"], Takes::One),
    (&["--approve-for-me"], Takes::Nothing),
    (
        &["--dangerously-bypass-approvals-and-sandbox"],
        Takes::Nothing,
    ),
    (&["--cd", "-C"], Takes::One),
    (&["--add-dir"], Takes::One),
    (&["--last"], Takes::Nothing),
    (&["--strict-config"], Takes::Nothing),
    // Of the terminal alone.
    (&["--no-alt-screen"], Takes::Nothing),
    (&["--search"], Takes::Nothing),
];
/// First words that make `codex` something other than a new session.
const SUBCOMMANDS: &[&str] = &[
    "a",
    "agents",
    "app-server",
    "apply",
    "archive",
    "cloud",
    "completion",
    "debug",
    "delete",
    "doctor",
    "e",
    "exec",
    "exec-server",
    "features",
    "help",
    "login",
    "logout",
    "mcp",
    "mcp-server",
    "migrate-nfs-db",
    "migrate-rollouts",
    "plugin",
    "queue",
    "remote-control",
    "review",
    "sandbox",
    "unarchive",
    "update",
];

impl ChatProvider for CodexProvider {
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

    /// The server's to give, once the session has reached it.
    fn load_history(&self, _pane_id: &str) -> Vec<v1::ChatItem> {
        Vec::new()
    }

    fn connect(&self, _pane_id: &str, sink: ChatSink) -> Box<dyn ChatSession> {
        Box::new(Session::start(ADAPTER_ID, Codex::default(), sink))
    }

    fn launch(&self, arguments: &[String], _resumed: Option<&str>) -> Option<Launch> {
        let read = Arguments::read(arguments, OPTIONS)?;
        let mut words = read.positional.iter().map(String::as_str);
        let last = read.has("--last");
        let (resume, prompt) = match words.next() {
            None => (Resume::Fresh, None),
            Some(verb @ ("resume" | "fork")) => {
                let resume = match words.next() {
                    Some(id) => Resume::Session {
                        id: id.to_owned(),
                        fork: verb == "fork",
                    },
                    None if last && verb == "resume" => Resume::Latest,
                    // A picker, or the newest session's copy: not known here.
                    None => return None,
                };
                (resume, words.next())
            }
            Some(other) if SUBCOMMANDS.contains(&other) => return None,
            Some(prompt) => (Resume::Fresh, Some(prompt)),
        };
        // More words than a session takes.
        if words.next().is_some() || (last && resume != Resume::Latest) {
            return None;
        }
        let mut passthrough = Vec::new();
        for (name, values) in &read.options {
            if matches!(
                *name,
                "--config" | "--enable" | "--disable" | "--strict-config"
            ) {
                passthrough.push((*name).to_owned());
                passthrough.extend(values.iter().cloned());
            }
        }
        // A session that may change nothing has no chat mode to become.
        if read.value("--sandbox") == Some("read-only") {
            return None;
        }
        let unchecked = read.value("--ask-for-approval") == Some("never")
            && read.value("--sandbox") == Some("danger-full-access");
        Some(Launch {
            cwd: read.value("--cd").unwrap_or_default().to_owned(),
            model: read.value("--model").unwrap_or_default().to_owned(),
            full_access: unchecked || read.has("--dangerously-bypass-approvals-and-sandbox"),
            directories: read.values("--add-dir").cloned().collect(),
            passthrough,
            resume,
            prompt: prompt.unwrap_or_default().to_owned(),
            ..Default::default()
        })
    }

    fn pane_command(&self, launch: &Launch, socket: &std::path::Path) -> Option<PaneCommand> {
        let mut argv = launch.program.clone();
        argv.push("app-server".to_owned());
        argv.extend(launch.passthrough.iter().cloned());
        argv.extend(["-c", QUESTIONS_FEATURE, "-c", QUIET_FEATURES, "--listen"].map(str::to_owned));
        argv.push(format!("unix://{}", socket.display()));
        Some(PaneCommand {
            argv,
            piped: false,
            // The thread is started over the socket; the session notes its
            // id on the pane when it has one.
            session: String::new(),
        })
    }

    fn terminal_options(&self, options: &v1::ChatOptions) -> Vec<String> {
        let mut arguments = Vec::new();
        if !options.model.is_empty() {
            arguments.extend(["--model".to_owned(), options.model.clone()]);
        }
        if options.permission == i32::from(Permission::Full) {
            arguments.push("--dangerously-bypass-approvals-and-sandbox".to_owned());
        }
        arguments
    }
}

/// A request of the host's, waiting for its answer.
enum Asked {
    Initialize,
    Models,
    Skills,
    /// The newest thread of the folder, to resume.
    Latest,
    /// `thread/start`, `thread/resume` or `thread/fork`.
    Thread,
    /// `turn/start`: its answer names the turn.
    Turn,
    /// `turn/steer`, with the message it carried.
    Steer(Held),
    /// `thread/compact/start` or `review/start`, for a built-in.
    Command,
    Revert,
    Other,
}

/// A request of the server's, waiting for the user.
struct Pending {
    /// The JSON-RPC id, which the answer repeats as it came.
    id: Value,
    method: String,
    params: Value,
}

struct Rewind {
    turn: String,
    after_item_id: String,
    /// `thread/revert` has been sent.
    reverting: bool,
}

struct Held {
    text: String,
    item_id: String,
    attachments: Vec<Attached>,
}

impl Held {
    /// The message as the chat shows it. `opens` when it will open a turn,
    /// which is what makes it one that can be gone back to.
    fn item(&self, opens: bool) -> v1::ChatItem {
        let mut item = items::text(self.item_id.clone(), Kind::User, self.text.clone());
        item.attachments = self.attachments.iter().map(Attached::wire).collect();
        item.at_unix_millis = super::now_unix_millis();
        item.rewindable = opens && !is_request(&self.text);
        item
    }
}

pub(super) enum Job {
    /// Note the thread's id on the pane, for the daemon that comes next.
    Tag { pane_id: String, thread: String },
    /// Ask the agent for a title, beside the chat.
    Title(naming::Ask),
}

pub(super) enum Done {
    Tagged,
    Titled(naming::Titled),
}

#[derive(Default)]
pub(super) struct Codex {
    launch: Launch,
    cwd: String,
    thread: String,
    /// The pane has been told which thread is its own.
    noted: bool,
    /// The thread is loaded on this link and takes turns.
    ready: bool,
    ended: bool,
    next_request: u64,
    asked: BTreeMap<u64, Asked>,
    held: VecDeque<Held>,
    state: v1::ChatState,
    models: Vec<v1::ChatModel>,
    /// The skills of the folder: each one's name and where it is.
    skills: Vec<(String, String, String)>,
    /// The turn that is running.
    turn: Option<String>,
    /// The turn the running one belongs to. A review runs as a turn inside
    /// the one `review/start` named, and that outer one is what completes.
    root: Option<String>,
    /// The warnings already shown, which the server may repeat.
    warned: BTreeSet<String>,
    /// The running or asked-for turn is a compaction's or a review's, which
    /// no message can join.
    command_turn: bool,
    /// A turn was asked for and has not started yet.
    starting: bool,
    pending: VecDeque<Pending>,
    /// The agent runtime has been told the agent waits on the user.
    reported_blocked: bool,
    /// The user messages that opened a turn, oldest first: the item's id and
    /// the turn's, which is empty until the server has named it.
    turns: Vec<(String, String)>,
    /// Tools still running and text still streaming, by item id.
    open: BTreeMap<String, v1::ChatItem>,
    /// The open commands unified exec started (`unifiedExecStartup`, the
    /// source of every command the model runs). One still running when its
    /// turn ends was put in the background (seen on 0.169.0): it runs on, and
    /// its own late `item/completed` ends it.
    execs: BTreeSet<String>,
    /// Full or Auto was chosen in the chat, and the next turn must say so.
    permission_chosen: bool,
    /// The user chose a speed in this chat. Until then the thread keeps the
    /// service tier it has, which may be one the chat has no name for.
    speed_chosen: bool,
    /// Stop was asked for a turn that has no id yet.
    stop_wanted: bool,
    rewind: Option<Rewind>,
    /// The row of each subagent spawned in the thread, by its thread, for
    /// the collab calls that say how it is.
    agents: BTreeMap<String, v1::ChatItem>,
    /// The turn each subagent was spawned in, by its thread.
    spawned_in: BTreeMap<String, String>,
    naming: Naming,
    /// The thread being opened is a new one: started, or a copy.
    new_thread: bool,
}

type Output = Out<Job>;

/// The enabled skills a `skills/list` answered with: each one's name, its
/// description and where it is.
pub(super) fn skills(result: &Value) -> Vec<(String, String, String)> {
    result["data"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .flat_map(|folder| folder["skills"].as_array().map_or(&[][..], Vec::as_slice))
        .filter(|skill| skill["enabled"].as_bool() != Some(false))
        .map(|skill| {
            let field = |name: &str| string(skill, name).to_owned();
            (field("name"), field("description"), field("path"))
        })
        .collect()
}

/// A skill as the composer lists it.
pub(super) fn skill_command(name: &str, description: &str) -> v1::ChatCommand {
    v1::ChatCommand {
        name: name.to_owned(),
        description: description.to_owned(),
        argument_hint: String::new(),
        kind: v1::ChatCommandKind::Skill.into(),
    }
}

/// A command of the agent's own that the chat runs for it. Codex's server
/// reads a typed `/compact` as words for the model: each is a request of its
/// own, or for `/init` a prompt of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Builtin {
    Compact,
    Review,
    Init,
}

/// Each built-in as the composer lists it: its name, description and what
/// it takes.
const BUILTINS: &[(Builtin, &str, &str, &str)] = &[
    (
        Builtin::Compact,
        "compact",
        "Summarize the conversation to free up context",
        "",
    ),
    (
        Builtin::Review,
        "review",
        "Review uncommitted changes, or those against a branch or of a commit",
        "[branch | commit | instructions]",
    ),
    (
        Builtin::Init,
        "init",
        "Create an AGENTS.md with instructions for this folder",
        "",
    ),
];

/// What `/init` asks of the agent.
const INIT_PROMPT: &str = "Create a file named AGENTS.md in this folder: a short guide for \
    an agent working in this repository. Read the repository first. Cover its layout, \
    how to build, test and run it, its coding style and testing conventions, and how \
    commits and pull requests are written here. Write only what the repository shows. \
    If an AGENTS.md is already here, improve it instead of starting over.";

/// A built-in the server runs as a request: it opens no turn of a message,
/// and its turn takes no message.
fn is_request(text: &str) -> bool {
    matches!(builtin(text), Some((Builtin::Compact | Builtin::Review, _)))
}

/// The built-in a message is, and what follows its name.
fn builtin(text: &str) -> Option<(Builtin, &str)> {
    let rest = text.trim().strip_prefix('/')?;
    let (name, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    BUILTINS
        .iter()
        .find(|(_, known, _, _)| *known == name)
        .map(|(builtin, ..)| (*builtin, rest.trim()))
}

fn builtin_command((_, name, description, hint): &(Builtin, &str, &str, &str)) -> v1::ChatCommand {
    v1::ChatCommand {
        name: (*name).to_owned(),
        description: (*description).to_owned(),
        argument_hint: (*hint).to_owned(),
        kind: v1::ChatCommandKind::Command.into(),
    }
}

/// What `/review` reviews: the uncommitted changes, a commit named by its
/// hash, the changes against a branch, or what the words ask for.
fn review_target(what: &str) -> Value {
    let hash = (7..=40).contains(&what.len()) && what.chars().all(|c| c.is_ascii_hexdigit());
    if what.is_empty() {
        json!({"type": "uncommittedChanges"})
    } else if hash {
        json!({"type": "commit", "sha": what})
    } else if !what.contains(char::is_whitespace) {
        json!({"type": "baseBranch", "branch": what})
    } else {
        json!({"type": "custom", "instructions": what})
    }
}

/// What a subagent is doing, from the step it has started.
fn activity(step: &v1::ChatTool) -> String {
    let what = &step.detail;
    match ToolKind::try_from(step.kind).unwrap_or_default() {
        ToolKind::Command => format!("Running {what}"),
        ToolKind::Edit => format!("Editing {what}"),
        ToolKind::Web => format!("Searching {what}"),
        _ if what.is_empty() => step.title.clone(),
        _ => format!("{} {what}", step.title),
    }
}

/// A plan the agent keeps for its turn, as one row it updates: what it
/// explained, how far it has got, and each step with how it stands.
fn plan_item(params: &Value) -> v1::ChatItem {
    let steps = params["plan"].as_array().map_or(&[][..], Vec::as_slice);
    let done = steps
        .iter()
        .filter(|step| string(step, "status") == "completed")
        .count();
    let detail = match string(params, "explanation") {
        "" => format!("{done} of {} done", steps.len()),
        explanation => explanation.to_owned(),
    };
    let id = format!("{}-plan", string(params, "turnId"));
    let mut row = items::tool(id, ToolKind::Other, "plan", detail);
    row.at_unix_millis = super::now_unix_millis();
    if let Some(tool) = row.tool.as_mut() {
        tool.status = Status::Ok.into();
        tool.result_summary = format!("{done}/{}", steps.len());
        tool.output_preview = steps
            .iter()
            .map(|step| {
                let mark = match string(step, "status") {
                    "completed" => "[x]",
                    "inProgress" => "[>]",
                    _ => "[ ]",
                };
                format!("{mark} {}", string(step, "step"))
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    row
}

/// One item of a thread as a timeline item. `None` for one that is not
/// shown.
fn thread_item(source: &Value, finished: bool) -> Option<v1::ChatItem> {
    let id = string(source, "id");
    let field = |name: &str| string(source, name);
    // Text is still arriving for it.
    let streaming = !finished && source["completedAtMs"].is_null();
    let texts = |name: &str| -> String {
        let mut text = String::new();
        for part in source[name].as_array().map_or(&[][..], Vec::as_slice) {
            // A skill is shown as it was typed: its name, then what follows.
            // One named with `$` is in the text already.
            let skill = string(part, "type") == "skill";
            // A file sent with the message is not its text.
            if matches!(string(part, "type"), "localImage" | "image" | "mention") {
                continue;
            }
            let piece = match part.as_str() {
                Some(piece) => piece.to_owned(),
                None if skill && !text.is_empty() => continue,
                None if skill => format!("/{}", string(part, "name")),
                None => string(part, "text").to_owned(),
            };
            if !text.is_empty() {
                text.push(if text.starts_with('/') && !text.contains([' ', '\n']) {
                    ' '
                } else {
                    '\n'
                });
            }
            text.push_str(&piece);
        }
        text
    };
    let status = |tool: &mut v1::ChatTool, failed: bool| {
        let (status, summary) = match field("status") {
            "inProgress" if !finished => (Status::Running, ""),
            "declined" => (Status::Failed, "declined"),
            "failed" | "interrupted" => (Status::Failed, "failed"),
            "inProgress" => (Status::Failed, "stopped"),
            _ if failed => (Status::Failed, "failed"),
            _ => (Status::Ok, "done"),
        };
        tool.status = status.into();
        tool.result_summary = summary.to_owned();
    };
    let mut produced = match field("type") {
        "userMessage" => {
            let client = field("clientId");
            let id = if client.is_empty() { id } else { client };
            let (text, mut attachments) = attach::split_paths(&texts("content"));
            let parts = source["content"].as_array().map_or(&[][..], Vec::as_slice);
            for part in parts {
                match string(part, "type") {
                    "localImage" => {
                        let path = string(part, "path");
                        let name = path.rsplit('/').next().unwrap_or(path);
                        attachments.push(v1::ChatAttachment {
                            path: path.to_owned(),
                            name: name.to_owned(),
                            media_type: attach::image_type_of_name(name).to_owned(),
                            size_bytes: 0,
                        });
                    }
                    "image" => attachments.push(attach::inline_image("", 0)),
                    _ => {}
                }
            }
            // `/init` is sent as its prompt, and shown as it was typed.
            let text = if text == INIT_PROMPT {
                "/init".to_owned()
            } else {
                text
            };
            let mut item = items::text(id, Kind::User, text);
            item.attachments = attachments;
            item
        }
        "agentMessage" | "plan" => {
            // One that has said nothing yet is shown when it does.
            if field("text").is_empty() {
                return None;
            }
            let mut item = items::text(id, Kind::Assistant, field("text"));
            item.streaming = streaming;
            item
        }
        "reasoning" => {
            let summary = texts("summary");
            let text = if summary.is_empty() {
                texts("content")
            } else {
                summary
            };
            if text.is_empty() {
                return None;
            }
            let mut item = items::text(id, Kind::Reasoning, text);
            item.streaming = streaming;
            item
        }
        "commandExecution" => {
            let action = string(&source["commandActions"][0], "command");
            let command = if action.is_empty() {
                field("command")
            } else {
                action
            };
            let mut row = items::tool(id, ToolKind::Command, "run", command);
            let tool = row.tool.as_mut()?;
            let code = source["exitCode"].as_i64();
            status(tool, code.is_some_and(|code| code != 0));
            if let Some(code) = code.filter(|code| *code != 0) {
                tool.result_summary = format!("exit {code}");
            }
            tool.output_preview = field("aggregatedOutput").to_owned();
            row
        }
        "fileChange" => {
            let changes = source["changes"].as_array().map_or(&[][..], Vec::as_slice);
            let paths: Vec<&str> = changes
                .iter()
                .map(|change| string(change, "path"))
                .collect();
            let mut row = items::tool(id, ToolKind::Edit, "edit", paths.join(", "));
            let tool = row.tool.as_mut()?;
            tool.path = paths.first().copied().unwrap_or_default().to_owned();
            for change in changes {
                let (added, removed) = items::diff_counts(string(change, "diff"));
                tool.added += added;
                tool.removed += removed;
            }
            status(tool, false);
            row
        }
        "mcpToolCall" => {
            let mut row = items::tool(id, ToolKind::Mcp, field("server"), field("tool"));
            status(row.tool.as_mut()?, !source["error"].is_null());
            row
        }
        "webSearch" => {
            let mut row = items::tool(id, ToolKind::Web, "search", field("query"));
            let tool = row.tool.as_mut()?;
            if finished {
                tool.status = Status::Ok.into();
                tool.result_summary = "done".to_owned();
            }
            row
        }
        // Only a spawn is a row: the subagent's. Waiting on it, writing to it
        // and the rest say how it is (`agent_states`), and are no rows.
        "collabAgentToolCall" if field("tool") == SPAWN => {
            let mut row = items::tool(id, ToolKind::Subagent, "agent", field("prompt"));
            match agent_states(source).first() {
                Some((_, state)) => agent_state(&mut row, state),
                None => status(row.tool.as_mut()?, false),
            }
            row
        }
        "dynamicToolCall" => {
            let mut row = items::tool(id, ToolKind::Other, field("tool"), "");
            status(
                row.tool.as_mut()?,
                source["success"].as_bool() == Some(false),
            );
            row
        }
        // Read when it has ended: while it runs the working line says so. The
        // item names no size.
        "contextCompaction" => items::text(id, Kind::Notice, "Context compacted"),
        "enteredReviewMode" => {
            items::text(id, Kind::Notice, format!("Reviewing {}", field("review")))
        }
        // An agent's message that follows it carries the review's text, live
        // and as read.
        "exitedReviewMode" => return None,
        "imageView" => {
            let mut row = items::tool(id, ToolKind::Read, "view", field("path"));
            let tool = row.tool.as_mut()?;
            field("path").clone_into(&mut tool.path);
            status(tool, false);
            row
        }
        "sleep" => {
            let seconds = source["durationMs"].as_u64().unwrap_or(0).div_ceil(1000);
            let mut row = items::tool(id, ToolKind::Other, "wait", format!("{seconds}s"));
            status(row.tool.as_mut()?, false);
            row
        }
        "imageGeneration" => {
            let detail = match field("savedPath") {
                "" => field("revisedPrompt"),
                path => path,
            };
            let mut row = items::tool(id, ToolKind::Other, "image", detail);
            status(row.tool.as_mut()?, !source["failure"].is_null());
            row
        }
        _ => return None,
    };
    if produced.at_unix_millis == 0 {
        produced.at_unix_millis = source["startedAtMs"].as_i64().unwrap_or(0);
    }
    Some(produced)
}

/// The collab tool that starts a subagent.
const SPAWN: &str = "spawnAgent";

/// What a collab tool call says of each subagent it names: its thread, and
/// how it is (`running`, `completed`, …). The spawned one first.
fn agent_states(source: &Value) -> Vec<(&str, &str)> {
    let states = &source["agentsStates"];
    let named = source["receiverThreadIds"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(Value::as_str);
    let mut seen: Vec<(&str, &str)> = named
        .filter_map(|thread| Some((thread, states[thread]["status"].as_str()?)))
        .collect();
    for (thread, state) in states.as_object().into_iter().flatten() {
        if let Some(state) = state["status"].as_str()
            && !seen.iter().any(|(known, _)| known == thread)
        {
            seen.push((thread.as_str(), state));
        }
    }
    seen
}

/// A command of the model's, which unified exec started: one that may be put
/// in the background.
fn is_exec(source: &Value) -> bool {
    string(source, "type") == "commandExecution" && string(source, "source") == "unifiedExecStartup"
}

/// Remembers the subagent a spawn's `row` stands for, under its thread.
fn remember_agent(agents: &mut BTreeMap<String, v1::ChatItem>, source: &Value, row: &v1::ChatItem) {
    let thread = source["receiverThreadIds"][0].as_str().unwrap_or_default();
    if row.tool.is_some() && !thread.is_empty() {
        agents.insert(thread.to_owned(), row.clone());
    }
}

/// The rows of the subagents a collab call other than a spawn says how they
/// are, changed to say so. A subagent spawned before what was read has no
/// row, and is passed over.
fn note_agents(agents: &mut BTreeMap<String, v1::ChatItem>, source: &Value) -> Vec<v1::ChatItem> {
    let mut changed = Vec::new();
    for (thread, state) in agent_states(source) {
        if let Some(row) = agents.get_mut(thread) {
            let before = row.clone();
            agent_state(row, state);
            if *row != before {
                changed.push(row.clone());
            }
        }
    }
    changed
}

/// A subagent's row as its state says: running while it starts or runs, and
/// what it is now on the line under it.
fn agent_state(row: &mut v1::ChatItem, state: &str) {
    let Some(tool) = row.tool.as_mut() else {
        return;
    };
    let (status, summary, activity) = match state {
        "pendingInit" => (Status::Running, "", "starting"),
        "running" => (Status::Running, "", "running"),
        "completed" => (Status::Ok, "done", ""),
        "interrupted" | "shutdown" => (Status::Failed, "stopped", ""),
        _ => (Status::Failed, "failed", ""),
    };
    tool.status = status.into();
    summary.clone_into(&mut tool.result_summary);
    activity.clone_into(&mut tool.activity);
}

fn permission(policy: &Value, sandbox: &Value) -> Permission {
    let open = sandbox.as_str() == Some("danger-full-access")
        || string(sandbox, "type") == "dangerFullAccess";
    if policy.as_str() == Some("never") && open {
        Permission::Full
    } else {
        Permission::Auto
    }
}

impl Codex {
    fn request(&mut self, method: &str, params: Value, asked: Asked, out: &mut Output) {
        self.next_request += 1;
        self.asked.insert(self.next_request, asked);
        out.send(json!({"id": self.next_request, "method": method, "params": params}).to_string());
    }

    fn turn_state(&self) -> Turn {
        if !self.pending.is_empty() {
            Turn::Waiting
        } else if self.turn.is_some() || self.starting {
            Turn::Working
        } else {
            Turn::Idle
        }
    }

    fn publish_state(&mut self, out: &mut Output) {
        let turn = self.turn_state();
        self.state.turn = turn.into();
        self.state.pending = self.pending.front().map(pending_request);
        self.state.native_session_id.clone_from(&self.thread);
        out.publish(Change::State(self.state.clone()));
        // The server's own hooks do not say that it waits on the user when
        // the request went to an automatic reviewer first.
        let blocked = turn == Turn::Waiting;
        if blocked != self.reported_blocked && !self.thread.is_empty() {
            self.reported_blocked = blocked;
            let payload = if blocked {
                json!({"hook_event_name": "PreToolUse", "tool_name": "request_user_input"})
            } else if turn == Turn::Idle {
                json!({"hook_event_name": "Stop"})
            } else {
                json!({"hook_event_name": "PostToolUse"})
            };
            let mut payload = payload;
            payload["session_id"] = Value::String(self.thread.clone());
            out.report(payload);
        }
    }

    fn notice(&self, id: String, text: &str, out: &mut Output) {
        let mut item = items::text(id, Kind::Notice, text);
        item.at_unix_millis = super::now_unix_millis();
        out.publish(Change::Item(item));
    }

    fn publish_catalog(&self, out: &mut Output) {
        // Muxflow's own come first: a list too long to send whole is cut at
        // its end.
        let mut commands = Vec::from(super::muxflow_commands());
        commands.extend(naming::with_command(
            BUILTINS.iter().map(builtin_command).chain(
                self.skills
                    .iter()
                    .filter(|(name, _, _)| builtin(&format!("/{name}")).is_none())
                    .map(|(name, description, _)| skill_command(name, description)),
            ),
        ));
        out.publish(Change::Catalog(v1::ChatCatalog {
            capabilities: None,
            models: self.models.clone(),
            commands,
        }));
    }

    /// Asks for a title for a chat that has no name yet, from its first
    /// message. Once per session: a chat whose agent could not give one
    /// keeps the first line of that message.
    fn ask_title(&mut self, out: &mut Output) {
        // Until the thread is open, what the chat holds may not be all of
        // it: it asks once it is.
        if !self.ready {
            return;
        }
        let first = out.first_message();
        if let Some(ticket) = self.naming.wants_title(first.as_deref()) {
            self.title_job(ticket, first.unwrap_or_default(), false, out);
        }
    }

    fn title_job(&self, ticket: u64, first: String, asked_by_user: bool, out: &mut Output) {
        let mut argv = self.launch.program.clone();
        if argv.is_empty() {
            argv.push("codex".to_owned());
        }
        argv.push("exec".to_owned());
        argv.extend(title_passthrough(&self.launch.passthrough));
        argv.extend(TITLE_ARGUMENTS.iter().map(|word| (*word).to_owned()));
        let model = self
            .state
            .options
            .as_ref()
            .map_or("", |options| options.model.as_str());
        if !model.is_empty() {
            argv.extend(["--model".to_owned(), model.to_owned()]);
        }
        out.background(Job::Title(naming::Ask {
            pane_id: out.pane_id().to_owned(),
            adapter_id: ADAPTER_ID,
            argv,
            answer_to: Some("-o"),
            more_arguments: mcp_servers_off,
            timeout: TITLE_TIMEOUT,
            first,
            ticket,
            given: self.naming.given(),
            asked_by_user,
        }));
    }

    /// The chat goes by `name` from now on, and the server is told: it keeps
    /// the name with the thread, and says `thread/name/updated`.
    fn rename(&mut self, name: String, out: &mut Output) {
        self.naming.heard(&name);
        out.name(&name);
        if self.ready && !self.thread.is_empty() {
            self.tell_name(name, out);
        } else {
            self.naming.unsent = Some(name);
        }
    }

    /// `ThreadSetNameParams` in the 0.169.0 schema.
    fn tell_name(&mut self, name: String, out: &mut Output) {
        let params = json!({"threadId": self.thread, "name": name});
        self.request("thread/name/set", params, Asked::Other, out);
    }

    /// Starts on the thread the pane is for, or the one its launch names.
    fn open_thread(&mut self, out: &mut Output) {
        self.new_thread = matches!(
            (&self.launch.resume, self.thread.is_empty()),
            (Resume::Fresh | Resume::Session { fork: true, .. }, true)
        );
        let mut params = json!({});
        if !self.launch.model.is_empty() {
            params["model"] = json!(self.launch.model);
        }
        let (policy, reviewer, sandbox) = if self.launch.full_access {
            ("never", "user", "danger-full-access")
        } else {
            ("on-request", "auto_review", "workspace-write")
        };
        let resumed = |mut params: Value, thread: &str| {
            params["threadId"] = json!(thread);
            params
        };
        if !self.thread.is_empty() {
            // The thread is this chat's already, with the settings it has.
            let params = json!({"threadId": self.thread});
            return self.request("thread/resume", params, Asked::Thread, out);
        }
        params["approvalPolicy"] = json!(policy);
        params["approvalsReviewer"] = json!(reviewer);
        params["sandbox"] = json!(sandbox);
        match self.launch.resume.clone() {
            Resume::Fresh => self.request("thread/start", params, Asked::Thread, out),
            Resume::Session { id, fork: false } => {
                self.request("thread/resume", resumed(params, &id), Asked::Thread, out);
            }
            Resume::Session { id, fork: true } => {
                let mut params = resumed(params, &id);
                // A reply's fork: through that turn, inclusive (0.169.0
                // schema; not yet tried against a live server).
                if !self.launch.resume_at.is_empty() {
                    params["lastTurnId"] = json!(self.launch.resume_at);
                }
                self.request("thread/fork", params, Asked::Thread, out);
            }
            Resume::Latest => {
                let params = json!({
                    "cwd": self.cwd, "limit": 1, "sortKey": "updated_at", "sortDirection": "desc",
                });
                self.request("thread/list", params, Asked::Latest, out);
            }
        }
    }

    /// The answer to `thread/start`, `thread/resume` or `thread/fork`: the
    /// thread, how it is set, and the conversation so far.
    fn thread_opened(&mut self, result: &Value, out: &mut Output) {
        let thread = &result["thread"];
        let id = string(thread, "id");
        if id.is_empty() {
            return self.failed("The agent could not open the conversation.", out);
        }
        if id != self.thread {
            self.thread = id.to_owned();
            self.noted = false;
        }
        // The thread's own name comes before the one the chat was started
        // under: the user may have renamed it since. A new thread is the
        // exception, as a copy may carry its source's name, which is not
        // its own: it goes by the one it was started under, and the server
        // is told so.
        let theirs = string(thread, "name");
        let name = if self.new_thread && !self.launch.name.is_empty() {
            self.naming
                .unsent
                .get_or_insert_with(|| self.launch.name.clone());
            self.launch.name.clone()
        } else if theirs.is_empty() && self.naming.name.is_empty() {
            self.launch.name.clone()
        } else if theirs.is_empty() {
            // Given in this session, and not yet heard back from the server.
            self.naming.name.clone()
        } else {
            theirs.to_owned()
        };
        self.naming.heard(&name);
        out.name(&name);
        let options = self.state.options.get_or_insert_default();
        if !self.permission_chosen {
            options.permission = permission(&result["approvalPolicy"], &result["sandbox"]).into();
        }
        options.model = string(result, "model").to_owned();
        options.effort = string(result, "reasoningEffort").to_owned();
        let fast = string(result, "serviceTier") == FAST_TIER;
        options.speed = if fast { Speed::Fast } else { Speed::Standard }.into();

        let mut timeline: Vec<v1::ChatItem> = Vec::new();
        self.turns.clear();
        // Text that was arriving when the link was lost is not in the thread
        // until it is whole.
        let mut streamed = std::mem::take(&mut self.open);
        streamed.retain(|_, item| item.streaming);
        self.turn = None;
        self.root = None;
        self.command_turn = false;
        self.agents.clear();
        self.spawned_in.clear();
        // The timeline is read anew: warnings may be said again.
        self.warned.clear();
        self.state.compacting = false;
        // The thread as read has every step of an ended turn ended.
        self.execs.clear();
        self.state.background.clear();
        let turns = thread["turns"].as_array().map_or(&[][..], Vec::as_slice);
        // The steps of a review's own turn, by the turn of the review.
        let mut reviews: BTreeMap<&str, Vec<v1::ChatItem>> = BTreeMap::new();
        for turn in turns {
            let running = string(turn, "status") == "inProgress";
            let turn_id = string(turn, "id");
            if running {
                self.turn = Some(turn_id.to_owned());
                self.root = match string(turn, "rootTurnId") {
                    "" => None,
                    root => Some(root.to_owned()),
                };
            }
            let mut opened = false;
            // A review runs in a turn of its own that names the review's
            // turn as its root. It is read before that turn, and as
            // interrupted although it ended: its steps go under the review's
            // notice, and its prompt, which Codex wrote, is not shown.
            let root = string(turn, "rootTurnId");
            let inner = !root.is_empty() && root != turn_id;
            for source in turn["items"].as_array().map_or(&[][..], Vec::as_slice) {
                if inner && string(source, "type") == "userMessage" {
                    continue;
                }
                if string(source, "type") == "collabAgentToolCall"
                    && string(source, "tool") != SPAWN
                {
                    for row in note_agents(&mut self.agents, source) {
                        if let Some(held) = timeline.iter_mut().find(|held| held.id == row.id) {
                            *held = row;
                        }
                    }
                    continue;
                }
                let Some(mut item) = thread_item(source, !running) else {
                    continue;
                };
                // A step of a turn that is over runs no more.
                if !running {
                    items::stop(&mut item);
                }
                if string(source, "type") == "collabAgentToolCall" {
                    remember_agent(&mut self.agents, source, &item);
                    self.note_spawn(source, turn_id);
                }
                // Going back to a message goes back to before its turn, so
                // only the message that opened a turn can be gone back to.
                if item.kind == i32::from(Kind::User) && !std::mem::replace(&mut opened, true) {
                    item.rewindable = true;
                    self.turns.push((item.id.clone(), turn_id.to_owned()));
                }
                if item.streaming || items::is_running(&item) {
                    self.open.insert(item.id.clone(), item.clone());
                    if is_exec(source) {
                        self.execs.insert(item.id.clone());
                    }
                }
                let entered = string(source, "type") == "enteredReviewMode";
                if inner {
                    reviews.entry(root).or_default().push(item);
                } else {
                    timeline.push(item);
                }
                if entered {
                    timeline.extend(reviews.remove(turn_id).unwrap_or_default());
                }
            }
            if string(turn, "status") == "interrupted" && !inner {
                let id = format!("{turn_id}-interrupted");
                timeline.push(items::text(id, Kind::Notice, "Interrupted"));
            }
        }
        // A review whose own turn was read without it.
        timeline.extend(reviews.into_values().flatten());
        if self.turn.is_some() {
            // What was shown of it stays, and goes on from where it is.
            for (id, item) in streamed {
                if !timeline.iter().any(|held| held.id == id) {
                    timeline.push(item.clone());
                    self.open.insert(id, item);
                }
            }
        }
        if !self.turns.is_empty() {
            self.note_thread(out);
        } else if !self.launch.directories.is_empty() {
            // A new thread is told of the agent's other folders with its
            // first turn.
            self.permission_chosen = true;
        }
        // What was sent before the thread was ready is still on its way.
        let opens = self.turn.is_none();
        for (index, held) in self.held.iter().enumerate() {
            timeline.push(held.item(opens && index == 0));
        }
        self.state.history_complete = true;
        out.reset(timeline, true);
        self.ready = true;
        self.starting = false;
        self.state.connection = Connection::Ready.into();
        self.publish_state(out);
        self.sync_agent_tasks(out);
        // No hook of the server's names the thread until a turn runs on it:
        // one that was resumed would not be known to be running until then.
        if !self.turns.is_empty() && self.turn.is_none() {
            out.report(json!({"hook_event_name": "SessionStart", "session_id": self.thread}));
        }
        // A name chosen while nothing listened.
        if let Some(name) = self.naming.unsent.take() {
            self.tell_name(name, out);
        }
        self.send_held(out);
        // A thread resumed with no name is named too.
        self.ask_title(out);
    }

    /// Notes the thread on the pane, for the daemon that comes next. Only a
    /// thread that has had a turn: the server keeps no record of one that
    /// has not, and could not open it again.
    fn note_thread(&mut self, out: &mut Output) {
        if !std::mem::replace(&mut self.noted, true) {
            out.job(Job::Tag {
                pane_id: out.pane_id().to_owned(),
                thread: self.thread.clone(),
            });
        }
    }

    fn failed(&mut self, reason: &str, out: &mut Output) {
        self.state.connection = Connection::Failed.into();
        self.state.failure = reason.to_owned();
        self.publish_state(out);
    }

    fn send_held(&mut self, out: &mut Output) {
        if !self.ready || self.rewind.is_some() {
            return;
        }
        while !self.starting
            && let Some(held) = self.held.pop_front()
        {
            // A built-in cannot join a turn: it waits for the turn's end, and
            // what was sent after it waits behind it.
            if self.turn.is_some() && (self.command_turn || builtin(&held.text).is_some()) {
                self.held.push_front(held);
                break;
            }
            self.send(held, out);
        }
    }

    /// The message as the server takes it: a skill named at its start with
    /// a slash is handed over as the skill, not as its name, and each one
    /// named with `$` anywhere is handed over beside the text.
    fn input(&self, text: &str) -> Value {
        let leading = text.strip_prefix('/').and_then(|rest| {
            let (name, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            let skill = self.skills.iter().find(|(skill, _, _)| skill == name)?;
            Some((skill, rest.trim()))
        });
        let item = |name: &str, path: &str| json!({"type": "skill", "name": name, "path": path});
        let (mut input, text, mut named) = match leading {
            Some(((name, _, path), rest)) => (vec![item(name, path)], rest, vec![name.as_str()]),
            None => (Vec::new(), text, Vec::new()),
        };
        // Blank space alone, as beside an image with no words, is no text.
        if !text.trim().is_empty() {
            input.push(json!({"type": "text", "text": text}));
        }
        for (at, _) in text.match_indices('$') {
            let inside = text[..at]
                .chars()
                .next_back()
                .is_some_and(|before| before.is_alphanumeric() || before == '$');
            let rest = &text[at + 1..];
            let end = rest
                .find(|c: char| !(c.is_alphanumeric() || "-_:.".contains(c)))
                .unwrap_or(rest.len());
            // A name does not end in the sentence's own punctuation.
            let name = rest[..end].trim_end_matches(['.', ':']);
            let skill = self.skills.iter().find(|(skill, _, _)| skill == name);
            if let Some((name, _, path)) = skill
                && !inside
                && !named.contains(&name.as_str())
            {
                named.push(name);
                input.push(item(name, path));
            }
        }
        Value::Array(input)
    }

    /// The message with its files: an image as a `localImage` the server
    /// reads itself, any other file named by its path under the text (D9).
    /// Both shapes are the 0.169.0 schema's `UserInput`; not yet tried
    /// against a live server.
    fn input_with(&self, text: &str, attachments: &[Attached]) -> Value {
        let named = attachments
            .iter()
            .filter(|attached| !attached.is_image())
            .map(|attached| attached.path.as_str());
        let mut input = self.input(&attach::with_paths(text, named));
        if let Value::Array(items) = &mut input {
            items.extend(
                attachments
                    .iter()
                    .filter(|attached| attached.is_image())
                    .map(|attached| json!({"type": "localImage", "path": attached.path})),
            );
        }
        input
    }

    fn send(&mut self, held: Held, out: &mut Output) {
        let thread = self.thread.clone();
        let text = match builtin(&held.text) {
            Some((Builtin::Compact, _)) => {
                self.starting = true;
                self.command_turn = true;
                let params = json!({"threadId": thread});
                self.request("thread/compact/start", params, Asked::Command, out);
                return self.publish_state(out);
            }
            Some((Builtin::Review, what)) => {
                self.starting = true;
                self.command_turn = true;
                let params = json!({"threadId": thread, "target": review_target(what)});
                self.request("review/start", params, Asked::Command, out);
                return self.publish_state(out);
            }
            Some((Builtin::Init, _)) => INIT_PROMPT,
            None => held.text.as_str(),
        };
        let input = self.input_with(text, &held.attachments);
        if let Some(turn) = self.turn.clone() {
            let params = json!({
                "threadId": self.thread, "expectedTurnId": turn, "input": input,
                "clientUserMessageId": held.item_id,
            });
            return self.request("turn/steer", params, Asked::Steer(held), out);
        }
        let options = self.state.options.clone().unwrap_or_default();
        let mut params = json!({
            "threadId": self.thread, "input": input, "clientUserMessageId": held.item_id,
        });
        if !options.model.is_empty() {
            params["model"] = json!(options.model);
        }
        if !options.effort.is_empty() {
            params["effort"] = json!(options.effort);
        }
        if self.speed_chosen && options.speed != i32::from(Speed::Unspecified) {
            let fast = options.speed == i32::from(Speed::Fast);
            params["serviceTier"] = json!(if fast { FAST_TIER } else { "default" });
        }
        if std::mem::take(&mut self.permission_chosen) {
            if options.permission == i32::from(Permission::Full) {
                params["approvalPolicy"] = json!("never");
                params["approvalsReviewer"] = json!("user");
                params["sandboxPolicy"] = json!({"type": "dangerFullAccess"});
            } else {
                params["approvalPolicy"] = json!("on-request");
                params["approvalsReviewer"] = json!("auto_review");
                params["sandboxPolicy"] = json!({
                    "type": "workspaceWrite",
                    "writableRoots": self.launch.directories,
                    "networkAccess": false,
                    "excludeTmpdirEnvVar": false,
                    "excludeSlashTmp": false,
                });
            }
        }
        self.starting = true;
        self.turns.push((held.item_id, String::new()));
        self.request("turn/start", params, Asked::Turn, out);
        self.publish_state(out);
    }

    fn item(&mut self, params: &Value, finished: bool, out: &mut Output) {
        let source = &params["item"];
        match (string(source, "type"), string(source, "tool")) {
            // While it compacts, the working line says so; the notice is for
            // when it has.
            ("contextCompaction", _) => {
                self.state.compacting = !finished;
                if !finished {
                    return self.publish_state(out);
                }
            }
            // The prompt Codex writes for a command it runs: the command
            // as typed is shown.
            ("userMessage", _) if self.command_turn && string(source, "clientId").is_empty() => {
                return;
            }
            ("collabAgentToolCall", tool) if tool != SPAWN => {
                for item in note_agents(&mut self.agents, source) {
                    self.track(&item);
                    out.publish(Change::Item(item));
                }
                return self.sync_agent_tasks(out);
            }
            _ => {}
        }
        let Some(mut item) = thread_item(source, finished) else {
            return;
        };
        let spawn = string(source, "type") == "collabAgentToolCall";
        if spawn {
            remember_agent(&mut self.agents, source, &item);
            self.note_spawn(source, string(params, "turnId"));
        }
        if item.kind == i32::from(Kind::User) {
            // The first message of a turn is the one a rewind can name.
            let turn = string(params, "turnId");
            let opened = self
                .turns
                .iter_mut()
                .find(|(id, known)| *id == item.id && (known.is_empty() || known == turn));
            item.rewindable = opened.is_some();
            if let Some((_, known)) = opened {
                turn.clone_into(known);
            }
        }
        self.track(&item);
        let (id, running) = (item.id.clone(), items::is_running(&item));
        out.publish(Change::Item(item));
        if string(source, "type") == "contextCompaction" {
            self.publish_state(out);
        }
        if spawn {
            self.sync_agent_tasks(out);
        }
        if is_exec(source) {
            if running {
                self.execs.insert(id);
            } else {
                // Its end, also after the turn it started in.
                self.execs.remove(&id);
                let listed = self.state.background.len();
                self.state.background.retain(|task| task.id != id);
                if self.state.background.len() != listed {
                    self.publish_state(out);
                }
            }
        }
    }

    /// Keeps `item` among the open ones while it streams or runs.
    fn track(&mut self, item: &v1::ChatItem) {
        if item.streaming || items::is_running(item) {
            self.open.insert(item.id.clone(), item.clone());
        } else {
            self.open.remove(&item.id);
        }
    }

    fn delta(&mut self, params: &Value, kind: Kind, out: &mut Output) {
        let (id, text) = (string(params, "itemId"), string(params, "delta"));
        if id.is_empty() || text.is_empty() {
            return;
        }
        match self.open.get_mut(id) {
            Some(item) => {
                item.text.push_str(text);
                out.publish(Change::Delta(v1::ChatItemDelta {
                    item_id: id.to_owned(),
                    text_append: text.to_owned(),
                }));
            }
            // Its start was before this link, or said nothing yet.
            None => {
                let mut item = items::text(id, kind, text);
                item.streaming = true;
                item.at_unix_millis = super::now_unix_millis();
                self.open.insert(id.to_owned(), item.clone());
                out.publish(Change::Item(item));
            }
        }
    }

    fn turn_completed(&mut self, params: &Value, out: &mut Output) {
        let turn = &params["turn"];
        let id = string(turn, "id");
        if self
            .turn
            .as_deref()
            .is_some_and(|running| running != id && self.root.as_deref() != Some(id))
        {
            return;
        }
        // Stopping a turn stops the subagents it spawned; their threads may
        // not say so.
        if string(turn, "status") == "interrupted" {
            let stopped: Vec<String> = [Some(id.to_owned()), self.turn.take(), self.root.take()]
                .into_iter()
                .flatten()
                .collect();
            self.forget_agents(&stopped);
        }
        self.turn = None;
        self.root = None;
        self.command_turn = false;
        self.starting = false;
        self.pending.clear();
        self.state.compacting = false;
        self.state.turns_ended += 1;
        for (id, mut item) in std::mem::take(&mut self.open) {
            // A command still running was put in the background: listed
            // until its own end. Codex has no stop for one.
            // A subagent works on past the turn that spawned it, until its
            // own thread says it has ended.
            if self
                .agents
                .values()
                .any(|row| row.id == id && items::is_running(row))
            {
                self.open.insert(id, item);
                continue;
            }
            if self.execs.contains(&id) {
                if !self.state.background.iter().any(|task| task.id == id) {
                    self.state.background.push(v1::ChatBackgroundTask {
                        id: id.clone(),
                        kind: v1::ChatTaskKind::Command.into(),
                        description: item
                            .tool
                            .as_ref()
                            .map_or_else(String::new, |tool| tool.detail.clone()),
                        started_at_unix_millis: match item.at_unix_millis {
                            0 => super::now_unix_millis(),
                            at => at,
                        },
                        stoppable: false,
                    });
                }
                self.open.insert(id, item);
                continue;
            }
            items::stop(&mut item);
            out.publish(Change::Item(item));
        }
        self.execs.retain(|id| self.open.contains_key(id));
        self.sync_agent_tasks(out);
        match string(turn, "status") {
            "interrupted" => self.notice(format!("{id}-interrupted"), "Interrupted", out),
            "failed" => {
                let reason = match string(&turn["error"], "message") {
                    "" => "The turn failed.",
                    reason => reason,
                };
                self.notice(format!("{id}-failed"), reason, out);
            }
            _ => {}
        }
        self.publish_state(out);
        if self.rewind.as_ref().is_some_and(|rewind| !rewind.reverting) {
            self.revert(out);
        } else {
            self.send_held(out);
        }
    }

    /// Notes the turn a spawn's subagent was started in.
    fn note_spawn(&mut self, source: &Value, turn: &str) {
        if let Some(thread) = source["receiverThreadIds"][0].as_str()
            && self.agents.contains_key(thread)
        {
            self.spawned_in.insert(thread.to_owned(), turn.to_owned());
        }
    }

    /// Stops following the subagents spawned in `turns`, and gives their
    /// rows' ids.
    fn forget_agents(&mut self, turns: &[String]) -> Vec<String> {
        let spawned_in = &self.spawned_in;
        let mut rows = Vec::new();
        self.agents.retain(|thread, row| {
            let gone = spawned_in
                .get(thread)
                .is_some_and(|turn| turns.contains(turn));
            if gone {
                rows.push(row.id.clone());
            }
            !gone
        });
        let agents = &self.agents;
        self.spawned_in
            .retain(|thread, _| agents.contains_key(thread));
        rows
    }

    /// What a subagent's own thread says of it, on its row: what it is
    /// doing, the steps it has taken, the tokens it has used, and its end.
    fn subagent_event(&mut self, thread: String, method: &str, params: &Value, out: &mut Output) {
        let Some(row) = self.agents.get_mut(&thread) else {
            return;
        };
        let before = row.clone();
        match method {
            "turn/started" => agent_state(row, "running"),
            "turn/completed" => {
                let state = match string(&params["turn"], "status") {
                    "interrupted" => "interrupted",
                    "failed" => "errored",
                    _ => "completed",
                };
                agent_state(row, state);
            }
            "item/started" | "item/completed" => {
                let finished = method == "item/completed";
                let step = thread_item(&params["item"], finished).and_then(|item| item.tool);
                if let (Some(step), Some(tool)) = (step, row.tool.as_mut())
                    && items::is_running(&before)
                {
                    if finished {
                        tool.tool_uses += 1;
                    } else {
                        tool.activity = activity(&step);
                    }
                }
            }
            "thread/tokenUsage/updated" => {
                if let (Some(tokens), Some(tool)) = (
                    params["tokenUsage"]["last"]["totalTokens"].as_u64(),
                    row.tool.as_mut(),
                ) {
                    tool.tokens = tokens;
                }
            }
            _ => {}
        }
        if *row != before {
            let row = row.clone();
            self.track(&row);
            out.publish(Change::Item(row));
            self.sync_agent_tasks(out);
        }
    }

    /// Lists each subagent that runs among the work outside the turn, as
    /// the composer's pill shows it, and no other.
    fn sync_agent_tasks(&mut self, out: &mut Output) {
        let subagent = i32::from(v1::ChatTaskKind::Subagent);
        let mut tasks: Vec<v1::ChatBackgroundTask> = self
            .state
            .background
            .iter()
            .filter(|task| task.kind != subagent)
            .cloned()
            .collect();
        for row in self.agents.values().filter(|row| items::is_running(row)) {
            // Its start as first listed, when the row names none.
            let listed = self.state.background.iter().find(|task| task.id == row.id);
            let started = match (row.at_unix_millis, listed) {
                (0, Some(task)) => task.started_at_unix_millis,
                (0, None) => super::now_unix_millis(),
                (at, _) => at,
            };
            tasks.push(v1::ChatBackgroundTask {
                id: row.id.clone(),
                kind: subagent,
                // The prompt's first line, as its row reads.
                description: row
                    .tool
                    .as_ref()
                    .and_then(|tool| tool.detail.lines().find(|line| !line.trim().is_empty()))
                    .unwrap_or_default()
                    .trim()
                    .to_owned(),
                started_at_unix_millis: started,
                stoppable: false,
            });
        }
        if tasks != self.state.background {
            self.state.background = tasks;
            self.publish_state(out);
        }
    }

    fn revert(&mut self, out: &mut Output) {
        let Some(rewind) = &mut self.rewind else {
            return;
        };
        rewind.reverting = true;
        let params = json!({"threadId": self.thread, "beforeTurnId": rewind.turn});
        self.request("thread/revert", params, Asked::Revert, out);
    }

    fn asked_by_server(&mut self, id: &Value, method: &str, params: &Value, out: &mut Output) {
        let answerable = matches!(
            method,
            "item/commandExecution/requestApproval"
                | "item/fileChange/requestApproval"
                | "item/permissions/requestApproval"
                | "item/tool/requestUserInput"
        );
        if !answerable {
            // Among these are requests for the account's tokens, which are
            // not the chat's to give.
            let error = json!({"code": -32601, "message": "Not supported by this client."});
            return out.send(json!({"id": id, "error": error}).to_string());
        }
        self.pending.push_back(Pending {
            id: id.clone(),
            method: method.to_owned(),
            params: params.clone(),
        });
        self.publish_state(out);
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
            .filter(|pending| request_key(&pending.id) == request_id)
            .ok_or_else(|| {
                ChatError::new(
                    "chat_request_not_pending",
                    "The agent is no longer waiting for that answer.",
                )
            })?;
        let result = match front.method.as_str() {
            "item/tool/requestUserInput" => {
                let questions = front.params["questions"]
                    .as_array()
                    .map_or(&[][..], Vec::as_slice);
                let mut answers = serde_json::Map::new();
                for given in answer.answers.iter().filter(|_| !answer.skipped) {
                    let question = questions
                        .iter()
                        .find(|question| string(question, "id") == given.question_id);
                    let Some(question) = question else { continue };
                    let mut chosen: Vec<&str> = given
                        .option_ids
                        .iter()
                        .filter_map(|id| id.parse::<usize>().ok())
                        .filter_map(|index| question["options"].get(index))
                        .map(|option| string(option, "label"))
                        .collect();
                    if !given.other_text.trim().is_empty() {
                        chosen.push(given.other_text.trim());
                    }
                    answers.insert(given.question_id.clone(), json!({"answers": chosen}));
                }
                json!({"answers": answers})
            }
            "item/permissions/requestApproval" => {
                if answer.allow {
                    json!({"permissions": front.params["permissions"]})
                } else {
                    json!({"permissions": {}})
                }
            }
            _ => json!({"decision": if answer.allow { "accept" } else { "decline" }}),
        };
        out.send(json!({"id": front.id, "result": result}).to_string());
        self.pending.pop_front();
        self.publish_state(out);
        Ok(())
    }

    fn answered(&mut self, id: u64, message: &Value, out: &mut Output) {
        let Some(asked) = self.asked.remove(&id) else {
            return;
        };
        let result = &message["result"];
        let failed = !message["error"].is_null();
        let list = |name: &str| result[name].as_array().map_or(&[][..], Vec::as_slice);
        match asked {
            Asked::Initialize if failed => self.failed("The agent refused the chat.", out),
            Asked::Initialize => {
                out.send(json!({"method": "initialized", "params": {}}).to_string());
                self.request("model/list", json!({}), Asked::Models, out);
                let params = json!({"cwds": [self.cwd]});
                self.request("skills/list", params, Asked::Skills, out);
                self.open_thread(out);
            }
            Asked::Models if !failed => {
                self.models = list("data")
                    .iter()
                    .filter(|model| model["hidden"].as_bool() != Some(true))
                    .map(|model| v1::ChatModel {
                        id: string(model, "id").to_owned(),
                        label: string(model, "displayName").to_owned(),
                        efforts: model["supportedReasoningEfforts"]
                            .as_array()
                            .map_or(&[][..], Vec::as_slice)
                            .iter()
                            .map(|effort| string(effort, "reasoningEffort").to_owned())
                            .collect(),
                        supports_fast: model["serviceTiers"]
                            .as_array()
                            .map_or(&[][..], Vec::as_slice)
                            .iter()
                            .any(|tier| string(tier, "id") == FAST_TIER),
                    })
                    .collect();
                self.publish_catalog(out);
            }
            Asked::Skills if !failed => {
                self.skills = skills(result);
                self.publish_catalog(out);
            }
            Asked::Latest => match string(&result["data"][0], "id") {
                // Nothing to resume is a new conversation.
                "" => {
                    self.launch.resume = Resume::Fresh;
                    self.open_thread(out);
                }
                id => {
                    self.launch.resume = Resume::Session {
                        id: id.to_owned(),
                        fork: false,
                    };
                    self.open_thread(out);
                }
            },
            Asked::Thread if failed => {
                let reason = match string(&message["error"], "message") {
                    "" => "The agent could not open the conversation.",
                    reason => reason,
                };
                self.failed(reason, out);
            }
            Asked::Thread => self.thread_opened(result, out),
            Asked::Turn => {
                self.starting = false;
                if failed {
                    self.stop_wanted = false;
                    let reason = match string(&message["error"], "message") {
                        "" => "The message could not be sent.",
                        reason => reason,
                    };
                    self.notice(format!("request-{id}-failed"), reason, out);
                } else if self.turn.is_none() {
                    self.turn = Some(string(&result["turn"], "id").to_owned());
                    self.stop_if_wanted(out);
                }
                self.publish_state(out);
                if failed {
                    // What waited behind it opens the next turn.
                    self.send_held(out);
                }
            }
            Asked::Revert => {
                let Some(rewind) = self.rewind.take() else {
                    return;
                };
                if failed {
                    self.notice(
                        format!("request-{id}-failed"),
                        "The conversation could not go back to that message.",
                        out,
                    );
                } else {
                    out.publish(Change::Truncate(v1::ChatTruncate {
                        after_item_id: rewind.after_item_id,
                    }));
                    // The turns from that one on are gone with their messages,
                    // and the rows of the subagents they spawned.
                    let gone = self.turns.iter().position(|(_, turn)| *turn == rewind.turn);
                    let gone: Vec<(String, String)> = self
                        .turns
                        .drain(gone.unwrap_or(self.turns.len())..)
                        .collect();
                    let turns: Vec<String> = gone.into_iter().map(|(_, turn)| turn).collect();
                    for row in self.forget_agents(&turns) {
                        self.open.remove(&row);
                    }
                    self.sync_agent_tasks(out);
                    // A message sent meanwhile is the next one of the
                    // conversation as it now is.
                    for (index, held) in self.held.iter().enumerate() {
                        out.publish(Change::Item(held.item(index == 0)));
                    }
                }
                self.send_held(out);
            }
            // The turn it was for ended first: it opens the next one.
            Asked::Steer(held) if failed && self.turn.is_none() => self.send(held, out),
            Asked::Steer(_) if failed => {
                let text = "The message could not join the turn that is running.";
                self.notice(format!("request-{id}-failed"), text, out);
            }
            Asked::Command if failed => {
                self.starting = false;
                self.command_turn = false;
                self.stop_wanted = false;
                let reason = match string(&message["error"], "message") {
                    "" => "The command could not run.",
                    reason => reason,
                };
                self.notice(format!("request-{id}-failed"), reason, out);
                self.publish_state(out);
                self.send_held(out);
            }
            // A review names its turn; a compaction's says it has started.
            Asked::Command => {
                let turn = string(&result["turn"], "id");
                if self.turn.is_none() && !turn.is_empty() {
                    self.turn = Some(turn.to_owned());
                    self.starting = false;
                    self.stop_if_wanted(out);
                    self.publish_state(out);
                }
            }
            Asked::Models | Asked::Skills | Asked::Steer(_) | Asked::Other => {}
        }
    }

    /// Sends the stop that was asked for before the turn had an id.
    fn stop_if_wanted(&mut self, out: &mut Output) {
        if std::mem::take(&mut self.stop_wanted)
            && let Some(turn) = self.turn.clone()
        {
            let params = json!({"threadId": self.thread, "turnId": turn});
            self.request("turn/interrupt", params, Asked::Other, out);
        }
    }

    fn notification(&mut self, method: &str, params: &Value, out: &mut Output) {
        // One server could hold other threads; this chat is one of them, and
        // the subagents it spawned say on theirs how they get on.
        let thread = string(params, "threadId");
        if !thread.is_empty() && thread != self.thread {
            if self.agents.contains_key(thread) {
                self.subagent_event(thread.to_owned(), method, params, out);
            }
            return;
        }
        match method {
            "turn/started" => {
                let turn = &params["turn"];
                self.turn = Some(string(turn, "id").to_owned());
                self.root = match string(turn, "rootTurnId") {
                    "" => None,
                    root => Some(root.to_owned()),
                };
                self.starting = false;
                self.stop_if_wanted(out);
                self.note_thread(out);
                self.publish_state(out);
                self.send_held(out);
            }
            "turn/completed" => self.turn_completed(params, out),
            "item/started" => self.item(params, false, out),
            "item/completed" => self.item(params, true, out),
            "item/agentMessage/delta" | "item/plan/delta" => {
                self.delta(params, Kind::Assistant, out);
            }
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                self.delta(params, Kind::Reasoning, out);
            }
            "thread/tokenUsage/updated" => {
                let usage = &params["tokenUsage"];
                self.state.usage = Some(v1::ChatUsage {
                    used_tokens: usage["last"]["totalTokens"].as_u64().unwrap_or(0),
                    window_tokens: usage["modelContextWindow"].as_u64().unwrap_or(0),
                });
                self.publish_state(out);
            }
            "serverRequest/resolved" => {
                let resolved = request_key(&params["requestId"]);
                self.pending
                    .retain(|pending| request_key(&pending.id) != resolved);
                self.publish_state(out);
            }
            "error" => {
                let reason = string(&params["error"], "message");
                let retries = params["willRetry"].as_bool() == Some(true);
                if !reason.is_empty() {
                    // One line for a turn's retries, said again as they go.
                    let (id, text) = if retries {
                        ("retry", format!("Retrying: {reason}"))
                    } else {
                        ("error", reason.to_owned())
                    };
                    let id = format!("{}-{id}", string(params, "turnId"));
                    self.notice(id, &text, out);
                }
            }
            "warning" | "configWarning" | "deprecationNotice" => {
                let said = match method {
                    "warning" => string(params, "message"),
                    _ => string(params, "summary"),
                };
                let text = match string(params, "details") {
                    "" => said.to_owned(),
                    details => format!("{said}\n{details}"),
                };
                if !text.trim().is_empty() && self.warned.insert(text.clone()) {
                    self.notice(format!("warning-{}", self.warned.len()), &text, out);
                }
            }
            "model/rerouted" => {
                let (from, to) = (string(params, "fromModel"), string(params, "toModel"));
                // For this turn only: the chat's own choice goes with the next.
                let id = format!("{}-rerouted", string(params, "turnId"));
                let text = format!("Codex switched this turn from {from} to {to}");
                self.notice(id, &text, out);
            }
            "turn/plan/updated" => out.publish(Change::Item(plan_item(params))),
            "thread/name/updated" => {
                let name = string(params, "threadName");
                if self.naming.heard(name) {
                    out.name(name);
                }
            }
            "skills/changed" => {
                let params = json!({"cwds": [self.cwd]});
                self.request("skills/list", params, Asked::Skills, out);
            }
            _ => {}
        }
    }
}

/// Settings no ask for a title takes from the chat's start: they could
/// have it write, keep a session, or run what the user set to run.
const NOT_FOR_A_TITLE: &[&str] = &[
    "approval",
    "ephemeral",
    "features",
    "history",
    "hooks",
    "mcp_servers",
    "notify",
    "permissions",
    "sandbox",
];

/// What of the chat's start an ask for a title is started with too: its
/// `-c` settings, such as the model provider or a profile, and
/// `--strict-config`. Not its features, nor a setting under one of
/// [`NOT_FOR_A_TITLE`].
fn title_passthrough(passthrough: &[String]) -> Vec<String> {
    let mut kept = Vec::new();
    let mut words = passthrough.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--strict-config" => kept.push(word.clone()),
            "--config" => {
                let Some(setting) = words.next() else { break };
                let key = setting.split('=').next().unwrap_or_default().trim();
                let refused = NOT_FOR_A_TITLE
                    .iter()
                    .any(|refused| key.trim_start_matches('"').starts_with(refused));
                if !refused {
                    kept.extend(["-c".to_owned(), setting.clone()]);
                }
            }
            // `--enable`, `--disable` and their feature.
            _ => {
                words.next();
            }
        }
    }
    kept
}

/// The most of a `config.toml` read for the MCP servers it names.
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// What turns off, for one run, each MCP server the agent's `config.toml`
/// names: `exec` has no option for none, and `-c mcp_servers={}` adds to
/// the table rather than replacing it (0.169.0, as `codex mcp list` shows
/// it). A server whose name `-c` could not say is left on.
fn mcp_servers_off(environment: &[(std::ffi::OsString, std::ffi::OsString)]) -> Vec<String> {
    use std::io::Read;
    let variable = |name: &str| {
        environment
            .iter()
            .find(|(given, _)| given == name)
            .map(|(_, value)| std::path::PathBuf::from(value))
            .filter(|value| !value.as_os_str().is_empty())
    };
    let Some(home) =
        variable("CODEX_HOME").or_else(|| variable("HOME").map(|home| home.join(".codex")))
    else {
        return Vec::new();
    };
    let mut text = String::new();
    let read = std::fs::File::open(home.join("config.toml"))
        .and_then(|file| file.take(MAX_CONFIG_BYTES).read_to_string(&mut text));
    let Ok(config) = read
        .map_err(drop)
        .and_then(|_| text.parse::<toml_edit::DocumentMut>().map_err(drop))
    else {
        return Vec::new();
    };
    let Some(servers) = config
        .get("mcp_servers")
        .and_then(toml_edit::Item::as_table_like)
    else {
        return Vec::new();
    };
    servers
        .iter()
        .map(|(name, _)| name)
        .filter(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .flat_map(|name| ["-c".to_owned(), format!("mcp_servers.{name}.enabled=false")])
        .collect()
}

/// A JSON-RPC id as the chat names the request: the id may be a number or
/// a string.
fn request_key(id: &Value) -> String {
    id.as_str().map_or_else(|| id.to_string(), str::to_owned)
}

fn pending_request(pending: &Pending) -> v1::ChatPendingRequest {
    let params = &pending.params;
    let request_id = request_key(&pending.id);
    if pending.method == "item/tool/requestUserInput" {
        let question = |question: &Value| v1::ChatQuestion {
            id: string(question, "id").to_owned(),
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
            multi_select: false,
            allow_other: question["isOther"].as_bool() == Some(true)
                || question["options"].as_array().is_none_or(Vec::is_empty),
        };
        return v1::ChatPendingRequest {
            request_id,
            kind: v1::ChatRequestKind::Question.into(),
            title: "Question".to_owned(),
            detail: String::new(),
            questions: params["questions"]
                .as_array()
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .map(question)
                .collect(),
        };
    }
    let (title, detail) = match pending.method.as_str() {
        "item/commandExecution/requestApproval" => {
            let action = string(&params["commandActions"][0], "command");
            let command = if action.is_empty() {
                string(params, "command")
            } else {
                action
            };
            ("Run a command", command.to_owned())
        }
        "item/fileChange/requestApproval" => ("Change files", string(params, "reason").to_owned()),
        _ => ("Grant access", string(params, "reason").to_owned()),
    };
    v1::ChatPendingRequest {
        request_id,
        kind: v1::ChatRequestKind::Permission.into(),
        title: title.to_owned(),
        detail,
        questions: Vec::new(),
    }
}

impl Dialect for Codex {
    const FRAMING: Framing = Framing::WebSocket;
    type Job = Job;
    type Done = Done;

    fn run(job: Job) -> Done {
        match job {
            Job::Tag { pane_id, thread } => {
                let _ = pane::set(&pane_id, &[(pane::SESSION, &thread)]);
                Done::Tagged
            }
            Job::Title(ask) => Done::Titled(naming::run(ask)),
        }
    }

    fn done(&mut self, done: Done, out: &mut Output) {
        let Done::Titled(titled) = done else {
            return;
        };
        if self.ended || !self.naming.still_wanted(titled.ticket) {
            return;
        }
        match titled.title {
            Some(title) => self.rename(title, out),
            None if titled.asked_by_user => {
                let id = format!("rename-{}", super::now_unix_millis());
                self.notice(id, naming::NOT_NAMED, out);
            }
            None => {}
        }
    }

    fn connected(&mut self, tag: &Tag, out: &mut Output) {
        self.launch = tag.launch.clone();
        self.cwd.clone_from(&tag.cwd);
        self.thread.clone_from(&tag.session);
        self.noted = !self.thread.is_empty();
        self.ready = false;
        self.asked.clear();
        self.pending.clear();
        // What a rewind waited for was asked over the link that is gone. The
        // thread says where it stands when it is opened again.
        self.rewind = None;
        self.stop_wanted = false;
        // A request the agent waited on is not known to have been answered:
        // losing the link is no word that the agent stopped waiting.
        self.reported_blocked = false;
        // Whether a command put in the background still runs is not known
        // over a new link: its end may have been said to nobody.
        self.execs.clear();
        for task in std::mem::take(&mut self.state.background) {
            if let Some(mut item) = self.open.remove(&task.id) {
                items::stop(&mut item);
                out.publish(Change::Item(item));
            }
        }
        self.state.connection = Connection::Connecting.into();
        self.publish_state(out);
        let version = env!("CARGO_PKG_VERSION");
        let client = json!({"name": "muxflow", "title": "Muxflow", "version": version});
        self.request(
            "initialize",
            json!({"clientInfo": client}),
            Asked::Initialize,
            out,
        );
    }

    fn message(&mut self, message: &str, out: &mut Output) {
        let Ok(message) = serde_json::from_str::<Value>(message) else {
            return;
        };
        let id = &message["id"];
        match message["method"].as_str() {
            Some(method) if !id.is_null() => {
                self.asked_by_server(id, method, &message["params"], out);
            }
            Some(method) => self.notification(method, &message["params"], out),
            None => {
                if let Some(id) = id.as_u64() {
                    self.answered(id, &message, out);
                }
            }
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
                let command = builtin(&text).map(|(builtin, _)| builtin);
                // A turn that was asked for and has no id yet cannot be
                // joined, a built-in joins none, and nothing goes ahead of
                // what already waits.
                let waits = !self.ready
                    || self.rewind.is_some()
                    || self.starting
                    || self.command_turn
                    || !self.held.is_empty()
                    || (command.is_some() && self.turn.is_some());
                if waits && self.held.len() >= MAX_HELD {
                    return Err(not_ready());
                }
                let held = Held {
                    text,
                    item_id,
                    attachments,
                };
                // A message that joins a running turn cannot be gone back to
                // on its own.
                let opens = self.turn.is_none() && !self.starting && self.held.is_empty();
                out.publish(Change::Item(held.item(opens)));
                if waits {
                    self.held.push_back(held);
                } else {
                    self.send(held, out);
                }
                self.ask_title(out);
                Ok(())
            }
            SessionCommand::Interrupt => {
                if !self.ready {
                    return Err(not_ready());
                }
                if let Some(turn) = self.turn.clone() {
                    let params = json!({"threadId": self.thread, "turnId": turn});
                    self.request("turn/interrupt", params, Asked::Other, out);
                } else if self.starting {
                    // A turn can be stopped once the agent has named it.
                    self.stop_wanted = true;
                }
                Ok(())
            }
            SessionCommand::Answer { request_id, answer } => self.answer(&request_id, &answer, out),
            SessionCommand::Rewind {
                item_id,
                after_item_id,
            } => {
                if !self.ready {
                    return Err(not_ready());
                }
                if self.rewind.is_some() {
                    return Err(ChatError::new(
                        "chat_turn_busy",
                        "The chat is already going back to an earlier message.",
                    ));
                }
                // A turn that has no id yet can be neither stopped nor
                // gone back past.
                if self.starting {
                    return Err(ChatError::new(
                        "chat_turn_busy",
                        "A message is just being sent. Try again in a moment.",
                    ));
                }
                let opened = self.turns.iter().find(|(id, _)| *id == item_id);
                let turn = opened.map(|(_, turn)| turn).filter(|turn| !turn.is_empty());
                let Some(turn) = turn.cloned() else {
                    return Err(ChatError::new(
                        "chat_item_not_rewindable",
                        "That message cannot be edited yet.",
                    ));
                };
                self.rewind = Some(Rewind {
                    turn,
                    after_item_id,
                    reverting: false,
                });
                match self.turn.clone() {
                    // `turn/completed` is what says it has stopped.
                    Some(running) => {
                        let params = json!({"threadId": self.thread, "turnId": running});
                        self.request("turn/interrupt", params, Asked::Other, out);
                    }
                    None => self.revert(out),
                }
                Ok(())
            }
            SessionCommand::SetOptions(options) => {
                let current = self.state.options.clone().unwrap_or_default();
                self.permission_chosen |= options.permission != current.permission;
                self.speed_chosen |= options.speed != current.speed;
                // They go with the next turn, which is when they apply.
                self.state.options = Some(options);
                self.publish_state(out);
                Ok(())
            }
            // The commands it lists in the background have no stop here: the
            // server's is an experimental API this chat does not take.
            SessionCommand::StopTask { .. } => Err(super::cannot_stop()),
            SessionCommand::Rename { item_id, name } => {
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

    /// A copy of the thread through the reply's turn: `thread/fork` with
    /// `lastTurnId`, which takes no turn still in progress (0.169.0 schema;
    /// not yet tried against a live server).
    fn fork(&mut self, at: &ForkAt, _out: &mut Output) -> Result<ForkPlan, ChatError> {
        if self.ended {
            return Err(ChatError::new("chat_unavailable", "The agent has exited."));
        }
        if self.thread.is_empty() {
            return Err(ChatError::new(
                "chat_fork_unavailable",
                "This chat has no conversation to fork yet.",
            ));
        }
        if self.rewind.is_some() {
            return Err(ChatError::new(
                "chat_turn_busy",
                "The chat is already going back to an earlier message.",
            ));
        }
        // The turn the newest message that opened one opened: a message
        // sent into a running turn joined it.
        let turn = at
            .before
            .iter()
            .find_map(|id| self.turns.iter().find(|(opened, _)| opened == id))
            .map(|(_, turn)| turn.clone())
            .ok_or_else(|| {
                ChatError::new("chat_fork_unavailable", "That reply cannot be forked.")
            })?;
        // Not yet named by the server, or still running.
        if turn.is_empty() || self.turn.as_deref() == Some(turn.as_str()) {
            return Err(still_writing());
        }
        let mut launch = self.launch.clone();
        let options = self.state.options.clone().unwrap_or_default();
        // The copy is set as the chat is now, not as it was first started.
        launch.model = options.model;
        launch.effort = options.effort;
        launch.full_access = options.permission == i32::from(Permission::Full);
        launch.resume = Resume::Session {
            id: self.thread.clone(),
            fork: true,
        };
        launch.resume_at = turn;
        Ok(Box::new(move || Ok(launch)))
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
        self.turn = None;
        self.root = None;
        self.command_turn = false;
        self.starting = false;
        self.stop_wanted = false;
        self.pending.clear();
        self.held.clear();
        self.rewind = None;
        self.agents.clear();
        self.spawned_in.clear();
        self.state.compacting = false;
        // What it ran in the background went with it.
        self.execs.clear();
        self.state.background.clear();
        for mut item in std::mem::take(&mut self.open).into_values() {
            items::stop(&mut item);
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
