//! `muxflow-host chat-pipe`: what runs in a chat pane in place of the agent's
//! terminal UI (docs/design/chat-plan.md §3).
//!
//! `chat-pipe [--env FILE] [--socket PATH] -- <argv>` starts `<argv>` in the
//! environment the agent was started in, which the daemon wrote to `FILE` and
//! which is deleted as soon as it is read.
//!
//! With `--socket` the child's stdin and stdout become a unix socket the daemon
//! connects to. Output produced while no daemon is connected is kept, up to
//! [`RING_BYTES`], and handed to the next one that connects, so a turn goes on
//! while the daemon restarts. What a daemon had taken and not yet handled when
//! it went is not sent again; the agent's own record of the conversation is
//! what covers that. The pipe carries bytes and knows nothing of what they
//! mean, so an old pipe and a new daemon cannot disagree about a protocol.
//!
//! Without `--socket` the child speaks to the daemon on a socket of its own,
//! and this process only waits for it.
//!
//! Either way the pane is still a terminal somebody can type into. While the
//! agent runs, what is typed there is neither shown nor turned into a signal,
//! and it is thrown away when the agent ends, so that the shell that follows
//! never runs it.

use std::{
    collections::VecDeque,
    ffi::OsString,
    os::unix::{ffi::OsStringExt, fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, bail};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        UnixListener, UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    signal::unix::{SignalKind, signal},
    sync::mpsc::UnboundedSender,
    task::JoinHandle,
};

/// The most output kept for a daemon that is not connected.
const RING_BYTES: usize = 4 * 1024 * 1024;
/// How long one write to the daemon may take before the daemon counts as
/// gone. A daemon that stopped reading must not stop the agent.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the last output of an agent that has ended is waited for. A
/// process it started may hold its output open for as long as it likes.
const LAST_WORDS_TIMEOUT: Duration = Duration::from_secs(2);
/// How long an agent asked to stop has to end by itself, and end what it
/// started, before it is killed.
const STOP_WAIT: Duration = Duration::from_secs(3);
const USAGE: &str = "usage: muxflow-host chat-pipe [--env FILE] [--socket PATH] -- <command>";

struct Options {
    environment: Option<PathBuf>,
    socket: Option<PathBuf>,
    command: Vec<OsString>,
}

fn parse(arguments: Vec<OsString>) -> anyhow::Result<Options> {
    let mut options = Options {
        environment: None,
        socket: None,
        command: Vec::new(),
    };
    let mut rest = arguments.into_iter();
    while let Some(argument) = rest.next() {
        match argument.to_str() {
            Some("--env") => options.environment = rest.next().map(PathBuf::from),
            Some("--socket") => options.socket = rest.next().map(PathBuf::from),
            Some("--") => {
                options.command = rest.collect();
                break;
            }
            _ => bail!(USAGE),
        }
    }
    if options.command.is_empty() {
        bail!(USAGE);
    }
    Ok(options)
}

/// The variables in `path`, written the way `/proc/<pid>/environ` is: each
/// `NAME=value` ends with a NUL. The file is gone once this returns.
fn take_environment(path: &Path) -> anyhow::Result<Vec<(OsString, OsString)>> {
    let bytes = std::fs::read(path);
    let _ = std::fs::remove_file(path);
    Ok(parse_environment(
        &bytes.context("read the chat environment")?,
    ))
}

pub(crate) fn parse_environment(bytes: &[u8]) -> Vec<(OsString, OsString)> {
    bytes
        .split(|byte| *byte == 0)
        .filter_map(|record| {
            let equals = record.iter().position(|byte| *byte == b'=')?;
            let (name, value) = record.split_at(equals);
            (!name.is_empty()).then(|| {
                (
                    OsString::from_vec(name.to_vec()),
                    OsString::from_vec(value[1..].to_vec()),
                )
            })
        })
        .collect()
}

/// The agent's environment, in the pane `pane` this runs in (the
/// `TMUX_PANE` tmux gave it). A reply's fork is started with its source's
/// agent's environment, which names the source's pane: the fork's hooks
/// would say its turns are the source's, and its session would be taken for
/// the one running there.
fn in_this_pane(
    mut environment: Vec<(OsString, OsString)>,
    pane: Option<OsString>,
) -> Vec<(OsString, OsString)> {
    if let Some(pane) = pane {
        environment.retain(|(name, _)| name != "TMUX_PANE");
        environment.push(("TMUX_PANE".into(), pane));
    }
    environment
}

