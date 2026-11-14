use std::path::Path;

use serde_json::{Value, json};

use super::super::tests::replay::{Replay, Wire};
use super::*;

const STREAM: &str = include_str!("../fixtures/claude-session.stream.jsonl");
const RESUMED: &str = include_str!("../fixtures/claude-resumed.stream.jsonl");
const SESSION_FILE: &str = include_str!("../fixtures/claude-session.jsonl");
const SESSION: &str = "5e551011-0000-4000-8000-000000000001";
/// The ids the recorded messages were sent under.
const M1: &str = "11111111-1111-4111-8111-111111111111";
const M2: &str = "22222222-2222-4222-8222-222222222222";
const M3: &str = "33333333-3333-4333-8333-333333333333";
const M4: &str = "44444444-4444-4444-8444-444444444444";
const M5: &str = "55555555-5555-4555-8555-555555555555";

impl Wire for Claude {
    fn asked(written: &Value) -> Option<(String, Value)> {
        let subtype = written["request"]["subtype"].as_str()?;
        (written["type"] == "control_request")
            .then(|| (subtype.to_owned(), written["request_id"].clone()))
    }

    fn answers(inbound: &mut Value) -> Option<&mut Value> {
        inbound.get_mut("response")?.get_mut("request_id")
    }
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn launch(words: &[&str], resumed: Option<&str>) -> Option<Launch> {
    ClaudeProvider.launch(&strings(words), resumed)
}

fn tag(session: &str, rewound_to: &str) -> Tag {
    Tag {
        adapter: ADAPTER_ID.into(),
        session: session.into(),
        rewound_to: rewound_to.into(),
        ..Default::default()
    }
}

/// A home whose one Claude session is the recorded one.
fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let project = home.path().join("projects/-work");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join(format!("{SESSION}.jsonl")), SESSION_FILE).unwrap();
    home
}

fn read_history(home: &Path, rewound_to: &str) -> Done {
    Done::History(history::load(home, SESSION, rewound_to))
}

// --- the command line -------------------------------------------------------

#[test]
fn a_terminal_start_is_read_into_what_its_chat_starts_with() {
    let plain = launch(&[], None).unwrap();
    assert_eq!(plain, Launch::default());

    let full = launch(
        &[
            "--model",
            "opus",
            "--effort=high",
            "--dangerously-skip-permissions",
            "--add-dir",
            "/a",
            "/b",
            "-n",
            "Fix tests",
            "--mcp-config",
            "one.json",
            "two.json",
            "--verbose",
            "--",
            "fix the tests",
        ],
        None,
    )
    .unwrap();
    assert_eq!(
        full,
        Launch {
            model: "opus".into(),
            effort: "high".into(),
            full_access: true,
            directories: strings(&["/a", "/b"]),
            name: "Fix tests".into(),
            passthrough: strings(&["--mcp-config", "one.json", "two.json"]),
            prompt: "fix the tests".into(),
            ..Default::default()
        }
    );
    assert!(
        launch(&["--permission-mode", "bypassPermissions"], None)
            .unwrap()
            .full_access
    );
    assert!(
        !launch(&["--permission-mode", "acceptEdits"], None)
            .unwrap()
            .full_access
    );
    assert_eq!(
        launch(&["explain this"], None).unwrap().prompt,
        "explain this"
    );
}

#[test]
fn the_session_a_chat_resumes_is_the_one_the_agent_was_asked_for_or_says_it_picked() {
    let other = "0ther000-0000-4000-8000-000000000002".replace("0ther000", "a0000000");
    let session = |id: &str, fork| Resume::Session {
        id: id.to_owned(),
        fork,
    };
    let resume = |words: &[&str], resumed| launch(words, resumed).map(|launch| launch.resume);
    assert_eq!(resume(&[], None), Some(Resume::Fresh));
    assert_eq!(
        resume(&["--resume", SESSION], None),
        Some(session(SESSION, false))
    );
    assert_eq!(
        resume(&["-r", SESSION], None),
        Some(session(SESSION, false))
    );
    // A copy is the chat's to make: the agent has not made it yet.
    assert_eq!(
        resume(&["--resume", SESSION, "--fork-session"], Some(&other)),
        Some(session(SESSION, true))
    );
    // A picker: the agent says what was picked, or nobody knows.
    assert_eq!(
        resume(&["--resume"], Some(SESSION)),
        Some(session(SESSION, false))
    );
    assert_eq!(
        resume(&["--resume", "tests"], Some(SESSION)),
        Some(session(SESSION, false))
    );
    assert_eq!(resume(&["--resume"], None), None);
    assert_eq!(resume(&["--resume", "tests"], None), None);
    assert_eq!(
        resume(&["--continue"], Some(SESSION)),
        Some(session(SESSION, false))
    );
    assert_eq!(resume(&["-c"], None), Some(Resume::Latest));
    // A started session that says so resumes nothing.
    assert_eq!(resume(&[], Some("not a session id")), Some(Resume::Fresh));
}

