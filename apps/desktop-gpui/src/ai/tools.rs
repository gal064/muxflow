//! The tools the assistant may ask for, and the words that describe them.
//! Port of `features/ai/tools.ts`.
//!
//! The table is in the OpenAI shape and passes through the engine untouched.
//! The set is small on purpose: each tool is something the panel can do with
//! plumbing it already has. The two that change anything — typing into a pane,
//! starting a run — wait on the user's approval; the three that only look
//! around (read a screen, list panes, show a pane) answer from state the panel
//! already holds and need none.
//!
//! Running a call is [`super::panel`]'s job — it needs the live controller,
//! the terminal engines and the helper bridges. This module owns the parts
//! that are pure: the table, the one-line summaries, the argument parsing, and
//! reading a pane of the current tab out of a fresh snapshot.

use std::sync::Arc;

use serde_json::{Value, json};
use tmux_control::{Pane, TmuxSnapshot};

/// A tool call the model completed, in the OpenAI echo shape.
#[derive(Debug, Clone)]
pub struct AiToolCall {
    pub id: String,
    pub name: String,
    /// The raw argument JSON the model streamed; parsed once, at execution.
    pub arguments: String,
}

/// The tool table, in the OpenAI shape.
pub fn tool_table() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "send_to_pane",
                "description":
                    "Type text into a terminal pane of the current tab. The user approves every use and sees the exact text first. \
                     Use press_enter=true only when the text is complete and meant to run; with false it only lands on the input line.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "The text to type. One command, or a short block." },
                        "pane": { "type": "integer", "description": "Pane index in the current window. Defaults to the active pane." },
                        "press_enter": { "type": "boolean", "description": "Press Enter after the text, submitting it. Default false." },
                    },
                    "required": ["text"],
                },
            },
        }),
        json!({
            "type": "function",
            "function": {
                "name": "read_pane",
                "description":
                    "Read a pane's screen as plain text: what the user sees right now, plus as much scrollback as the terminal holds. \
                     The truthful way to find out what an agent or command printed — the question-time snapshot never contains screen text. Read-only.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pane": { "type": "integer", "description": "Pane index in the current window. Defaults to the active pane." },
                        "lines": { "type": "integer", "description": "Rows to read from the bottom. Default 200." },
                    },
                },
            },
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_panes",
                "description":
                    "List the panes of the current tab as they are right now: index, active, working directory and running command, \
                     plus any coding agents in them. Call it after acting — the snapshot from question time is stale by then.",
                "parameters": { "type": "object", "properties": {} },
            },
        }),
        json!({
            "type": "function",
            "function": {
                "name": "show_pane",
                "description": "Bring one pane of the current tab on screen. Harmless: it only moves what the user is looking at.",
                "parameters": {
                    "type": "object",
                    "properties": { "pane": { "type": "integer", "description": "The pane index to show." } },
                    "required": ["pane"],
                },
            },
        }),
        json!({
            "type": "function",
            "function": {
                "name": "run_automation",
                "description":
                    "Start one scheduled automation on this host now, outside its schedule. The user approves it; it opens a new tab and runs the agent there.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "automation_id": { "type": "string", "description": "The automation's id, as the Automations snapshot named it." },
                    },
                    "required": ["automation_id"],
                },
            },
        }),
    ]
}

/// Tools that change something outside the screen; each one waits on the user.
pub fn needs_approval(name: &str) -> bool {
    matches!(name, "send_to_pane" | "run_automation")
}

/// The longest text the assistant may type into a pane in one call.
pub const MAX_SEND_CHARS: usize = 8_000;
/// Rows `read_pane` takes when the model does not say, and its hard ceiling.
pub const DEFAULT_READ_LINES: i64 = 200;
pub const MAX_READ_LINES: i64 = 2_000;
/// Characters `read_pane` returns; the newest tail of the screen is kept.
pub const MAX_READ_CHARS: usize = 16_000;

/// The raw argument JSON as an object; anything unreadable is no arguments,
/// which every tool below answers in words rather than by failing.
pub fn parse_arguments(call: &AiToolCall) -> Value {
    serde_json::from_str(&call.arguments)
        .ok()
        .filter(|parsed: &Value| parsed.is_object())
        .unwrap_or_else(|| json!({}))
}

fn one_line(text: &str) -> String {
    let folded = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if folded.chars().count() > 80 {
        format!("{}…", folded.chars().take(77).collect::<String>())
    } else {
        folded
    }
}

