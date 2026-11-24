use std::path::Path;

use serde_json::{Value, json};

use super::super::tests::replay::{Replay, Wire};
use super::*;

const STREAM: &str = include_str!("../fixtures/claude-session.stream.jsonl");
const RESUMED: &str = include_str!("../fixtures/claude-resumed.stream.jsonl");
const SESSION_FILE: &str = include_str!("../fixtures/claude-session.jsonl");
const THINKING: &str = include_str!("../fixtures/claude-thinking.stream.jsonl");
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
        Said::User("fix it".into(), Vec::new())
    );
    assert_eq!(
        said(&entry(
            json!([{"type": "text", "text": "one"}, {"type": "text", "text": "two"}])
        )),
        Said::User("one\ntwo".into(), Vec::new())
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
        Said::User("/review 42".into(), Vec::new())
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
        Said::User("why does <T> not compile?".into(), Vec::new())
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

#[tokio::test]
async fn a_failed_result_only_adds_information_not_already_displayed_in_that_turn() {
    let mut replay = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    replay.connect(&tag(SESSION, ""));
    let assistant = |id, text| {
        json!({"type": "assistant", "message": {
            "id": id, "content": [{"type": "text", "text": text}]
        }})
        .to_string()
    };
    let result = |text| json!({"type": "result", "is_error": true, "result": text}).to_string();
    let notices = |replay: &Replay<Claude>| {
        replay
            .client
            .items
            .iter()
            .filter(|item| item.kind == i32::from(Kind::Notice))
            .count()
    };

    replay.agent(&assistant("billing-1", "Subscription unavailable"));
    replay.agent(&result("Subscription unavailable"));
    assert_eq!(notices(&replay), 0);
    assert_eq!(replay.client.items.len(), 1);
    assert_eq!(replay.client.state.turn, i32::from(Turn::Idle));

    // The same error on another turn must still be visible.
    replay.agent(&result("Subscription unavailable"));
    assert_eq!(notices(&replay), 1);
    replay.agent(&assistant("billing-2", "Subscription unavailable"));
    replay.agent(&result("Subscription unavailable"));
    assert_eq!(notices(&replay), 1);

    // Different details and failures without text are not suppressed.
    replay.agent(&assistant("partial", "Partial answer"));
    replay.agent(&result("Network disconnected"));
    replay.agent(&result(""));
    assert_eq!(notices(&replay), 3);
    assert_eq!(
        replay.client.items.last().unwrap().text,
        "The turn ended with an error."
    );

    replay.agent(&assistant("before-reconnect", "Subscription unavailable"));
    replay.disconnect();
    replay.connect(&tag(SESSION, ""));
    replay.agent(&result("Subscription unavailable"));
    assert_eq!(notices(&replay), 4);
}

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
    // session, at the message before that one.
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
        history::message_before(home.path(), SESSION, M3),
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

/// Thinking the model keeps to itself is an empty item from its first
/// `thinking_tokens`, so the chat can say that the agent thinks, and is gone
/// when its block ends. Thinking that streams text is one item, the same one,
/// from its first `thinking_tokens` to its end. The fixture is cut from two
/// recorded turns (2.1.326): `thinking_tokens` as a headless turn sent them,
/// around the stream events of the recorded session.
#[tokio::test]
async fn thinking_is_one_item_and_one_with_no_text_goes_when_it_ends() {
    let mut lines = THINKING.lines().peekable();
    let mut replay = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    replay.connect(&tag(SESSION, ""));
    let reasoning = |replay: &Replay<Claude>| {
        replay
            .client
            .items
            .iter()
            .filter(|item| item.kind == i32::from(Kind::Reasoning))
            .map(|item| (item.id.clone(), item.text.clone(), item.streaming))
            .collect::<Vec<_>>()
    };

    // A block that streams no text.
    replay.play(&mut lines, "\"content_block_start\"");
    assert!(reasoning(&replay).is_empty(), "nothing to say yet");
    replay.play(&mut lines, "thinking_tokens");
    assert_eq!(
        reasoning(&replay),
        [("msg_t1:0".into(), String::new(), true)]
    );
    replay.play(&mut lines, "thinking_tokens");
    assert_eq!(reasoning(&replay).len(), 1, "one item per block");
    replay.play(&mut lines, "\"type\":\"assistant\"");
    assert!(reasoning(&replay).is_empty(), "gone with its block");
    replay.play(&mut lines, "tool_result");
    let ids: Vec<&str> = replay
        .client
        .items
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(ids, [M1, "toolu_t1"]);

    // A block that streams text.
    replay.play(&mut lines, "thinking_tokens");
    assert_eq!(
        reasoning(&replay),
        [("msg_t2:0".into(), String::new(), true)]
    );
    replay.play(&mut lines, "\" Say so.\"");
    assert_eq!(
        reasoning(&replay),
        [(
            "msg_t2:0".into(),
            "The command printed done. Say so.".into(),
            true
        )]
    );
    replay.play(&mut lines, "\"type\":\"result\"");
    assert_eq!(
        reasoning(&replay),
        [(
            "msg_t2:0".into(),
            "The command printed done. Say so.".into(),
            false
        )]
    );
    let ids: Vec<&str> = replay
        .client
        .items
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(ids, [M1, "toolu_t1", "msg_t2:0", "msg_t2:1"]);
}

/// Thinking with no text that is not the newest item when it ends stays,
/// ended: only the end of the timeline can be cut.
#[tokio::test]
async fn thinking_with_no_text_behind_a_newer_item_is_ended_in_place() {
    let mut replay = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    replay.connect(&tag(SESSION, ""));
    let mut lines = THINKING.lines();
    // The user's message, the block's start and its first `thinking_tokens`.
    for _ in 0..4 {
        replay.agent(lines.next().unwrap());
    }
    assert!(replay.client.item("msg_t1:0").unwrap().streaming);
    // A message the user sent meanwhile lands after it.
    let user = json!({"type": "user", "isReplay": true, "session_id": SESSION,
        "uuid": M2, "message": {"role": "user", "content": "And then?"}});
    replay.agent(&user.to_string());
    replay.agent(&json!({"type": "result", "subtype": "success", "is_error": false}).to_string());
    let thinking = replay.client.item("msg_t1:0").unwrap();
    assert_eq!((thinking.text.as_str(), thinking.streaming), ("", false));
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
            attachments: Vec::new(),
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

/// Claude Code 2.1.326's `initialize` answer, as recorded: its fourteen
/// models, the default first.
const INITIALIZE: &str = include_str!("../fixtures/claude-initialize.json");

#[tokio::test]
async fn the_default_model_is_named_after_the_model_it_runs() {
    let mut replay = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    replay.connect(&tag(SESSION, ""));
    replay.done(Done::History(history::load(
        Path::new("/nonexistent"),
        SESSION,
        "",
    )));
    replay.agent(INITIALIZE.trim());
    let models = &replay.client.catalog.models;
    assert_eq!(models.len(), 14);
    let labels: Vec<(&str, &str)> = models
        .iter()
        .take(3)
        .map(|model| (model.id.as_str(), model.label.as_str()))
        .collect();
    // Only the default's label changes; the entry it duplicates stays.
    assert_eq!(
        labels,
        [
            ("default", "Opus 5.5 (default)"),
            ("opus", "Opus 5.5"),
            ("fable", "Fable 5.2"),
        ]
    );
}

#[test]
fn a_default_no_other_entry_names_is_named_from_its_description_or_its_model() {
    let entry = |fields: Value| {
        let mut entry = json!({"value": "default", "displayName": "Default (recommended)"});
        entry
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        entry
    };
    let haiku = json!({"value": "haiku", "resolvedModel": "claude-haiku-5-5",
                       "displayName": "Haiku 5.5"});
    let described = entry(json!({"resolvedModel": "claude-opus-5-5",
                                 "description": "Opus 5.5 · Best for everyday, complex tasks"}));
    assert_eq!(
        default_label(&described, &[described.clone(), haiku.clone()]),
        "Opus 5.5 (default)"
    );
    let bare = entry(json!({"resolvedModel": "claude-opus-5-5",
                            "description": "Best for everyday, complex tasks"}));
    assert_eq!(default_label(&bare, &[haiku]), "claude-opus-5-5 (default)");
    let unknown = entry(json!({}));
    assert_eq!(default_label(&unknown, &[]), "Default (recommended)");
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
                attachments: Vec::new(),
            })
            .unwrap();
        replay.written.last().unwrap()["message"]["content"].clone()
    };
    assert_eq!(
        sent("$release-notes since v1.2"),
        "/release-notes since v1.2"
    );
    assert_eq!(sent("$release-notes"), "/release-notes");
    // Not one of the agent's, or not at the start: as typed.
    assert_eq!(sent("$HOME is where"), "$HOME is where");
    assert_eq!(sent("use $release-notes"), "use $release-notes");
}