#[test]
fn a_start_that_is_not_a_session_stays_a_terminal() {
    for words in [
        &["-p", "say hi"][..],
        &["--print"],
        &["mcp", "list"],
        &["update"],
        &["doctor"],
        &["--version"],
        &["--help"],
        &["--bg"],
        &["--worktree"],
        &["--permission-mode", "plan"],
        &["--some-new-option"],
        &["two", "prompts"],
    ] {
        assert_eq!(launch(words, None), None, "{words:?}");
    }
}

#[test]
fn the_pane_runs_the_agent_headless_on_the_session_asked_for() {
    let command = |launch: &Launch| {
        ClaudeProvider
            .pane_command(launch, Path::new("/run/chat/a.sock"))
            .unwrap()
    };
    let fresh = command(&Launch {
        program: strings(&["node", "/opt/claude/cli.js"]),
        model: "opus".into(),
        full_access: true,
        directories: strings(&["/a", "/b"]),
        name: "Fix tests".into(),
        passthrough: strings(&["--settings", "s.json"]),
        ..Default::default()
    });
    assert!(fresh.piped);
    assert!(is_session_id(&fresh.session));
    let expected = [
        "node",
        "/opt/claude/cli.js",
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--replay-user-messages",
        "--permission-prompt-tool",
        "stdio",
        "--allow-dangerously-skip-permissions",
        "--permission-mode",
        "bypassPermissions",
        "--model",
        "opus",
        "--name",
        "Fix tests",
        "--settings",
        "s.json",
        "--add-dir",
        "/a",
        "/b",
        "--session-id",
        fresh.session.as_str(),
    ];
    assert_eq!(fresh.argv, expected);

    let tail = |launch: Launch| {
        let command = command(&Launch {
            program: strings(&["claude"]),
            ..launch
        });
        let from = command.argv.iter().position(|word| word == "auto").unwrap();
        (command.argv[from + 1..].to_vec(), command.session)
    };
    let resumed = Resume::Session {
        id: SESSION.into(),
        fork: false,
    };
    let (argv, session) = tail(Launch {
        resume: resumed.clone(),
        ..Default::default()
    });
    assert_eq!(
        (argv, session.as_str()),
        (strings(&["--resume", SESSION]), SESSION)
    );
    // Part of the way: where a rewind starts the agent again.
    let (argv, session) = tail(Launch {
        resume: resumed,
        resume_at: M2.into(),
        ..Default::default()
    });
    assert_eq!(argv, ["--resume", SESSION, "--resume-session-at", M2]);
    assert_eq!(session, SESSION);
    // A copy gets an id of its own, known before the agent says it.
    let (argv, session) = tail(Launch {
        resume: Resume::Session {
            id: SESSION.into(),
            fork: true,
        },
        ..Default::default()
    });
    assert_eq!(
        argv,
        [
            "--resume",
            SESSION,
            "--fork-session",
            "--session-id",
            &session
        ]
    );
    assert_ne!(session, SESSION);
    let (argv, session) = tail(Launch {
        resume: Resume::Latest,
        ..Default::default()
    });
    assert_eq!((argv, session.as_str()), (strings(&["--continue"]), ""));
}

// --- single messages --------------------------------------------------------