pub(crate) async fn run(arguments: Vec<OsString>) -> anyhow::Result<()> {
    let options = parse(arguments)?;
    let mut command = std::process::Command::new(&options.command[0]);
    command.args(&options.command[1..]);
    // A pane with no environment file keeps the one tmux gave it.
    if let Some(path) = &options.environment
        && let Ok(environment) = take_environment(path)
    {
        let environment = in_this_pane(environment, std::env::var_os("TMUX_PANE"));
        command.env_clear().envs(environment);
    }
    println!("Muxflow chat session");
    let terminal = Terminal::quiet();
    let code = match &options.socket {
        Some(socket) => {
            let code = bridge(command, socket).await;
            let _ = std::fs::remove_file(socket);
            code
        }
        None => attend(command).await,
    };
    // Leaving through `exit` runs no destructor.
    drop(terminal);
    std::process::exit(code?);
}

/// The pane's terminal, made deaf for as long as this lives: nothing typed is
/// echoed, and no key is a signal or stops the output.
struct Terminal {
    saved: Option<libc::termios>,
}

impl Terminal {
    fn quiet() -> Self {
        let mut state = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: `tcgetattr` fills the struct it is given when it returns 0,
        // and fails without touching memory when fd 0 is not a terminal.
        let saved = unsafe {
            (libc::tcgetattr(libc::STDIN_FILENO, state.as_mut_ptr()) == 0)
                .then(|| state.assume_init())
        };
        if let Some(saved) = &saved {
            let mut quiet = *saved;
            quiet.c_lflag &= !(libc::ISIG | libc::ECHO);
            quiet.c_iflag &= !libc::IXON;
            // SAFETY: `quiet` is a whole `termios` read from this terminal.
            unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &quiet) };
        }
        Self { saved }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if let Some(saved) = &self.saved {
            // SAFETY: fd 0 is the terminal `saved` was read from.
            unsafe {
                libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH);
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, saved);
            }
        }
    }
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

/// Asks the agent to stop, as closing a terminal would, so that it ends the
/// commands it started; kills it once [`STOP_WAIT`] has passed.
async fn stop(child: &mut tokio::process::Child) -> std::io::Result<std::process::ExitStatus> {
    if let Some(pid) = child.id().and_then(|pid| libc::pid_t::try_from(pid).ok()) {
        // SAFETY: kill(2) takes any pid; this one is the child's, not yet
        // reaped, since `id` still answers.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    if let Ok(status) = tokio::time::timeout(STOP_WAIT, child.wait()).await {
        return status;
    }
    let _ = child.start_kill();
    child.wait().await
}

/// Runs a child that talks to the daemon by itself, until it ends or the pane
/// closes. Returns the exit code to leave with.
async fn attend(command: std::process::Command) -> anyhow::Result<i32> {
    let mut child = tokio::process::Command::from(command)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("start the chat agent")?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        status = child.wait() => return Ok(exit_code(status.context("wait for the chat agent")?)),
        _ = hangup.recv() => {}
        _ = terminate.recv() => {}
    }
    let status = stop(&mut child).await.context("wait for the chat agent")?;
    Ok(exit_code(status))
}

/// Output waiting for a daemon. Past its bound the oldest bytes go, and what
/// is left then starts in the middle of a line.
#[derive(Default)]
struct Ring {
    bytes: VecDeque<u8>,
    cut: bool,
}

impl Ring {
    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend(chunk);
        let excess = self.bytes.len().saturating_sub(RING_BYTES);
        if excess > 0 {
            self.bytes.drain(..excess);
            self.cut = true;
        }
    }

    /// Everything kept, from the first whole line on.
    fn take(&mut self) -> Vec<u8> {
        let mut bytes: Vec<u8> = std::mem::take(&mut self.bytes).into();
        if std::mem::take(&mut self.cut) {
            let line_end = bytes.iter().position(|byte| *byte == b'\n');
            bytes.drain(..line_end.map_or(bytes.len(), |end| end + 1));
        }
        bytes
    }
}

async fn send(daemon: &mut OwnedWriteHalf, bytes: &[u8]) -> bool {
    matches!(
        tokio::time::timeout(WRITE_TIMEOUT, daemon.write_all(bytes)).await,
        Ok(Ok(()))
    )
}

/// The daemon that is connected.
struct Daemon {
    write: OwnedWriteHalf,
    /// Passes what it says on to the agent, until it goes.
    reading: JoinHandle<()>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.reading.abort();
    }
}