fn attached(path: &str, media_type: &str, inline: Option<&str>) -> Attached {
    Attached {
        path: path.into(),
        name: path.rsplit('/').next().unwrap().into(),
        media_type: media_type.into(),
        size: 9,
        inline: inline.map(str::to_owned),
    }
}

#[tokio::test]
async fn a_small_image_goes_as_an_image_and_anything_else_as_its_path() {
    let mut lines = STREAM.lines().peekable();
    let mut replay = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    replay.connect(&tag(SESSION, ""));
    replay.done(Done::History(history::load(
        Path::new("/nonexistent"),
        SESSION,
        "",
    )));
    replay.play(&mut lines, "@get_settings");
    let staged = "/home/u/.cache/muxflow/uploads";
    let shot = attached(&format!("{staged}/shot.png"), "image/png", Some("iVBORw0K"));
    let large = attached(&format!("{staged}/big.jpg"), "image/jpeg", None);
    let log = attached(&format!("{staged}/app log.txt"), "", None);
    replay
        .command(SessionCommand::Send {
            text: "Why does it overlap?".into(),
            item_id: "m-files".into(),
            attachments: vec![shot.clone(), large.clone(), log.clone()],
        })
        .unwrap();
    let written = replay.written.last().unwrap();
    assert_eq!(
        written["message"]["content"],
        json!([
            {"type": "text", "text": format!(
                "Why does it overlap?\n\n{staged}/big.jpg\n{staged}/app log.txt")},
            {"type": "image", "source": {
                "type": "base64", "media_type": "image/png", "data": "iVBORw0K"}},
        ])
    );
    // The chat shows the message as it was typed, with its files.
    let item = replay.client.item("m-files").unwrap();
    assert_eq!(item.text, "Why does it overlap?");
    let names: Vec<&str> = item.attachments.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["shot.png", "big.jpg", "app log.txt"]);
    assert_eq!(item.attachments[0].path, shot.path);

    // An image alone is a message.
    replay
        .command(SessionCommand::Send {
            text: String::new(),
            item_id: "m-image".into(),
            attachments: vec![shot.clone()],
        })
        .unwrap();
    let content = &replay.written.last().unwrap()["message"]["content"];
    assert_eq!(content.as_array().unwrap().len(), 1);
    assert_eq!(content[0]["type"], "image");
    // So is one with blank space for its text: the agent takes no blank text.
    replay
        .command(SessionCommand::Send {
            text: " \n\t".into(),
            item_id: "m-image-blank".into(),
            attachments: vec![shot],
        })
        .unwrap();
    let content = &replay.written.last().unwrap()["message"]["content"];
    assert_eq!(content.as_array().unwrap().len(), 1);
    assert_eq!(content[0]["type"], "image");
    // No files: the text alone, as before.
    replay
        .command(SessionCommand::Send {
            text: "plain".into(),
            item_id: "m-plain".into(),
            attachments: Vec::new(),
        })
        .unwrap();
    assert_eq!(
        replay.written.last().unwrap()["message"]["content"],
        "plain"
    );
}

#[test]
fn a_recorded_message_gives_back_its_inline_images_and_named_files() {
    let staged = tempfile::tempdir().unwrap();
    let path = staged.path().join("notes.md");
    let entry = json!({"type": "user", "message": {"role": "user", "content": [
        {"type": "text", "text": format!("Read these.\n\n{}", path.display())},
        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}},
    ]}});
    let Said::User(text, attachments) = said_in(&entry, Some(staged.path())) else {
        panic!("a message of the user's");
    };
    assert_eq!(text, "Read these.");
    let read: Vec<(&str, &str, &str)> = attachments
        .iter()
        .map(|a| (a.name.as_str(), a.path.as_str(), a.media_type.as_str()))
        .collect();
    assert_eq!(
        read,
        [
            ("notes.md", path.to_str().unwrap(), ""),
            ("Image", "", "image/png")
        ]
    );
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
    assert_eq!(history::message_before(home.path(), SESSION, M5), None);
    assert_eq!(
        history::message_before(home.path(), SESSION, M1),
        Some(None)
    );
}