#[test]
fn a_user_entry_is_a_message_a_stop_or_the_agents_own_note() {
    let entry =
        |content: Value| json!({"type": "user", "message": {"role": "user", "content": content}});
    assert_eq!(
        said(&entry(json!("  fix it "))),
        Said::User("fix it".into())
    );
    assert_eq!(
        said(&entry(
            json!([{"type": "text", "text": "one"}, {"type": "text", "text": "two"}])
        )),
        Said::User("one\ntwo".into())
    );
    assert_eq!(
        said(&entry(
            json!([{"type": "text", "text": "[Request interrupted by user for tool use]"}])
        )),
        Said::Interrupted
    );
    assert_eq!(
        said(&entry(json!(
            "<command-message>review</command-message>\n<command-name>/review</command-name>\n<command-args>42</command-args>"
        ))),
        Said::User("/review 42".into())
    );
    for own in [
        json!("<local-command-stdout>done</local-command-stdout>"),
        json!("<system-reminder>remember</system-reminder>"),
        json!([{"type": "tool_result", "tool_use_id": "t", "content": "x"}]),
        json!(""),
        json!(null),
    ] {
        assert_eq!(said(&entry(own.clone())), Said::Nothing, "{own}");
    }
    let mut meta = entry(json!(
        "Caveat: the messages below were generated by the user"
    ));
    meta["isMeta"] = json!(true);
    assert_eq!(said(&meta), Said::Nothing);
    // Markup inside a message is the user's own.
    assert_eq!(
        said(&entry(json!("why does <T> not compile?"))),
        Said::User("why does <T> not compile?".into())
    );
}

#[test]
fn each_tool_is_drawn_as_the_row_of_its_kind() {
    let row = |name: &str, input: Value| {
        let item = tool_item("t1", name, &input);
        let tool = item.tool.unwrap();
        let kind = v1::ChatToolKind::try_from(tool.kind).unwrap();
        (
            kind,
            tool.title,
            tool.detail,
            tool.path,
            tool.added,
            tool.removed,
        )
    };
    let plain = |kind, title: &str, detail: &str| {
        (
            kind,
            title.to_owned(),
            detail.to_owned(),
            String::new(),
            0,
            0,
        )
    };
    assert_eq!(
        row(
            "Bash",
            json!({"command": "cargo test", "description": "Run the tests"})
        ),
        plain(ToolKind::Command, "run", "cargo test")
    );
    assert_eq!(
        row("Read", json!({"file_path": "/work/a.rs"})),
        (
            ToolKind::Read,
            "read".into(),
            "/work/a.rs".into(),
            "/work/a.rs".into(),
            0,
            0
        )
    );
    assert_eq!(
        row(
            "Edit",
            json!({"file_path": "/work/a.rs", "old_string": "a\nb", "new_string": "a\nb\nc"})
        ),
        (
            ToolKind::Edit,
            "edit".into(),
            "/work/a.rs".into(),
            "/work/a.rs".into(),
            3,
            2
        )
    );
    assert_eq!(
        row(
            "Write",
            json!({"file_path": "/work/b.rs", "content": "one\ntwo\n"})
        ),
        (
            ToolKind::Edit,
            "write".into(),
            "/work/b.rs".into(),
            "/work/b.rs".into(),
            2,
            0
        )
    );
    assert_eq!(
        row("Grep", json!({"pattern": "hard_wrap", "path": "src"})),
        plain(ToolKind::Search, "search", "hard_wrap in src")
    );
    assert_eq!(
        row("WebFetch", json!({"url": "https://docs.rs"})),
        plain(ToolKind::Web, "fetch", "https://docs.rs")
    );
    assert_eq!(
        row("mcp__docs__resolve-library-id", json!({"name": "x"})),
        plain(ToolKind::Mcp, "docs", "resolve-library-id")
    );
    assert_eq!(
        row(
            "Task",
            json!({"description": "Find the callers", "prompt": "long"})
        ),
        plain(ToolKind::Subagent, "agent", "Find the callers")
    );
    assert_eq!(
        row("Skill", json!({"skill": "review"})),
        plain(ToolKind::Other, "skill", "{\"skill\":\"review\"}")
    );
}

#[test]
fn a_message_id_that_is_no_uuid_becomes_one_that_is_always_the_same() {
    assert_eq!(message_uuid(M1), M1);
    assert_eq!(message_uuid(&M1.to_uppercase()), M1);
    let derived = message_uuid("user-1");
    assert!(is_session_id(&derived));
    assert_eq!(derived, message_uuid("user-1"));
    assert_ne!(derived, message_uuid("user-2"));
}