/// One human line naming a call, for the approval card and the action chip.
pub fn summarize_call(call: &AiToolCall) -> String {
    let args = parse_arguments(call);
    match call.name.as_str() {
        "send_to_pane" => {
            let shown = one_line(args.get("text").and_then(Value::as_str).unwrap_or(""));
            let where_ = match args.get("pane").and_then(Value::as_i64) {
                Some(pane) => format!("pane {pane}"),
                None => "the active pane".to_owned(),
            };
            if args.get("press_enter").and_then(Value::as_bool) == Some(true) {
                format!("type “{shown}” into {where_} and press Enter")
            } else {
                format!("type “{shown}” into {where_}")
            }
        }
        "read_pane" => match args.get("pane").and_then(Value::as_i64) {
            Some(pane) => format!("read pane {pane}"),
            None => "read the active pane's screen".to_owned(),
        },
        "list_panes" => "read the current tab's panes".into(),
        "show_pane" => match args.get("pane").and_then(Value::as_i64) {
            Some(pane) => format!("show pane {pane}"),
            None => "show the active pane".to_owned(),
        },
        "run_automation" => {
            format!(
                "run automation “{}” now",
                args.get("automation_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            )
        }
        _ => call.name.clone(),
    }
}

/// The command in full when the summary had to flatten or cut it, for the
/// card's click-to-expand; `None` when the summary already says all.
pub fn call_detail(call: &AiToolCall) -> Option<String> {
    if call.name != "send_to_pane" {
        return None;
    }
    let text = parse_arguments(call)
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_owned)?;
    let folded = one_line(&text);
    // The summary flattens whitespace and cuts at eighty characters; either
    // loss means the full text is worth a click.
    if text.trim() != folded || folded.chars().count() > 80 {
        Some(text)
    } else {
        None
    }
}

/// The approval card's question, naming the risk in its own words.
pub fn approval_title(name: &str) -> &'static str {
    if name == "run_automation" {
        "Run this automation now?"
    } else {
        "Type this into the terminal?"
    }
}