#[test]
fn a_rewound_session_ends_where_it_was_rewound_to_until_a_message_follows() {
    let home = home();
    let kept = history::message_before(home.path(), SESSION, M3)
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

/// Between two messages the agent records entries that are not one: a Stop
/// hook's summary, files it attached, a turn's duration. The agent cuts a
/// session only at a message, so a rewind, an edit and a fork cut at the
/// message before them (seen live on 2.1.326: a fork cut at a hook's summary
/// failed with "No message found with message.uuid").
#[test]
fn a_cut_falls_on_the_message_before_whatever_else_the_agent_recorded() {
    const OTHER: &str = "5e551011-0000-4000-8000-000000000002";
    const FIRST_REPLY: &str = "a0000000-0000-4000-8000-0000000000a1";
    const SECOND_REPLY: &str = "a0000000-0000-4000-8000-0000000000a2";
    let entry = |kind: &str, subtype: &str, uuid: &str, parent: Option<&str>| {
        let mut entry = json!({"type": kind, "uuid": uuid, "parentUuid": parent,
                               "isSidechain": false, "timestamp": "2026-01-01T10:00:00.000Z"});
        match kind {
            "system" => entry["subtype"] = subtype.into(),
            "user" => entry["message"] = json!({"role": "user", "content": "go on"}),
            "assistant" => {
                entry["message"] = json!({"id": format!("msg_{uuid}"), "role": "assistant",
                                          "content": [{"type": "text", "text": "Done."}]});
            }
            _ => entry["attachment"] = json!({"type": "hook_success"}),
        }
        entry.to_string()
    };
    let hook = "b0000000-0000-4000-8000-0000000000b1";
    let attached = "b0000000-0000-4000-8000-0000000000b2";
    let second_hook = "b0000000-0000-4000-8000-0000000000b3";
    let lines = [
        // Only entries that are not messages come before the first.
        entry(
            "attachment",
            "",
            "b0000000-0000-4000-8000-0000000000b0",
            None,
        ),
        entry(
            "system",
            "informational",
            "b0000000-0000-4000-8000-0000000000b9",
            Some("b0000000-0000-4000-8000-0000000000b0"),
        ),
        entry("user", "", M1, Some("b0000000-0000-4000-8000-0000000000b9")),
        entry("assistant", "", FIRST_REPLY, Some(M1)),
        entry("system", "stop_hook_summary", hook, Some(FIRST_REPLY)),
        entry("user", "", M2, Some(hook)),
        entry("assistant", "", SECOND_REPLY, Some(M2)),
        entry("attachment", "", attached, Some(SECOND_REPLY)),
        entry("system", "stop_hook_summary", second_hook, Some(attached)),
        entry("user", "", M3, Some(second_hook)),
        // Of what came before this one, the file holds nothing.
        entry("user", "", M4, Some("b0000000-0000-4000-8000-0000000000bf")),
    ];
    let home = home();
    std::fs::write(
        home.path().join(format!("projects/-work/{OTHER}.jsonl")),
        lines.join("\n") + "\n",
    )
    .unwrap();
    let before = |uuid: &str| history::message_before(home.path(), OTHER, uuid);
    assert_eq!(before(M2), Some(Some(FIRST_REPLY.to_owned())));
    assert_eq!(before(M3), Some(Some(SECOND_REPLY.to_owned())));
    assert_eq!(before(M1), Some(None));
    assert_eq!(before(M4), None);
    assert_eq!(before(M5), None);

    // A fork of the first reply, and of the second.
    let fork = |next: &str| fork_launch(home.path(), OTHER, None, Some(next), Launch::default());
    let copy = |resume_at: &str| Launch {
        resume: Resume::Session {
            id: OTHER.into(),
            fork: true,
        },
        resume_at: resume_at.into(),
        ..Launch::default()
    };
    assert_eq!(fork(M2).unwrap(), copy(FIRST_REPLY));
    assert_eq!(fork(M3).unwrap(), copy(SECOND_REPLY));
    assert_eq!(fork(M1).unwrap_err().code, "chat_fork_unavailable");

    // A rewind, and an edit, which is one.
    let rewind = |uuid: &str| {
        let mut launch = Launch::default();
        rewind_launch(home.path(), OTHER, uuid, &mut launch).map(|dropped| (launch, dropped))
    };
    let resumed = |resume_at: &str| Launch {
        resume: Resume::Session {
            id: OTHER.into(),
            fork: false,
        },
        resume_at: resume_at.into(),
        ..Launch::default()
    };
    assert_eq!(rewind(M2).unwrap(), (resumed(FIRST_REPLY), M2.to_owned()));
    assert_eq!(rewind(M3).unwrap(), (resumed(SECOND_REPLY), M3.to_owned()));
    // Nothing that is a message came before the first: it starts over.
    let (fresh, dropped) = rewind(M1).unwrap();
    assert_eq!(
        (fresh.resume, fresh.resume_at, dropped),
        (Resume::Fresh, String::new(), String::new())
    );
}

// --- a fork from a reply ----------------------------------------------------

/// The entry of the reply to the second message: the last before the third.
const SECOND_REPLY: &str = "00000000-0000-4000-8000-000000000033";
/// The newest message of the recording: the note that the fourth's reply
/// was stopped.
const NEWEST: &str = "00000000-0000-4000-8000-000000000162";

#[test]
fn a_replys_fork_starts_a_copy_cut_after_that_reply() {
    let command = |launch: Launch| {
        ClaudeProvider
            .pane_command(
                &Launch {
                    program: strings(&["claude"]),
                    ..launch
                },
                Path::new("/run/a.sock"),
            )
            .unwrap()
    };
    let copy = command(Launch {
        resume: Resume::Session {
            id: SESSION.into(),
            fork: true,
        },
        resume_at: SECOND_REPLY.into(),
        ..Default::default()
    });
    let from = copy.argv.iter().position(|word| word == "auto").unwrap();
    assert_eq!(
        copy.argv[from + 1..].to_vec(),
        [
            "--resume",
            SESSION,
            "--fork-session",
            "--session-id",
            copy.session.as_str(),
            "--resume-session-at",
            SECOND_REPLY
        ]
    );
    // A session of its own, known before the agent says it.
    assert!(is_session_id(&copy.session) && copy.session != SESSION);

    let home = home();
    let base = Launch {
        model: "opus".into(),
        name: "Fix tests".into(),
        ..Default::default()
    };
    let fork = |next: Option<&str>| fork_launch(home.path(), SESSION, None, next, base.clone());
    // Through the reply to the second message: where the third began.
    let through_second = fork(Some(M3)).unwrap();
    assert_eq!(
        through_second,
        Launch {
            resume: Resume::Session {
                id: SESSION.into(),
                fork: true,
            },
            resume_at: SECOND_REPLY.into(),
            ..base.clone()
        }
    );
    // The newest reply: through the newest message.
    assert_eq!(fork(None).unwrap().resume_at, NEWEST);
    // A next message the agent has not recorded: it may still be replying.
    assert_eq!(fork(Some(M5)).unwrap_err().code, "chat_turn_busy");
    // Nothing comes before the first message.
    assert_eq!(fork(Some(M1)).unwrap_err().code, "chat_fork_unavailable");
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(
        fork_launch(empty.path(), SESSION, None, None, base.clone())
            .unwrap_err()
            .message,
        "This chat has no conversation to fork yet."
    );
}

#[test]
fn a_fork_with_nothing_said_in_it_shows_its_source_up_to_the_cut() {
    let home = home();
    let through = history::load_through(home.path(), SESSION, SECOND_REPLY);
    let ids: Vec<&str> = through.items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, [M1, "msg_01:0", M2, "toolu_01", "msg_03:0"]);
    assert!(through.complete);
    assert!(!history::load_through(home.path(), SESSION, "missing").complete);

    // The copy's chat reads its own record, and its source's while it has
    // none.
    let copy = "c0b10000-0000-4000-8000-000000000001";
    let mut replay = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    replay.connect(&Tag {
        launch: Launch {
            resume: Resume::Session {
                id: SESSION.into(),
                fork: true,
            },
            resume_at: SECOND_REPLY.into(),
            ..Default::default()
        },
        ..tag(copy, "")
    });
    assert!(matches!(
        replay.jobs.as_slice(),
        [Job::History { session, copied: Some((source, cut)), .. }]
            if session == copy && source == SESSION && cut == SECOND_REPLY
    ));
    // A plain resume copies nothing.
    let mut replay = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    replay.connect(&tag(SESSION, ""));
    assert!(matches!(
        replay.jobs.as_slice(),
        [Job::History { copied: None, .. }]
    ));
}

/// Before its first turn a copy has no record: an edit, a rewind or a fork
/// of what it shows is cut from its source's record, as a new copy.
#[test]
fn a_fork_with_nothing_said_in_it_is_rewound_and_forked_from_its_source() {
    let home = home();
    let copy = "c0b10000-0000-4000-8000-000000000002";
    let copied = || Some((SESSION.to_owned(), SECOND_REPLY.to_owned()));
    let copy_launch = Launch {
        resume: Resume::Session {
            id: SESSION.into(),
            fork: true,
        },
        resume_at: SECOND_REPLY.into(),
        name: "Fix tests (fork)".into(),
        ..Default::default()
    };
    let before_m2 = history::message_before(home.path(), SESSION, M2)
        .unwrap()
        .unwrap();
    let from_source = |resume_at: &str| Launch {
        resume: Resume::Session {
            id: SESSION.into(),
            fork: true,
        },
        resume_at: resume_at.into(),
        ..copy_launch.clone()
    };

    // A fork of an earlier reply, and of the newest: the copy's cut.
    let fork =
        |next: Option<&str>| fork_launch(home.path(), copy, copied(), next, copy_launch.clone());
    assert_eq!(fork(Some(M2)).unwrap(), from_source(&before_m2));
    assert_eq!(fork(None).unwrap(), from_source(SECOND_REPLY));

    // A rewind (and an edit, which is one) of its second message.
    let rewind = |uuid: &str| {
        let mut launch = copy_launch.clone();
        rewind_launch(home.path(), copy, uuid, &mut launch).map(|dropped| (launch, dropped))
    };
    assert_eq!(
        rewind(M2).unwrap(),
        (from_source(&before_m2), String::new())
    );
    // Of its first: a new conversation.
    let (fresh, dropped) = rewind(M1).unwrap();
    assert_eq!(
        (fresh.resume, fresh.resume_at, dropped),
        (Resume::Fresh, String::new(), String::new())
    );
    // A message sent in it and not yet recorded is in no record.
    assert_eq!(
        rewind(M5).unwrap_err(),
        "That message is not in the session's record yet."
    );

    // Once something is said in it the copy has its record, and goes on in it.
    let project = home.path().join("projects/-work");
    std::fs::write(project.join(format!("{copy}.jsonl")), SESSION_FILE).unwrap();
    let (resumed, dropped) = rewind(M2).unwrap();
    assert_eq!(
        (resumed.resume, resumed.resume_at, dropped),
        (
            Resume::Session {
                id: copy.into(),
                fork: false
            },
            before_m2.clone(),
            M2.to_owned()
        )
    );
    assert_eq!(fork(None).unwrap().resume_at, NEWEST);
}