// --- the recorded session ---------------------------------------------------

/// The whole recorded conversation, with what a client sees at each step.
#[tokio::test]
async fn the_recorded_session_reads_as_the_conversation_it_was() {
    let home = home();
    let empty = tempfile::tempdir().unwrap();
    let mut lines = STREAM.lines().peekable();
    let mut replay = Replay::new(Claude::default(), ClaudeProvider.capabilities());

    // The agent is reached. Its session has said nothing yet.
    replay.connect(&tag(SESSION, ""));
    assert!(matches!(replay.jobs.as_slice(), [Job::History { session, .. }] if session == SESSION));
    replay.done(Done::History(history::load(empty.path(), SESSION, "")));
    assert_eq!(
        replay.client.state.connection,
        i32::from(Connection::Connecting)
    );
    replay.play(&mut lines, "@get_settings");
    replay.client.assert_settled();
    assert!(replay.client.state.history_complete);

    // A plain answer.
    replay.send(M1, "Reply with exactly: ok");
    assert_eq!(replay.client.state.turn, i32::from(Turn::Working));
    assert_eq!(
        replay.written.last().unwrap(),
        &json!({
            "type": "user",
            "message": {"role": "user", "content": "Reply with exactly: ok"},
            "uuid": M1,
            "parent_tool_use_id": null,
        })
    );
    replay.play(&mut lines, "@get_context_usage");
    replay
        .client
        .assert_answered(M1, "Reply with exactly: ok", "ok");
    replay.client.assert_describes_itself();
    // The pane goes by the conversation's first message from then on.
    assert_eq!(replay.titles, ["Reply with exactly: ok"]);
    let options = replay.client.state.options.clone().unwrap();
    assert_eq!(
        (options.model.as_str(), options.effort.as_str()),
        ("haiku", "low")
    );
    assert_eq!(options.permission, i32::from(Permission::Auto));
    assert_eq!(options.speed, i32::from(Speed::Standard));

    // A question, answered with one of its choices.
    let ask = "Use the AskUserQuestion tool to ask me whether I prefer red or blue, \
               then reply with just the color I chose.";
    replay.send(M2, ask);
    replay.play(&mut lines, "can_use_tool");
    let answer = replay.client.assert_asks("Blue");
    replay.command(answer).unwrap();
    let decision = &replay.written.last().unwrap()["response"]["response"];
    assert_eq!(decision["behavior"], "allow");
    assert_eq!(
        decision["updatedInput"]["answers"],
        json!({"Do you prefer red or blue?": "Blue"})
    );
    assert_eq!(replay.client.state.turn, i32::from(Turn::Working));
    replay.play(&mut lines, "\"completed\"");
    replay.client.assert_answered(M2, ask, "blue");

    // A command the agent needs leave for.
    let run = "Run this exact shell command and nothing else: echo hi > probe.txt";
    replay.send(M3, run);
    replay.play(&mut lines, "can_use_tool");
    let answer = replay.client.assert_asks_leave("echo hi > probe.txt");
    replay.command(answer).unwrap();
    let decision = &replay.written.last().unwrap()["response"]["response"];
    assert_eq!(decision["behavior"], "allow");
    assert_eq!(decision["updatedInput"]["command"], "echo hi > probe.txt");
    replay.play(&mut lines, "\"completed\"");
    replay.client.assert_answered(M3, run, "");
    replay.client.assert_ran(M3, "echo hi > probe.txt");
    // The agent runtime heard of both waits, and of their ends.
    let reported: Vec<&str> = replay
        .reports
        .iter()
        .map(|report| report["hook_event_name"].as_str().unwrap())
        .collect();
    assert_eq!(
        reported,
        [
            "PermissionRequest",
            "PostToolUse",
            "PermissionRequest",
            "PostToolUse"
        ]
    );
    assert!(
        replay
            .reports
            .iter()
            .all(|report| report["session_id"] == SESSION)
    );

    // A long answer, stopped part of the way.
    replay.send(
        M4,
        "Count from 1 to 400, one number per line, no other text.",
    );
    replay.play_before(&mut lines, "@interrupt");
    assert!(replay.client.items.last().unwrap().streaming);
    replay.command(SessionCommand::Interrupt).unwrap();
    assert_eq!(
        replay.last_asked("interrupt")["request"],
        json!({"subtype": "interrupt"})
    );
    replay.play(&mut lines, "\"cancelled\"");
    replay.client.assert_stopped(M4);
    // No hook of the agent's says a turn was stopped: the chat does.
    assert_eq!(replay.reports.last().unwrap()["hook_event_name"], "Stop");

    // What the agent kept of all that is what the chat showed of it.
    let Done::History(kept) = read_history(home.path(), "") else {
        unreachable!()
    };
    assert!(kept.complete);
    assert_eq!(
        kept.items,
        replay.client.items,
        "{:#?}",
        replay.client.outline()
    );

    // Going back to the third message: the agent is started again on the
    // session, at the entry before that message.
    let before = replay.client.items.len();
    replay
        .command(SessionCommand::Rewind {
            item_id: M3.into(),
            after_item_id: String::new(),
        })
        .unwrap();
    let Some(Job::Rewind { session, uuid, .. }) = replay.jobs.last() else {
        panic!("a rewind starts the agent again");
    };
    assert_eq!((session.as_str(), uuid.as_str()), (SESSION, M3));
    assert!(matches!(
        history::parent_of(home.path(), SESSION, M3),
        Some(Some(_))
    ));
    // Nothing changes until the agent has been started again, and a message
    // sent meanwhile waits for the one that is starting.
    assert_eq!(replay.client.items.len(), before);
    let list = "In one short line, list every request I have made in this conversation \
                before this one.";
    replay.send(M5, list);
    assert!(replay.written.last().unwrap()["type"] != "user");
    replay.done(Done::Rewound(Ok(())));
    assert_eq!(replay.reconnects, 1);
    assert!(replay.client.item(M3).is_none() && replay.client.item(M4).is_none());
    let ids: Vec<&str> = replay
        .client
        .items
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(ids, [M1, "msg_01:0", M2, "toolu_01", "msg_03:0", M5]);

    replay.disconnect();
    replay.connect(&tag(SESSION, M3));
    replay.done(read_history(home.path(), M3));
    // The conversation as the agent has it now is the one already shown.
    assert_eq!(replay.client.snapshots, 0);
    let mut lines = RESUMED.lines().peekable();
    assert!(replay.written.last().unwrap()["type"] != "user");
    replay.play(&mut lines, "@initialize");
    assert_eq!(
        replay
            .written
            .iter()
            .rev()
            .find(|written| written["type"] == "user")
            .unwrap()["uuid"],
        M5
    );
    replay.play(&mut lines, "\"completed\"");
    replay.client.assert_answered(M5, list, "red or blue");
    assert_eq!(replay.client.snapshots, 0);
}

