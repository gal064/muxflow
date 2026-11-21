//! The slash commands and skills of an agent running in a terminal pane, as
//! a chat lists them. A chat hears them from the agent it runs; an agent in a
//! terminal says nothing to the host. So the host starts the agent's own
//! program once beside it, in its folder and with its environment, asks it
//! what a chat would ask, and stops it. What it answered is kept for a few
//! minutes for each program and folder.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{BufRead, BufReader, Read, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Mutex, PoisonError,
        mpsc::{self, RecvTimeoutError},
    },
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tmux_agent_protocol::v1;

use super::{
    claude, codex, convert,
    items::string,
    processes::{self, MAX_ENVIRONMENT_BYTES, Processes},
};

/// How long an agent may take to answer before it is stopped.
const TIMEOUT: Duration = Duration::from_secs(15);
/// How long an answer is kept.
const KEPT_FOR: Duration = Duration::from_secs(5 * 60);
/// How long a failed ask is answered with nothing before it is tried again,
/// so an agent that hangs does not hold every terminal opened after it for
/// as long as asking it takes.
const FAILED_KEPT_FOR: Duration = Duration::from_secs(60);
/// The most answers kept at once.
const MAX_KEPT: usize = 64;
/// The most of an agent's output read for its answer. Claude Code's answer
/// to `initialize` lists its models and its account too.
const MAX_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;
/// The id Claude Code's `initialize` is asked under. One process is asked
/// once.
const REQUEST_ID: &str = "muxflow-commands";
/// Claude Code reading requests on its stdin, with no hook of its own run:
/// its `SessionStart` would tell the agent runtime that the pane's agent
/// started again. Nor any MCP server, and no session left on disk.
const CLAUDE_ARGUMENTS: &[&str] = &[
    "-p",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--settings",
    r#"{"disableAllHooks":true}"#,
    "--strict-mcp-config",
    "--no-session-persistence",
];

/// The agent to ask: whose it is, what runs it, and where and how it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    adapter_id: &'static str,
    /// The program, or an interpreter and its script.
    program: Vec<String>,
    folder: PathBuf,
    /// The agent's own, when it could be read; the host's is used otherwise.
    environment: Option<Vec<(OsString, OsString)>>,
}