#[tokio::test]
async fn a_turn_stopped_with_a_message_behind_it_is_counted_as_ended() {
    let mut replay = ready();
    replay.send(M5, "count to 400");
    replay.send("m6", "one more thing");
    let queued = json!({"type": "command_lifecycle", "state": "queued",
                        "command_uuid": message_uuid("m6")});
    replay.agent(&queued.to_string());
    assert_eq!(replay.client.state.turns_ended, 0);
    let stopped = json!({"type": "result", "subtype": "error_during_execution",
                         "is_error": true, "terminal_reason": "aborted_streaming"});
    replay.agent(&stopped.to_string());
    // The message behind it keeps the agent working, on a turn of its own.
    assert_eq!(replay.client.state.turn, i32::from(Turn::Working));
    assert_eq!(replay.client.state.turns_ended, 1);
    let done = json!({"type": "result", "subtype": "success", "is_error": false});
    replay.agent(
        &json!({"type": "command_lifecycle", "state": "completed",
                         "command_uuid": message_uuid("m6")})
        .to_string(),
    );
    replay.agent(&done.to_string());
    assert_eq!(replay.client.state.turn, i32::from(Turn::Idle));
    assert_eq!(replay.client.state.turns_ended, 2);
}

#[tokio::test]
async fn a_reply_is_not_forked_while_its_turn_can_still_add_to_it() {
    let home = home();
    let mut replay = ready();
    replay.done(read_history(home.path(), ""));
    // A message the agent has put behind the turn that runs: the reply
    // before it is still that turn's.
    replay.send(M5, "and then?");
    replay.send("m6", "one more thing");
    let queued = json!({"type": "command_lifecycle", "state": "queued",
                        "command_uuid": message_uuid("m6")});
    replay.agent(&queued.to_string());
    let refused = replay.fork(M5).unwrap_err();
    assert_eq!(refused.code, "chat_turn_busy");
    assert_eq!(
        refused.message,
        "That reply is still being written. Fork it once it has finished."
    );
    // The reply a turn is adding to is refused by the runtime.
    assert_eq!(replay.fork("m6").unwrap_err().code, "chat_turn_busy");
    assert_eq!(replay.fork("gone").unwrap_err().code, "chat_item_not_found");

    // A chat that has not said which session it is has nothing to fork.
    let mut replay = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    replay.connect(&tag("", ""));
    replay.done(read_history(home.path(), ""));
    assert_eq!(
        replay.fork("msg_01:0").unwrap_err().message,
        "This chat has no conversation to fork yet."
    );
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
    // Longer than the first window, which holds plenty of it: its newest
    // part, and more said. It is not read again from further back.
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

/// Messages carrying as many inline images as one may: each fits on one of
/// the agent's lines and inside the first window of its record, and a
/// conversation of several, longer than that window, is still read whole.
#[test]
fn messages_with_the_most_inline_images_fit_a_line_and_are_read_back_whole() {
    let budget = attach::MAX_INLINE_TOTAL_BYTES;
    let images: Vec<Attached> = [5 * 1024 * 1024, budget - 5 * 1024 * 1024]
        .into_iter()
        .map(|len| Attached {
            inline: Some("A".repeat(len)),
            ..attached("/staged/shot.png", "image/png", None)
        })
        .collect();
    let text = "Why does it overlap? ".repeat(1000);
    let content = content(&text, &images);
    let line = json!({"type": "user", "message": {"role": "user", "content": content},
                      "uuid": M1, "parent_tool_use_id": null})
    .to_string();
    assert!(line.len() < super::super::session::MAX_MESSAGE_BYTES);
    assert!((line.len() as u64) < history::TAIL_BYTES);

    let home = tempfile::tempdir().unwrap();
    let project = home.path().join("projects/-work");
    std::fs::create_dir_all(&project).unwrap();
    let id = |index: u32| format!("d0000000-0000-4000-8000-{index:012}");
    let mut record = String::new();
    let mut parent = Value::Null;
    let turns = 3;
    for turn in 0..turns {
        let asked = json!({"parentUuid": parent, "type": "user", "uuid": id(2 * turn),
            "message": {"role": "user", "content": content}});
        let reply = json!({"parentUuid": id(2 * turn), "type": "assistant",
            "uuid": id(2 * turn + 1), "message": {"id": format!("msg_{turn}"),
            "role": "assistant", "content": [{"type": "text", "text": "Fixed."}]}});
        record.push_str(&format!("{asked}\n{reply}\n"));
        parent = Value::String(id(2 * turn + 1));
    }
    assert!(record.len() as u64 > history::TAIL_BYTES);
    std::fs::write(project.join(format!("{SESSION}.jsonl")), record).unwrap();

    let read = history::load(home.path(), SESSION, "");
    assert!(read.complete);
    assert_eq!(read.items.len(), 2 * turns as usize);
    let first = &read.items[0];
    assert_eq!(first.id, id(0));
    assert_eq!(first.attachments.len(), 2);
    // A copy cut at the first reply finds it as far back as it lies.
    let through = history::load_through(home.path(), SESSION, &id(1));
    assert!(through.complete);
    assert_eq!(through.items.len(), 2);
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

// --- background tasks, subagents, compacting ----------------------------------

/// One recorded headless turn (2.1.326): a command run in the background, an
/// `Explore` subagent in the foreground, and a command in the foreground that
/// waits for the first. Trimmed, with its paths and ids replaced.
const TASKS: &str = include_str!("../fixtures/claude-tasks.stream.jsonl");
/// Composed from the shapes the agent was seen sending live (2.1.326): a
/// command left running when the turn ends, stopped from the chat, the turn
/// the agent then starts by itself, and a manual `/compact`.
const BACKGROUND: &str = include_str!("../fixtures/claude-background.stream.jsonl");
/// One recorded headless chat (2.1.326): an `Explore` subagent run in the
/// background, which goes on after its turn ends, hands its report back as
/// a user message, and the turn the agent then starts by itself. Trimmed
/// (no stream deltas, shorter texts), with its paths and ids replaced.
const BACKGROUND_AGENT: &str = include_str!("../fixtures/claude-background-agent.stream.jsonl");

/// A chat whose agent has answered `initialize`, so it takes commands.
fn ready() -> Replay<Claude> {
    let mut replay = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    replay.connect(&tag(SESSION, ""));
    replay.agent(
        &json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": "@initialize", "response": {}}})
        .to_string(),
    );
    replay
}

fn listed(replay: &Replay<Claude>) -> Vec<(String, v1::ChatTaskKind, String, bool)> {
    let task = |task: &v1::ChatBackgroundTask| {
        (
            task.id.clone(),
            v1::ChatTaskKind::try_from(task.kind).unwrap(),
            task.description.clone(),
            task.stoppable,
        )
    };
    replay.client.state.background.iter().map(task).collect()
}

fn tool_of<'a>(replay: &'a Replay<Claude>, id: &str) -> &'a v1::ChatTool {
    let item = replay.client.item(id);
    let item = item.unwrap_or_else(|| panic!("{id}: {:#?}", replay.client.outline()));
    item.tool.as_ref().unwrap()
}