/// A session started under a name (`--name`, as a fork is) goes by it, not by
/// what is said in it.
#[tokio::test]
async fn a_named_session_gives_its_pane_its_name() {
    let mut replay = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    let launch = launch(&["--name", "Fix tests (fork)"], None).unwrap();
    replay.connect(&Tag {
        launch,
        ..tag(SESSION, "")
    });
    replay.send(M1, "What is 2+2?");
    assert_eq!(replay.titles, ["Fix tests (fork)"]);
}

/// A daemon that starts while the agent waits on its user finds the
/// conversation, the turn and the request as they are.
#[tokio::test]
async fn a_chat_reached_mid_turn_picks_up_where_the_agent_is() {
    let home = home();
    let mut replay = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    replay.connect(&tag(SESSION, ""));
    replay.done(read_history(home.path(), ""));
    assert_eq!(replay.client.snapshots, 1);
    assert_eq!(replay.client.items.len(), 11);
    assert_eq!(
        replay.client.state.connection,
        i32::from(Connection::Connecting)
    );
    // Not ready yet: a command that needs the agent is refused, a message
    // waits.
    let refused = replay.command(SessionCommand::Interrupt).unwrap_err();
    assert_eq!(refused.code, "chat_not_ready");

    let request = json!({
        "type": "control_request",
        "request_id": "asked-before",
        "request": {
            "subtype": "can_use_tool", "tool_name": "Bash", "display_name": "Bash",
            "input": {"command": "rm -rf target"}, "tool_use_id": "toolu_90",
        },
    });
    let initialized = json!({"type": "control_response", "response": {
    "subtype": "success", "request_id": "@initialize", "response": {
        "session_state": "requires_action",
        "current_permission_mode": "bypassPermissions",
        "fast_mode_state": "on",
        "pending_permission_requests": [request],
    }}});
    replay.agent(&initialized.to_string());
    let state = &replay.client.state;
    assert_eq!(state.connection, i32::from(Connection::Ready));
    assert_eq!(state.turn, i32::from(Turn::Waiting));
    let options = state.options.clone().unwrap();
    assert_eq!(options.permission, i32::from(Permission::Full));
    assert_eq!(options.speed, i32::from(Speed::Fast));
    let pending = state.pending.clone().unwrap();
    assert_eq!(
        (
            pending.request_id.as_str(),
            pending.title.as_str(),
            pending.detail.as_str()
        ),
        ("asked-before", "Bash", "rm -rf target")
    );
    // The same request again, as the pipe replays what it kept, is one.
    replay.agent(&request.to_string());
    replay
        .command(SessionCommand::Answer {
            request_id: "asked-before".into(),
            answer: v1::ChatAnswer::default(),
        })
        .unwrap();
    assert_eq!(
        replay.written.last().unwrap()["response"],
        json!({"subtype": "success", "request_id": "asked-before", "response": {
            "behavior": "deny", "message": "The user denied this action."}})
    );
    // Denied, the turn goes on: the agent said it was in one.
    assert_eq!(replay.client.state.turn, i32::from(Turn::Working));

    // Text whose start was before this daemon is shown from where it is.
    let event = |event: Value| {
        json!({"type": "stream_event", "event": event, "parent_tool_use_id": null,
               "api_message_id": "msg_90", "session_id": SESSION})
        .to_string()
    };
    let delta = json!({"type": "content_block_delta", "index": 0,
                       "delta": {"type": "text_delta", "text": "ond half"}});
    replay.agent(&event(delta.clone()));
    replay.agent(&event(delta));
    let streamed = replay.client.item("msg_90:0").unwrap();
    assert!(streamed.streaming);
    assert_eq!(streamed.text, "ond halfond half");
    let whole = json!({"type": "assistant", "parent_tool_use_id": null, "session_id": SESSION,
        "message": {"id": "msg_90", "role": "assistant",
                    "content": [{"type": "text", "text": "first half, second half"}]}});
    replay.agent(&whole.to_string());
    let whole = replay.client.item("msg_90:0").unwrap();
    assert_eq!(
        (whole.text.as_str(), whole.streaming),
        ("first half, second half", false)
    );
    // What a subagent says is not this conversation.
    let nested = json!({"type": "assistant", "parent_tool_use_id": "toolu_91", "session_id": SESSION,
        "message": {"id": "msg_91", "role": "assistant", "content": [{"type": "text", "text": "aside"}]}});
    replay.agent(&nested.to_string());
    assert!(replay.client.item("msg_91:0").is_none());

    replay.agent(&json!({"type": "result", "subtype": "success", "is_error": false}).to_string());
    replay.client.assert_settled();

    // The agent goes: the chat ends, and takes nothing more.
    replay.end("");
    assert_eq!(replay.client.state.connection, i32::from(Connection::Ended));
    let refused = replay
        .command(SessionCommand::Send {
            text: "anyone?".into(),
            item_id: "late".into(),
        })
        .unwrap_err();
    assert_eq!(refused.code, "chat_unavailable");
}