type Key = (&'static str, Vec<String>, PathBuf);
/// Until when, and what.
type Kept = BTreeMap<Key, (Instant, Vec<v1::ChatCommand>)>;

static KEPT: Mutex<Kept> = Mutex::new(BTreeMap::new());
/// One agent is asked at a time on this host, so that asks for the same one
/// wait for the first answer rather than each starting it.
static ASKING: Mutex<()> = Mutex::new(());

/// The commands and skills of the agent in the pane whose own process is
/// `pane_pid`, without Muxflow's own. `current_path` is the pane's folder,
/// for an agent whose own cannot be read. Empty when the pane runs no agent
/// whose commands can be read, or when asking it failed. Blocks for as long
/// as asking takes: run it off the async runtime.
pub(crate) fn terminal_commands(pane_pid: u32, current_path: &str) -> Vec<v1::ChatCommand> {
    let Some(target) = target(&processes::system(), pane_pid, current_path) else {
        return Vec::new();
    };
    let key = (
        target.adapter_id,
        target.program.clone(),
        target.folder.clone(),
    );
    if let Some(commands) = kept(&key) {
        return commands;
    }
    let _one = ASKING.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(commands) = kept(&key) {
        return commands;
    }
    match ask(&target, TIMEOUT) {
        Ok(mut commands) => {
            super::bound_commands(&mut commands);
            keep(key, commands.clone(), KEPT_FOR);
            commands
        }
        Err(error) => {
            eprintln!(
                "terminal commands of {} in {}: {error}",
                target.adapter_id,
                target.folder.display()
            );
            keep(key, Vec::new(), FAILED_KEPT_FOR);
            Vec::new()
        }
    }
}

fn kept(key: &Key) -> Option<Vec<v1::ChatCommand>> {
    let kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    kept.get(key)
        .filter(|(until, _)| Instant::now() < *until)
        .map(|(_, commands)| commands.clone())
}

fn keep(key: Key, commands: Vec<v1::ChatCommand>, kept_for: Duration) {
    let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    let now = Instant::now();
    kept.retain(|_, (until, _)| now < *until);
    if kept.len() >= MAX_KEPT {
        kept.clear();
    }
    kept.insert(key, (now + kept_for, commands));
}

/// The agent beneath the pane's process, when it is one whose commands can be
/// read.
fn target(processes: &impl Processes, pane_pid: u32, current_path: &str) -> Option<Target> {
    let (adapter_id, agent) = convert::find_any_agent(processes, pane_pid)?;
    if ![claude::ADAPTER_ID, codex::ADAPTER_ID].contains(&adapter_id) {
        return None;
    }
    let (program, _) = convert::split_program(&agent.argv)?;
    let folder = processes
        .folder(agent.pid)
        .ok()
        .or_else(|| (!current_path.is_empty()).then(|| PathBuf::from(current_path)))?;
    let environment = processes
        .environment(agent.pid)
        .ok()
        .filter(|bytes| bytes.len() <= MAX_ENVIRONMENT_BYTES)
        .map(|bytes| crate::chat_pipe::parse_environment(&bytes));
    Some(Target {
        adapter_id,
        program: program.to_vec(),
        folder,
        environment,
    })
}

fn ask(target: &Target, timeout: Duration) -> Result<Vec<v1::ChatCommand>, String> {
    let (program, arguments) = target
        .program
        .split_first()
        .ok_or_else(|| "nothing to run".to_owned())?;
    let mut command = Command::new(program);
    command.args(arguments);
    if target.adapter_id == claude::ADAPTER_ID {
        command.args(CLAUDE_ARGUMENTS);
        let initialize = json!({
            "type": "control_request",
            "request_id": REQUEST_ID,
            "request": {"subtype": "initialize"},
        });
        converse(command, target, initialize, timeout, claude_answer)
    } else {
        command.arg("app-server");
        let version = env!("CARGO_PKG_VERSION");
        let client = json!({"name": "muxflow", "title": "Muxflow", "version": version});
        let initialize = json!({"id": 1, "method": "initialize", "params": {"clientInfo": client}});
        converse(command, target, initialize, timeout, |message| {
            codex_answer(&target.folder, message)
        })
    }
}

/// What a line of the agent's output comes to.
#[derive(Debug, PartialEq)]
enum Heard<T> {
    Nothing,
    /// Lines to write to the agent.
    Say(Vec<Value>),
    Answer(Result<T, String>),
}

/// Claude Code's commands, from its answer to `initialize`. Which of them are
/// skills it says only in a session's first turn, which is never taken here:
/// a chat lists them all as commands until then too.
fn claude_answer(message: &Value) -> Heard<Vec<v1::ChatCommand>> {
    let response = &message["response"];
    if string(message, "type") != "control_response" || string(response, "request_id") != REQUEST_ID
    {
        return Heard::Nothing;
    }
    if string(response, "subtype") != "success" {
        let error = string(response, "error");
        return Heard::Answer(Err(format!("it refused initialize: {error}")));
    }
    let commands = response["response"]["commands"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    Heard::Answer(Ok(commands.iter().map(claude::command).collect()))
}

/// The skills `codex app-server` has for `folder`, asked for once it has
/// answered `initialize`, as a chat asks.
fn codex_answer(folder: &Path, message: &Value) -> Heard<Vec<v1::ChatCommand>> {
    let failed = !message["error"].is_null();
    match message["id"].as_u64() {
        Some(1) if failed => Heard::Answer(Err("it refused initialize".to_owned())),
        Some(1) => Heard::Say(vec![
            json!({"method": "initialized", "params": {}}),
            json!({"id": 2, "method": "skills/list", "params": {"cwds": [folder]}}),
        ]),
        Some(2) if failed => Heard::Answer(Err("it refused skills/list".to_owned())),
        Some(2) => Heard::Answer(Ok(codex::skills(&message["result"])
            .iter()
            .map(|(name, description, _)| codex::skill_command(name, description))
            .collect())),
        _ => Heard::Nothing,
    }
}

/// Starts `command` as `target` runs, writes `opening` to it and reads what
/// it says, one JSON message a line, until `heard` has an answer or
/// `timeout` passes. Whatever came of it, the agent and all it started are
/// stopped and reaped before this returns.
fn converse<T>(
    mut command: Command,
    target: &Target,
    opening: Value,
    timeout: Duration,
    heard: impl FnMut(&Value) -> Heard<T>,
) -> Result<T, String> {
    if let Some(environment) = &target.environment {
        command
            .env_clear()
            .envs(environment.iter().map(|(name, value)| (name, value)));
    }
    command
        // Not the pane's: the agent's hooks would report this run as the
        // pane's agent.
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .current_dir(&target.folder)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Its own group, so that what it starts goes with it.
        .process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start {}: {error}", target.program[0]))?;
    let answer = talk(&mut child, opening, timeout, heard);
    let group = child.id() as libc::pid_t;
    // SAFETY: signals the process group this function started.
    unsafe {
        libc::kill(-group, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
    answer
}

fn talk<T>(
    child: &mut Child,
    opening: Value,
    timeout: Duration,
    mut heard: impl FnMut(&Value) -> Heard<T>,
) -> Result<T, String> {
    let deadline = Instant::now() + timeout;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err("it has no input or output".to_owned());
    };
    // Not waited on: what the agent started may hold its output open after
    // it has gone. It ends once the group is stopped.
    let (said, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout.take(MAX_OUTPUT_BYTES)).lines() {
            let Ok(line) = line else { break };
            if said.send(line).is_err() {
                break;
            }
        }
    });
    let mut say = |message: &Value| {
        writeln!(stdin, "{message}").map_err(|error| format!("could not write to it: {error}"))
    };
    say(&opening)?;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = match lines.recv_timeout(left) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => {
                return Err(format!("no answer in {} s", timeout.as_secs_f32()));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err("it ended without answering".to_owned());
            }
        };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match heard(&message) {
            Heard::Nothing => {}
            Heard::Say(messages) => messages.iter().try_for_each(&mut say)?,
            Heard::Answer(answer) => return answer,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_INITIALIZE: &str = include_str!("fixtures/claude-initialize.json");
    const CODEX_SESSION: &str = include_str!("fixtures/codex-session.stream.jsonl");

    fn names(commands: &[v1::ChatCommand]) -> Vec<(&str, v1::ChatCommandKind)> {
        commands
            .iter()
            .map(|command| (command.name.as_str(), command.kind()))
            .collect()
    }

    #[test]
    fn claude_codes_commands_are_read_from_its_answer_to_initialize() {
        let answer: Value =
            serde_json::from_str(&CLAUDE_INITIALIZE.replace("@initialize", REQUEST_ID)).unwrap();
        let Heard::Answer(Ok(commands)) = claude_answer(&answer) else {
            panic!("no commands");
        };
        let kind = v1::ChatCommandKind::Command;
        assert_eq!(
            names(&commands),
            [("compact", kind), ("context", kind), ("init", kind)]
        );
        assert_eq!(
            commands[0].argument_hint,
            "<optional custom summarization instructions>"
        );
        assert_eq!(commands[1].description, "Show current context usage");

        // Anything else it says is passed over, and a refusal is a failure.
        let other: Value = serde_json::from_str(CLAUDE_INITIALIZE).unwrap();
        assert_eq!(claude_answer(&other), Heard::Nothing);
        assert_eq!(claude_answer(&json!({"type": "system"})), Heard::Nothing);
        let refused = json!({
            "type": "control_response",
            "response": {"subtype": "error", "request_id": REQUEST_ID, "error": "no"},
        });
        assert_eq!(
            claude_answer(&refused),
            Heard::Answer(Err("it refused initialize: no".to_owned()))
        );
    }

    #[test]
    fn codex_is_asked_for_the_folders_skills_once_it_has_initialized() {
        let folder = Path::new("/work");
        let Heard::Say(said) = codex_answer(folder, &json!({"id": 1, "result": {}})) else {
            panic!("nothing asked");
        };
        assert_eq!(said[0]["method"], "initialized");
        assert_eq!(said[1]["method"], "skills/list");
        assert_eq!(said[1]["params"]["cwds"], json!(["/work"]));

        let listed = CODEX_SESSION
            .lines()
            .find(|line| line.contains("@skills/list"))
            .unwrap()
            .replace("\"@skills/list\"", "2");
        let Heard::Answer(Ok(commands)) =
            codex_answer(folder, &serde_json::from_str(&listed).unwrap())
        else {
            panic!("no skills");
        };
        assert_eq!(
            names(&commands),
            [("release-notes", v1::ChatCommandKind::Skill)]
        );
        assert_eq!(
            commands[0].description,
            "Draft release notes from the commits since the last tag"
        );

        let refused = json!({"id": 1, "error": {"message": "no"}});
        assert!(matches!(
            codex_answer(folder, &refused),
            Heard::Answer(Err(_))
        ));
        let notification = json!({"method": "account/updated", "params": {}});
        assert_eq!(codex_answer(folder, &notification), Heard::Nothing);
    }

    fn process(root: &Path, pid: u32, argv: &[&str], children: &[u32]) {
        let directory = root.join(pid.to_string());
        let task = directory.join("task").join(pid.to_string());
        std::fs::create_dir_all(&task).unwrap();
        let mut cmdline = argv.join("\0").into_bytes();
        cmdline.push(0);
        std::fs::write(directory.join("cmdline"), cmdline).unwrap();
        let children: Vec<String> = children.iter().map(u32::to_string).collect();
        std::fs::write(task.join("children"), children.join(" ")).unwrap();
    }

    #[test]
    fn the_agent_is_run_as_it_was_started_where_it_runs() {
        let root = tempfile::tempdir().unwrap();
        let proc = processes::Proc(root.path());
        process(root.path(), 10, &["-zsh"], &[11]);
        process(
            root.path(),
            11,
            &["node", "/opt/claude-code/cli.js", "--model", "opus"],
            &[],
        );
        std::fs::write(
            root.path().join("11/environ"),
            b"HOME=/home/alice\0PATH=/bin\0",
        )
        .unwrap();
        std::os::unix::fs::symlink("/work", root.path().join("11/cwd")).unwrap();
        let target = target(&proc, 10, "/elsewhere").unwrap();
        assert_eq!(
            target,
            Target {
                adapter_id: claude::ADAPTER_ID,
                program: vec!["node".into(), "/opt/claude-code/cli.js".into()],
                folder: "/work".into(),
                environment: Some(vec![
                    ("HOME".into(), "/home/alice".into()),
                    ("PATH".into(), "/bin".into()),
                ]),
            }
        );

        // A process whose folder and environment cannot be read runs in the
        // pane's folder, with the host's.
        process(root.path(), 20, &["-zsh"], &[21]);
        process(root.path(), 21, &["codex", "--yolo"], &[]);
        let target = super::target(&proc, 20, "/pane").unwrap();
        assert_eq!(target.adapter_id, codex::ADAPTER_ID);
        assert_eq!(target.program, ["codex"]);
        assert_eq!(target.folder, Path::new("/pane"));
        assert_eq!(target.environment, None);

        // No agent, or none at all.
        process(root.path(), 30, &["-zsh"], &[31]);
        process(root.path(), 31, &["vim", "notes"], &[]);
        assert_eq!(super::target(&proc, 30, "/pane"), None);
        assert_eq!(super::target(&proc, 0, "/pane"), None);
    }

    fn script(folder: &Path, body: &str) -> Target {
        use std::os::unix::fs::PermissionsExt;
        let path = folder.join("claude");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Target {
            adapter_id: claude::ADAPTER_ID,
            program: vec![path.to_string_lossy().into_owned()],
            folder: folder.to_owned(),
            environment: Some(vec![("PATH".into(), "/usr/bin:/bin".into())]),
        }
    }

    /// The process `pid_file` names is gone, within a moment: once its
    /// parent has gone, it is reaped by whoever takes it on.
    fn stopped(pid_file: &Path) -> bool {
        let pid: libc::pid_t = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        // SAFETY: signal 0 only asks whether the process is there.
        while unsafe { libc::kill(pid, 0) } == 0 {
            if Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        true
    }

    /// The agent is stopped once it has answered, and what it started with it.
    #[test]
    fn an_agent_is_stopped_once_it_answers_or_its_time_is_up() {
        let folder = tempfile::tempdir().unwrap();
        let answer = json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": REQUEST_ID,
                "response": {"commands": [{"name": "review", "description": "Review"}]},
            },
        });
        let answers = script(
            folder.path(),
            &format!(
                "sleep 60 & echo $! > started\nread line\necho '{{\"type\":\"system\"}}'\n\
                 echo '{answer}'\nwait"
            ),
        );
        let started = Instant::now();
        let commands = ask(&answers, Duration::from_secs(10)).unwrap();
        assert_eq!(names(&commands), [("review", v1::ChatCommandKind::Command)]);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(stopped(&folder.path().join("started")));

        let silent = script(folder.path(), "sleep 60 & echo $! > started\nwait");
        let started = Instant::now();
        let error = ask(&silent, Duration::from_millis(300)).unwrap_err();
        assert!(error.starts_with("no answer"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(stopped(&folder.path().join("started")));

        let ends = script(folder.path(), "read line\nexit 0");
        assert_eq!(
            ask(&ends, Duration::from_secs(10)).unwrap_err(),
            "it ended without answering"
        );
    }

    /// Runs this machine's `claude` and `codex` in this folder:
    /// `cargo test -p muxflow-host terminal_commands -- --ignored --nocapture`.
    #[test]
    #[ignore = "runs the real agents"]
    fn the_real_agents_answer() {
        let folder = std::env::current_dir().unwrap();
        for (adapter_id, program) in [(claude::ADAPTER_ID, "claude"), (codex::ADAPTER_ID, "codex")]
        {
            let target = Target {
                adapter_id,
                program: vec![program.to_owned()],
                folder: folder.clone(),
                environment: None,
            };
            let started = Instant::now();
            let commands = ask(&target, TIMEOUT).unwrap();
            println!(
                "{program}: {} commands in {:?}: {:?}",
                commands.len(),
                started.elapsed(),
                names(&commands).iter().take(8).collect::<Vec<_>>()
            );
            assert!(!commands.is_empty());
        }
    }
}
