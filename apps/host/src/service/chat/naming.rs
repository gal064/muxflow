//! What a chat is called. A headless agent names none of its conversations:
//! Claude Code run with `-p` writes no title, and a thread `codex app-server`
//! starts has no name. So once the first message of a chat with no name is
//! sent, the chat asks the agent's own command line, once and beside the
//! conversation, for a short title. The answer is handed to the agent as the
//! user's own rename, so that the agent keeps it and says it again the next
//! time the conversation is opened.
//!
//! A message that is `/rename`, or `/rename` and a name, is not sent to the
//! agent: it renames the chat, or names it again from its first message.

use std::{
    ffi::OsString,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tmux_agent_protocol::v1;

use super::{convert, items};

/// The longest title asked for, in characters.
pub(super) const MAX_TITLE_CHARS: usize = 48;
/// How much of the first message the agent is shown.
const MAX_REQUEST_CHARS: usize = 4000;
/// The most of an answer that is read. A title is one line.
const MAX_ANSWER_BYTES: u64 = 64 * 1024;
/// How long to wait before asking again, after each failed ask.
const RETRIES: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(4)];
/// How often a running ask is looked at.
const POLL: Duration = Duration::from_millis(50);
/// How long an agent that has ended may leave its output open, at least:
/// something it started and left running can hold it open for good.
const OUTPUT_GRACE: Duration = Duration::from_secs(2);
/// What the command is called in the composer's list.
const COMMAND: &str = "rename";

/// One title is asked for at a time on this host: a daemon that starts finds
/// every chat it adopts at once, and each ask is a whole agent starting.
static ASKING: Mutex<()> = Mutex::new(());

