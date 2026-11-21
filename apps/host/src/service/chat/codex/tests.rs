use std::path::Path;

use serde_json::{Value, json};

use super::super::tests::replay::{Replay, Wire};
use super::*;

/// One conversation over three links to the server, each after the line
/// that marks a new one.
const STREAM: &str = include_str!("../fixtures/codex-session.stream.jsonl");
const NEW_LINK: &str = "{\"muxflow_fixture\":\"reconnect\"}\n";
const THREAD: &str = "0c0de000-0000-7000-8000-000000000001";
/// The third turn of the recording.
const THIRD_TURN: &str = "0c0de000-0000-7000-8000-000000000009";

impl Wire for Codex {
    fn asked(written: &Value) -> Option<(String, Value)> {
        let method = written["method"].as_str()?;
        Some((method.to_owned(), written.get("id")?.clone()))
    }

    fn answers(inbound: &mut Value) -> Option<&mut Value> {
        if inbound.get("method").is_some() {
            return None;
        }
        inbound.get_mut("id")
    }
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn launch(words: &[&str]) -> Option<Launch> {
    CodexProvider.launch(&strings(words), None)
}

fn tag(thread: &str, launch: Launch) -> Tag {
    Tag {
        adapter: ADAPTER_ID.into(),
        session: thread.into(),
        cwd: "/work".into(),
        launch,
        ..Default::default()
    }
}

fn replay() -> Replay<Codex> {
    Replay::new(Codex::default(), CodexProvider.capabilities())
}

/// A chat on the recorded thread that has reached its server, and the first
/// link's lines from after the thread was opened.
fn opened(launch: Launch) -> (Replay<Codex>, impl Iterator<Item = &'static str>) {
    let mut lines = STREAM.lines();
    let mut replay = replay();
    replay.connect(&tag("", launch));
    replay.play(&mut lines, "@thread/start");
    (replay, lines)
}

// --- the command line -------------------------------------------------------

#[test]
fn a_terminal_start_is_read_into_what_its_chat_starts_with() {
    assert_eq!(launch(&[]), Some(Launch::default()));
    let full = launch(&[
        "-m",
        "gpt-6.1-sol",
        "-c",
        "model_reasoning_effort=\"high\"",
        "--enable",
        "web_search",
        "-C",
        "/work/sub",
        "--add-dir",
        "/a",
        "--add-dir",
        "/b",
        "--dangerously-bypass-approvals-and-sandbox",
        "--no-alt-screen",
        "fix the tests",
    ])
    .unwrap();
    assert_eq!(
        full,
        Launch {
            cwd: "/work/sub".into(),
            model: "gpt-6.1-sol".into(),
            full_access: true,
            directories: strings(&["/a", "/b"]),
            passthrough: strings(&[
                "--config",
                "model_reasoning_effort=\"high\"",
                "--enable",
                "web_search"
            ]),
            prompt: "fix the tests".into(),
            ..Default::default()
        }
    );
    // Unchecked is both halves: no approvals and no sandbox.
    assert!(
        launch(&["-a", "never", "-s", "danger-full-access"])
            .unwrap()
            .full_access
    );
    assert!(!launch(&["-a", "never"]).unwrap().full_access);
    assert!(!launch(&["-s", "danger-full-access"]).unwrap().full_access);
}

#[test]
fn the_thread_a_chat_resumes_is_the_one_the_command_line_names() {
    let resume = |words: &[&str]| launch(words).map(|launch| (launch.resume, launch.prompt));
    let session = |fork| Resume::Session {
        id: THREAD.into(),
        fork,
    };
    assert_eq!(
        resume(&["resume", THREAD]),
        Some((session(false), String::new()))
    );
    assert_eq!(
        resume(&["fork", THREAD]),
        Some((session(true), String::new()))
    );
    assert_eq!(
        resume(&["resume", THREAD, "go on"]),
        Some((session(false), "go on".into()))
    );
    assert_eq!(
        resume(&["resume", "--last"]),
        Some((Resume::Latest, String::new()))
    );
    // A picker, and a copy of whichever session is newest, are not known here.
    assert_eq!(resume(&["resume"]), None);
    assert_eq!(resume(&["fork"]), None);
    assert_eq!(resume(&["fork", "--last"]), None);
    assert_eq!(resume(&["--last"]), None);
}

#[test]
fn a_start_that_is_not_a_session_stays_a_terminal() {
    for words in [
        &["exec", "say hi"][..],
        &["e", "say hi"],
        &["app-server"],
        &["mcp", "list"],
        &["login"],
        &["review"],
        &["--version"],
        &["--help"],
        &["--some-new-option"],
        &["-i", "shot.png", "what is this"],
        &["--sandbox", "read-only"],
        &["two", "prompts"],
        &["resume", THREAD, "go on", "more"],
    ] {
        assert_eq!(launch(words), None, "{words:?}");
    }
}

#[test]
fn the_pane_runs_the_agents_server_on_the_panes_socket() {
    let launch = Launch {
        program: strings(&["node", "/opt/codex/bin/codex.js"]),
        model: "gpt-6.1-sol".into(),
        passthrough: strings(&["--config", "a=1", "--strict-config"]),
        resume: Resume::Latest,
        ..Default::default()
    };
    let command = CodexProvider
        .pane_command(&launch, Path::new("/run/chat/a.sock"))
        .unwrap();
    assert_eq!(
        command.argv,
        [
            "node",
            "/opt/codex/bin/codex.js",
            "app-server",
            "--config",
            "a=1",
            "--strict-config",
            "-c",
            "features.default_mode_request_user_input=true",
            "--listen",
            "unix:///run/chat/a.sock",
        ]
    );
    // It reads nothing from the pane, and the thread is opened over the
    // socket.
    assert!(!command.piped);
    assert_eq!(command.session, "");
}

// --- single items -----------------------------------------------------------

#[test]
fn each_item_of_a_thread_is_drawn_as_the_row_of_its_kind() {
    let row = |source: Value, finished: bool| {
        let item = thread_item(&source, finished).unwrap();
        let tool = item.tool.unwrap();
        let kind = v1::ChatToolKind::try_from(tool.kind).unwrap();
        let status = v1::ChatToolStatus::try_from(tool.status).unwrap();
        (kind, tool.title, tool.detail, status, tool.result_summary)
    };
    let expect = |kind, title: &str, detail: &str, status, summary: &str| {
        (
            kind,
            title.to_owned(),
            detail.to_owned(),
            status,
            summary.to_owned(),
        )
    };
    let command = |status: &str, code: Value| {
        json!({"type": "commandExecution", "id": "c", "status": status, "exitCode": code,
               "command": "/usr/bin/bash -lc 'cargo test'",
               "commandActions": [{"type": "unknown", "command": "cargo test"}]})
    };
    assert_eq!(
        row(command("inProgress", Value::Null), false),
        expect(ToolKind::Command, "run", "cargo test", Status::Running, "")
    );
    assert_eq!(
        row(command("completed", json!(0)), true),
        expect(ToolKind::Command, "run", "cargo test", Status::Ok, "done")
    );
    assert_eq!(
        row(command("failed", json!(101)), true),
        expect(
            ToolKind::Command,
            "run",
            "cargo test",
            Status::Failed,
            "exit 101"
        )
    );
    assert_eq!(
        row(command("declined", Value::Null), true),
        expect(
            ToolKind::Command,
            "run",
            "cargo test",
            Status::Failed,
            "declined"
        )
    );
    // One a turn left running did not finish.
    assert_eq!(
        row(command("inProgress", Value::Null), true),
        expect(
            ToolKind::Command,
            "run",
            "cargo test",
            Status::Failed,
            "stopped"
        )
    );
    assert_eq!(
        row(
            json!({"type": "mcpToolCall", "id": "m", "server": "docs", "tool": "search",
                   "status": "completed", "error": null}),
            true
        ),
        expect(ToolKind::Mcp, "docs", "search", Status::Ok, "done")
    );
    assert_eq!(
        row(
            json!({"type": "webSearch", "id": "w", "query": "tmux respawn-pane"}),
            true
        ),
        expect(
            ToolKind::Web,
            "search",
            "tmux respawn-pane",
            Status::Ok,
            "done"
        )
    );
    assert_eq!(
        row(
            json!({"type": "collabAgentToolCall", "id": "a", "tool": "spawnAgent",
                   "prompt": "Find the callers", "status": "completed"}),
            true
        ),
        expect(
            ToolKind::Subagent,
            "agent",
            "Find the callers",
            Status::Ok,
            "done"
        )
    );
    // Waiting on a subagent is no row of its own.
    assert!(
        thread_item(
            &json!({"type": "collabAgentToolCall", "id": "w", "tool": "wait",
                    "status": "inProgress"}),
            false
        )
        .is_none()
    );

    let change = thread_item(
        &json!({"type": "fileChange", "id": "f", "status": "completed", "changes": [
            {"path": "/work/a.rs", "diff": "@@ -1,2 +1,3 @@\n a\n-b\n+c\n+d\n"},
            {"path": "/work/b.rs", "diff": "@@ -0,0 +1 @@\n+e\n"},
        ]}),
        true,
    )
    .unwrap()
    .tool
    .unwrap();
    assert_eq!(change.detail, "/work/a.rs, /work/b.rs");
    assert_eq!(
        (change.path.as_str(), change.added, change.removed),
        ("/work/a.rs", 3, 1)
    );

    // Reasoning is its summary, or what there is of it; none is no item.
    let reasoning = |summary: Value, content: Value| {
        let source =
            json!({"type": "reasoning", "id": "r", "summary": summary, "content": content});
        thread_item(&source, true).map(|item| item.text)
    };
    assert_eq!(
        reasoning(json!(["Plan", "Do"]), json!(["raw"])),
        Some("Plan\nDo".into())
    );
    assert_eq!(reasoning(json!([]), json!(["raw"])), Some("raw".into()));
    assert_eq!(reasoning(json!([]), json!([])), None);
    // A message of the user's goes by the id its client gave it.
    let user = thread_item(
        &json!({"type": "userMessage", "id": "server", "clientId": "mine",
                "content": [{"type": "text", "text": "one"}, {"type": "text", "text": "two"}]}),
        true,
    )
    .unwrap();
    assert_eq!((user.id.as_str(), user.text.as_str()), ("mine", "one\ntwo"));
    assert!(
        thread_item(
            &json!({"type": "agentMessage", "id": "m", "text": ""}),
            true
        )
        .is_none()
    );
    assert!(thread_item(&json!({"type": "somethingNew", "id": "n"}), true).is_none());
}

// --- the recorded session ---------------------------------------------------

/// The whole recorded conversation, with what a client sees at each step.
#[tokio::test]
async fn the_recorded_session_reads_as_the_conversation_it_was() {
    let mut links = STREAM.split(NEW_LINK);
    let mut lines = links.next().unwrap().lines().peekable();
    let mut replay = replay();

    // The server is reached, and a thread started on it.
    replay.connect(&tag("", Launch::default()));
    assert_eq!(
        replay.client.state.connection,
        i32::from(Connection::Connecting)
    );
    assert_eq!(replay.written[0]["method"], "initialize");
    assert_eq!(replay.written[0]["params"]["clientInfo"]["name"], "muxflow");
    replay.play(&mut lines, "@thread/start");
    assert_eq!(
        replay.written[1],
        json!({"method": "initialized", "params": {}})
    );
    assert_eq!(
        replay.last_asked("skills/list")["params"],
        json!({"cwds": ["/work"]})
    );
    assert_eq!(
        replay.last_asked("thread/start")["params"],
        json!({"approvalPolicy": "on-request", "approvalsReviewer": "auto_review",
               "sandbox": "workspace-write"})
    );
    replay.client.assert_settled();
    assert!(replay.client.state.history_complete);
    assert!(replay.client.items.is_empty());
    // A thread that has had no turn is one the server could not open again:
    // the pane is not told of it yet.
    assert!(replay.jobs.is_empty());

    // A plain answer.
    replay.send("m1", "Reply with exactly: ok");
    assert_eq!(replay.client.state.turn, i32::from(Turn::Working));
    assert_eq!(
        replay.last_asked("turn/start")["params"],
        json!({
            "threadId": THREAD,
            "input": [{"type": "text", "text": "Reply with exactly: ok"}],
            "clientUserMessageId": "m1",
            "model": "gpt-5.6-luna",
            "effort": "high",
        })
    );
    replay.play(&mut lines, "turn/completed");
    // The pane is told which thread is its own, for the daemon that is next.
    assert!(matches!(replay.jobs.as_slice(), [Job::Tag { thread, .. }] if thread == THREAD));
    // It goes by the conversation's first message, until the thread is
    // given a name; another thread's name is not this chat's.
    assert_eq!(replay.titles, ["Reply with exactly: ok"]);
    let named = |thread: &str, name: &str| {
        json!({"method": "thread/name/updated",
               "params": {"threadId": thread, "threadName": name}})
        .to_string()
    };
    replay.agent(&named("0c0de000-0000-7000-8000-00000000ffff", "Another"));
    replay.agent(&named(THREAD, "Say ok"));
    assert_eq!(replay.titles, ["Reply with exactly: ok", "Say ok"]);
    replay
        .client
        .assert_answered("m1", "Reply with exactly: ok", "ok");
    replay.client.assert_describes_itself();
    let options = replay.client.state.options.clone().unwrap();
    assert_eq!(options.permission, i32::from(Permission::Auto));
    assert_eq!(options.speed, i32::from(Speed::Fast));

    // A question, answered with one of its choices.
    let ask = "Ask me, with the request_user_input tool, whether I prefer red or blue, \
               then reply with just the color.";
    replay.send("m2", ask);
    replay.play(&mut lines, "item/tool/requestUserInput");
    let answer = replay.client.assert_asks("Blue");
    replay.command(answer).unwrap();
    assert_eq!(
        replay.written.last().unwrap(),
        &json!({"id": 0, "result": {"answers": {"preferred_color": {"answers": ["Blue"]}}}})
    );
    assert_eq!(replay.client.state.turn, i32::from(Turn::Working));
    replay.play(&mut lines, "turn/completed");
    replay.client.assert_answered("m2", ask, "blue");

    // A command the agent needs leave for.
    let run = "Run this exact shell command and nothing else: echo hi > probe.txt";
    replay.send("m3", run);
    replay.play(&mut lines, "item/commandExecution/requestApproval");
    let answer = replay.client.assert_asks_leave("echo hi > probe.txt");
    replay.command(answer).unwrap();
    assert_eq!(
        replay.written.last().unwrap(),
        &json!({"id": 1, "result": {"decision": "accept"}})
    );
    replay.play(&mut lines, "turn/completed");
    replay.client.assert_settled();
    replay.client.assert_ran("m3", "echo hi > probe.txt");
    assert_eq!(lines.next(), None);
    // The agent runtime heard of both waits, and of their ends.
    let reported: Vec<&str> = replay
        .reports
        .iter()
        .map(|report| report["hook_event_name"].as_str().unwrap())
        .collect();
    assert_eq!(
        reported,
        ["PreToolUse", "PostToolUse", "PreToolUse", "PostToolUse"]
    );
    assert!(
        replay
            .reports
            .iter()
            .all(|report| report["session_id"] == THREAD)
    );

    // The link is lost and found again: the thread as the server has it is
    // the conversation already shown.
    let shown = replay.client.items.clone();
    replay.disconnect();
    assert_eq!(
        replay.client.state.connection,
        i32::from(Connection::Connecting)
    );
    let mut lines = links.next().unwrap().lines().peekable();
    replay.connect(&tag(THREAD, Launch::default()));
    replay.play(&mut lines, "@thread/resume");
    assert_eq!(
        replay.last_asked("thread/resume")["params"],
        json!({"threadId": THREAD})
    );
    replay.client.assert_settled();
    assert_eq!(replay.client.items, shown, "{:#?}", replay.client.outline());
    assert_eq!(replay.jobs.len(), 1);
    // The agent runtime is told which thread a chat came back to: the
    // server's hooks name it only once a turn runs.
    assert_eq!(
        replay.reports.last().unwrap(),
        &json!({"hook_event_name": "SessionStart", "session_id": THREAD})
    );
    let reported = replay.reports.len();

    // A long answer, and the link lost in the middle of it.
    replay.send(
        "m4",
        "Count from 1 to 600, one number per line, no other text.",
    );
    for line in lines {
        replay.agent(line);
    }
    let said = replay.client.items.last().unwrap().clone();
    assert!(said.streaming && said.text.starts_with("1\n2\n3"));
    replay.disconnect();
    let mut lines = links.next().unwrap().lines().peekable();
    replay.connect(&tag(THREAD, Launch::default()));
    replay.play(&mut lines, "@thread/resume");
    // The turn is still the server's, and what was said of it still shown.
    // A thread in a turn is one the server's hooks speak for.
    assert_eq!(replay.reports.len(), reported);
    assert_eq!(replay.client.state.turn, i32::from(Turn::Working));
    assert_eq!(replay.client.items.last().unwrap(), &said);

    // It goes on from where it is, and is stopped.
    replay.play_before(&mut lines, "@turn/interrupt");
    let longer = replay.client.items.last().unwrap();
    assert!(longer.streaming && longer.text.len() > said.text.len());
    replay.command(SessionCommand::Interrupt).unwrap();
    let interrupt = &replay.last_asked("turn/interrupt")["params"];
    assert_eq!(interrupt["threadId"], THREAD);
    replay.play(&mut lines, "turn/completed");
    replay.client.assert_stopped("m4");

    // Going back to the third message takes the thread back to before its
    // turn.
    replay
        .command(SessionCommand::Rewind {
            item_id: "m3".into(),
            after_item_id: String::new(),
        })
        .unwrap();
    assert_eq!(
        replay.last_asked("thread/revert")["params"],
        json!({"threadId": THREAD, "beforeTurnId": THIRD_TURN})
    );
    // Nothing changes until the server has done it; a message sent meanwhile
    // waits.
    assert!(replay.client.item("m3").is_some());
    let list = "In one short line, list every request I made before this one.";
    replay.send("m5", list);
    assert_eq!(
        replay.last_asked("turn/start")["params"]["clientUserMessageId"],
        "m4"
    );
    replay.play(&mut lines, "@thread/revert");
    assert!(replay.client.item("m3").is_none() && replay.client.item("m4").is_none());
    assert_eq!(
        replay.last_asked("turn/start")["params"]["clientUserMessageId"],
        "m5"
    );
    for line in lines {
        replay.agent(line);
    }
    replay.client.assert_answered("m5", list, "red or blue");
    let ids: Vec<&str> = replay
        .client
        .items
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(ids, ["m1", "msg_04", "m2", "msg_08", "m5", "msg_20"]);
    // The message after a rewind can be gone back to in its turn.
    assert!(replay.client.item("m5").unwrap().rewindable);
}

#[tokio::test]
async fn a_thread_is_opened_the_way_its_agent_was_started() {
    let params = |launch: Launch, tag_thread: &str| {
        let mut replay = replay();
        let mut lines = STREAM.lines();
        replay.connect(&tag(tag_thread, launch));
        replay.play(&mut lines, "@skills/list");
        let opened = replay.written.last().unwrap().clone();
        (
            opened["method"].as_str().unwrap().to_owned(),
            opened["params"].clone(),
        )
    };
    let checked = json!({"approvalPolicy": "on-request", "approvalsReviewer": "auto_review",
                         "sandbox": "workspace-write"});
    let with = |mut params: Value, name: &str, value: &str| {
        params[name] = json!(value);
        params
    };
    assert_eq!(
        params(
            Launch {
                model: "gpt-6.1-sol".into(),
                full_access: true,
                ..Default::default()
            },
            ""
        ),
        (
            "thread/start".to_owned(),
            json!({"model": "gpt-6.1-sol", "approvalPolicy": "never", "approvalsReviewer": "user",
                   "sandbox": "danger-full-access"})
        )
    );
    let session = |fork| Launch {
        resume: Resume::Session {
            id: "other".into(),
            fork,
        },
        ..Default::default()
    };
    assert_eq!(
        params(session(false), ""),
        (
            "thread/resume".to_owned(),
            with(checked.clone(), "threadId", "other")
        )
    );
    assert_eq!(
        params(session(true), ""),
        (
            "thread/fork".to_owned(),
            with(checked.clone(), "threadId", "other")
        )
    );
    // A pane that has its thread already goes back to it, however it started.
    assert_eq!(
        params(session(true), THREAD),
        ("thread/resume".to_owned(), json!({"threadId": THREAD}))
    );

    // The newest thread of the folder is asked for first, and resumed; with
    // none, the conversation is a new one.
    let latest = Launch {
        resume: Resume::Latest,
        ..Default::default()
    };
    let (method, listed) = params(latest.clone(), "");
    assert_eq!(method, "thread/list");
    assert_eq!(
        listed,
        json!({"cwd": "/work", "limit": 1, "sortKey": "updated_at", "sortDirection": "desc"})
    );
    for (found, method, thread) in [
        (json!([{"id": "newest"}]), "thread/resume", json!("newest")),
        (json!([]), "thread/start", Value::Null),
    ] {
        let mut replay = replay();
        let mut lines = STREAM.lines();
        replay.connect(&tag("", latest.clone()));
        replay.play(&mut lines, "@skills/list");
        replay.agent(&json!({"id": "@thread/list", "result": {"data": found}}).to_string());
        let opened = replay.written.last().unwrap();
        assert_eq!(opened["method"], method);
        assert_eq!(opened["params"]["threadId"], thread);
        assert_eq!(opened["params"]["sandbox"], "workspace-write");
    }
}

/// The first turn of the recording.
const FIRST_TURN: &str = "0c0de000-0000-7000-8000-000000000002";

#[tokio::test]
async fn a_replys_fork_is_the_thread_through_that_turn() {
    // The copy's chat asks for the thread through the reply's turn.
    let mut replay = replay();
    let mut lines = STREAM.lines();
    replay.connect(&tag(
        "",
        Launch {
            resume: Resume::Session {
                id: "other".into(),
                fork: true,
            },
            resume_at: "t1".into(),
            ..Default::default()
        },
    ));
    replay.play(&mut lines, "@skills/list");
    assert_eq!(
        replay.last_asked("thread/fork")["params"],
        json!({"threadId": "other", "lastTurnId": "t1", "approvalPolicy": "on-request",
               "approvalsReviewer": "auto_review", "sandbox": "workspace-write"})
    );

    // The source: a reply whose turn is over forks through that turn, set
    // as the chat is now and named after its source.
    let (mut replay, mut lines) = opened(Launch::default());
    replay.send("m1", "Reply with exactly: ok");
    replay.play(&mut lines, "turn/completed");
    let launch = replay.fork("msg_04").unwrap();
    assert_eq!(
        (
            launch.resume,
            launch.resume_at.as_str(),
            launch.name.as_str()
        ),
        (
            Resume::Session {
                id: THREAD.into(),
                fork: true,
            },
            FIRST_TURN,
            "Fix tests (fork)"
        )
    );
    assert_eq!(launch.model, "gpt-5.6-luna");
    // While the next turn runs, that reply still forks; the next does not.
    let ask = "Ask me, with the request_user_input tool, whether I prefer red or blue, \
               then reply with just the color.";
    replay.send("m2", ask);
    replay.play(&mut lines, "item/tool/requestUserInput");
    assert_eq!(replay.fork("msg_04").unwrap().resume_at, FIRST_TURN);
    assert_eq!(replay.fork("m2").unwrap_err().code, "chat_turn_busy");

    // A message sent into a running turn joins it: the reply before it is
    // still that turn's, which the server takes no fork of.
    let (mut replay, _lines) = opened(Launch::default());
    replay.send("m1", "start");
    let started = json!({"method": "turn/started", "params": {
        "threadId": THREAD, "turn": {"id": "t1", "status": "inProgress"}}});
    replay.agent(&started.to_string());
    replay.send("m2", "and also");
    assert_eq!(replay.fork("m1").unwrap_err().code, "chat_turn_busy");
}

#[tokio::test]
async fn a_thread_that_cannot_be_opened_says_why() {
    let mut replay = replay();
    let mut lines = STREAM.lines();
    replay.connect(&tag(THREAD, Launch::default()));
    replay.play(&mut lines, "@skills/list");
    let refused =
        json!({"id": "@thread/resume", "error": {"code": -32600, "message": "no rollout found"}});
    replay.agent(&refused.to_string());
    let state = &replay.client.state;
    assert_eq!(state.connection, i32::from(Connection::Failed));
    assert_eq!(state.failure, "no rollout found");
}

#[tokio::test]
async fn options_go_with_the_next_turn() {
    let (mut replay, _lines) = opened(Launch {
        directories: strings(&["/a"]),
        ..Default::default()
    });
    replay
        .command(SessionCommand::SetOptions(v1::ChatOptions {
            model: "gpt-6.1-sol".into(),
            effort: "xhigh".into(),
            permission: Permission::Full.into(),
            speed: Speed::Standard.into(),
        }))
        .unwrap();
    // Shown at once: there is nothing to ask the server until a turn.
    let options = replay.client.state.options.clone().unwrap();
    assert_eq!(options.model, "gpt-6.1-sol");
    assert_eq!(options.permission, i32::from(Permission::Full));
    replay.send("m1", "go");
    let turn = replay.last_asked("turn/start")["params"].clone();
    assert_eq!(
        turn,
        json!({
            "threadId": THREAD,
            "input": [{"type": "text", "text": "go"}],
            "clientUserMessageId": "m1",
            "model": "gpt-6.1-sol",
            "effort": "xhigh",
            "serviceTier": "default",
            "approvalPolicy": "never",
            "approvalsReviewer": "user",
            "sandboxPolicy": {"type": "dangerFullAccess"},
        })
    );
    // The permission is said once: the thread keeps it.
    let turn_id = "0c0de000-0000-7000-8000-000000000002";
    replay.agent(&json!({"id": "@turn/start", "result": {"turn": {"id": turn_id}}}).to_string());
    let completed = json!({"method": "turn/completed", "params": {
        "threadId": THREAD, "turn": {"id": turn_id, "status": "completed"}}});
    replay.agent(&completed.to_string());
    replay.send("m2", "again");
    assert!(
        replay.last_asked("turn/start")["params"]
            .get("approvalPolicy")
            .is_none()
    );

    // Back to checked, with the folders the agent was given.
    replay.agent(&json!({"id": "@turn/start", "result": {"turn": {"id": "t2"}}}).to_string());
    let completed = json!({"method": "turn/completed", "params": {
        "threadId": THREAD, "turn": {"id": "t2", "status": "completed"}}});
    replay.agent(&completed.to_string());
    replay
        .command(SessionCommand::SetOptions(v1::ChatOptions {
            permission: Permission::Auto.into(),
            ..Default::default()
        }))
        .unwrap();
    replay.send("m3", "and again");
    let turn = &replay.last_asked("turn/start")["params"];
    assert_eq!(turn["approvalPolicy"], "on-request");
    assert_eq!(turn["approvalsReviewer"], "auto_review");
    assert_eq!(turn["sandboxPolicy"]["type"], "workspaceWrite");
    assert_eq!(turn["sandboxPolicy"]["writableRoots"], json!(["/a"]));
}

#[tokio::test]
async fn a_thread_keeps_its_service_tier_until_a_speed_is_chosen() {
    let (mut replay, _lines) = opened(Launch::default());
    replay.send("m1", "go");
    assert!(
        replay.last_asked("turn/start")["params"]
            .get("serviceTier")
            .is_none()
    );
}

#[tokio::test]
async fn a_stop_asked_before_the_turn_has_an_id_is_sent_when_it_has_one() {
    let (mut replay, _lines) = opened(Launch::default());
    replay.send("m1", "start");
    replay.command(SessionCommand::Interrupt).unwrap();
    // Nothing can be gone back past either, until the turn is known.
    let refused = replay.command(SessionCommand::Rewind {
        item_id: "m1".into(),
        after_item_id: String::new(),
    });
    assert_eq!(refused.unwrap_err().code, "chat_turn_busy");
    replay.agent(&json!({"id": "@turn/start", "result": {"turn": {"id": "t1"}}}).to_string());
    let interrupt = &replay.last_asked("turn/interrupt")["params"];
    assert_eq!(interrupt["turnId"], "t1");
    assert_eq!(interrupt["threadId"], THREAD);
}

#[tokio::test]
async fn a_message_sent_during_a_turn_joins_it() {
    let (mut replay, _lines) = opened(Launch::default());
    replay.send("m1", "start");
    // The turn has no id yet: the next message waits for one.
    replay.send("m2", "and also");
    assert_eq!(
        replay.last_asked("turn/start")["params"]["clientUserMessageId"],
        "m1"
    );
    let started = json!({"method": "turn/started", "params": {
        "threadId": THREAD, "turn": {"id": "t1", "status": "inProgress"}}});
    replay.agent(&started.to_string());
    assert_eq!(
        replay.last_asked("turn/steer")["params"],
        json!({"threadId": THREAD, "expectedTurnId": "t1", "clientUserMessageId": "m2",
               "input": [{"type": "text", "text": "and also"}]})
    );
    // Only the message that opened the turn can be gone back to.
    assert!(replay.client.item("m1").unwrap().rewindable);
    assert!(!replay.client.item("m2").unwrap().rewindable);
    let refused = replay
        .command(SessionCommand::Rewind {
            item_id: "m2".into(),
            after_item_id: String::new(),
        })
        .unwrap_err();
    assert_eq!(refused.code, "chat_item_not_rewindable");

    // One that reaches the server after the turn has ended opens the next.
    replay.send("m3", "one more");
    let completed = json!({"method": "turn/completed", "params": {
        "threadId": THREAD, "turn": {"id": "t1", "status": "completed"}}});
    replay.agent(&completed.to_string());
    replay.agent(&json!({"id": "@turn/steer", "result": {}}).to_string());
    let late = json!({"id": "@turn/steer", "error": {"code": -32600, "message": "no active turn"}});
    replay.agent(&late.to_string());
    assert_eq!(
        replay.last_asked("turn/start")["params"]["clientUserMessageId"],
        "m3"
    );
    assert_eq!(replay.client.state.turn, i32::from(Turn::Working));
}

#[tokio::test]
async fn a_skill_named_at_the_start_of_a_message_is_handed_over_as_the_skill() {
    let (mut replay, _lines) = opened(Launch::default());
    replay.send("m1", "/release-notes since v1.2");
    assert_eq!(
        replay.last_asked("turn/start")["params"]["input"],
        json!([
            {"type": "skill", "name": "release-notes",
             "path": "/work/.codex/skills/release-notes/SKILL.md"},
            {"type": "text", "text": "since v1.2"},
        ])
    );
}

#[tokio::test]
async fn skills_named_with_a_dollar_anywhere_are_handed_over_beside_the_text() {
    let (mut replay, _lines) = opened(Launch::default());
    let text =
        "Write them up with $release-notes. Not $unknown, a$release-notes or $release-notes again";
    replay.send("m1", text);
    assert_eq!(
        replay.last_asked("turn/start")["params"]["input"],
        json!([
            {"type": "text", "text": text},
            {"type": "skill", "name": "release-notes",
             "path": "/work/.codex/skills/release-notes/SKILL.md"},
        ])
    );
}

#[tokio::test]
async fn a_skill_named_with_a_slash_and_again_with_a_dollar_is_one() {
    let (mut replay, _lines) = opened(Launch::default());
    replay.send("m1", "/release-notes and $release-notes");
    assert_eq!(
        replay.last_asked("turn/start")["params"]["input"],
        json!([
            {"type": "skill", "name": "release-notes",
             "path": "/work/.codex/skills/release-notes/SKILL.md"},
            {"type": "text", "text": "and $release-notes"},
        ])
    );
}

#[tokio::test]
async fn what_the_server_asks_is_answered_or_refused() {
    let (mut replay, _lines) = opened(Launch::default());
    // The account's tokens are not the chat's to give.
    let refresh = json!({"id": 7, "method": "account/chatgptAuthTokens/refresh", "params": {}});
    replay.agent(&refresh.to_string());
    assert_eq!(
        replay.written.last().unwrap(),
        &json!({"id": 7, "error": {"code": -32601, "message": "Not supported by this client."}})
    );
    assert!(replay.client.state.pending.is_none());

    let ask = |id: Value, method: &str, params: Value| {
        json!({"id": id, "method": method, "params": params}).to_string()
    };
    let answer = |allow| v1::ChatAnswer {
        allow,
        ..Default::default()
    };
    // Two at once are shown one at a time, oldest first.
    replay.agent(&ask(
        json!("a"),
        "item/fileChange/requestApproval",
        json!({"reason": "Edit a.rs"}),
    ));
    let permissions = json!({"fileSystem": {"write": ["/elsewhere"]}});
    replay.agent(&ask(
        json!(8),
        "item/permissions/requestApproval",
        json!({"reason": "Write outside the workspace", "permissions": permissions}),
    ));
    let pending = replay.client.state.pending.clone().unwrap();
    assert_eq!(
        (
            pending.request_id.as_str(),
            pending.title.as_str(),
            pending.detail.as_str()
        ),
        ("a", "Change files", "Edit a.rs")
    );
    // The one behind it cannot be answered yet.
    let early = replay
        .command(SessionCommand::Answer {
            request_id: "8".into(),
            answer: answer(true),
        })
        .unwrap_err();
    assert_eq!(early.code, "chat_request_not_pending");
    replay
        .command(SessionCommand::Answer {
            request_id: "a".into(),
            answer: answer(false),
        })
        .unwrap();
    assert_eq!(
        replay.written.last().unwrap(),
        &json!({"id": "a", "result": {"decision": "decline"}})
    );
    let pending = replay.client.state.pending.clone().unwrap();
    assert_eq!(
        (pending.request_id.as_str(), pending.title.as_str()),
        ("8", "Grant access")
    );
    replay
        .command(SessionCommand::Answer {
            request_id: "8".into(),
            answer: answer(true),
        })
        .unwrap();
    assert_eq!(
        replay.written.last().unwrap(),
        &json!({"id": 8, "result": {"permissions": permissions}})
    );
    assert!(replay.client.state.pending.is_none());

    // A request the server withdraws is no longer asked.
    replay.agent(&ask(
        json!(9),
        "item/fileChange/requestApproval",
        json!({"reason": "Edit b.rs"}),
    ));
    assert!(replay.client.state.pending.is_some());
    let resolved = json!({"method": "serverRequest/resolved", "params": {
        "threadId": THREAD, "requestId": 9}});
    replay.agent(&resolved.to_string());
    assert!(replay.client.state.pending.is_none());

    // A question with no choices takes the user's own words; one skipped is
    // answered with nothing.
    let question = json!({"questions": [{"id": "why", "header": "Why", "question": "Why?"}]});
    replay.agent(&ask(
        json!(10),
        "item/tool/requestUserInput",
        question.clone(),
    ));
    let pending = replay.client.state.pending.clone().unwrap();
    assert!(pending.questions[0].allow_other && pending.questions[0].options.is_empty());
    replay
        .command(SessionCommand::Answer {
            request_id: "10".into(),
            answer: v1::ChatAnswer {
                answers: vec![v1::ChatQuestionAnswer {
                    question_id: "why".into(),
                    option_ids: Vec::new(),
                    other_text: " because ".into(),
                }],
                ..Default::default()
            },
        })
        .unwrap();
    assert_eq!(
        replay.written.last().unwrap(),
        &json!({"id": 10, "result": {"answers": {"why": {"answers": ["because"]}}}})
    );
    replay.agent(&ask(json!(11), "item/tool/requestUserInput", question));
    replay
        .command(SessionCommand::Answer {
            request_id: "11".into(),
            answer: v1::ChatAnswer {
                skipped: true,
                ..Default::default()
            },
        })
        .unwrap();
    assert_eq!(
        replay.written.last().unwrap(),
        &json!({"id": 11, "result": {"answers": {}}})
    );
}

#[tokio::test]
async fn a_turn_that_fails_says_why_and_another_threads_events_are_not_this_chats() {
    let (mut replay, _lines) = opened(Launch::default());
    replay.send("m1", "go");
    replay.agent(&json!({"id": "@turn/start", "result": {"turn": {"id": "t1"}}}).to_string());
    let other = json!({"method": "item/started", "params": {"threadId": "another", "turnId": "x",
        "item": {"type": "agentMessage", "id": "theirs", "text": "not here"}}});
    replay.agent(&other.to_string());
    assert!(replay.client.item("theirs").is_none());

    let running = json!({"method": "item/started", "params": {"threadId": THREAD, "turnId": "t1",
        "item": {"type": "commandExecution", "id": "exec", "status": "inProgress",
                 "command": "sleep 100"}}});
    replay.agent(&running.to_string());
    let failed = json!({"method": "turn/completed", "params": {"threadId": THREAD,
        "turn": {"id": "t1", "status": "failed", "error": {"message": "usage limit reached"}}}});
    replay.agent(&failed.to_string());
    replay.client.assert_settled();
    let notice = replay.client.items.last().unwrap();
    assert_eq!(notice.kind, i32::from(Kind::Notice));
    assert_eq!(notice.text, "usage limit reached");
    let stopped = replay.client.item("exec").unwrap().tool.clone().unwrap();
    assert_eq!(stopped.status, i32::from(Status::Failed));

    // The server goes: the chat ends, and takes nothing more.
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

// --- compacting and subagents ----------------------------------------------
//
// Composed from the 0.169.0 schema: neither was seen live.

fn item_event(method: &str, item: Value) -> String {
    json!({"method": method, "params": {"threadId": THREAD, "turnId": "t1", "item": item}})
        .to_string()
}

#[tokio::test]
async fn compacting_is_said_while_it_runs_and_its_notice_comes_when_it_has() {
    let (mut replay, _lines) = opened(Launch::default());
    replay.send("m1", "go");
    replay.agent(&json!({"id": "@turn/start", "result": {"turn": {"id": "t1"}}}).to_string());
    let compaction = json!({"type": "contextCompaction", "id": "cc1"});
    replay.agent(&item_event("item/started", compaction.clone()));
    assert!(replay.client.state.compacting);
    assert!(
        replay.client.item("cc1").is_none(),
        "nothing has been compacted yet"
    );
    replay.agent(&item_event("item/completed", compaction.clone()));
    assert!(!replay.client.state.compacting);
    let notice = replay.client.item("cc1").unwrap();
    assert_eq!(
        (notice.kind, notice.text.as_str()),
        (i32::from(Kind::Notice), "Context compacted")
    );

    // A turn that ends while it compacts ends the compacting too.
    replay.agent(&item_event(
        "item/started",
        json!({"type": "contextCompaction", "id": "cc2"}),
    ));
    assert!(replay.client.state.compacting);
    let completed = json!({"method": "turn/completed", "params": {
        "threadId": THREAD, "turn": {"id": "t1", "status": "completed"}}});
    replay.agent(&completed.to_string());
    assert!(!replay.client.state.compacting);
}

#[tokio::test]
async fn a_subagent_is_its_spawns_row_and_the_calls_after_it_say_how_it_is() {
    let (mut replay, _lines) = opened(Launch::default());
    replay.send("m1", "go");
    replay.agent(&json!({"id": "@turn/start", "result": {"turn": {"id": "t1"}}}).to_string());
    let call = |id: &str, tool: &str, status: &str, state: &str| {
        json!({"type": "collabAgentToolCall", "id": id, "tool": tool, "status": status,
               "senderThreadId": THREAD, "receiverThreadIds": ["sub-1"],
               "prompt": if tool == "spawnAgent" { "Find where uploads are staged\nand list each caller" } else { "" },
               "agentsStates": {"sub-1": {"status": state}}})
    };
    replay.agent(&item_event(
        "item/started",
        call("spawn", "spawnAgent", "inProgress", "pendingInit"),
    ));
    replay.agent(&item_event(
        "item/completed",
        call("spawn", "spawnAgent", "completed", "running"),
    ));
    let row = |replay: &Replay<Codex>| replay.client.item("spawn").unwrap().tool.clone().unwrap();
    let spawned = row(&replay);
    assert_eq!(spawned.kind, i32::from(ToolKind::Subagent));
    assert_eq!(
        spawned.detail,
        "Find where uploads are staged\nand list each caller"
    );
    // Codex names no type and counts nothing: the line under it is its state.
    assert_eq!(
        (
            spawned.status,
            spawned.activity.as_str(),
            spawned.agent_type.as_str()
        ),
        (i32::from(Status::Running), "running", "")
    );
    assert_eq!((spawned.tool_uses, spawned.tokens), (0, 0));

    // Waiting on it is no row; its end is the spawn's.
    replay.agent(&item_event(
        "item/started",
        call("wait-1", "wait", "inProgress", "running"),
    ));
    assert!(replay.client.item("wait-1").is_none());
    replay.agent(&item_event(
        "item/completed",
        call("wait-1", "wait", "completed", "completed"),
    ));
    assert!(replay.client.item("wait-1").is_none());
    let done = row(&replay);
    assert_eq!(
        (
            done.status,
            done.result_summary.as_str(),
            done.activity.as_str()
        ),
        (i32::from(Status::Ok), "done", "")
    );
    // A call about a subagent this chat never saw spawned changes nothing.
    let stranger = json!({"type": "collabAgentToolCall", "id": "w2", "tool": "wait",
        "status": "completed", "receiverThreadIds": ["sub-9"],
        "agentsStates": {"sub-9": {"status": "errored"}}});
    replay.agent(&item_event("item/completed", stranger));
    assert!(replay.client.item("w2").is_none());
    let completed = json!({"method": "turn/completed", "params": {
        "threadId": THREAD, "turn": {"id": "t1", "status": "completed"}}});
    replay.agent(&completed.to_string());
    replay.client.assert_settled();

    // Nothing to stop: a Codex chat lists no background tasks.
    let refused = replay
        .command(SessionCommand::StopTask {
            task_id: "sub-1".into(),
        })
        .unwrap_err();
    assert_eq!(refused.code, "chat_task_not_found");
}

// --- commands put in the background ------------------------------------------
//
// Seen live on 0.169.0: such a command is still in progress when its turn
// completes, with no `item/completed`; that comes once it ends, naming the
// turn it started in.

#[tokio::test]
async fn a_command_put_in_the_background_runs_on_past_its_turn_until_its_own_end() {
    let (mut replay, _lines) = opened(Launch::default());
    let command = |id: &str, turn: &str, status: &str, exit: Value| {
        json!({"method": if status == "inProgress" { "item/started" } else { "item/completed" },
            "params": {"threadId": THREAD, "turnId": turn, "item": {
                "type": "commandExecution", "id": id,
                "command": "/usr/bin/bash -lc 'sleep 120'", "cwd": "/work",
                "processId": "50638", "source": "unifiedExecStartup", "status": status,
                "commandActions": [{"type": "unknown", "command": "sleep 120"}],
                "aggregatedOutput": null, "exitCode": exit, "startedAtMs": 1794934273998_i64}}})
        .to_string()
    };
    let completed = |turn: &str| {
        json!({"method": "turn/completed", "params": {
            "threadId": THREAD, "turn": {"id": turn, "status": "completed"}}})
        .to_string()
    };
    let status = |replay: &Replay<Codex>, id: &str| {
        replay
            .client
            .item(id)
            .unwrap()
            .tool
            .as_ref()
            .unwrap()
            .status
    };
    let listed = |replay: &Replay<Codex>| -> Vec<(String, i32, bool)> {
        let tasks = &replay.client.state.background;
        tasks
            .iter()
            .map(|task| (task.id.clone(), task.kind, task.stoppable))
            .collect()
    };
    replay.send("m1", "start it and leave it running");
    replay.agent(&json!({"id": "@turn/start", "result": {"turn": {"id": "t1"}}}).to_string());
    // One that ends in its turn is no background one.
    replay.agent(&command("exec-0", "t1", "inProgress", Value::Null));
    replay.agent(&command("exec-0", "t1", "completed", json!(0)));
    replay.agent(&command("exec-1", "t1", "inProgress", Value::Null));
    replay.agent(&completed("t1"));
    assert_eq!(replay.client.state.turn, i32::from(v1::ChatTurnState::Idle));
    assert_eq!(status(&replay, "exec-0"), i32::from(Status::Ok));
    assert_eq!(status(&replay, "exec-1"), i32::from(Status::Running));
    let command_kind = i32::from(v1::ChatTaskKind::Command);
    assert_eq!(
        listed(&replay),
        [("exec-1".to_owned(), command_kind, false)]
    );

    // Its end comes after its turn, and names it.
    replay.agent(&command("exec-1", "t1", "failed", json!(-1)));
    assert_eq!(status(&replay, "exec-1"), i32::from(Status::Failed));
    assert!(listed(&replay).is_empty());
    replay.client.assert_settled();

    // A new link does not know that one still runs.
    replay.send("m2", "and another");
    replay.agent(&json!({"id": "@turn/start", "result": {"turn": {"id": "t2"}}}).to_string());
    replay.agent(&command("exec-2", "t2", "inProgress", Value::Null));
    replay.agent(&completed("t2"));
    assert_eq!(listed(&replay).len(), 1);
    replay.disconnect();
    replay.connect(&tag("", Launch::default()));
    assert!(listed(&replay).is_empty());
    assert_ne!(status(&replay, "exec-2"), i32::from(Status::Running));
}

// --- attachments ------------------------------------------------------------
//
// Composed from the 0.169.0 schema's `UserInput`: not tried live.

#[tokio::test]
async fn an_image_goes_as_a_local_image_and_a_file_as_its_path_under_the_text() {
    let (mut replay, _lines) = opened(Launch::default());
    let staged = "/home/u/.cache/muxflow/uploads";
    let file = |name: &str, media_type: &str| Attached {
        path: format!("{staged}/{name}"),
        name: name.into(),
        media_type: media_type.into(),
        size: 3,
        inline: None,
    };
    replay
        .command(SessionCommand::Send {
            text: "What broke?".into(),
            item_id: "m-files".into(),
            attachments: vec![file("shot.png", "image/png"), file("crash.log", "")],
        })
        .unwrap();
    let written = replay.written.last().unwrap();
    assert_eq!(written["method"], "turn/start");
    assert_eq!(
        written["params"]["input"],
        json!([
            {"type": "text", "text": format!("What broke?\n\n{staged}/crash.log")},
            {"type": "localImage", "path": format!("{staged}/shot.png")},
        ])
    );
    let item = replay.client.item("m-files").unwrap();
    assert_eq!(item.text, "What broke?");
    assert_eq!(item.attachments.len(), 2);

    // An image with blank space for its text goes alone: no blank text.
    assert_eq!(
        Codex::default().input_with(" \n ", &[file("shot.png", "image/png")]),
        json!([{"type": "localImage", "path": format!("{staged}/shot.png")}])
    );

    // The thread's record of it reads back as it was sent.
    let staging = attach::staging_directory().unwrap();
    let recorded = json!({"type": "userMessage", "id": "u1", "clientId": "m-files", "content": [
        {"type": "text", "text": format!("What broke?\n\n{}", staging.join("crash.log").display())},
        {"type": "localImage", "path": staging.join("shot.png")},
    ]});
    let read = thread_item(&recorded, true).unwrap();
    assert_eq!(read.text, "What broke?");
    let names: Vec<(&str, &str)> = read
        .attachments
        .iter()
        .map(|a| (a.name.as_str(), a.media_type.as_str()))
        .collect();
    assert_eq!(names, [("crash.log", ""), ("shot.png", "image/png")]);
}

// --- names ------------------------------------------------------------------

fn asked_title(replay: &Replay<Codex>) -> naming::Ask {
    match replay.background.last() {
        Some(Job::Title(ask)) => ask.clone(),
        _ => panic!("no title was asked for"),
    }
}

/// The thread's first message has a title asked for beside the chat, of the
/// thread's model, and the server is asked to name the thread with it.
#[tokio::test]
async fn a_thread_with_no_name_is_named_after_its_first_message() {
    let (mut replay, _lines) = opened(Launch::default());
    assert!(replay.background.is_empty());
    replay.send("m1", "Fix the agents bar");
    let ask = asked_title(&replay);
    let model = replay.client.state.options.clone().unwrap().model;
    assert!(!model.is_empty());
    assert_eq!(ask.answer_to, Some("-o"));
    assert_eq!(ask.first, "Fix the agents bar");
    let mut argv = strings(&[
        "codex",
        "exec",
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
        "--model",
    ]);
    argv.push(model);
    assert_eq!(ask.argv, argv);

    replay.done(Done::Titled(naming::Titled {
        ticket: ask.ticket,
        asked_by_user: false,
        title: Some("Agents bar".into()),
    }));
    assert_eq!(replay.titles.last().unwrap(), "Agents bar");
    assert_eq!(
        replay.last_asked("thread/name/set")["params"],
        json!({"threadId": THREAD, "name": "Agents bar"})
    );
    let titled = replay.titles.len();
    let updated = json!({"method": "thread/name/updated", "params": {
        "threadId": THREAD, "threadName": "Agents bar"}});
    replay.agent(&updated.to_string());
    assert_eq!(replay.titles.len(), titled);
}

/// `/rename` names the thread and sends nothing to the agent's turn.
#[tokio::test]
async fn rename_names_the_thread_without_a_turn() {
    let (mut replay, _lines) = opened(Launch::default());
    replay.send("r1", "/rename Agents bar");
    assert_eq!(
        replay.client.item("r1").unwrap().kind,
        i32::from(Kind::Notice)
    );
    assert_eq!(
        replay.last_asked("thread/name/set")["params"],
        json!({"threadId": THREAD, "name": "Agents bar"})
    );
    assert!(
        replay
            .written
            .iter()
            .all(|written| written["method"] != "turn/start")
    );
    assert_eq!(replay.titles, ["Agents bar"]);
    // The server renaming it later is heard.
    let updated = json!({"method": "thread/name/updated", "params": {
        "threadId": THREAD, "threadName": "Renamed elsewhere"}});
    replay.agent(&updated.to_string());
    assert_eq!(replay.titles.last().unwrap(), "Renamed elsewhere");
    let rename = replay
        .client
        .catalog
        .commands
        .iter()
        .find(|command| command.name == "rename");
    assert_eq!(
        rename.map(|command| command.kind),
        Some(i32::from(v1::ChatCommandKind::Command))
    );
}

/// Every MCP server the agent's configuration names is turned off for an ask
/// for a title, by a `-c` the agent takes.
#[test]
fn an_ask_for_a_title_turns_off_the_configured_mcp_servers() {
    let home = tempfile::tempdir().unwrap();
    let codex = home.path().join("codex-home");
    std::fs::create_dir_all(&codex).unwrap();
    std::fs::write(
        codex.join("config.toml"),
        "model = \"m\"\n[mcp_servers.cua-driver]\ncommand = \"x\"\n\
         [mcp_servers.slack]\ncommand = \"y\"\n[mcp_servers.slack.env]\nA = \"1\"\n\
         [mcp_servers.\"odd name\"]\ncommand = \"z\"\n",
    )
    .unwrap();
    let environment = |name: &str, value: &Path| vec![(name.into(), value.as_os_str().to_owned())];
    let off = strings(&[
        "-c",
        "mcp_servers.cua-driver.enabled=false",
        "-c",
        "mcp_servers.slack.enabled=false",
    ]);
    assert_eq!(mcp_servers_off(&environment("CODEX_HOME", &codex)), off);
    // Without CODEX_HOME, where the agent looks under its home.
    std::fs::rename(&codex, home.path().join(".codex")).unwrap();
    assert_eq!(mcp_servers_off(&environment("HOME", home.path())), off);
    assert!(mcp_servers_off(&environment("HOME", &home.path().join("none"))).is_empty());
    assert!(mcp_servers_off(&[]).is_empty());
}

/// The answer that opens `thread` on the server, with `name`.
fn thread_open(asked: &str, name: Value) -> String {
    json!({"id": format!("@{asked}"), "result": {
        "thread": {"id": THREAD, "name": name, "turns": []}, "model": "m"}})
    .to_string()
}

fn initialized(replay: &mut Replay<Codex>) {
    replay.agent(&json!({"id": "@initialize", "result": {}}).to_string());
}

/// The thread's own name comes before the one the chat was started under,
/// but a new thread, a copy among them, goes by the one it was started
/// under: a copy may carry its source's name.
#[tokio::test]
async fn a_thread_goes_by_its_own_name_and_a_new_one_by_its_start() {
    let named = Launch {
        name: "Fix tests (fork)".into(),
        resume: Resume::Session {
            id: "0c0de000-0000-7000-8000-00000000aaaa".into(),
            fork: true,
        },
        ..Launch::default()
    };
    let mut copy = replay();
    copy.connect(&tag("", named.clone()));
    initialized(&mut copy);
    copy.agent(&thread_open("thread/fork", json!("Source name")));
    assert_eq!(copy.titles, ["Fix tests (fork)"]);
    assert_eq!(
        copy.last_asked("thread/name/set")["params"],
        json!({"threadId": THREAD, "name": "Fix tests (fork)"})
    );

    let mut resumed = replay();
    resumed.connect(&tag(THREAD, named));
    initialized(&mut resumed);
    resumed.agent(&thread_open("thread/resume", json!("Mine")));
    assert_eq!(resumed.titles, ["Mine"]);
    assert!(
        resumed
            .written
            .iter()
            .all(|written| written["method"] != "thread/name/set")
    );
}

/// A message sent before the thread is open is named from once it is.
#[tokio::test]
async fn a_title_waits_for_the_thread_to_open() {
    let mut replay = replay();
    replay.connect(&tag("", Launch::default()));
    replay.send("m1", "Fix the agents bar");
    assert!(replay.background.is_empty());
    initialized(&mut replay);
    replay.agent(&thread_open("thread/start", Value::Null));
    assert_eq!(asked_title(&replay).first, "Fix the agents bar");
}

/// An ask for a title is started with the chat's own settings, before its
/// own, which win; not with what could have it write or keep a session.
#[test]
fn an_ask_for_a_title_keeps_the_chats_settings_but_not_its_risky_ones() {
    let launch = launch(&[
        "-c",
        "model_provider=work",
        "--enable",
        "web_search",
        "-c",
        "sandbox_mode=danger-full-access",
        "--config=features.hooks=true",
        "-c",
        "notify=[\"x\"]",
        "-c",
        "profile=work",
        "--disable",
        "hooks",
        "--strict-config",
    ])
    .unwrap();
    assert_eq!(
        title_passthrough(&launch.passthrough),
        strings(&[
            "-c",
            "model_provider=work",
            "-c",
            "profile=work",
            "--strict-config",
        ])
    );
}