#[tokio::test]
async fn background_work_is_listed_and_a_subagents_row_has_its_figures() {
    let mut lines = TASKS.lines().peekable();
    let mut replay = ready();
    replay.send(M1, "Run a command in the background and count the files.");

    // The background command: listed as soon as the agent says, and its
    // row ends at once, as its tool says it has.
    replay.play(&mut lines, "background_tasks_changed");
    let command = (
        "bemhbkm4q".to_owned(),
        v1::ChatTaskKind::Command,
        "Run sleep 12 then echo done in background".to_owned(),
        true,
    );
    assert_eq!(listed(&replay), std::slice::from_ref(&command));
    let started = replay.client.state.background[0].started_at_unix_millis;
    assert!(started > 0);
    replay.play(&mut lines, "Command running in background");
    assert_eq!(
        tool_of(&replay, "toolu_011J1VNnwubb3AU3yZ3Vxr17").status,
        i32::from(Status::Ok)
    );

    // The subagent: its type, then what it does and its figures as it works.
    let agent = "toolu_01GGX8A9njwe71AdehLB2HCD";
    replay.play(&mut lines, "\"name\":\"Agent\"");
    assert_eq!(tool_of(&replay, agent).agent_type, "Explore");
    assert_eq!(tool_of(&replay, agent).status, i32::from(Status::Running));
    replay.play(&mut lines, "task_started");
    replay.play(&mut lines, "task_progress");
    let tool = tool_of(&replay, agent);
    assert_eq!(
        (tool.activity.as_str(), tool.tool_uses, tool.tokens),
        (
            "Running List all entries in the current directory",
            1,
            15_264
        )
    );
    assert!(!tool.background);
    // Its own conversation stays its own.
    replay.play(&mut lines, "img.out\"");
    assert!(
        replay
            .client
            .item("toolu_01XcxZShYVVe8Rt3ekCdgung")
            .is_none()
    );
    // It ends with its last figures, and its result ends the row.
    replay.play(&mut lines, "\"task_notification\"");
    assert_eq!(tool_of(&replay, agent).tokens, 19_714);
    replay.play(&mut lines, "The directory holds 3 files");
    let tool = tool_of(&replay, agent);
    assert_eq!(
        (tool.status, tool.result_summary.as_str(), tool.tool_uses),
        (i32::from(Status::Ok), "done", 1)
    );

    // A command in the foreground is a task too, and changes neither the
    // list nor its row.
    let waiting = "toolu_018G1A5HyUZN4Y8s5QnCji4N";
    replay.play(&mut lines, "\"task_started\"");
    assert_eq!(listed(&replay), [command]);
    assert_eq!(
        replay.client.state.background[0].started_at_unix_millis,
        started
    );
    let before = tool_of(&replay, waiting).clone();
    assert_eq!(before.kind, i32::from(ToolKind::Command));

    // The background command ends: the list empties.
    replay.play(&mut lines, "\"tasks\":[]");
    assert!(replay.client.state.background.is_empty());
    replay.play(&mut lines, "\"task_notification\"");
    assert_eq!(tool_of(&replay, waiting), &before);
    replay.play(&mut lines, "\"type\":\"result\"");
    replay.client.assert_settled();
}

#[tokio::test]
async fn a_background_subagent_runs_on_with_its_figures_and_its_report_is_no_users_message() {
    let mut lines = BACKGROUND_AGENT.lines().peekable();
    let mut replay = ready();
    replay.send(
        M1,
        "Use the Explore subagent, running in the background, to find which file contains the \
         word 'needle'. Then tell me the file name.",
    );
    let agent = "toolu_01QdB1q9UGBzrasx4dcWr64c";
    let running = i32::from(Status::Running);
    let task = (
        "a88635c133db0bb45".to_owned(),
        v1::ChatTaskKind::Subagent,
        "Find file containing needle".to_owned(),
        true,
    );

    // Its tool call returns at once, saying only that it started it: the
    // subagent runs on, and is listed.
    replay.play(&mut lines, "Async agent launched");
    let tool = tool_of(&replay, agent);
    assert_eq!(
        (
            tool.status,
            tool.agent_type.as_str(),
            tool.background,
            tool.output_preview.as_str()
        ),
        (running, "Explore", true, "")
    );
    assert_eq!(listed(&replay), std::slice::from_ref(&task));
    // What it does and its figures, as it works.
    replay.play(&mut lines, "\"task_progress\"");
    let tool = tool_of(&replay, agent);
    assert_eq!(
        (
            tool.status,
            tool.activity.as_str(),
            tool.tool_uses,
            tool.tokens
        ),
        (
            running,
            "Running Recursively list files containing the word needle",
            1,
            12_368
        )
    );
    // Its own conversation stays its own.
    assert!(
        replay
            .client
            .item("toolu_015gh4gV25mLwmaEP3Zv7Xbq")
            .is_none()
    );

    // The turn ends; the subagent goes on, and its figures with it.
    replay.play(&mut lines, "\"type\":\"result\"");
    assert_eq!(replay.client.state.turn, i32::from(Turn::Idle));
    assert_eq!(tool_of(&replay, agent).status, running);
    assert_eq!(listed(&replay), std::slice::from_ref(&task));
    replay.play(&mut lines, "\"last_tool_name\":\"SubagentHandback\"");
    assert_eq!(tool_of(&replay, agent).tokens, 16_428);
    assert_eq!(replay.client.state.turn, i32::from(Turn::Idle));

    // It ends: its outcome, then its last figures and its report.
    replay.play(&mut lines, "\"task_updated\"");
    let tool = tool_of(&replay, agent);
    assert_eq!(
        (tool.status, tool.result_summary.as_str()),
        (i32::from(Status::Ok), "done")
    );
    replay.play(&mut lines, "\"task_notification\"");
    let finished = tool_of(&replay, agent).clone();
    assert_eq!(
        (
            finished.status,
            finished.result_summary.as_str(),
            finished.tool_uses,
            finished.tokens,
            finished.output_preview.as_str()
        ),
        (i32::from(Status::Ok), "done", 1, 16_600, "haystack.txt")
    );
    replay.play(&mut lines, "\"tasks\":[]");
    assert!(replay.client.state.background.is_empty());

    // Its report comes as a message in the user's name, which the user did
    // not write.
    let report = "9976c633-9436-4771-8343-fa98ac72dc9b";
    replay.play(&mut lines, report);
    assert!(replay.client.item(report).is_none());

    // The agent starts a turn of its own on it.
    replay.play(&mut lines, "message_start");
    assert_eq!(replay.client.state.turn, i32::from(Turn::Working));
    replay.play(&mut lines, "\"type\":\"result\"");
    assert!(lines.next().is_none());
    replay.client.assert_settled();
    assert!(replay.client.state.background.is_empty());
    assert_eq!(tool_of(&replay, agent), &finished);
    let users: Vec<&str> = replay
        .client
        .items
        .iter()
        .filter(|item| item.kind == i32::from(Kind::User))
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(users, [M1]);
    assert!(
        replay
            .client
            .items
            .last()
            .unwrap()
            .text
            .starts_with("The file that contains \"needle\" is **`haystack.txt`**")
    );
}

/// A background subagent whose end goes unsaid does not run for ever.
#[tokio::test]
async fn a_background_subagents_row_ends_when_its_task_or_its_agent_is_gone() {
    let agent = "toolu_bg1";
    let started = |replay: &mut Replay<Claude>| {
        for line in [
            json!({"type": "assistant", "parent_tool_use_id": null, "message": {
                "id": "msg_bg1", "role": "assistant", "content": [{
                    "type": "tool_use", "id": agent, "name": "Agent", "input": {
                        "description": "Audit", "subagent_type": "Explore",
                        "run_in_background": true}}]}}),
            json!({"type": "system", "subtype": "background_tasks_changed", "tasks": [
                {"task_id": "abg1", "task_type": "local_agent", "description": "Audit"}]}),
            json!({"type": "system", "subtype": "task_started", "task_id": "abg1",
                   "tool_use_id": agent, "subagent_type": "Explore",
                   "is_backgrounded": true, "task_type": "local_agent"}),
            json!({"type": "user", "parent_tool_use_id": null, "message": {"role": "user",
                "content": [{"type": "tool_result", "tool_use_id": agent,
                             "content": [{"type": "text", "text": "Async agent launched."}]}]}}),
            json!({"type": "result", "subtype": "success", "is_error": false}),
        ] {
            replay.agent(&line.to_string());
        }
        assert_eq!(tool_of(replay, agent).status, i32::from(Status::Running));
    };
    let stopped = |replay: &Replay<Claude>| {
        let tool = tool_of(replay, agent);
        (tool.status, tool.result_summary.clone())
    };
    let stopped_row = (i32::from(Status::Failed), "stopped".to_owned());

    // It leaves the list before its end is said: it is stopped, until the
    // agent says how it ended.
    let mut replay = ready();
    started(&mut replay);
    replay.agent(
        &json!({"type": "system", "subtype": "background_tasks_changed", "tasks": []}).to_string(),
    );
    assert_eq!(stopped(&replay), stopped_row);
    replay.agent(
        &json!({"type": "system", "subtype": "task_notification", "task_id": "abg1",
                "tool_use_id": agent, "status": "failed", "summary": "It broke.",
                "usage": {"total_tokens": 900, "tool_uses": 2}})
        .to_string(),
    );
    let tool = tool_of(&replay, agent);
    assert_eq!(
        (
            tool.status,
            tool.result_summary.as_str(),
            tool.tool_uses,
            tool.tokens,
            tool.output_preview.as_str()
        ),
        (i32::from(Status::Failed), "failed", 2, 900, "It broke.")
    );
    replay.client.assert_settled();

    // The agent ends.
    let mut replay = ready();
    started(&mut replay);
    replay.end("");
    assert_eq!(stopped(&replay), stopped_row);

    // The link to it is lost and made again: its row is read again.
    let mut replay = ready();
    started(&mut replay);
    replay.disconnect();
    replay.connect(&tag(SESSION, ""));
    assert_eq!(stopped(&replay), stopped_row);
}