/// The name a message that is the rename command gives the chat: empty for
/// `/rename` alone, which names it again from its first message. `None` for
/// any other message.
pub(super) fn rename(text: &str) -> Option<&str> {
    let rest = text.trim().strip_prefix('/')?.strip_prefix(COMMAND)?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

/// The rename command as the composer lists it. It is no command of
/// Muxflow's own kind: the clients take those for side questions.
pub(super) fn command() -> v1::ChatCommand {
    v1::ChatCommand {
        name: COMMAND.to_owned(),
        description: "Rename this chat, or name it again from its first message".to_owned(),
        argument_hint: "[name]".to_owned(),
        kind: v1::ChatCommandKind::Command.into(),
    }
}

/// The agent's commands for the catalog, behind the rename command. One of
/// its own by that name would never reach it.
pub(super) fn with_command(
    commands: impl IntoIterator<Item = v1::ChatCommand>,
) -> impl Iterator<Item = v1::ChatCommand> {
    std::iter::once(command()).chain(
        commands
            .into_iter()
            .filter(|command| command.name != COMMAND),
    )
}

/// What the agent is asked, about the chat's first message.
pub(super) fn prompt(first: &str) -> String {
    let first = first.trim();
    let first = match first.char_indices().nth(MAX_REQUEST_CHARS) {
        Some((end, _)) => &first[..end],
        None => first,
    };
    format!(
        "Name this chat from the user's first request.\n\
         Write a short, specific title of at most {MAX_TITLE_CHARS} characters.\n\
         Output only the title, with no heading, quotes, or explanation.\n\
         \n\
         First request:\n\
         {first}\n\
         \n\
         This is a naming-only task. Do not edit files, run commands, use tools, or perform the first request."
    )
}

/// A title from what the agent answered: its first line that says anything,
/// without a heading's `#`s, the emphasis, quotes or `Title:` around it, on
/// one line and cut to `max_chars`. `None` when nothing is left, or when the
/// answer is the model's thinking.
pub(super) fn sanitize(text: &str, max_chars: usize) -> Option<String> {
    if text.to_lowercase().contains("<think>") {
        return None;
    }
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let mut title = line.trim_start_matches('#').trim();
    const WRAPPERS: [(&str, &str); 7] = [
        ("**", "**"),
        ("__", "__"),
        ("\"", "\""),
        ("'", "'"),
        ("`", "`"),
        ("\u{201c}", "\u{201d}"),
        ("\u{2018}", "\u{2019}"),
    ];
    loop {
        let before = title;
        if title
            .get(..6)
            .is_some_and(|label| label.eq_ignore_ascii_case("title:"))
        {
            title = title[6..].trim();
        }
        for (open, close) in WRAPPERS {
            if title.len() >= open.len() + close.len()
                && let Some(inside) = title
                    .strip_prefix(open)
                    .and_then(|rest| rest.strip_suffix(close))
            {
                title = inside.trim();
            }
        }
        if title == before {
            break;
        }
    }
    typed(title, max_chars)
}

/// A name as the user typed it after `/rename`, kept as typed: on one line,
/// its runs of blank space and control characters one space, and cut to
/// `max_chars`. `None` when nothing is left.
pub(super) fn typed(text: &str, max_chars: usize) -> Option<String> {
    let name = text
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let name = match name.char_indices().nth(max_chars) {
        Some((end, _)) => name[..end].trim_end().to_owned(),
        None => name,
    };
    (!name.is_empty()).then_some(name)
}

/// The notice that stands for a rename in the timeline, under the id the
/// client sent its message with: the client then knows it arrived.
pub(super) fn notice(item_id: String, text: &str) -> v1::ChatItem {
    let mut item = items::text(item_id, v1::ChatItemKind::Notice, text);
    item.at_unix_millis = super::now_unix_millis();
    item
}

/// What the chat says when a title the user asked for could not be had.
pub(super) const NOT_NAMED: &str = "This chat could not be named. Try /rename and a name.";

/// What a notice of a rename says.
pub(super) fn renamed(name: &str) -> String {
    if name.is_empty() {
        "Naming this chat from its first message.".to_owned()
    } else {
        format!("Renamed to \u{201c}{name}\u{201d}.")
    }
}

/// Counts the names a chat has been given. A title is asked for under the
/// count as it was, and is wanted while the count is still that: the asks
/// that wait their turn look at it too, so that one overtaken by a name, or
/// whose session has gone, starts no agent.
#[derive(Debug, Clone, Default)]
pub(super) struct Given(Arc<AtomicU64>);

impl Given {
    /// What the count is once the session has gone: no ticket is ever that.
    const GONE: u64 = u64::MAX;

    fn get(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// The title `ticket` was asked for is still wanted.
    pub(super) fn wants(&self, ticket: u64) -> bool {
        ticket != Self::GONE && self.get() == ticket
    }
}

/// What a session knows of its chat's name.
#[derive(Debug, Default)]
pub(super) struct Naming {
    /// The name the chat goes by; empty while it has none.
    pub(super) name: String,
    given: Given,
    /// A title has been asked for without the user asking.
    asked: bool,
    /// A name chosen here that the agent has not been told yet: it was not
    /// listening when the name was chosen.
    pub(super) unsent: Option<String>,
}

impl Naming {
    /// Takes a name the chat has from elsewhere: its start, the agent.
    /// Answers whether it is another than the one it had.
    pub(super) fn heard(&mut self, name: &str) -> bool {
        if name == self.name {
            return false;
        }
        name.clone_into(&mut self.name);
        self.given.next();
        true
    }

    /// The ticket of a title to ask for now, from the chat's first message
    /// `first`: once per session, and only for a chat with no name.
    pub(super) fn wants_title(&mut self, first: Option<&str>) -> Option<u64> {
        if !self.name.is_empty() || self.asked || first.is_none_or(|first| first.trim().is_empty())
        {
            return None;
        }
        self.asked = true;
        Some(self.given.get())
    }

    /// The ticket of a title the user asked for, whatever the chat is
    /// called now.
    pub(super) fn retitle(&mut self) -> u64 {
        self.asked = true;
        self.given.next()
    }

    /// The chat has not been named since the title `ticket` was asked for.
    pub(super) fn still_wanted(&self, ticket: u64) -> bool {
        self.given.wants(ticket)
    }

    /// The count the asks for a title look at.
    pub(super) fn given(&self) -> Given {
        self.given.clone()
    }

    /// The agent has gone: no title asked for is wanted any more.
    pub(super) fn stop(&self) {
        self.given.0.store(Given::GONE, Ordering::Release);
    }
}

impl Drop for Naming {
    /// Its session has gone, with the chat.
    fn drop(&mut self) {
        self.stop();
    }
}

/// One title to ask an agent for: by starting `argv` with the environment
/// of the pane's agent, and handing it the prompt on its stdin.
#[derive(Debug, Clone)]
pub(super) struct Ask {
    pub(super) pane_id: String,
    pub(super) adapter_id: &'static str,
    /// What runs the agent and the arguments that have it answer once.
    pub(super) argv: Vec<String>,
    /// The option that names the file the agent writes its answer to,
    /// last on its command line. `None` for one that answers on stdout.
    pub(super) answer_to: Option<&'static str>,
    /// More arguments, from the agent's environment: what turns off the
    /// MCP servers its configuration names, for an agent that has no
    /// option for that.
    pub(super) more_arguments: fn(&[(OsString, OsString)]) -> Vec<String>,
    pub(super) timeout: Duration,
    pub(super) first: String,
    pub(super) ticket: u64,
    pub(super) given: Given,
    /// The user asked, with `/rename`, and is told when it fails.
    pub(super) asked_by_user: bool,
}

/// [`Ask::more_arguments`] for an agent that needs none.
pub(super) fn no_more_arguments(_: &[(OsString, OsString)]) -> Vec<String> {
    Vec::new()
}

/// What came of an [`Ask`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Titled {
    pub(super) ticket: u64,
    pub(super) asked_by_user: bool,
    /// `None` when no title could be had.
    pub(super) title: Option<String>,
}

/// Asks for the title, a few times, waiting longer before each. Blocks for
/// as long as that takes.
pub(super) fn run(ask: Ask) -> Titled {
    // One folder for every ask, so that an agent that keeps a note of each
    // folder it ran in has one more, not one per ask.
    use std::os::unix::fs::DirBuilderExt;
    let folder = crate::paths::state_dir().join("chat-titles");
    let made = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&folder);
    let title = match (
        made,
        convert::agent_environment(&ask.pane_id, ask.adapter_id),
    ) {
        (Ok(()), Some(environment)) => title(&ask, &environment, &RETRIES, &folder),
        (Err(error), _) => {
            eprintln!(
                "chat title for {}: no folder to ask in: {error}",
                ask.pane_id
            );
            None
        }
        (_, None) => {
            eprintln!("chat title for {}: the agent is not running", ask.pane_id);
            None
        }
    };
    Titled {
        ticket: ask.ticket,
        asked_by_user: ask.asked_by_user,
        title,
    }
}