#[tokio::test]
async fn options_are_asked_of_the_agent_and_shown_as_it_answers() {
    let mut lines = STREAM.lines().peekable();
    let mut replay = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    replay.connect(&tag(SESSION, ""));
    replay.done(Done::History(history::load(
        Path::new("/nonexistent"),
        SESSION,
        "",
    )));
    replay.play(&mut lines, "@get_settings");
    let before = replay.written.len();

    replay
        .command(SessionCommand::SetOptions(v1::ChatOptions {
            model: "default".into(),
            effort: "high".into(),
            permission: Permission::Full.into(),
            speed: Speed::Fast.into(),
        }))
        .unwrap();
    let asked: Vec<&Value> = replay.written[before..]
        .iter()
        .map(|written| &written["request"])
        .collect();
    assert_eq!(
        asked,
        [
            &json!({"subtype": "set_model", "model": "default"}),
            &json!({"subtype": "apply_flag_settings", "settings": {"effortLevel": "high"}}),
            &json!({"subtype": "apply_flag_settings", "settings": {"fastMode": true}}),
            &json!({"subtype": "set_permission_mode", "mode": "bypassPermissions"}),
            &json!({"subtype": "initialize"}),
            &json!({"subtype": "get_settings"}),
        ]
    );
    // Nothing is shown as changed until the agent says it is.
    assert_eq!(replay.client.state.options.clone().unwrap().model, "haiku");

    let answer = |what: &str, response: Value| {
        json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": format!("@{what}"), "response": response}})
        .to_string()
    };
    // The account has no fast mode: the chat says so and stays standard.
    replay.agent(&answer(
        "initialize",
        json!({"current_permission_mode": "bypassPermissions", "fast_mode_state": "off",
               "fast_mode_disabled_reason": "extra_usage_disabled"}),
    ));
    replay.agent(&answer(
        "get_settings",
        json!({"applied": {"model": "claude-opus-5-5", "effort": "high"}}),
    ));
    let options = replay.client.state.options.clone().unwrap();
    assert_eq!(
        options,
        v1::ChatOptions {
            model: "default".into(),
            effort: "high".into(),
            permission: Permission::Full.into(),
            speed: Speed::Standard.into(),
        }
    );
    let notice = replay.client.items.last().unwrap();
    assert_eq!(notice.kind, i32::from(Kind::Notice));
    assert_eq!(
        notice.text,
        "Fast mode is not available (extra_usage_disabled)."
    );
    // The models and commands were not listed again, and stay.
    assert!(replay.client.catalog.models.len() >= 2);
    assert!(!replay.client.catalog.commands.is_empty());
}