#[tokio::test]
async fn a_task_outlives_its_turn_is_stopped_from_the_chat_and_the_agent_says_so() {
    let mut lines = BACKGROUND.lines().peekable();
    let mut replay = ready();
    replay.send(M1, "Start the dev server and leave it running.");
    replay.play(&mut lines, "\"type\":\"result\"");
    // The turn is over and the command still runs.
    replay.client.assert_settled();
    assert_eq!(
        listed(&replay),
        [(
            "bdev1".to_owned(),
            v1::ChatTaskKind::Command,
            "Run the dev server".to_owned(),
            true
        )]
    );

    // Only a task the list holds can be stopped.
    let stop = |task_id: &str| SessionCommand::StopTask {
        task_id: task_id.into(),
    };
    let refused = replay.command(stop("bgone")).unwrap_err();
    assert_eq!(refused.code, "chat_task_not_found");
    assert_eq!(
        replay.command(stop("")).unwrap_err().code,
        "chat_task_not_found"
    );
    replay.command(stop("bdev1")).unwrap();
    assert_eq!(
        replay.last_asked("stop_task")["request"],
        json!({"subtype": "stop_task", "task_id": "bdev1"})
    );
    replay.play(&mut lines, "\"tasks\":[]");
    assert!(replay.client.state.background.is_empty());
    replay.play(&mut lines, "@stop_task");
    let notices = |replay: &Replay<Claude>| {
        replay
            .client
            .items
            .iter()
            .filter(|item| item.kind == i32::from(Kind::Notice))
            .count()
    };
    assert_eq!(notices(&replay), 0);

    // The agent starts a turn of its own about it: working, with no message
    // of the user's to say so.
    replay.play(&mut lines, "message_start");
    assert_eq!(replay.client.state.turn, i32::from(Turn::Working));
    replay.play(&mut lines, "\"type\":\"result\"");
    replay.client.assert_settled();
    assert_eq!(
        replay.client.items.last().unwrap().text,
        "The dev server was stopped."
    );
}

#[tokio::test]
async fn compacting_is_said_while_it_lasts_and_leaves_a_notice_with_the_size_before() {
    let mut lines = BACKGROUND
        .lines()
        .skip_while(|line| !line.contains("/compact"))
        .peekable();
    let mut replay = ready();
    replay.send(M2, "/compact");
    replay.play(&mut lines, "\"compacting\"");
    assert!(replay.client.state.compacting);
    assert_eq!(replay.client.state.turn, i32::from(Turn::Working));
    replay.play(&mut lines, "compact_result");
    assert!(!replay.client.state.compacting);
    replay.play(&mut lines, "\"type\":\"result\"");
    replay.client.assert_settled();
    // The summary and the command's output are the agent's, not the user's.
    let outline: Vec<(i32, &str)> = replay
        .client
        .items
        .iter()
        .map(|item| (item.kind, item.text.as_str()))
        .collect();
    assert_eq!(
        outline,
        [
            (i32::from(Kind::User), "/compact"),
            (
                i32::from(Kind::Notice),
                "Context compacted, was 182k tokens"
            ),
        ]
    );

    // A turn that ends while it compacts ends the compacting too.
    replay
        .agent(&json!({"type": "system", "subtype": "status", "status": "compacting"}).to_string());
    assert!(replay.client.state.compacting);
    replay.agent(&json!({"type": "result", "subtype": "success", "is_error": false}).to_string());
    assert!(!replay.client.state.compacting);
    // So does the agent's end, which takes its tasks with it.
    replay.agent(
        &json!({"type": "system", "subtype": "background_tasks_changed",
                "tasks": [{"task_id": "b2", "task_type": "local_agent", "description": "Audit"}]})
        .to_string(),
    );
    assert_eq!(listed(&replay)[0].1, v1::ChatTaskKind::Subagent);
    replay
        .agent(&json!({"type": "system", "subtype": "status", "status": "compacting"}).to_string());
    replay.end("");
    assert!(replay.client.state.background.is_empty());
    assert!(!replay.client.state.compacting);
}

#[tokio::test]
async fn only_the_conversations_own_status_says_whether_it_compacts() {
    let mut replay = ready();
    let status = |fields: Value| {
        let mut line = json!({"type": "system", "subtype": "status"});
        line.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        line.to_string()
    };
    replay.agent(&status(json!({"status": "compacting"})));
    assert!(replay.client.state.compacting);
    // A status line that does not name the status leaves it as it is.
    replay.agent(&status(json!({"permissionMode": "bypassPermissions"})));
    assert!(replay.client.state.compacting);
    // A subagent's lines are about its own conversation.
    replay.agent(&status(
        json!({"status": null, "parent_tool_use_id": "toolu_9"}),
    ));
    assert!(replay.client.state.compacting);
    let boundary = json!({"type": "system", "subtype": "compact_boundary",
                          "parent_tool_use_id": "toolu_9", "compact_metadata": {}});
    replay.agent(&boundary.to_string());
    assert!(replay.client.state.compacting);
    replay.agent(&status(json!({"status": null})));
    assert!(!replay.client.state.compacting);
    replay.agent(&status(
        json!({"status": "compacting", "parent_tool_use_id": "toolu_9"}),
    ));
    assert!(!replay.client.state.compacting);

    // A link lost while it compacts does not leave it compacting.
    replay.agent(&status(json!({"status": "compacting"})));
    assert!(replay.client.state.compacting);
    replay.disconnect();
    replay.connect(&tag(SESSION, ""));
    assert!(!replay.client.state.compacting);
}

#[tokio::test]
async fn a_stop_the_agent_refuses_is_said_and_the_task_stays() {
    let mut replay = ready();
    replay.agent(
        &json!({"type": "system", "subtype": "background_tasks_changed",
                "tasks": [{"task_id": "b1", "task_type": "local_bash", "description": "Serve"}]})
        .to_string(),
    );
    replay
        .command(SessionCommand::StopTask {
            task_id: "b1".into(),
        })
        .unwrap();
    replay.agent(
        &json!({"type": "control_response", "response": {
            "subtype": "error", "request_id": "@stop_task", "error": "No such task"}})
        .to_string(),
    );
    assert_eq!(
        replay.client.items.last().unwrap().text,
        "The task could not be stopped: No such task"
    );
    assert_eq!(listed(&replay).len(), 1);
}

#[test]
fn token_counts_and_the_compaction_notice_read_as_the_chat_says_them() {
    assert_eq!(tokens(950), "950");
    assert_eq!(tokens(19_714), "19.7k");
    assert_eq!(tokens(24_100), "24.1k");
    assert_eq!(tokens(61_000), "61k");
    assert_eq!(tokens(182_000), "182k");
    assert_eq!(tokens(1_250_000), "1.3M");
    assert_eq!(
        compacted(&json!({"pre_tokens": 182_000})),
        "Context compacted, was 182k tokens"
    );
    // The session's record names it in camel case.
    assert_eq!(
        compacted(&json!({"preTokens": 19_997})),
        "Context compacted, was 20k tokens"
    );
    assert_eq!(compacted(&Value::Null), "Context compacted");
    let summary = json!({"type": "user", "isReplay": true, "message": {"role": "user",
        "content": "This session is being continued from a previous conversation that ran out of context."}});
    assert_eq!(said(&summary), Said::Nothing);
}