/// [`run`], with the agent's environment read, and the waits between tries
/// and the folder it runs in given.
fn title(
    ask: &Ask,
    environment: &[(OsString, OsString)],
    retries: &[Duration],
    folder: &Path,
) -> Option<String> {
    let prompt = prompt(&ask.first);
    let waits = std::iter::once(Duration::ZERO).chain(retries.iter().copied());
    for (attempt, wait) in waits.enumerate() {
        std::thread::sleep(wait);
        let answer = {
            let _one_at_a_time = ASKING.lock().unwrap_or_else(PoisonError::into_inner);
            // It may have waited its turn long enough to be overtaken.
            if !ask.given.wants(ask.ticket) {
                return None;
            }
            once(ask, environment, &prompt, folder)
        };
        match answer.and_then(|answer| {
            sanitize(&answer, MAX_TITLE_CHARS).ok_or_else(|| "it gave no title".to_owned())
        }) {
            Ok(title) => return Some(title),
            Err(error) => eprintln!(
                "chat title for {} (try {}): {error}",
                ask.pane_id,
                attempt + 1
            ),
        }
    }
    None
}

/// Starts the agent once, in `folder`, and answers what it wrote.
fn once(
    ask: &Ask,
    environment: &[(OsString, OsString)],
    prompt: &str,
    folder: &Path,
) -> Result<String, String> {
    let answer_file = folder.join(format!("answer-{}.txt", uuid::Uuid::new_v4().simple()));
    let answer = asked(ask, environment, prompt, folder, &answer_file);
    let _ = std::fs::remove_file(&answer_file);
    answer
}