/// The shell's current view, fresh at call time — not the question's snapshot.
/// The panel rebuilds this before every tool so acting sees what is on screen
/// now.
#[derive(Clone)]
pub struct SessionNow {
    pub snapshot: Arc<TmuxSnapshot>,
    pub active_window_id: Option<String>,
    /// The host's coding agents, as (window id, label, lifecycle words).
    pub agents: Vec<(String, String, &'static str)>,
}

impl SessionNow {
    pub fn window(&self) -> Option<tmux_control::Window> {
        self.snapshot
            .windows
            .iter()
            .find(|candidate| Some(&candidate.id) == self.active_window_id.as_ref())
            .cloned()
    }
}

/// A pane of the current window by index, or the active one when no index is
/// given.
pub fn resolve_pane(session: &SessionNow, index: Option<i64>) -> Option<Pane> {
    let window = session.window()?;
    let panes: Vec<&Pane> = session
        .snapshot
        .panes
        .iter()
        .filter(|pane| pane.window_id == window.id)
        .collect();
    match index {
        Some(index) if index >= 0 => panes
            .into_iter()
            .find(|pane| pane.index as i64 == index)
            .cloned(),
        _ => panes
            .iter()
            .find(|pane| pane.active)
            .or_else(|| panes.first())
            .map(|pane| (*pane).clone()),
    }
}

fn describe_panes(session: &SessionNow) -> String {
    let Some(window) = session.window() else {
        return "The current tab is gone.".into();
    };
    let panes: Vec<&Pane> = session
        .snapshot
        .panes
        .iter()
        .filter(|pane| pane.window_id == window.id)
        .collect();
    if panes.is_empty() {
        return format!("Window \"{}\" has no panes.", window.name);
    }
    let mut lines: Vec<String> = panes
        .iter()
        .map(|pane| {
            format!(
                "#{}{}: {} — running {}",
                pane.index,
                if pane.active { " (active)" } else { "" },
                if pane.current_path.is_empty() {
                    "?"
                } else {
                    &pane.current_path
                },
                if pane.current_command.is_empty() {
                    "?"
                } else {
                    &pane.current_command
                },
            )
        })
        .collect();
    for (window_id, label, lifecycle) in &session.agents {
        if window_id == &window.id {
            lines.push(format!(
                "agent {label} in pane of \"{}\": {lifecycle}",
                window.name
            ));
        }
    }
    lines.join("\n")
}

/// The `lines` bound of a `read_pane`: floored, at least one row, capped.
pub fn read_rows(args: &Value) -> usize {
    let requested = args
        .get("lines")
        .and_then(Value::as_f64)
        .map(|lines| lines.floor() as i64);
    requested
        .unwrap_or(DEFAULT_READ_LINES)
        .clamp(1, MAX_READ_LINES) as usize
}

/// The newest tail of a screen, kept under the character budget.
pub fn screen_tail(text: &str) -> String {
    if text.chars().count() <= MAX_READ_CHARS {
        return text.to_owned();
    }
    let keep: String = text
        .chars()
        .rev()
        .take(MAX_READ_CHARS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("[…older text dropped…]\n{keep}")
}

/// What a call cannot do comes back as a refusal the model can read — a tool
/// error is information, not an exception. These are the answers and refusals
/// that need no live state; the panel's executor runs the rest.
#[derive(Debug, PartialEq, Eq)]
pub enum Preflight {
    /// The call is answered here: `list_panes` from the fresh snapshot, or a
    /// refusal the model can read.
    Answer(String),
    /// Needs the live world: a host to type into, a screen to read.
    Proceed,
}

/// The stateless half of running a call: the description, and the input
/// sanity checks that do not need a host.
pub fn preflight(call: &AiToolCall, session: Option<&SessionNow>) -> Preflight {
    let args = parse_arguments(call);
    match call.name.as_str() {
        "send_to_pane" => {
            let text = args.get("text").and_then(Value::as_str).unwrap_or("");
            if text.is_empty() {
                return Preflight::Answer("Nothing to type: the text was empty.".into());
            }
            if text.chars().count() > MAX_SEND_CHARS {
                return Preflight::Answer(format!(
                    "Refused: {} characters is more than one screen of typing.",
                    text.chars().count()
                ));
            }
            Preflight::Proceed
        }
        "list_panes" => match session {
            Some(session) => Preflight::Answer(describe_panes(session)),
            None => Preflight::Answer("The shell state is unavailable.".into()),
        },
        _ => Preflight::Proceed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tmux_control::{Pane, Session, Window};

    fn call(name: &str, arguments: &str) -> AiToolCall {
        AiToolCall {
            id: "c".into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    fn session(active_window: Option<&str>) -> SessionNow {
        SessionNow {
            snapshot: Arc::new(TmuxSnapshot {
                sessions: vec![Session {
                    id: "$1".into(),
                    name: "work".into(),
                    window_count: 1,
                    attached_clients: 0,
                    order: 0,
                    pinned: false,
                }],
                windows: vec![Window {
                    id: "@1".into(),
                    session_id: "$1".into(),
                    index: 0,
                    name: "build".into(),
                    active: true,
                    layout: String::new(),
                    zoomed: false,
                    pinned: false,
                }],
                panes: vec![
                    Pane {
                        id: "%1".into(),
                        session_id: "$1".into(),
                        window_id: "@1".into(),
                        index: 0,
                        active: false,
                        width: 80,
                        height: 24,
                        left: 0,
                        top: 0,
                        current_path: "/work".into(),
                        current_command: "bash".into(),
                        pane_pid: 1,
                        start_command: String::new(),
                        title: String::new(),
                        chat_adapter_id: String::new(),
                    },
                    Pane {
                        id: "%2".into(),
                        session_id: "$1".into(),
                        window_id: "@1".into(),
                        index: 1,
                        active: true,
                        width: 80,
                        height: 24,
                        left: 40,
                        top: 0,
                        current_path: String::new(),
                        current_command: "cargo build".into(),
                        pane_pid: 2,
                        start_command: String::new(),
                        title: String::new(),
                        chat_adapter_id: String::new(),
                    },
                ],
            }),
            active_window_id: active_window.map(str::to_owned),
            agents: vec![("@1".to_owned(), "Codex".to_owned(), "working")],
        }
    }

    #[test]
    fn summaries_fold_flatten_and_point_at_the_right_pane() {
        assert_eq!(
            summarize_call(&call(
                "send_to_pane",
                r#"{"text":"git  status","press_enter":true}"#
            )),
            "type “git status” into the active pane and press Enter"
        );
        assert_eq!(
            summarize_call(&call("read_pane", "{}")),
            "read the active pane's screen"
        );
        assert_eq!(
            summarize_call(&call("show_pane", r#"{"pane":3}"#)),
            "show pane 3"
        );
        assert_eq!(
            summarize_call(&call("run_automation", r#"{"automation_id":"nightly"}"#)),
            "run automation “nightly” now"
        );
        let long = format!(r#"{{"text":"{}"}}"#, "x".repeat(200));
        let summary = summarize_call(&call("send_to_pane", &long));
        assert!(summary.contains('…'));
        assert!(summary.chars().count() < 110);
    }

    #[test]
    fn the_detail_appears_only_when_the_summary_lost_something() {
        assert_eq!(
            call_detail(&call("send_to_pane", r#"{"text":"ls -la"}"#)),
            None
        );
        let multiline = call_detail(&call("send_to_pane", r#"{"text":"ls\ngit status"}"#)).unwrap();
        assert!(multiline.contains('\n'));
        assert_eq!(call_detail(&call("list_panes", "{}")), None);
        assert_eq!(call_detail(&call("send_to_pane", "not json")), None);
    }

    #[test]
    fn arguments_parse_to_an_object_or_nothing_useable() {
        assert_eq!(parse_arguments(&call("x", r#"{"a":1}"#)), json!({"a": 1}));
        assert_eq!(parse_arguments(&call("x", "broken")), json!({}));
        assert_eq!(parse_arguments(&call("x", "[1,2]")), json!({}));
    }

    #[test]
    fn a_pane_resolves_by_index_or_to_the_active_one() {
        let live = session(Some("@1"));
        assert_eq!(resolve_pane(&live, Some(0)).unwrap().id, "%1");
        assert_eq!(resolve_pane(&live, Some(1)).unwrap().id, "%2");
        assert_eq!(resolve_pane(&live, None).unwrap().id, "%2");
        assert_eq!(resolve_pane(&live, Some(9)), None);
        // A gone tab has no panes to resolve.
        assert_eq!(resolve_pane(&session(None), None), None);
    }

    #[test]
    fn list_panes_describes_the_tab_as_it_is_now() {
        let Preflight::Answer(text) =
            preflight(&call("list_panes", "{}"), Some(&session(Some("@1"))))
        else {
            panic!("list_panes answers inline");
        };
        assert!(text.contains("#1 (active): ? — running cargo build"));
        assert!(text.contains("#0: /work — running bash"));
        assert!(text.contains("agent Codex in pane of \"build\": working"));
        assert_eq!(
            preflight(&call("list_panes", "{}"), None),
            Preflight::Answer("The shell state is unavailable.".into())
        );
    }

    #[test]
    fn send_to_pane_refuses_empty_and_oversized_text_before_asking_the_user() {
        assert_eq!(
            preflight(
                &call("send_to_pane", r#"{"text":""}"#),
                Some(&session(Some("@1")))
            ),
            Preflight::Answer("Nothing to type: the text was empty.".into())
        );
        let big = format!(r#"{{"text":"{}"}}"#, "x".repeat(MAX_SEND_CHARS + 1));
        assert!(matches!(
            preflight(&call("send_to_pane", &big), Some(&session(Some("@1")))),
            Preflight::Answer(message) if message.contains("more than one screen")
        ));
        assert!(matches!(
            preflight(&call("send_to_pane", r#"{"text":"ls"}"#), None),
            Preflight::Proceed
        ));
    }

    #[test]
    fn read_bounds_stay_sane_and_the_screen_tail_keeps_the_newest_rows() {
        assert_eq!(read_rows(&json!({})), 200);
        assert_eq!(read_rows(&json!({"lines": 5.7})), 5);
        assert_eq!(read_rows(&json!({"lines": -3})), 1);
        assert_eq!(read_rows(&json!({"lines": 999999})), 2_000);
        assert_eq!(screen_tail("short"), "short");
        let long = "y".repeat(MAX_READ_CHARS + 100);
        let tail = screen_tail(&long);
        assert!(tail.starts_with("[…older text dropped…]"));
        assert_eq!(
            tail.chars().count() - "[…older text dropped…]\n".chars().count(),
            MAX_READ_CHARS
        );
    }

    #[test]
    fn the_table_offers_exactly_the_five_tools() {
        let table = tool_table();
        let names: Vec<&str> = table
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "send_to_pane",
                "read_pane",
                "list_panes",
                "show_pane",
                "run_automation"
            ]
        );
        assert!(needs_approval("send_to_pane") && needs_approval("run_automation"));
        assert!(
            !needs_approval("read_pane")
                && !needs_approval("list_panes")
                && !needs_approval("show_pane")
        );
    }
}