/// What the daemon says goes to the agent in a task of its own, so that it is
/// read whatever else the pipe is waiting for: a daemon that waits to be read
/// while the pipe waits for that daemon to read would stop them both.
async fn forward(mut daemon: OwnedReadHalf, to_child: UnboundedSender<Vec<u8>>) {
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        match daemon.read(&mut buffer).await {
            Ok(count) if count > 0 => {
                if to_child.send(buffer[..count].to_vec()).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

/// Hands the agent's output to the daemon, or keeps it for the next one.
async fn deliver(daemon: &mut Option<Daemon>, ring: &mut Ring, chunk: &[u8]) {
    let delivered = match daemon {
        Some(daemon) => send(&mut daemon.write, chunk).await,
        None => false,
    };
    if !delivered {
        *daemon = None;
        ring.push(chunk);
    }
}

/// Runs the child between its own pipes and whichever daemon is connected.
/// Returns the exit code to leave with.
async fn bridge(command: std::process::Command, socket: &Path) -> anyhow::Result<i32> {
    crate::paths::check_socket_path_length(socket)?;
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket).context("bind the chat socket")?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;

    let mut command = tokio::process::Command::from(command);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("start the chat agent")?;
    // The agent's input is written from a task of its own: an agent that
    // waits for its output to be read before it reads more must never find
    // this loop waiting for it in turn.
    let (to_child, mut for_child) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let mut child_input = child.stdin.take().context("the agent has no input")?;
    let writer = tokio::spawn(async move {
        while let Some(bytes) = for_child.recv().await {
            if child_input.write_all(&bytes).await.is_err() {
                // The agent closed its input: nothing more can reach it.
                break;
            }
        }
    });
    let mut child_output = child.stdout.take().context("the agent has no output")?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut terminate = signal(SignalKind::terminate())?;

    let mut ring = Ring::default();
    let mut daemon: Option<Daemon> = None;
    let mut from_child = vec![0_u8; 64 * 1024];
    let ended = loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                // A daemon that connects takes over from the one before it.
                let (read, write) = UnixStream::into_split(stream);
                let reading = tokio::spawn(forward(read, to_child.clone()));
                daemon = Some(Daemon { write, reading });
                let kept = ring.take();
                deliver(&mut daemon, &mut ring, &kept).await;
            }
            read = child_output.read(&mut from_child) => {
                match read {
                    Ok(count) if count > 0 => {
                        deliver(&mut daemon, &mut ring, &from_child[..count]).await;
                    }
                    _ => break None,
                }
            }
            _ = async {
                match &mut daemon {
                    Some(daemon) => {
                        let _ = (&mut daemon.reading).await;
                    }
                    None => std::future::pending().await,
                }
            } => daemon = None,
            status = child.wait() => break Some(status),
            _ = hangup.recv() => break None,
            _ = terminate.recv() => break None,
        }
    };
    let status = match ended {
        // The agent ended by itself: what it said last is still on its way.
        Some(status) => {
            let last_words = async {
                while let Ok(count) = child_output.read(&mut from_child).await
                    && count > 0
                {
                    deliver(&mut daemon, &mut ring, &from_child[..count]).await;
                }
            };
            let _ = tokio::time::timeout(LAST_WORDS_TIMEOUT, last_words).await;
            status
        }
        // The agent's output ended, or the pane is closing: either way it
        // goes.
        None => stop(&mut child).await,
    };
    drop(to_child);
    writer.abort();
    Ok(exit_code(status.context("wait for the chat agent")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_environment_file_reads_like_proc_environ() {
        let variables = parse_environment(b"PATH=/a:/b\0EMPTY=\0X=1=2\0=bad\0noequals\0");
        let pairs: Vec<_> = variables
            .iter()
            .map(|(name, value)| (name.to_str().unwrap(), value.to_str().unwrap()))
            .collect();
        assert_eq!(pairs, [("PATH", "/a:/b"), ("EMPTY", ""), ("X", "1=2")]);
    }

    /// A reply's fork runs with its source's environment, in a pane of its
    /// own: its agent is told that pane, or its hooks land on the source.
    #[test]
    fn the_agent_is_told_the_pane_it_runs_in() {
        let pairs = |environment: Vec<(OsString, OsString)>| -> Vec<(String, String)> {
            environment
                .into_iter()
                .map(|(name, value)| (name.into_string().unwrap(), value.into_string().unwrap()))
                .collect()
        };
        let source = parse_environment(b"PATH=/a\0TMUX_PANE=%4\0TMUX=/s,1,0\0");
        assert_eq!(
            pairs(in_this_pane(source.clone(), Some("%9".into()))),
            [
                ("PATH".into(), "/a".into()),
                ("TMUX".into(), "/s,1,0".into()),
                ("TMUX_PANE".into(), "%9".into()),
            ]
        );
        // Outside tmux there is no pane to say: the environment is as given.
        assert_eq!(pairs(in_this_pane(source.clone(), None)), pairs(source));
    }

    #[test]
    fn a_full_ring_starts_again_at_a_whole_line() {
        let mut ring = Ring::default();
        ring.push(b"first\n");
        assert_eq!(ring.take(), b"first\n");

        ring.push(&vec![b'a'; RING_BYTES]);
        ring.push(b"tail\nwhole\n");
        // The oldest bytes went, so the first line kept is a fragment.
        assert_eq!(ring.take(), b"whole\n");
        assert!(ring.take().is_empty());
    }

    /// The real thing, with `cat` as the agent: what the daemon writes comes
    /// back, and what the agent says while no daemon is connected is kept for
    /// the next one.
    #[tokio::test]
    async fn output_with_no_daemon_connected_reaches_the_next_one() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("pipe.sock");
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "echo ready; exec cat"]);
        let bridged = {
            let socket = socket.clone();
            tokio::spawn(async move { bridge(command, &socket).await })
        };
        let connect = || async {
            for _ in 0..200 {
                if let Ok(stream) = UnixStream::connect(&socket).await {
                    return stream;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("the pipe never listened");
        };
        let read_line = |stream: UnixStream| async move {
            let mut reader = tokio::io::BufReader::new(stream);
            let mut line = String::new();
            tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line)
                .await
                .unwrap();
            (line, reader.into_inner())
        };

        // Said before anyone connected.
        let (line, mut first) = read_line(connect().await).await;
        assert_eq!(line, "ready\n");
        first.write_all(b"one\n").await.unwrap();
        let (line, mut first) = read_line(first).await;
        assert_eq!(line, "one\n");

        // The daemon goes while the agent is still answering.
        first.write_all(b"two\n").await.unwrap();
        first.shutdown().await.unwrap();
        drop(first);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (line, second) = read_line(connect().await).await;
        assert_eq!(line, "two\n");

        drop(second);
        bridged.abort();
    }

    /// An agent that writes more than a pipe holds before it reads anything,
    /// to a daemon that writes more than a pipe holds before it reads
    /// anything: neither waits for the other for ever.
    #[tokio::test]
    async fn neither_side_can_stall_the_other() {
        const BYTES: usize = 1024 * 1024;
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("pipe.sock");
        let mut command = std::process::Command::new("sh");
        command.args([
            "-c",
            &format!("head -c {BYTES} /dev/zero; head -c {BYTES} | wc -c"),
        ]);
        let bridged = {
            let socket = socket.clone();
            tokio::spawn(async move { bridge(command, &socket).await })
        };
        let mut stream = loop {
            if let Ok(stream) = UnixStream::connect(&socket).await {
                break stream;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let exchange = async {
            stream.write_all(&vec![b'x'; BYTES]).await.unwrap();
            let mut said = Vec::new();
            let mut buffer = vec![0_u8; 64 * 1024];
            while !said.ends_with(b"\n") {
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0, "the agent ended early");
                said.extend_from_slice(&buffer[..count]);
            }
            said
        };
        let said = tokio::time::timeout(Duration::from_secs(8), exchange)
            .await
            .expect("the pipe stalled");
        let (zeros, counted) = said.split_at(BYTES);
        assert!(zeros.iter().all(|byte| *byte == 0));
        assert_eq!(
            String::from_utf8_lossy(counted).trim(),
            BYTES.to_string(),
            "the agent did not get all that was written to it"
        );
        // The agent ends by itself, and the pipe with it.
        assert_eq!(bridged.await.unwrap().unwrap(), 0);
    }

    /// An agent that has ended is known to have ended, even while a process
    /// it started still holds its output open.
    #[tokio::test]
    async fn an_agent_that_ended_is_not_waited_for() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("pipe.sock");
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "sleep 30 & echo last; exit 3"]);
        let code = tokio::time::timeout(Duration::from_secs(10), bridge(command, &socket))
            .await
            .expect("the pipe waited for a process that was not the agent");
        assert_eq!(code.unwrap(), 3);
    }

    /// An agent told to stop is asked, not killed: it ends the way it chooses.
    #[tokio::test]
    async fn an_agent_told_to_stop_ends_by_itself() {
        let mut child = tokio::process::Command::new("sh")
            .args([
                "-c",
                "trap 'exit 7' TERM; echo ready; while :; do sleep 0.05; done",
            ])
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut ready = [0_u8; 6];
        child
            .stdout
            .as_mut()
            .unwrap()
            .read_exact(&mut ready)
            .await
            .unwrap();
        let status = tokio::time::timeout(Duration::from_secs(2), stop(&mut child))
            .await
            .expect("the agent was not asked to stop")
            .unwrap();
        assert_eq!(status.code(), Some(7));
    }
}