// --- the session's file -----------------------------------------------------

#[tokio::test]
async fn a_skill_named_with_a_dollar_at_the_start_is_sent_with_a_slash() {
    let mut lines = STREAM.lines().peekable();
    let mut replay = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    replay.connect(&tag(SESSION, ""));
    replay.done(Done::History(history::load(
        Path::new("/nonexistent"),
        SESSION,
        "",
    )));
    replay.play(&mut lines, "@get_settings");
    let mut sent = |text: &str| {
        replay
            .command(SessionCommand::Send {
                text: text.into(),
                item_id: text.into(),
            })
            .unwrap();
        replay.written.last().unwrap()["message"]["content"].clone()
    };
    assert_eq!(sent("$release-notes since v1.2"), "/release-notes since v1.2");
    assert_eq!(sent("$release-notes"), "/release-notes");
    // Not one of the agent's, or not at the start: as typed.
    assert_eq!(sent("$HOME is where"), "$HOME is where");
    assert_eq!(sent("use $release-notes"), "use $release-notes");
}

#[test]
fn a_session_that_cannot_be_read_says_so_and_one_with_no_file_is_empty() {
    let home = home();
    let none = history::load(home.path(), "a0000000-0000-4000-8000-000000000009", "");
    assert!(none.items.is_empty() && none.complete);
    // Not an id: it names no file, whatever it says.
    let unsafe_name = history::load(home.path(), "../../etc/passwd", "");
    assert!(unsafe_name.items.is_empty() && !unsafe_name.complete);
    assert!(!history::load(home.path(), "", "").complete);
    assert_eq!(history::parent_of(home.path(), SESSION, M5), None);
    assert_eq!(history::parent_of(home.path(), SESSION, M1), Some(None));
}

#[test]
fn a_rewound_session_ends_where_it_was_rewound_to_until_a_message_follows() {
    let home = home();
    let kept = history::parent_of(home.path(), SESSION, M3)
        .unwrap()
        .unwrap();
    let rewound = history::load(home.path(), SESSION, M3);
    let ids: Vec<&str> = rewound.items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, [M1, "msg_01:0", M2, "toolu_01", "msg_03:0"]);
    assert!(rewound.complete);

    // A message after the rewind grows the branch that is the conversation.
    let file = home.path().join(format!("projects/-work/{SESSION}.jsonl"));
    let mut text = std::fs::read_to_string(&file).unwrap();
    let next = json!({"parentUuid": kept, "isSidechain": false, "type": "user", "uuid": M5,
        "message": {"role": "user", "content": "and now?"}, "timestamp": "2026-01-01T11:00:00.000Z"});
    text.push_str(&format!("{next}\n"));
    std::fs::write(&file, text).unwrap();
    let grown = history::load(home.path(), SESSION, M3);
    let ids: Vec<&str> = grown.items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, [M1, "msg_01:0", M2, "toolu_01", "msg_03:0", M5]);
}