// --- names ------------------------------------------------------------------

/// A chat that has reached its agent and read its conversation, in which
/// nothing has been said yet.
fn read() -> Replay<Claude> {
    let mut replay = ready();
    let nothing = tempfile::tempdir().unwrap();
    replay.done(Done::History(history::load(nothing.path(), SESSION, "")));
    replay
}

/// A home whose recorded session also has the names given as lines, appended
/// after its conversation as the agent appends them.
fn named_home(names: &[Value]) -> tempfile::TempDir {
    let home = home();
    let file = home.path().join(format!("projects/-work/{SESSION}.jsonl"));
    let mut text = std::fs::read_to_string(&file).unwrap();
    for name in names {
        text.push_str(&format!("{name}\n"));
    }
    std::fs::write(&file, text).unwrap();
    home
}

#[test]
fn the_session_goes_by_the_users_name_else_the_agents() {
    let custom =
        |name: &str| json!({"type": "custom-title", "customTitle": name, "sessionId": SESSION});
    let ai = |name: &str| json!({"type": "ai-title", "aiTitle": name, "sessionId": SESSION});
    let agent = |name: &str| json!({"type": "agent-name", "agentName": name, "sessionId": SESSION});
    let name = |names: &[Value]| history::load(named_home(names).path(), SESSION, "").name;

    assert_eq!(name(&[]), "");
    assert_eq!(name(&[agent("Worker")]), "Worker");
    assert_eq!(name(&[agent("Worker"), ai("Fix the bar")]), "Fix the bar");
    assert_eq!(
        name(&[
            custom("Old"),
            ai("Fix the bar"),
            custom("Mine"),
            agent("Worker")
        ]),
        "Mine"
    );
    // The newest of each kind counts; a cleared one gives way to the next.
    assert_eq!(name(&[ai("First"), ai("Second")]), "Second");
    assert_eq!(
        name(&[custom("Mine"), custom(""), ai("Fix the bar")]),
        "Fix the bar"
    );
    // A rewind does not take the name back.
    let home = named_home(&[custom("Mine")]);
    assert_eq!(history::load(home.path(), SESSION, M3).name, "Mine");
    // A copy with nothing said in it is read from its source, without the
    // source's name.
    let copy = history::load_through(home.path(), SESSION, M3);
    assert!(!copy.items.is_empty());
    assert_eq!(copy.name, "");
    // Nor is the conversation read any differently.
    assert_eq!(
        history::load(home.path(), SESSION, "").items,
        history::load(named_home(&[]).path(), SESSION, "").items
    );
}

/// A resumed session takes the name its file gives it, also over the one
/// it was started under, which stands in until the file is read.
#[tokio::test]
async fn a_resumed_session_is_named_from_its_file_and_asks_for_no_title() {
    let home =
        named_home(&[json!({"type": "ai-title", "aiTitle": "Arithmetic", "sessionId": SESSION})]);
    let mut replay = ready();
    replay.done(read_history(home.path(), ""));
    assert_eq!(replay.titles.last().map(String::as_str), Some("Arithmetic"));
    replay.send("m-new", "And 3+3?");
    assert!(replay.background.is_empty());

    let mut started = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    started.connect(&Tag {
        launch: launch(&["--name", "Fix tests (fork)"], None).unwrap(),
        ..tag(SESSION, "")
    });
    started.done(read_history(home.path(), ""));
    assert_eq!(started.titles, ["Fix tests (fork)", "Arithmetic"]);
}

fn asked_title(replay: &Replay<Claude>) -> &naming::Ask {
    match replay.background.last() {
        Some(Job::Title(ask)) => ask,
        _ => panic!("no title was asked for"),
    }
}

/// The chat's first message has a title asked for beside the chat; the
/// title becomes the chat's name, which the agent is told as a rename.
#[tokio::test]
async fn a_chat_with_no_name_is_named_after_its_first_message() {
    let mut replay = read();
    replay.send(M1, "I dont love how this bar looks.\nCan we fix it?");
    // The first line stands in until then.
    assert_eq!(replay.titles, ["I dont love how this bar looks."]);
    let ask = asked_title(&replay).clone();
    assert_eq!(ask.pane_id, "%1");
    assert_eq!(ask.adapter_id, ADAPTER_ID);
    assert_eq!(ask.first, "I dont love how this bar looks.\nCan we fix it?");
    assert_eq!(ask.answer_to, None);
    assert!(!ask.asked_by_user);
    assert_eq!(
        ask.argv,
        strings(&[
            "claude",
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
            "--strict-mcp-config",
        ])
    );
    // Once only.
    replay.send(M2, "And the colours?");
    assert_eq!(replay.background.len(), 1);

    replay.done(Done::Titled(naming::Titled {
        ticket: ask.ticket,
        asked_by_user: false,
        title: Some("Fix the agents bar".into()),
    }));
    assert_eq!(replay.titles.last().unwrap(), "Fix the agents bar");
    assert_eq!(
        replay.last_asked("rename_session")["request"],
        json!({"subtype": "rename_session", "title": "Fix the agents bar"})
    );
    // The pane's start does not carry it: the agent keeps it.
    assert!(
        replay
            .jobs
            .iter()
            .all(|job| matches!(job, Job::History { .. } | Job::Tag { .. }))
    );
    // The agent saying so changes nothing more.
    let changed = json!({"type": "system", "subtype": "session_title_changed",
        "title": "Fix the agents bar", "session_id": SESSION});
    let titled = replay.titles.len();
    replay.agent(&changed.to_string());
    assert_eq!(replay.titles.len(), titled);
    // A later rename of the agent's own counts, and outlives a new link.
    let renamed = json!({"type": "system", "subtype": "session_title_changed",
        "title": "Renamed in a terminal", "session_id": SESSION});
    replay.agent(&renamed.to_string());
    replay.disconnect();
    replay.connect(&tag(SESSION, ""));
    assert_eq!(replay.titles.last().unwrap(), "Renamed in a terminal");
}

/// A session read only from the end of its file has no first message to
/// name it from: it keeps the first line it has, and asks for no title.
#[tokio::test]
async fn a_session_read_only_in_part_asks_for_no_title() {
    let home = home();
    let mut replay = ready();
    let mut history = history::load(home.path(), SESSION, "");
    history.complete = false;
    replay.done(Done::History(history));
    assert!(!replay.client.state.history_complete);
    assert!(replay.background.is_empty());
    replay.send("m-new", "And 3+3?");
    assert!(replay.background.is_empty());
}

/// A title that comes after the chat was named otherwise is dropped, and one
/// that could not be had leaves the first line, saying nothing.
#[tokio::test]
async fn a_title_is_dropped_once_the_chat_has_a_name_or_when_there_is_none() {
    let mut replay = read();
    replay.send(M1, "What is 2+2?");
    let ticket = asked_title(&replay).ticket;
    let renamed = json!({"type": "system", "subtype": "session_title_changed",
        "title": "Named in a terminal", "session_id": SESSION});
    replay.agent(&renamed.to_string());
    replay.done(Done::Titled(naming::Titled {
        ticket,
        asked_by_user: false,
        title: Some("Arithmetic".into()),
    }));
    assert_eq!(replay.titles.last().unwrap(), "Named in a terminal");
    assert!(
        replay
            .written
            .iter()
            .all(|written| written["request"]["subtype"] != "rename_session")
    );

    let mut failed = read();
    failed.send(M1, "What is 2+2?");
    let items = failed.client.items.len();
    let ticket = asked_title(&failed).ticket;
    failed.done(Done::Titled(naming::Titled {
        ticket,
        asked_by_user: false,
        title: None,
    }));
    assert_eq!(failed.titles, ["What is 2+2?"]);
    assert_eq!(failed.client.items.len(), items);
}