fn asked(
    ask: &Ask,
    environment: &[(OsString, OsString)],
    prompt: &str,
    folder: &Path,
    answer_file: &Path,
) -> Result<String, String> {
    use std::os::unix::process::CommandExt;
    let (program, arguments) = ask
        .argv
        .split_first()
        .ok_or_else(|| "nothing to run".to_owned())?;
    let mut command = Command::new(program);
    command.args(arguments);
    command.args((ask.more_arguments)(environment));
    if let Some(option) = ask.answer_to {
        command.arg(option).arg(answer_file);
    }
    // Not the pane's: the agent's hooks would report this run as the pane's
    // agent.
    let environment = environment
        .iter()
        .filter(|(name, _)| name != "TMUX" && name != "TMUX_PANE");
    command
        .env_clear()
        .envs(environment.map(|(name, value)| (name, value)))
        .current_dir(folder)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Its own group, so that what it starts goes with it on a timeout.
        .process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start {program}: {error}"))?;
    // Neither thread is waited on for longer than the ask may take: what the
    // agent started may hold its input or output open after it has gone.
    let mut stdin = child.stdin.take();
    let prompt = prompt.to_owned();
    std::thread::spawn(move || {
        if let Some(stdin) = stdin.as_mut() {
            let _ = stdin.write_all(prompt.as_bytes());
        }
    });
    let stdout = child.stdout.take();
    let (output, said) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut read = Vec::new();
        if let Some(stdout) = stdout {
            let _ = stdout.take(MAX_ANSWER_BYTES).read_to_end(&mut read);
        }
        let _ = output.send(read);
    });
    let group = child.id() as libc::pid_t;
    // SAFETY: signals the process group this function started.
    let stop_group = || unsafe {
        libc::kill(-group, libc::SIGKILL);
    };
    let deadline = Instant::now() + ask.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            // Overtaken by a name, or its session gone: stopped as it runs.
            Ok(None) if Instant::now() < deadline && ask.given.wants(ask.ticket) => {
                std::thread::sleep(POLL);
            }
            outcome => {
                stop_group();
                let _ = child.kill();
                let _ = child.wait();
                return Err(match outcome {
                    Err(error) => format!("could not wait for it: {error}"),
                    _ if !ask.given.wants(ask.ticket) => "no longer wanted".to_owned(),
                    _ => format!("no answer in {} s", ask.timeout.as_secs()),
                });
            }
        }
    };
    let left = deadline.saturating_duration_since(Instant::now());
    let Ok(said) = said.recv_timeout(left.max(OUTPUT_GRACE)) else {
        stop_group();
        return Err("it ended and left its output open".to_owned());
    };
    if !status.success() {
        return Err(format!("it ended with {status}"));
    }
    match ask.answer_to {
        Some(_) => {
            let mut answer = String::new();
            std::fs::File::open(answer_file)
                .and_then(|file| file.take(MAX_ANSWER_BYTES).read_to_string(&mut answer))
                .map_err(|error| format!("could not read its answer: {error}"))?;
            Ok(answer)
        }
        None => Ok(String::from_utf8_lossy(&said).into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_rename_command_is_one() {
        assert_eq!(rename("/rename"), Some(""));
        assert_eq!(rename("  /rename  "), Some(""));
        assert_eq!(rename("/rename Fix the bar "), Some("Fix the bar"));
        assert_eq!(rename("/rename\nFix the bar"), Some("Fix the bar"));
        assert_eq!(rename("/renamed"), None);
        assert_eq!(rename("/renameit"), None);
        assert_eq!(rename("rename this"), None);
        assert_eq!(rename("please /rename"), None);
        assert_eq!(rename("/btw rename"), None);
    }

    #[test]
    fn an_answer_is_cut_down_to_its_title() {
        let title = |text: &str| sanitize(text, MAX_TITLE_CHARS);
        assert_eq!(
            title("Fix the agents bar").as_deref(),
            Some("Fix the agents bar")
        );
        assert_eq!(
            title("\n\n  # Fix the bar\nmore").as_deref(),
            Some("Fix the bar")
        );
        assert_eq!(title("**Fix the bar**").as_deref(), Some("Fix the bar"));
        assert_eq!(title("__Fix the bar__").as_deref(), Some("Fix the bar"));
        assert_eq!(title("\"Fix the bar\"").as_deref(), Some("Fix the bar"));
        assert_eq!(title("`Fix the bar`").as_deref(), Some("Fix the bar"));
        assert_eq!(
            title("\u{201c}Fix the bar\u{201d}").as_deref(),
            Some("Fix the bar")
        );
        assert_eq!(title("Title: Fix the bar").as_deref(), Some("Fix the bar"));
        assert_eq!(
            title("**Title: \"Fix the bar\"**").as_deref(),
            Some("Fix the bar")
        );
        assert_eq!(
            title("## title: 'Fix the bar'").as_deref(),
            Some("Fix the bar")
        );
        assert_eq!(title("Fix\tthe   bar").as_deref(), Some("Fix the bar"));
        assert_eq!(title("It's Bob's bar").as_deref(), Some("It's Bob's bar"));
        assert_eq!(title("<think>hmm</think>Fix the bar"), None);
        assert_eq!(title("   \n # \n"), None);
        assert_eq!(title("\"\""), None);
        assert_eq!(title(""), None);
        let long = "é".repeat(60);
        assert_eq!(title(&long), Some("é".repeat(MAX_TITLE_CHARS)));
        assert_eq!(
            title("A title that runs on well past the forty-eight characters").as_deref(),
            Some("A title that runs on well past the forty-eight c")
        );
    }

    #[test]
    fn the_prompt_carries_the_first_message_cut_to_its_bound() {
        let prompt = prompt("  Fix the bar.  ");
        assert!(prompt.starts_with("Name this chat from the user's first request.\n"));
        assert!(prompt.contains("at most 48 characters.\n"));
        assert!(prompt.contains("\n\nFirst request:\nFix the bar.\n\nThis is a naming-only task."));
        assert!(prompt.ends_with("or perform the first request."));
        let long = super::prompt(&"x".repeat(5000));
        assert!(long.contains(&format!("\n{}\n", "x".repeat(MAX_REQUEST_CHARS))));
    }

    #[test]
    fn a_title_is_asked_for_once_and_dropped_when_the_chat_was_named_meanwhile() {
        let mut naming = Naming::default();
        assert_eq!(naming.wants_title(None), None);
        let ticket = naming.wants_title(Some("Fix the bar")).unwrap();
        assert_eq!(naming.wants_title(Some("Fix the bar")), None);
        assert!(naming.still_wanted(ticket));
        assert!(naming.heard("Named elsewhere"));
        assert!(!naming.still_wanted(ticket));

        let mut named = Naming::default();
        named.heard("Fix tests (fork)");
        assert_eq!(named.wants_title(Some("Fix the bar")), None);
        // The user can still ask.
        let ticket = named.retitle();
        assert!(named.still_wanted(ticket));
        assert!(!named.heard("Fix tests (fork)"));
        assert!(named.still_wanted(ticket));
    }

    fn script(folder: &Path, body: &str) -> Vec<String> {
        use std::os::unix::fs::PermissionsExt;
        let path = folder.join("agent");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        vec![path.to_string_lossy().into_owned(), "-p".to_owned()]
    }

    fn ask(argv: Vec<String>, answer_to: Option<&'static str>) -> Ask {
        Ask {
            pane_id: "%1".into(),
            adapter_id: "claude-code",
            argv,
            answer_to,
            more_arguments: no_more_arguments,
            timeout: Duration::from_secs(5),
            first: "Fix the bar".into(),
            ticket: 0,
            given: Given::default(),
            asked_by_user: false,
        }
    }

    /// An ask overtaken while it waited its turn, or whose session has
    /// gone, starts no agent.
    #[test]
    fn an_ask_no_longer_wanted_starts_no_agent() {
        let folder = tempfile::tempdir().unwrap();
        let count = folder.path().join("count");
        let argv = script(
            folder.path(),
            &format!(
                "cat > /dev/null\necho x >> '{}'\necho Title",
                count.display()
            ),
        );
        let mut naming = Naming::default();
        let ticket = naming.wants_title(Some("Fix the bar")).unwrap();
        let stale = Ask {
            ticket,
            given: naming.given(),
            ..ask(argv.clone(), None)
        };
        naming.heard("Named meanwhile");
        assert_eq!(title(&stale, &environment(), &[], folder.path()), None);

        let ticket = naming.retitle();
        let gone = Ask {
            ticket,
            given: naming.given(),
            ..ask(argv, None)
        };
        assert!(gone.given.wants(ticket));
        drop(naming);
        assert!(!gone.given.wants(ticket));
        assert_eq!(title(&gone, &environment(), &[], folder.path()), None);
        assert!(!count.exists());
    }

    /// An agent already running when its ask is overtaken is stopped, not
    /// waited on until its answer or its timeout.
    #[test]
    fn a_running_ask_overtaken_is_stopped() {
        let folder = tempfile::tempdir().unwrap();
        let argv = script(folder.path(), "cat > /dev/null\nsleep 30\necho Title");
        let given = Given::default();
        let mut running = ask(argv, None);
        running.timeout = Duration::from_secs(20);
        running.given = given.clone();
        let overtaking = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            given.next();
        });
        let started = Instant::now();
        assert_eq!(title(&running, &environment(), &[], folder.path()), None);
        assert!(started.elapsed() < Duration::from_secs(5));
        overtaking.join().unwrap();
    }

    /// What the agent's environment adds goes before the answer's file.
    #[test]
    fn more_arguments_come_from_the_agents_environment() {
        let folder = tempfile::tempdir().unwrap();
        let argv = script(
            folder.path(),
            "cat > /dev/null\n[ \"$2\" = \"--from\" ] && [ \"$3\" = \"/usr/bin:/bin\" ] && [ \"$4\" = \"-o\" ] || exit 5\necho Title > \"$5\"",
        );
        let path = |environment: &[(OsString, OsString)]| {
            let path = environment.iter().find(|(name, _)| name == "PATH");
            vec![
                "--from".to_owned(),
                path.map(|(_, value)| value.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ]
        };
        let asked = Ask {
            more_arguments: path,
            ..ask(argv, Some("-o"))
        };
        assert_eq!(
            title(&asked, &environment(), &[], folder.path()).as_deref(),
            Some("Title")
        );
    }

    fn environment() -> Vec<(OsString, OsString)> {
        vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("TMUX_PANE".into(), "%1".into()),
        ]
    }

    #[test]
    fn the_agent_is_handed_the_prompt_and_its_answer_is_the_title() {
        let folder = tempfile::tempdir().unwrap();
        // It reads the prompt, is not told it runs in the pane, and answers
        // with markdown around the title.
        let argv = script(
            folder.path(),
            "grep -q 'First request:' || exit 3\n[ -z \"$TMUX_PANE\" ] || exit 4\necho '**Fix the bar**'",
        );
        assert_eq!(
            title(&ask(argv, None), &environment(), &[], folder.path()).as_deref(),
            Some("Fix the bar")
        );

        // One that writes its answer to the file it is named.
        let argv = script(
            folder.path(),
            "cat > /dev/null\nprintf 'Fix the bar\\n' > \"$3\"",
        );
        assert_eq!(
            title(&ask(argv, Some("-o")), &environment(), &[], folder.path()).as_deref(),
            Some("Fix the bar")
        );
        // It ran in the folder it was given, and left nothing there.
        let mut left: Vec<_> = std::fs::read_dir(folder.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(left, ["agent"]);
    }

    /// Something the agent started and left running holds its output open:
    /// the ask fails in bounded time instead of waiting on it.
    #[test]
    fn an_agent_that_leaves_its_output_open_is_not_waited_on() {
        let folder = tempfile::tempdir().unwrap();
        let argv = script(
            folder.path(),
            "cat > /dev/null\nsleep 30 &\necho 'Fix the bar'",
        );
        let mut held = ask(argv, None);
        held.timeout = Duration::from_millis(300);
        let started = Instant::now();
        assert_eq!(title(&held, &environment(), &[], folder.path()), None);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_typed_name_is_kept_as_typed() {
        assert_eq!(
            typed("#123 login bug", 120).as_deref(),
            Some("#123 login bug")
        );
        assert_eq!(
            typed("  \"Quoted\"  **bold**\tTitle: x  ", 120).as_deref(),
            Some("\"Quoted\" **bold** Title: x")
        );
        assert_eq!(typed("a\nb\u{7}c", 120).as_deref(), Some("a b c"));
        assert_eq!(typed("abcdef", 3).as_deref(), Some("abc"));
        assert_eq!(typed(" \n\t ", 120), None);
    }

    #[test]
    fn an_agent_that_fails_is_asked_again_and_then_given_up_on() {
        let folder = tempfile::tempdir().unwrap();
        let count = folder.path().join("count");
        let argv = script(
            folder.path(),
            &format!("cat > /dev/null\necho x >> '{}'\nexit 1", count.display()),
        );
        let retries = [Duration::ZERO, Duration::ZERO];
        assert_eq!(
            title(&ask(argv, None), &environment(), &retries, folder.path()),
            None
        );
        assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 3);

        // One that does not answer in time is stopped.
        let argv = script(folder.path(), "sleep 30");
        let mut slow = ask(argv, None);
        slow.timeout = Duration::from_millis(300);
        let started = Instant::now();
        assert_eq!(title(&slow, &environment(), &[], folder.path()), None);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