#[test]
fn a_long_session_opens_on_its_newest_part_and_says_there_is_more() {
    let home = tempfile::tempdir().unwrap();
    let project = home.path().join("projects/-work");
    std::fs::create_dir_all(&project).unwrap();
    let mut text = String::new();
    let mut parent = Value::Null;
    let padding = "x".repeat(64 * 1024);
    let count = 200;
    for index in 0..count {
        let uuid = format!("b0000000-0000-4000-8000-{index:012}");
        let entry = json!({"parentUuid": parent, "type": "user", "uuid": uuid,
            "message": {"role": "user", "content": format!("message {index} {padding}")}});
        text.push_str(&format!("{entry}\n"));
        parent = Value::String(uuid);
    }
    std::fs::write(project.join(format!("{SESSION}.jsonl")), text).unwrap();
    let history = history::load(home.path(), SESSION, "");
    assert!(!history.complete);
    assert!(history.items.len() < count && history.items.len() > 100);
    assert!(
        history
            .items
            .last()
            .unwrap()
            .text
            .starts_with("message 199 ")
    );
}

#[test]
fn a_tool_that_ran_beside_another_has_its_result_off_the_chain() {
    let home = tempfile::tempdir().unwrap();
    let project = home.path().join("projects/-work");
    std::fs::create_dir_all(&project).unwrap();
    let id = |index: u32| format!("c0000000-0000-4000-8000-{index:012}");
    let tool = |index: u32, parent: u32, tool: &str| {
        json!({"parentUuid": id(parent), "type": "assistant", "uuid": id(index),
            "message": {"id": "msg_01", "role": "assistant", "content": [
                {"type": "tool_use", "id": tool, "name": "Read", "input": {"file_path": "/work/a"}}]}})
    };
    let result = |index: u32, parent: u32, tool: &str| {
        json!({"parentUuid": id(parent), "type": "user", "uuid": id(index),
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": tool, "content": "read"}]}})
    };
    let entries = [
        json!({"parentUuid": null, "type": "user", "uuid": id(1),
            "message": {"role": "user", "content": "read both"}}),
        tool(2, 1, "toolu_a"),
        tool(3, 2, "toolu_b"),
        // The first tool's result hangs off the first tool, and nothing
        // follows it.
        result(4, 2, "toolu_a"),
        result(5, 3, "toolu_b"),
        json!({"parentUuid": id(5), "type": "assistant", "uuid": id(6),
            "message": {"id": "msg_02", "role": "assistant", "content": [
                {"type": "text", "text": "Both read."}]}}),
    ];
    let text: String = entries.iter().map(|entry| format!("{entry}\n")).collect();
    std::fs::write(project.join(format!("{SESSION}.jsonl")), text).unwrap();

    let history = history::load(home.path(), SESSION, "");
    let status = |tool: &str| {
        let item = history.items.iter().find(|item| item.id == tool).unwrap();
        item.tool.as_ref().unwrap().status
    };
    assert_eq!(status("toolu_a"), i32::from(Status::Ok));
    assert_eq!(status("toolu_b"), i32::from(Status::Ok));
    assert!(!history.reader.has_open());
    assert_eq!(history.items.len(), 4);
}

#[test]
fn the_sessions_are_where_the_agents_environment_says() {
    let said = |text: &str| Some(OsString::from(text));
    let account = || Some(PathBuf::from("/account"));
    assert_eq!(
        configuration_directory(said("/configured"), said("/elsewhere"), account()),
        PathBuf::from("/configured")
    );
    assert_eq!(
        configuration_directory(None, said("/elsewhere"), account()),
        PathBuf::from("/elsewhere/.claude")
    );
    assert_eq!(
        configuration_directory(said(""), said(""), account()),
        PathBuf::from("/account/.claude")
    );
}