/// `/rename` is not sent to the agent: the chat is renamed, and a notice
/// stands where the message would have been.
#[tokio::test]
async fn rename_names_the_chat_without_a_message_to_the_agent() {
    let mut replay = read();
    // Nothing to name it from yet.
    let refused = replay
        .command(SessionCommand::Send {
            text: "/rename".into(),
            item_id: "r0".into(),
            attachments: Vec::new(),
        })
        .unwrap_err();
    assert_eq!(refused.code, "chat_rename_unavailable");

    replay.send("r1", "/rename   #123  login\tbug  ");
    let notice = replay.client.item("r1").unwrap();
    assert_eq!(notice.kind, i32::from(Kind::Notice));
    assert_eq!(notice.text, "Renamed to \u{201c}#123 login bug\u{201d}.");
    assert_eq!(replay.titles, ["#123 login bug"]);
    assert_eq!(
        replay.last_asked("rename_session")["request"]["title"],
        "#123 login bug"
    );
    assert!(
        replay
            .written
            .iter()
            .all(|written| written["type"] != "user")
    );
    // Named, the chat asks for no title of its own.
    replay.send(M1, "What is 2+2?");
    assert!(replay.background.is_empty());
    assert_eq!(replay.titles, ["#123 login bug"]);

    // With no name, it is named again from its first message, even mid-turn.
    replay.send("r2", "/rename");
    assert_eq!(
        replay.client.item("r2").unwrap().text,
        "Naming this chat from its first message."
    );
    let ask = asked_title(&replay).clone();
    assert!(ask.asked_by_user);
    assert_eq!(ask.first, "What is 2+2?");
    replay.done(Done::Titled(naming::Titled {
        ticket: ask.ticket,
        asked_by_user: true,
        title: Some("Arithmetic".into()),
    }));
    assert_eq!(replay.titles.last().unwrap(), "Arithmetic");

    // One that fails says so.
    replay.send("r3", "/rename");
    let ticket = asked_title(&replay).ticket;
    replay.done(Done::Titled(naming::Titled {
        ticket,
        asked_by_user: true,
        title: None,
    }));
    let said = replay.client.items.last().unwrap();
    assert_eq!(said.kind, i32::from(Kind::Notice));
    assert!(said.text.starts_with("This chat could not be named."));
    assert_eq!(replay.titles.last().unwrap(), "Arithmetic");

    // A rename carries no files.
    let refused = replay
        .command(SessionCommand::Send {
            text: "/rename Files".into(),
            item_id: "r4".into(),
            attachments: vec![attached("/tmp/a.txt", "", None)],
        })
        .unwrap_err();
    assert_eq!(refused.code, "chat_invalid_request");
}

/// A name chosen before the agent listens is told it once it does.
#[tokio::test]
async fn a_name_chosen_before_the_agent_is_ready_is_told_when_it_is() {
    let mut replay = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    replay.connect(&tag(SESSION, ""));
    replay.send(M1, "What is 2+2?");
    replay.send("r1", "/rename Arithmetic");
    assert!(
        replay
            .written
            .iter()
            .all(|written| written["request"]["subtype"] != "rename_session")
    );
    replay.agent(
        &json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": "@initialize", "response": {}}})
        .to_string(),
    );
    assert_eq!(
        replay.last_asked("rename_session")["request"]["title"],
        "Arithmetic"
    );
}

#[tokio::test]
async fn the_composer_lists_rename_as_a_command() {
    let replay = ready();
    let rename = replay
        .client
        .catalog
        .commands
        .iter()
        .find(|command| command.name == "rename")
        .expect("rename is listed");
    assert_eq!(rename.kind, i32::from(v1::ChatCommandKind::Command));
}

/// A name the session has of its own, given since it started or read from
/// its file, comes before the one it was started under, over a new link and
/// a new daemon alike.
#[tokio::test]
async fn a_name_given_since_comes_before_the_one_started_under() {
    let started = Tag {
        launch: launch(&["--name", "Fix tests (fork)"], None).unwrap(),
        ..tag(SESSION, "")
    };
    let mut replay = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    replay.connect(&started);
    replay.send("r1", "/rename Mine");
    assert_eq!(replay.titles, ["Fix tests (fork)", "Mine"]);
    replay.disconnect();
    replay.connect(&started);
    let nothing = tempfile::tempdir().unwrap();
    replay.done(Done::History(history::load(nothing.path(), SESSION, "")));
    assert_eq!(replay.titles, ["Fix tests (fork)", "Mine"]);

    // A daemon started since reads the name from the session's file.
    let home =
        named_home(&[json!({"type": "custom-title", "customTitle": "Mine", "sessionId": SESSION})]);
    let mut restarted = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    restarted.connect(&started);
    restarted.done(read_history(home.path(), ""));
    assert_eq!(restarted.titles.last().unwrap(), "Mine");
}

/// A message sent before the conversation has been read is named from once
/// it has been, from the conversation's own first message.
#[tokio::test]
async fn a_title_waits_for_the_conversation_to_be_read() {
    let mut replay = ready();
    replay.send(M1, "Fix the agents bar");
    assert!(replay.background.is_empty());
    let nothing = tempfile::tempdir().unwrap();
    replay.done(Done::History(history::load(nothing.path(), SESSION, "")));
    assert_eq!(asked_title(&replay).first, "Fix the agents bar");

    // Read from the end of its file only, it is not named from its first
    // message, by the user either.
    let home = home();
    let mut partial = ready();
    let mut history = history::load(home.path(), SESSION, "");
    history.complete = false;
    partial.done(Done::History(history));
    let refused = partial
        .command(SessionCommand::Send {
            text: "/rename".into(),
            item_id: "r1".into(),
            attachments: Vec::new(),
        })
        .unwrap_err();
    assert_eq!(refused.code, "chat_rename_unavailable");
    // A name typed is still taken.
    partial.send("r2", "/rename Arithmetic");
    assert_eq!(partial.titles.last().unwrap(), "Arithmetic");
}

/// A chat started with settings of its own may reach another account or
/// endpoint: it is not asked for a title, and says so when the user asks.
#[tokio::test]
async fn a_chat_started_with_its_own_settings_asks_for_no_title() {
    let mut replay = Replay::new(Claude::default(), ClaudeProvider.capabilities());
    replay.connect(&Tag {
        launch: launch(&["--settings", "/work/other.json"], None).unwrap(),
        ..tag(SESSION, "")
    });
    let nothing = tempfile::tempdir().unwrap();
    replay.done(Done::History(history::load(nothing.path(), SESSION, "")));
    replay.send(M1, "Fix the agents bar");
    replay.send("r1", "/rename");
    assert!(replay.background.is_empty());
    assert_eq!(replay.client.item("r1").unwrap().text, naming::NOT_NAMED);
    assert_eq!(replay.titles, ["Fix the agents bar"]);
}

/// Seen on 2.1.330: a message sent while a command runs is `queued` at once,
/// and replayed with `started` only once the command has ended.
#[tokio::test]
async fn a_message_sent_while_a_step_runs_is_queued_until_the_agent_reads_it() {
    let mut replay = ready();
    replay.send(M5, "run sleep 15");
    // Sent to an agent that waits on nothing: it is read at once.
    assert!(!replay.client.item(M5).unwrap().queued);
    replay.send("m6", "and banana");
    replay.send("m7", "and cherry");
    assert!(replay.client.item("m6").unwrap().queued);
    for id in ["m6", "m7"] {
        let queued = json!({"type": "command_lifecycle", "state": "queued",
                            "command_uuid": message_uuid(id)});
        replay.agent(&queued.to_string());
    }
    assert!(replay.client.item("m7").unwrap().queued);
    let read = json!({"type": "user", "isReplay": true, "session_id": SESSION,
        "uuid": message_uuid("m6"), "message": {"role": "user", "content": "and banana"}});
    replay.agent(&read.to_string());
    let m6 = replay.client.item("m6").unwrap();
    assert_eq!((m6.text.as_str(), m6.queued), ("and banana", false));
    // One the agent lets go waits no longer either.
    let cancelled = json!({"type": "command_lifecycle", "state": "cancelled",
                           "command_uuid": message_uuid("m7")});
    replay.agent(&cancelled.to_string());
    assert!(!replay.client.item("m7").unwrap().queued);
}
